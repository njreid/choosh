//! Fail-closed HTTP forwarding policy for authenticated loopback gateways.

use std::collections::BTreeSet;

const MAX_METHOD_BYTES: usize = 16;
const MAX_TOKEN_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredDestination {
    host: String,
    port: u16,
}

impl RegisteredDestination {
    /// Registers one immutable loopback destination.
    ///
    /// # Errors
    ///
    /// Rejects non-loopback hosts and port zero.
    pub fn loopback(host: &str, port: u16) -> Result<Self, GatewayHttpError> {
        if port == 0 || !matches!(host, "127.0.0.1" | "::1" | "localhost") {
            return Err(GatewayHttpError::UnsafeDestination);
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForwardLimits {
    pub max_headers: usize,
    pub max_header_bytes: usize,
    pub max_target_bytes: usize,
    pub max_body_bytes: u64,
    pub max_buffered_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IncomingRequest<'a> {
    pub method: &'a str,
    pub target: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub gateway_cookie_name: &'a str,
    pub expected_token: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForwardMode {
    Http,
    ServerSentEvents,
    WebSocket,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SanitizedRequest {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub mode: ForwardMode,
}

/// Authenticates, validates, and removes gateway-only HTTP metadata.
///
/// # Errors
///
/// Rejects malformed or ambiguous requests, missing/wrong/repeated gateway
/// cookies, unsafe hop-by-hop metadata, invalid upgrades, and configured limits.
pub fn sanitize_request(
    request: IncomingRequest<'_>,
    limits: ForwardLimits,
) -> Result<SanitizedRequest, GatewayHttpError> {
    validate_limits(limits)?;
    validate_token(request.expected_token)?;
    validate_method(request.method)?;
    validate_target(request.target, limits.max_target_bytes)?;
    if request.headers.len() > limits.max_headers {
        return Err(GatewayHttpError::HeaderLimit);
    }

    let mut header_bytes = 0_usize;
    let mut seen = BTreeSet::new();
    let mut connection_tokens = BTreeSet::new();
    for &(name, value) in request.headers {
        validate_header(name, value)?;
        header_bytes = header_bytes
            .checked_add(name.len())
            .and_then(|sum| sum.checked_add(value.len()))
            .ok_or(GatewayHttpError::HeaderLimit)?;
        if header_bytes > limits.max_header_bytes {
            return Err(GatewayHttpError::HeaderLimit);
        }
        let lower = name.to_ascii_lowercase();
        if !seen.insert(lower.clone()) && !matches!(lower.as_str(), "cookie") {
            return Err(GatewayHttpError::AmbiguousHeader);
        }
        if lower == "connection" {
            for token in value.split(',') {
                let token = token.trim().to_ascii_lowercase();
                if token.is_empty() || !is_token(&token) {
                    return Err(GatewayHttpError::InvalidHeader);
                }
                connection_tokens.insert(token);
            }
        }
    }

    authenticate_cookie(
        request.headers,
        request.gateway_cookie_name,
        request.expected_token,
    )?;
    let websocket = validate_websocket(request.headers)?;
    let sse = accepts_sse(request.headers);
    if websocket && sse {
        return Err(GatewayHttpError::AmbiguousProtocol);
    }

    let mut headers = Vec::new();
    for &(name, value) in request.headers {
        let lower = name.to_ascii_lowercase();
        if is_hop_by_hop(&lower)
            || connection_tokens.contains(&lower)
            || lower == "cookie"
            || lower == "host"
        {
            continue;
        }
        headers.push((lower, value.trim().to_owned()));
    }
    headers.sort();
    Ok(SanitizedRequest {
        method: request.method.to_owned(),
        target: request.target.to_owned(),
        headers,
        mode: if websocket {
            ForwardMode::WebSocket
        } else if sse {
            ForwardMode::ServerSentEvents
        } else {
            ForwardMode::Http
        },
    })
}

fn validate_limits(limits: ForwardLimits) -> Result<(), GatewayHttpError> {
    if limits.max_headers == 0
        || limits.max_header_bytes == 0
        || limits.max_target_bytes == 0
        || limits.max_body_bytes == 0
        || limits.max_buffered_bytes == 0
    {
        return Err(GatewayHttpError::InvalidLimits);
    }
    Ok(())
}

fn validate_token(token: &str) -> Result<(), GatewayHttpError> {
    if token.is_empty()
        || token.len() > MAX_TOKEN_BYTES
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(GatewayHttpError::InvalidToken);
    }
    Ok(())
}

fn validate_method(method: &str) -> Result<(), GatewayHttpError> {
    if method.is_empty()
        || method.len() > MAX_METHOD_BYTES
        || !method.bytes().all(|byte| byte.is_ascii_uppercase())
    {
        return Err(GatewayHttpError::InvalidMethod);
    }
    Ok(())
}

fn validate_target(target: &str, max_bytes: usize) -> Result<(), GatewayHttpError> {
    if target.is_empty()
        || target.len() > max_bytes
        || !target.starts_with('/')
        || target.starts_with("//")
        || target.contains('#')
        || target
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(GatewayHttpError::InvalidTarget);
    }
    Ok(())
}

fn validate_header(name: &str, value: &str) -> Result<(), GatewayHttpError> {
    if name.is_empty()
        || !is_token(name)
        || value.bytes().any(|byte| matches!(byte, b'\r' | b'\n' | 0))
    {
        return Err(GatewayHttpError::InvalidHeader);
    }
    Ok(())
}

fn is_token(value: &str) -> bool {
    value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
            )
    })
}

