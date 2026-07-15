//! Pure authorization and resource-accounting model for an authenticated loopback gateway.
//!
//! Listener ownership, HTTP parsing, cookie installation, and SSH channel creation belong to
//! outer adapters. This module deliberately returns an immutable tunnel destination only after
//! every local request check and authentication check succeeds.

use std::collections::HashMap;
use std::fmt;

const LOOPBACK_HOST: &str = "127.0.0.1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GatewayLifecycle {
    Open,
    Draining,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewayLimits {
    pub max_method_bytes: usize,
    pub max_target_bytes: usize,
    pub max_header_count: usize,
    pub max_header_bytes: usize,
    pub max_connections: usize,
    pub max_queued_bytes_per_connection: usize,
    pub max_queued_bytes_total: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestHeader<'a> {
    pub name: &'a str,
    pub value: &'a [u8],
}

/// Already-parsed request metadata. The adapter must remove the gateway cookie before forwarding.
pub struct GatewayRequest<'a> {
    pub method: &'a str,
    pub target: &'a str,
    pub headers: &'a [RequestHeader<'a>],
    /// Exact opaque value extracted from the one gateway authentication cookie.
    pub gateway_cookie: Option<&'a [u8]>,
}

impl fmt::Debug for GatewayRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GatewayRequest")
            .field("method", &self.method)
            .field("target_bytes", &self.target.len())
            .field("header_count", &self.headers.len())
            .field("gateway_cookie", &self.gateway_cookie.map(|_| "[REDACTED]"))
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GatewayError {
    NotAccepting,
    AuthenticationRequired,
    InvalidMethod,
    InvalidTarget,
    InvalidHeaders,
    ConnectionLimit,
    UnknownAuthorization,
    QueueLimit,
    AccountingOverflow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TunnelAuthorization {
    pub authorization_id: u64,
    pub service_id: String,
    pub destination_host: &'static str,
    pub destination_port: u16,
}

#[derive(Clone)]
struct SecretToken(Vec<u8>);

impl fmt::Debug for SecretToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretToken([REDACTED])")
    }
}

#[derive(Debug)]
pub struct Gateway {
    service_id: String,
    registered_port: u16,
    token: SecretToken,
    limits: GatewayLimits,
    lifecycle: GatewayLifecycle,
    next_authorization_id: u64,
    queued_bytes_total: usize,
    active: HashMap<u64, usize>,
}

impl Gateway {
    /// Creates an open gateway for one exact registered service and destination port.
    ///
    /// # Errors
    ///
    /// Returns a stable error for an empty identity, port zero, empty token, or unusable limits.
    pub fn new(
        service_id: String,
        registered_port: u16,
        token: Vec<u8>,
        limits: GatewayLimits,
    ) -> Result<Self, GatewayError> {
        if service_id.is_empty()
            || registered_port == 0
            || token.is_empty()
            || limits.max_method_bytes == 0
            || limits.max_target_bytes == 0
            || limits.max_connections == 0
        {
            return Err(GatewayError::InvalidHeaders);
        }
        Ok(Self {
            service_id,
            registered_port,
            token: SecretToken(token),
            limits,
            lifecycle: GatewayLifecycle::Open,
            next_authorization_id: 1,
            queued_bytes_total: 0,
            active: HashMap::new(),
        })
    }

    #[must_use]
    pub const fn lifecycle(&self) -> GatewayLifecycle {
        self.lifecycle
    }

    #[must_use]
    pub fn active_connections(&self) -> usize {
        self.active.len()
    }

    #[must_use]
    pub const fn queued_bytes(&self) -> usize {
        self.queued_bytes_total
    }

    /// Validates and authenticates a request before reserving a tunnel slot.
    ///
    /// # Errors
    ///
    /// Returns a stable validation or authentication error before reserving any resources,
    /// `ConnectionLimit` when the bounded connection pool is full, or `AccountingOverflow` when
    /// no further authorization identifier can be represented.
    pub fn authorize(
        &mut self,
        request: &GatewayRequest<'_>,
    ) -> Result<TunnelAuthorization, GatewayError> {
        if self.lifecycle != GatewayLifecycle::Open {
            return Err(GatewayError::NotAccepting);
        }
        self.validate_request(request)?;
        let Some(cookie) = request.gateway_cookie else {
            return Err(GatewayError::AuthenticationRequired);
        };
        if !constant_time_equal(cookie, &self.token.0) {
            return Err(GatewayError::AuthenticationRequired);
        }
        if self.active.len() >= self.limits.max_connections {
            return Err(GatewayError::ConnectionLimit);
        }

        let id = self.next_authorization_id;
        self.next_authorization_id = id.checked_add(1).ok_or(GatewayError::AccountingOverflow)?;
        self.active.insert(id, 0);
        Ok(TunnelAuthorization {
            authorization_id: id,
            service_id: self.service_id.clone(),
            destination_host: LOOPBACK_HOST,
            destination_port: self.registered_port,
        })
    }

