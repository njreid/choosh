//! Pre-authentication SSH session composition.
//!
//! This module deliberately stops at creating a bounded Russh configuration
//! and exact-host-key handler. It does not create a socket, authenticate a
//! user, or expose channels. Those capabilities require the generated-key
//! admission harness and separate, explicitly reviewed interfaces.

use std::sync::Arc;
use std::time::Duration;

use choosh_core::ssh_identity::PublicKeyFingerprint;
use russh::client;

use crate::ExactHostKeyHandler;

const MIN_WINDOW_SIZE: u32 = 65_536;
const MAX_WINDOW_SIZE: u32 = 16 * 1024 * 1024;
const MIN_PACKET_SIZE: u32 = 1_024;
const MAX_PACKET_SIZE: u32 = 65_535;
const MAX_CHANNEL_BUFFER_SIZE: usize = 1_024;
const MAX_KEEPALIVE_SECONDS: u64 = 3_600;

/// Stable failures while constructing a pre-authentication session plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionConfigError {
    WindowSizeOutOfRange,
    PacketSizeOutOfRange,
    PacketExceedsWindow,
    ChannelBufferOutOfRange,
    KeepaliveIntervalOutOfRange,
    ZeroKeepaliveMaximum,
}

/// Explicit resource and keepalive bounds for one future SSH connection.
///
/// These values do not grant authentication or channel authority. Their
/// validation happens before a Russh configuration is allocated so callers
/// receive stable, dependency-independent errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionLimits {
    window_size: u32,
    maximum_packet_size: u32,
    channel_buffer_size: usize,
    keepalive_interval_seconds: Option<u64>,
    keepalive_max: usize,
}

impl SessionLimits {
    /// Creates a bounded connection configuration.
    ///
    /// # Errors
    ///
    /// Returns a typed error when a value is outside the adapter's documented
    /// bounds or a packet cannot fit inside its channel window.
    pub fn new(
        window_size: u32,
        maximum_packet_size: u32,
        channel_buffer_size: usize,
        keepalive_interval_seconds: Option<u64>,
        keepalive_max: usize,
    ) -> Result<Self, SessionConfigError> {
        if !(MIN_WINDOW_SIZE..=MAX_WINDOW_SIZE).contains(&window_size) {
            return Err(SessionConfigError::WindowSizeOutOfRange);
        }
        if !(MIN_PACKET_SIZE..=MAX_PACKET_SIZE).contains(&maximum_packet_size) {
            return Err(SessionConfigError::PacketSizeOutOfRange);
        }
        if maximum_packet_size > window_size {
            return Err(SessionConfigError::PacketExceedsWindow);
        }
        if channel_buffer_size == 0 || channel_buffer_size > MAX_CHANNEL_BUFFER_SIZE {
            return Err(SessionConfigError::ChannelBufferOutOfRange);
        }
        if let Some(seconds) = keepalive_interval_seconds
            && seconds > MAX_KEEPALIVE_SECONDS
        {
            return Err(SessionConfigError::KeepaliveIntervalOutOfRange);
        }
        if keepalive_interval_seconds.is_some() && keepalive_max == 0 {
            return Err(SessionConfigError::ZeroKeepaliveMaximum);
        }

        Ok(Self {
            window_size,
            maximum_packet_size,
            channel_buffer_size,
            keepalive_interval_seconds,
            keepalive_max,
        })
    }

    /// Conservative resource bounds suitable for the first admission harness.
    #[must_use]
    pub const fn admission_default() -> Self {
        Self {
            window_size: 2 * 1024 * 1024,
            maximum_packet_size: 32 * 1024,
            channel_buffer_size: 64,
            keepalive_interval_seconds: Some(30),
            keepalive_max: 3,
        }
    }

    #[must_use]
    pub const fn window_size(self) -> u32 {
        self.window_size
    }

    #[must_use]
    pub const fn maximum_packet_size(self) -> u32 {
        self.maximum_packet_size
    }

    #[must_use]
    pub const fn channel_buffer_size(self) -> usize {
        self.channel_buffer_size
    }
}