fn authenticate_cookie(
    headers: &[(&str, &str)],
    name: &str,
    expected: &str,
) -> Result<(), GatewayHttpError> {
    if name.is_empty() || !is_token(name) {
        return Err(GatewayHttpError::InvalidToken);
    }
    let mut matched = 0_usize;
    for value in headers
        .iter()
        .filter(|(header, _)| header.eq_ignore_ascii_case("cookie"))
        .map(|(_, value)| *value)
    {
        for pair in value.split(';') {
            let (cookie_name, cookie_value) = pair
                .trim()
                .split_once('=')
                .ok_or(GatewayHttpError::Unauthorized)?;
            if cookie_name == name {
                matched += 1;
                if cookie_value != expected {
                    return Err(GatewayHttpError::Unauthorized);
                }
            }
        }
    }
    if matched == 1 {
        Ok(())
    } else {
        Err(GatewayHttpError::Unauthorized)
    }
}

fn header<'a>(headers: &'a [(&str, &str)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| *value)
}

fn validate_websocket(headers: &[(&str, &str)]) -> Result<bool, GatewayHttpError> {
    let upgrade = header(headers, "upgrade");
    let version = header(headers, "sec-websocket-version");
    let key = header(headers, "sec-websocket-key");
    let connection_upgrade = header(headers, "connection").is_some_and(|value| {
        value
            .split(',')
            .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
    });
    let any = upgrade.is_some() || version.is_some() || key.is_some() || connection_upgrade;
    if !any {
        return Ok(false);
    }
    if !upgrade.is_some_and(|value| value.trim().eq_ignore_ascii_case("websocket"))
        || version.map(str::trim) != Some("13")
        || !connection_upgrade
        || !key.is_some_and(valid_websocket_key)
    {
        return Err(GatewayHttpError::InvalidWebSocket);
    }
    Ok(true)
}

fn valid_websocket_key(value: &str) -> bool {
    let value = value.trim();
    value.len() == 24
        && value.ends_with("==")
        && value[..22]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
}

fn accepts_sse(headers: &[(&str, &str)]) -> bool {
    header(headers, "accept").is_some_and(|value| {
        value.split(',').any(|part| {
            part.split(';')
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case("text/event-stream"))
        })
    })
}

fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BodyBudget {
    max_body_bytes: u64,
    max_buffered_bytes: usize,
    accepted: u64,
    buffered: usize,
}

impl BodyBudget {
    #[must_use]
    pub fn from_limits(limits: ForwardLimits) -> Self {
        Self {
            max_body_bytes: limits.max_body_bytes,
            max_buffered_bytes: limits.max_buffered_bytes,
            accepted: 0,
            buffered: 0,
        }
    }

    /// Accounts bytes accepted from upstream but not yet consumed downstream.
    ///
    /// # Errors
    ///
    /// Rejects body overflow and backpressure-buffer overflow.
    pub fn receive(&mut self, bytes: usize) -> Result<(), GatewayHttpError> {
        let accepted = self
            .accepted
            .checked_add(u64::try_from(bytes).map_err(|_| GatewayHttpError::BodyLimit)?)
            .ok_or(GatewayHttpError::BodyLimit)?;
        let buffered = self
            .buffered
            .checked_add(bytes)
            .ok_or(GatewayHttpError::BackpressureLimit)?;
        if accepted > self.max_body_bytes {
            return Err(GatewayHttpError::BodyLimit);
        }
        if buffered > self.max_buffered_bytes {
            return Err(GatewayHttpError::BackpressureLimit);
        }
        self.accepted = accepted;
        self.buffered = buffered;
        Ok(())
    }