    /// Reserves bounded buffered bytes for an authenticated live connection.
    ///
    /// # Errors
    ///
    /// Returns `UnknownAuthorization` for a stale identifier, `QueueLimit` without changing
    /// accounting when either byte bound would be exceeded, or `AccountingOverflow` if the
    /// requested addition cannot be represented.
    pub fn reserve_queued_bytes(&mut self, id: u64, bytes: usize) -> Result<(), GatewayError> {
        let current = *self
            .active
            .get(&id)
            .ok_or(GatewayError::UnknownAuthorization)?;
        let connection_total = current
            .checked_add(bytes)
            .ok_or(GatewayError::AccountingOverflow)?;
        let gateway_total = self
            .queued_bytes_total
            .checked_add(bytes)
            .ok_or(GatewayError::AccountingOverflow)?;
        if connection_total > self.limits.max_queued_bytes_per_connection
            || gateway_total > self.limits.max_queued_bytes_total
        {
            return Err(GatewayError::QueueLimit);
        }
        self.active.insert(id, connection_total);
        self.queued_bytes_total = gateway_total;
        Ok(())
    }

    /// Releases buffered-byte accounting for an authenticated live connection.
    ///
    /// # Errors
    ///
    /// Returns `UnknownAuthorization` for a stale identifier or `AccountingOverflow` when more
    /// bytes are released than that connection currently owns.
    pub fn release_queued_bytes(&mut self, id: u64, bytes: usize) -> Result<(), GatewayError> {
        let queued = self
            .active
            .get_mut(&id)
            .ok_or(GatewayError::UnknownAuthorization)?;
        if bytes > *queued {
            return Err(GatewayError::AccountingOverflow);
        }
        *queued -= bytes;
        self.queued_bytes_total -= bytes;
        Ok(())
    }

    /// Removes one authorization and all queued bytes attributed to it.
    ///
    /// # Errors
    ///
    /// Returns `UnknownAuthorization` when the identifier is not active.
    pub fn close_authorization(&mut self, id: u64) -> Result<(), GatewayError> {
        let queued = self
            .active
            .remove(&id)
            .ok_or(GatewayError::UnknownAuthorization)?;
        self.queued_bytes_total -= queued;
        Ok(())
    }

    /// Stops new acceptance. Existing authorizations remain visible for ordered draining.
    pub fn begin_draining(&mut self) {
        if self.lifecycle == GatewayLifecycle::Open {
            self.lifecycle = GatewayLifecycle::Draining;
        }
    }

    /// Cancels every authorization and permanently closes this gateway instance.
    pub fn close(&mut self) {
        self.lifecycle = GatewayLifecycle::Closed;
        self.active.clear();
        self.queued_bytes_total = 0;
        self.token.0.fill(0);
    }

    /// Unpinning tears down only client-side gateway resources; it does not represent service stop.
    pub fn unpin(&mut self) {
        self.close();
    }

    /// An SSH disconnect cancels all local tunnel authorizations without changing service state.
    pub fn disconnect(&mut self) {
        self.close();
    }

    /// Invalidates all old authorizations and credentials, then opens with a caller-supplied token.
    ///
    /// # Errors
    ///
    /// Returns `NotAccepting` when the replacement token is empty or the gateway is permanently
    /// closed. On error the existing lifecycle and credential remain unchanged.
    pub fn rotate_token(&mut self, token: Vec<u8>) -> Result<(), GatewayError> {
        if token.is_empty() || self.lifecycle == GatewayLifecycle::Closed {
            return Err(GatewayError::NotAccepting);
        }
        self.begin_draining();
        self.active.clear();
        self.queued_bytes_total = 0;
        self.token.0.fill(0);
        self.token = SecretToken(token);
        self.lifecycle = GatewayLifecycle::Open;
        Ok(())
    }

    fn validate_request(&self, request: &GatewayRequest<'_>) -> Result<(), GatewayError> {
        if request.method.is_empty()
            || request.method.len() > self.limits.max_method_bytes
            || !request
                .method
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte == b'-')
        {
            return Err(GatewayError::InvalidMethod);
        }
        if request.target.is_empty()
            || request.target.len() > self.limits.max_target_bytes
            || !request.target.starts_with('/')
            || request.target.starts_with("//")
            || request
                .target
                .bytes()
                .any(|byte| byte <= 0x20 || byte == 0x7f)
        {
            return Err(GatewayError::InvalidTarget);
        }
        if request.headers.len() > self.limits.max_header_count {
            return Err(GatewayError::InvalidHeaders);
        }
        let mut bytes = 0usize;
        for header in request.headers {
            if header.name.is_empty()
                || !header.name.bytes().all(is_header_name_byte)
                || header
                    .value
                    .iter()
                    .any(|byte| *byte == b'\r' || *byte == b'\n')
            {
                return Err(GatewayError::InvalidHeaders);
            }
            bytes = bytes
                .checked_add(header.name.len())
                .and_then(|value| value.checked_add(header.value.len()))
                .ok_or(GatewayError::AccountingOverflow)?;
            if bytes > self.limits.max_header_bytes {
                return Err(GatewayError::InvalidHeaders);
            }
        }
        Ok(())
    }
}