/// A connection plan that can only evaluate the presented host key.
///
/// It owns Russh configuration privately. Authentication and channel APIs are
/// intentionally absent until a later capability is admitted by the shared
/// local-server harness.
pub struct PreAuthenticationSession {
    limits: SessionLimits,
    pub(crate) handler: ExactHostKeyHandler,
    #[allow(
        dead_code,
        reason = "a later admitted capability consumes this only after the local-server harness passes"
    )]
    pub(crate) config: Arc<client::Config>,
}

impl PreAuthenticationSession {
    /// Composes bounded Russh settings with one exact persisted host key.
    #[must_use]
    pub fn new(expected_host_key: PublicKeyFingerprint, limits: SessionLimits) -> Self {
        let config = client::Config {
            window_size: limits.window_size,
            maximum_packet_size: limits.maximum_packet_size,
            channel_buffer_size: limits.channel_buffer_size,
            keepalive_interval: limits.keepalive_interval_seconds.map(Duration::from_secs),
            keepalive_max: limits.keepalive_max,
            anonymous: false,
            nodelay: true,
            ..client::Config::default()
        };

        Self {
            limits,
            handler: ExactHostKeyHandler::new(expected_host_key),
            config: Arc::new(config),
        }
    }

    #[must_use]
    pub const fn limits(&self) -> SessionLimits {
        self.limits
    }

    /// Returns whether the presented key is the one exact persisted key.
    #[must_use]
    pub fn accepts_presented_key(&self, key: &russh::keys::PublicKey) -> bool {
        self.handler.matches(key)
    }

    /// Transfers the private Russh pieces to the only future capability that
    /// is allowed to establish an authenticated connection.
    pub(crate) fn into_russh_parts(self) -> (Arc<client::Config>, ExactHostKeyHandler) {
        (self.config, self.handler)
    }
}

#[cfg(test)]
mod tests {
    use super::{PreAuthenticationSession, SessionConfigError, SessionLimits};
    use crate::presented_fingerprint;
    use choosh_core::ssh_identity::PublicKeyFingerprint;
    use russh::keys::PublicKey;

    const FIXTURE_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti";

    fn fixture_key() -> PublicKey {
        FIXTURE_KEY
            .parse()
            .expect("repository fixture public key is valid")
    }

    #[test]
    fn configuration_is_bounded_before_russh_composition() {
        assert_eq!(
            SessionLimits::new(0, 1_024, 1, None, 0),
            Err(SessionConfigError::WindowSizeOutOfRange)
        );
        assert_eq!(
            SessionLimits::new(65_536, 0, 1, None, 0),
            Err(SessionConfigError::PacketSizeOutOfRange)
        );
        assert_eq!(
            SessionLimits::new(65_536, 65_535, 0, None, 0),
            Err(SessionConfigError::ChannelBufferOutOfRange)
        );
        assert_eq!(
            SessionLimits::new(65_536, 1_024, 1, Some(1), 0),
            Err(SessionConfigError::ZeroKeepaliveMaximum)
        );
        assert_eq!(
            SessionLimits::new(65_536, 1_024, 1, Some(3_601), 1),
            Err(SessionConfigError::KeepaliveIntervalOutOfRange)
        );
    }

    #[test]
    fn pre_authentication_session_composes_exact_key_and_bounded_config() {
        let key = fixture_key();
        let expected = PublicKeyFingerprint::parse(presented_fingerprint(&key)).unwrap();
        let limits = SessionLimits::admission_default();
        let session = PreAuthenticationSession::new(expected, limits);

        assert!(session.accepts_presented_key(&key));
        assert_eq!(session.limits(), limits);
        assert!(!session.config.anonymous);
        assert_eq!(session.config.maximum_packet_size, 32 * 1024);
        assert_eq!(session.config.channel_buffer_size, 64);
        assert_eq!(session.config.keepalive_max, 3);
        assert!(session.handler.matches(&key));
    }

    #[test]
    fn different_key_cannot_pass_pre_authentication_admission() {
        let key = fixture_key();
        let expected =
            PublicKeyFingerprint::parse("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .unwrap();
        let session = PreAuthenticationSession::new(expected, SessionLimits::admission_default());

        assert!(!session.accepts_presented_key(&key));
    }
}
