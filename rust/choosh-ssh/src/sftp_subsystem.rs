//! Admission of the fixed SFTP subsystem over an authenticated Russh session.
//!
//! This module deliberately exposes no filesystem operation.  Starting an
//! SFTP subsystem is not, by itself, proof that the server has confined its
//! filesystem view to a selected workspace root (including symlink handling).
//! Only a host adapter that provides that proof may implement
//! [`crate::RootedSftpTransport`].  Keeping the raw session private prevents a
//! caller from bypassing the lexical and root-confinement boundary in
//! [`crate::ConfinedSftp`].

use russh_sftp::client::{Config as RusshSftpConfig, SftpSession};

use crate::VerifiedConnection;

const SFTP_SUBSYSTEM_NAME: &str = "sftp";
const MAX_PACKET_BYTES: u32 = 256 * 1024;
const MAX_CONCURRENT_WRITES: usize = 8;
const MAX_REQUEST_TIMEOUT_SECONDS: u64 = 60;

/// Bounded resource policy for one admitted SFTP subsystem.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SftpSubsystemLimits {
    pub max_packet_bytes: u32,
    pub max_concurrent_writes: usize,
    pub request_timeout_seconds: u64,
}

impl SftpSubsystemLimits {
    /// Validates bounded SFTP client resource limits.
    ///
    /// # Errors
    ///
    /// Returns [`SftpSubsystemError::InvalidLimits`] when a value is zero or
    /// exceeds this adapter's fixed resource ceiling.
    pub const fn new(
        max_packet_bytes: u32,
        max_concurrent_writes: usize,
        request_timeout_seconds: u64,
    ) -> Result<Self, SftpSubsystemError> {
        if max_packet_bytes == 0
            || max_packet_bytes > MAX_PACKET_BYTES
            || max_concurrent_writes == 0
            || max_concurrent_writes > MAX_CONCURRENT_WRITES
            || request_timeout_seconds == 0
            || request_timeout_seconds > MAX_REQUEST_TIMEOUT_SECONDS
        {
            return Err(SftpSubsystemError::InvalidLimits);
        }
        Ok(Self {
            max_packet_bytes,
            max_concurrent_writes,
            request_timeout_seconds,
        })
    }

    #[must_use]
    pub const fn default_bounded() -> Self {
        Self {
            max_packet_bytes: 64 * 1024,
            max_concurrent_writes: 4,
            request_timeout_seconds: 10,
        }
    }

    const fn as_russh_config(self) -> RusshSftpConfig {
        RusshSftpConfig {
            max_packet_len: self.max_packet_bytes,
            max_concurrent_writes: self.max_concurrent_writes,
            request_timeout_secs: self.request_timeout_seconds,
        }
    }
}

/// Stable failures while opening or closing the fixed SFTP subsystem.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SftpSubsystemError {
    InvalidLimits,
    TransportFailed,
}

impl SftpSubsystemError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::TransportFailed => "transport_error",
        }
    }
}

/// A live, bounded SFTP subsystem with no public path-operation authority.
pub struct RusshSftpSubsystem {
    session: SftpSession,
}

impl VerifiedConnection {
    /// Opens only the constant `sftp` SSH subsystem with bounded client limits.
    ///
    /// This method cannot select a remote root or accept a path.  The returned
    /// session remains opaque until a root-confined host transport adapter is
    /// available, so a standard SFTP server cannot accidentally acquire
    /// authority over arbitrary remote paths through this API.
    ///
    /// # Errors
    ///
    /// Returns [`SftpSubsystemError::TransportFailed`] when the server rejects
    /// the subsystem, terminates the channel, or fails SFTP initialization.
    pub async fn open_sftp_subsystem(
        &self,
        limits: SftpSubsystemLimits,
    ) -> Result<RusshSftpSubsystem, SftpSubsystemError> {
        let channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|_| SftpSubsystemError::TransportFailed)?;
        channel
            .request_subsystem(true, SFTP_SUBSYSTEM_NAME)
            .await
            .map_err(|_| SftpSubsystemError::TransportFailed)?;
        let session = SftpSession::new_with_config(channel.into_stream(), limits.as_russh_config())
            .await
            .map_err(|_| SftpSubsystemError::TransportFailed)?;
        Ok(RusshSftpSubsystem { session })
    }
}

impl RusshSftpSubsystem {
    /// Closes the opaque SFTP subsystem without exposing filesystem operations.
    ///
    /// # Errors
    ///
    /// Returns [`SftpSubsystemError::TransportFailed`] if the channel cannot
    /// be shut down cleanly.
    pub async fn close(self) -> Result<(), SftpSubsystemError> {
        self.session
            .close()
            .await
            .map_err(|_| SftpSubsystemError::TransportFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::{SftpSubsystemError, SftpSubsystemLimits};

    #[test]
    fn bounded_defaults_are_stable_and_accepted() {
        let limits = SftpSubsystemLimits::default_bounded();
        assert_eq!(limits.max_packet_bytes, 64 * 1024);
        assert_eq!(limits.max_concurrent_writes, 4);
        assert_eq!(limits.request_timeout_seconds, 10);
        assert_eq!(
            SftpSubsystemLimits::new(
                limits.max_packet_bytes,
                limits.max_concurrent_writes,
                limits.request_timeout_seconds,
            ),
            Ok(limits)
        );
    }

    #[test]
    fn invalid_or_unbounded_resource_requests_fail_closed() {
        for limits in [
            SftpSubsystemLimits::new(0, 1, 1),
            SftpSubsystemLimits::new(256 * 1024 + 1, 1, 1),
            SftpSubsystemLimits::new(1, 0, 1),
            SftpSubsystemLimits::new(1, 9, 1),
            SftpSubsystemLimits::new(1, 1, 0),
            SftpSubsystemLimits::new(1, 1, 61),
        ] {
            assert_eq!(limits, Err(SftpSubsystemError::InvalidLimits));
        }
    }
}