    /// Records downstream consumption.
    ///
    /// # Errors
    ///
    /// Rejects accounting underflow.
    pub fn consume(&mut self, bytes: usize) -> Result<(), GatewayHttpError> {
        self.buffered = self
            .buffered
            .checked_sub(bytes)
            .ok_or(GatewayHttpError::AccountingUnderflow)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GatewayHttpError {
    InvalidLimits,
    UnsafeDestination,
    InvalidToken,
    InvalidMethod,
    InvalidTarget,
    InvalidHeader,
    HeaderLimit,
    AmbiguousHeader,
    Unauthorized,
    InvalidWebSocket,
    AmbiguousProtocol,
    BodyLimit,
    BackpressureLimit,
    AccountingUnderflow,
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: ForwardLimits = ForwardLimits {
        max_headers: 16,
        max_header_bytes: 512,
        max_target_bytes: 128,
        max_body_bytes: 10,
        max_buffered_bytes: 4,
    };

    fn sanitize<'a>(
        headers: &'a [(&'a str, &'a str)],
    ) -> Result<SanitizedRequest, GatewayHttpError> {
        sanitize_request(
            IncomingRequest {
                method: "GET",
                target: "/path?q=1",
                headers,
                gateway_cookie_name: "choosh_gateway",
                expected_token: "secret-token",
            },
            LIMITS,
        )
    }

    #[test]
    fn strips_auth_host_and_hop_by_hop_headers() {
        let request = sanitize(&[
            ("Cookie", "other=x; choosh_gateway=secret-token"),
            ("Host", "attacker.invalid"),
            ("Connection", "keep-alive, X-Remove"),
            ("X-Remove", "secret"),
            ("X-Safe", "yes"),
        ])
        .unwrap();
        assert_eq!(request.headers, vec![("x-safe".into(), "yes".into())]);
        assert_eq!(request.mode, ForwardMode::Http);
    }

    #[test]
    fn rejects_crlf_duplicate_auth_and_absolute_targets() {
        assert_eq!(
            sanitize(&[("Cookie", "choosh_gateway=secret-token"), ("X", "a\r\nb")]),
            Err(GatewayHttpError::InvalidHeader)
        );
        assert_eq!(
            sanitize(&[(
                "Cookie",
                "choosh_gateway=secret-token; choosh_gateway=secret-token"
            )]),
            Err(GatewayHttpError::Unauthorized)
        );
        let request = IncomingRequest {
            method: "GET",
            target: "http://evil.invalid/",
            headers: &[("Cookie", "choosh_gateway=secret-token")],
            gateway_cookie_name: "choosh_gateway",
            expected_token: "secret-token",
        };
        assert_eq!(
            sanitize_request(request, LIMITS),
            Err(GatewayHttpError::InvalidTarget)
        );
    }

    #[test]
    fn validates_complete_websocket_handshake() {
        let request = sanitize(&[
            ("Cookie", "choosh_gateway=secret-token"),
            ("Connection", "Upgrade"),
            ("Upgrade", "websocket"),
            ("Sec-WebSocket-Version", "13"),
            ("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ])
        .unwrap();
        assert_eq!(request.mode, ForwardMode::WebSocket);
        assert_eq!(
            sanitize(&[
                ("Cookie", "choosh_gateway=secret-token"),
                ("Upgrade", "websocket")
            ]),
            Err(GatewayHttpError::InvalidWebSocket)
        );
    }

    #[test]
    fn classifies_sse_without_forwarding_cookie() {
        let request = sanitize(&[
            ("Cookie", "choosh_gateway=secret-token"),
            ("Accept", "text/event-stream; charset=utf-8"),
        ])
        .unwrap();
        assert_eq!(request.mode, ForwardMode::ServerSentEvents);
        assert!(request.headers.iter().all(|(name, _)| name != "cookie"));
    }

    #[test]
    fn body_accounting_enforces_backpressure_and_total_limits() {
        let mut budget = BodyBudget::from_limits(LIMITS);
        budget.receive(4).unwrap();
        assert_eq!(budget.receive(1), Err(GatewayHttpError::BackpressureLimit));
        budget.consume(4).unwrap();
        budget.receive(4).unwrap();
        budget.consume(4).unwrap();
        assert_eq!(budget.receive(3), Err(GatewayHttpError::BodyLimit));
    }

    #[test]
    fn destination_is_registered_loopback_only() {
        assert!(RegisteredDestination::loopback("127.0.0.1", 8080).is_ok());
        assert_eq!(
            RegisteredDestination::loopback("0.0.0.0", 8080),
            Err(GatewayHttpError::UnsafeDestination)
        );
        assert_eq!(
            RegisteredDestination::loopback("example.invalid", 443),
            Err(GatewayHttpError::UnsafeDestination)
        );
    }
}