fn is_header_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let maximum = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..maximum {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(left_byte ^ right_byte);
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> GatewayLimits {
        GatewayLimits {
            max_method_bytes: 16,
            max_target_bytes: 128,
            max_header_count: 8,
            max_header_bytes: 256,
            max_connections: 2,
            max_queued_bytes_per_connection: 8,
            max_queued_bytes_total: 12,
        }
    }

    fn request(cookie: Option<&[u8]>) -> GatewayRequest<'_> {
        GatewayRequest {
            method: "GET",
            target: "/index.html",
            headers: &[],
            gateway_cookie: cookie,
        }
    }

    fn gateway() -> Gateway {
        Gateway::new(
            "service-1".into(),
            3000,
            b"opaque-secret".to_vec(),
            limits(),
        )
        .unwrap()
    }

    #[test]
    fn unauthenticated_and_malformed_requests_create_no_authorization() {
        let mut gateway = gateway();
        assert_eq!(
            gateway.authorize(&request(None)),
            Err(GatewayError::AuthenticationRequired)
        );
        assert_eq!(
            gateway.authorize(&request(Some(b"wrong"))),
            Err(GatewayError::AuthenticationRequired)
        );
        let malformed = GatewayRequest {
            method: "G ET",
            target: "/",
            headers: &[],
            gateway_cookie: Some(b"opaque-secret"),
        };
        assert_eq!(
            gateway.authorize(&malformed),
            Err(GatewayError::InvalidMethod)
        );
        assert_eq!(gateway.active_connections(), 0);
    }

    #[test]
    fn hostile_host_and_target_cannot_substitute_destination() {
        let mut gateway = gateway();
        let headers = [RequestHeader {
            name: "Host",
            value: b"attacker.invalid:65535",
        }];
        let request = GatewayRequest {
            method: "GET",
            target: "/http://attacker.invalid:4444/path?port=9",
            headers: &headers,
            gateway_cookie: Some(b"opaque-secret"),
        };
        let authorization = gateway.authorize(&request).unwrap();
        assert_eq!(authorization.destination_host, "127.0.0.1");
        assert_eq!(authorization.destination_port, 3000);

        let absolute_target = GatewayRequest {
            target: "http://attacker.invalid/",
            ..request
        };
        assert_eq!(
            gateway.authorize(&absolute_target),
            Err(GatewayError::InvalidTarget)
        );
        assert_eq!(gateway.active_connections(), 1);
    }

    #[test]
    fn limits_apply_without_partial_accounting() {
        let mut gateway = gateway();
        let first = gateway.authorize(&request(Some(b"opaque-secret"))).unwrap();
        let second = gateway.authorize(&request(Some(b"opaque-secret"))).unwrap();
        assert_eq!(
            gateway.authorize(&request(Some(b"opaque-secret"))),
            Err(GatewayError::ConnectionLimit)
        );
        gateway
            .reserve_queued_bytes(first.authorization_id, 8)
            .unwrap();
        assert_eq!(
            gateway.reserve_queued_bytes(second.authorization_id, 5),
            Err(GatewayError::QueueLimit)
        );
        assert_eq!(gateway.queued_bytes(), 8);
        gateway.close_authorization(first.authorization_id).unwrap();
        assert_eq!(gateway.queued_bytes(), 0);
    }

    #[test]
    fn draining_close_and_rotation_invalidate_authorizations() {
        let mut gateway = gateway();
        let old = gateway.authorize(&request(Some(b"opaque-secret"))).unwrap();
        gateway.begin_draining();
        assert_eq!(
            gateway.authorize(&request(Some(b"opaque-secret"))),
            Err(GatewayError::NotAccepting)
        );
        gateway.rotate_token(b"replacement".to_vec()).unwrap();
        assert_eq!(gateway.active_connections(), 0);
        assert_eq!(
            gateway.close_authorization(old.authorization_id),
            Err(GatewayError::UnknownAuthorization)
        );
        assert_eq!(
            gateway.authorize(&request(Some(b"opaque-secret"))),
            Err(GatewayError::AuthenticationRequired)
        );
        assert!(gateway.authorize(&request(Some(b"replacement"))).is_ok());
        gateway.close();
        assert_eq!(gateway.active_connections(), 0);
        assert_eq!(gateway.lifecycle(), GatewayLifecycle::Closed);
    }

    #[test]
    fn debug_and_errors_do_not_expose_token() {
        let gateway = gateway();
        let debug = format!("{gateway:?}");
        assert!(!debug.contains("opaque-secret"));
        assert_eq!(
            format!("{:?}", GatewayError::AuthenticationRequired),
            "AuthenticationRequired"
        );
    }
}
