//! Typed, deterministic protocol negotiation independent of wire serialization.

use std::collections::BTreeSet;

pub const PROTOCOL_V1_MAJOR: u16 = 1;
pub const MAX_IDENTITY_BYTES: usize = 128;
pub const MAX_CAPABILITIES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Capability {
    Events,
    GitBlobs,
    Services,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolLimits {
    pub max_control_frame_bytes: u32,
    pub max_in_flight_requests: u16,
}

impl ProtocolLimits {
    /// Creates positive resource limits.
    ///
    /// # Errors
    ///
    /// Returns `invalid_limits` when either bound is zero.
    pub const fn new(
        max_control_frame_bytes: u32,
        max_in_flight_requests: u16,
    ) -> Result<Self, HandshakeError> {
        if max_control_frame_bytes == 0 || max_in_flight_requests == 0 {
            return Err(HandshakeError::InvalidLimits);
        }
        Ok(Self {
            max_control_frame_bytes,
            max_in_flight_requests,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerIdentity {
    pub name: String,
    pub version: String,
}

impl PeerIdentity {
    /// Validates a display-only peer identity without interpreting its contents.
    ///
    /// # Errors
    ///
    /// Returns `invalid_identity` for empty, oversized, or control-character values.
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, HandshakeError> {
        let identity = Self {
            name: name.into(),
            version: version.into(),
        };
        if !valid_identity_field(&identity.name) || !valid_identity_field(&identity.version) {
            return Err(HandshakeError::InvalidIdentity);
        }
        Ok(identity)
    }
}

fn valid_identity_field(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_IDENTITY_BYTES && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hello {
    pub protocol: ProtocolVersion,
    pub client: PeerIdentity,
    capabilities: BTreeSet<Capability>,
}

impl Hello {
    /// Creates a bounded hello message and canonicalizes capability order.
    ///
    /// # Errors
    ///
    /// Returns `too_many_capabilities` if the input is not bounded.
    pub fn new(
        protocol: ProtocolVersion,
        client: PeerIdentity,
        capabilities: impl IntoIterator<Item = Capability>,
    ) -> Result<Self, HandshakeError> {
        let capabilities = bounded_capabilities(capabilities)?;
        Ok(Self {
            protocol,
            client,
            capabilities,
        })
    }

    #[must_use]
    pub fn capabilities(&self) -> impl ExactSizeIterator<Item = Capability> + '_ {
        self.capabilities.iter().copied()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Welcome {
    pub protocol: ProtocolVersion,
    pub daemon: PeerIdentity,
    pub host: PeerIdentity,
    pub limits: ProtocolLimits,
    capabilities: BTreeSet<Capability>,
}

impl Welcome {
    /// Reconstructs a bounded welcome received from the wire.
    ///
    /// Client-side negotiation must still validate the selected version and
    /// capabilities against the original hello.
    ///
    /// # Errors
    ///
    /// Returns `too_many_capabilities` when the input exceeds the bound.
    pub fn new(
        protocol: ProtocolVersion,
        daemon: PeerIdentity,
        host: PeerIdentity,
        capabilities: impl IntoIterator<Item = Capability>,
        limits: ProtocolLimits,
    ) -> Result<Self, HandshakeError> {
        Ok(Self {
            protocol,
            daemon,
            host,
            limits,
            capabilities: bounded_capabilities(capabilities)?,
        })
    }

    #[must_use]
    pub fn capabilities(&self) -> impl ExactSizeIterator<Item = Capability> + '_ {
        self.capabilities.iter().copied()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Incompatible {
    pub client: ProtocolVersion,
    pub daemon: ProtocolVersion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerReply {
    Welcome(Welcome),
    Incompatible(Incompatible),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandshakeError {
    InvalidLimits,
    InvalidIdentity,
    TooManyCapabilities,
    ExpectedHello,
    ExpectedWelcome,
    NegotiationComplete,
    MajorVersionMismatch,
    InvalidSelectedVersion,
    UnadvertisedCapability,
}

impl HandshakeError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::InvalidIdentity => "invalid_identity",
            Self::TooManyCapabilities => "too_many_capabilities",
            Self::ExpectedHello => "expected_hello",
            Self::ExpectedWelcome => "expected_welcome",
            Self::NegotiationComplete => "negotiation_complete",
            Self::MajorVersionMismatch => "major_version_mismatch",
            Self::InvalidSelectedVersion => "invalid_selected_version",
            Self::UnadvertisedCapability => "unadvertised_capability",
        }
    }
}

fn bounded_capabilities(
    capabilities: impl IntoIterator<Item = Capability>,
) -> Result<BTreeSet<Capability>, HandshakeError> {
    let mut bounded = BTreeSet::new();
    for capability in capabilities {
        if bounded.len() == MAX_CAPABILITIES && !bounded.contains(&capability) {
            return Err(HandshakeError::TooManyCapabilities);
        }
        bounded.insert(capability);
    }
    Ok(bounded)
}

#[derive(Debug)]
pub struct ServerNegotiator {
    version: ProtocolVersion,
    daemon: PeerIdentity,
    host: PeerIdentity,
    capabilities: BTreeSet<Capability>,
    limits: ProtocolLimits,
    complete: bool,
}

impl ServerNegotiator {
    /// Creates a reusable configuration for one handshake.
    ///
    /// # Errors
    ///
    /// Returns `too_many_capabilities` if supported capabilities exceed the bound.
    pub fn new(
        version: ProtocolVersion,
        daemon: PeerIdentity,
        host: PeerIdentity,
        capabilities: impl IntoIterator<Item = Capability>,
        limits: ProtocolLimits,
    ) -> Result<Self, HandshakeError> {
        Ok(Self {
            version,
            daemon,
            host,
            capabilities: bounded_capabilities(capabilities)?,
            limits,
            complete: false,
        })
    }

    /// Accepts the one permitted pre-negotiation message.
    ///
    /// # Errors
    ///
    /// Returns `negotiation_complete` after the first hello, preventing renegotiation.
    pub fn receive_hello(&mut self, hello: &Hello) -> Result<ServerReply, HandshakeError> {
        if self.complete {
            return Err(HandshakeError::NegotiationComplete);
        }
        self.complete = true;
        if hello.protocol.major != self.version.major {
            return Ok(ServerReply::Incompatible(Incompatible {
                client: hello.protocol,
                daemon: self.version,
            }));
        }
        let capabilities = hello
            .capabilities
            .intersection(&self.capabilities)
            .copied()
            .collect();
        Ok(ServerReply::Welcome(Welcome {
            protocol: ProtocolVersion::new(
                self.version.major,
                hello.protocol.minor.min(self.version.minor),
            ),
            daemon: self.daemon.clone(),
            host: self.host.clone(),
            limits: self.limits,
            capabilities,
        }))
    }

    /// Rejects every non-hello first message with a stable ordering error.
    ///
    /// # Errors
    ///
    /// Returns `expected_hello` initially and `negotiation_complete` thereafter.
    pub fn receive_non_hello(&mut self) -> Result<ServerReply, HandshakeError> {
        if self.complete {
            Err(HandshakeError::NegotiationComplete)
        } else {
            self.complete = true;
            Err(HandshakeError::ExpectedHello)
        }
    }
}

#[derive(Debug)]
pub struct ClientNegotiator {
    advertised_version: ProtocolVersion,
    advertised_capabilities: BTreeSet<Capability>,
    complete: bool,
}

impl ClientNegotiator {
    #[must_use]
    pub fn new(hello: &Hello) -> Self {
        Self {
            advertised_version: hello.protocol,
            advertised_capabilities: hello.capabilities.clone(),
            complete: false,
        }
    }

    /// Validates the daemon selection against the original hello.
    ///
    /// # Errors
    ///
    /// Fails closed for reordered messages, invalid versions, or capabilities the client did
    /// not advertise. Any first response, including an invalid one, ends negotiation.
    pub fn receive_welcome(&mut self, welcome: &Welcome) -> Result<(), HandshakeError> {
        if self.complete {
            return Err(HandshakeError::NegotiationComplete);
        }
        self.complete = true;
        if welcome.protocol.major != self.advertised_version.major {
            return Err(HandshakeError::MajorVersionMismatch);
        }
        if welcome.protocol.minor > self.advertised_version.minor {
            return Err(HandshakeError::InvalidSelectedVersion);
        }
        if !welcome
            .capabilities
            .is_subset(&self.advertised_capabilities)
        {
            return Err(HandshakeError::UnadvertisedCapability);
        }
        Ok(())
    }

    /// Validates that an incompatible response describes the original version mismatch.
    ///
    /// # Errors
    ///
    /// Returns `major_version_mismatch` for an inconsistent response, and
    /// `negotiation_complete` after any first response.
    pub fn receive_incompatible(
        &mut self,
        incompatible: Incompatible,
    ) -> Result<(), HandshakeError> {
        if self.complete {
            return Err(HandshakeError::NegotiationComplete);
        }
        self.complete = true;
        if incompatible.client != self.advertised_version
            || incompatible.client.major == incompatible.daemon.major
        {
            return Err(HandshakeError::MajorVersionMismatch);
        }
        Ok(())
    }

    /// Rejects every non-welcome/non-incompatible first response.
    ///
    /// # Errors
    ///
    /// Returns `expected_welcome` initially and `negotiation_complete` thereafter.
    pub fn receive_non_response(&mut self) -> Result<(), HandshakeError> {
        if self.complete {
            Err(HandshakeError::NegotiationComplete)
        } else {
            self.complete = true;
            Err(HandshakeError::ExpectedWelcome)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str) -> PeerIdentity {
        PeerIdentity::new(name, "0.1.0").unwrap()
    }

    fn hello(version: ProtocolVersion, capabilities: &[Capability]) -> Hello {
        Hello::new(
            version,
            identity("choosh-android"),
            capabilities.iter().copied(),
        )
        .unwrap()
    }

    fn server(version: ProtocolVersion, capabilities: &[Capability]) -> ServerNegotiator {
        ServerNegotiator::new(
            version,
            identity("chooshd"),
            identity("test-host"),
            capabilities.iter().copied(),
            ProtocolLimits::new(1_048_576, 64).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn selects_lower_minor_and_capability_intersection_deterministically() {
        let hello = hello(
            ProtocolVersion::new(PROTOCOL_V1_MAJOR, 4),
            &[Capability::Services, Capability::Events],
        );
        let mut server = server(
            ProtocolVersion::new(PROTOCOL_V1_MAJOR, 2),
            &[Capability::GitBlobs, Capability::Events],
        );
        let ServerReply::Welcome(welcome) = server.receive_hello(&hello).unwrap() else {
            panic!("matching major must produce welcome");
        };
        assert_eq!(welcome.protocol, ProtocolVersion::new(1, 2));
        assert_eq!(
            welcome.capabilities().collect::<Vec<_>>(),
            [Capability::Events]
        );
        assert_eq!(welcome.limits.max_control_frame_bytes, 1_048_576);
        assert_eq!(
            ClientNegotiator::new(&hello).receive_welcome(&welcome),
            Ok(())
        );
    }

    #[test]
    fn mismatched_major_returns_incompatible_and_ends_server_negotiation() {
        let hello = hello(ProtocolVersion::new(2, 0), &[]);
        let mut server = server(ProtocolVersion::new(1, 9), &[]);
        assert_eq!(
            server.receive_hello(&hello),
            Ok(ServerReply::Incompatible(Incompatible {
                client: ProtocolVersion::new(2, 0),
                daemon: ProtocolVersion::new(1, 9),
            }))
        );
        assert_eq!(
            server.receive_hello(&hello),
            Err(HandshakeError::NegotiationComplete)
        );
    }

    #[test]
    fn ordering_violations_fail_closed_on_both_peers() {
        let hello = hello(ProtocolVersion::new(1, 0), &[]);
        let mut server = server(ProtocolVersion::new(1, 0), &[]);
        assert_eq!(
            server.receive_non_hello(),
            Err(HandshakeError::ExpectedHello)
        );
        assert_eq!(
            server.receive_hello(&hello),
            Err(HandshakeError::NegotiationComplete)
        );

        let mut client = ClientNegotiator::new(&hello);
        assert_eq!(
            client.receive_non_response(),
            Err(HandshakeError::ExpectedWelcome)
        );
        assert_eq!(
            client.receive_non_response(),
            Err(HandshakeError::NegotiationComplete)
        );
    }

    #[test]
    fn client_rejects_version_upgrade_and_unadvertised_capability() {
        let hello = hello(ProtocolVersion::new(1, 1), &[Capability::Events]);
        let base = Welcome {
            protocol: ProtocolVersion::new(1, 2),
            daemon: identity("chooshd"),
            host: identity("host"),
            limits: ProtocolLimits::new(1, 1).unwrap(),
            capabilities: BTreeSet::new(),
        };
        assert_eq!(
            ClientNegotiator::new(&hello).receive_welcome(&base),
            Err(HandshakeError::InvalidSelectedVersion)
        );
        let mut capabilities = BTreeSet::new();
        capabilities.insert(Capability::Services);
        let welcome = Welcome {
            protocol: ProtocolVersion::new(1, 0),
            capabilities,
            ..base
        };
        assert_eq!(
            ClientNegotiator::new(&hello).receive_welcome(&welcome),
            Err(HandshakeError::UnadvertisedCapability)
        );
    }

    #[test]
    fn incompatible_response_must_describe_the_original_major_mismatch() {
        let hello = hello(ProtocolVersion::new(1, 0), &[]);
        let mut client = ClientNegotiator::new(&hello);
        assert_eq!(
            client.receive_incompatible(Incompatible {
                client: ProtocolVersion::new(1, 0),
                daemon: ProtocolVersion::new(1, 9),
            }),
            Err(HandshakeError::MajorVersionMismatch)
        );
    }

    #[test]
    fn identity_and_limit_inputs_are_bounded() {
        assert_eq!(
            PeerIdentity::new("", "1"),
            Err(HandshakeError::InvalidIdentity)
        );
        assert_eq!(
            PeerIdentity::new("x".repeat(MAX_IDENTITY_BYTES + 1), "1"),
            Err(HandshakeError::InvalidIdentity)
        );
        assert_eq!(
            PeerIdentity::new("host\nname", "1"),
            Err(HandshakeError::InvalidIdentity)
        );
        assert_eq!(
            ProtocolLimits::new(0, 1),
            Err(HandshakeError::InvalidLimits)
        );
        assert_eq!(
            ProtocolLimits::new(1, 0),
            Err(HandshakeError::InvalidLimits)
        );
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(HandshakeError::ExpectedHello.code(), "expected_hello");
        assert_eq!(HandshakeError::ExpectedWelcome.code(), "expected_welcome");
        assert_eq!(
            HandshakeError::NegotiationComplete.code(),
            "negotiation_complete"
        );
    }
}
