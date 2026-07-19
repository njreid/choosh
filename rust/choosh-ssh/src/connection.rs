//! Authenticated SSH connection admission without channel authority.
//!
//! A caller injects a stream and a credential signer. The connection first
//! completes Russh's exact-host-key callback via [`PreAuthenticationSession`],
//! then attempts public-key authentication. The Russh handle is retained
//! privately and this module exposes no PTY, exec, SFTP, or forwarding API.

use tokio::io::{AsyncRead, AsyncWrite};

use crate::{CredentialSigner, CredentialSignerAdapter, PreAuthenticationSession};

const MAX_USERNAME_BYTES: usize = 64;

/// A bounded SSH user name safe to pass to the protocol layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshUsername(String);

impl SshUsername {
    /// Validates the SSH user name before any network or signing operation.
    ///
    /// # Errors
    ///
    /// Returns a stable error for empty, oversized, or non-ASCII-safe names.
    pub fn parse(value: impl Into<String>) -> Result<Self, UsernameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(UsernameError::Empty);
        }
        if value.len() > MAX_USERNAME_BYTES {
            return Err(UsernameError::TooLong);
        }
        let mut characters = value.bytes();
        let Some(first) = characters.next() else {
            return Err(UsernameError::Empty);
        };
        if !(first.is_ascii_alphanumeric() || first == b'_')
            || !characters
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(UsernameError::InvalidCharacter);
        }
        Ok(Self(value))
    }

    fn into_inner(self) -> String {
        self.0
    }
}

/// Stable SSH user-name validation failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsernameError {
    Empty,
    TooLong,
    InvalidCharacter,
}

/// Typed failure from the narrow verified-connection admission path.
#[derive(Debug)]
pub enum VerifiedConnectionError<E> {
    /// The exact-host-key handshake or underlying stream failed.
    TransportFailed,
    /// The injected signer declined or failed to produce a public-key proof.
    CredentialSigner(E),
    /// The remote SSH server rejected the validated public-key attempt.
    AuthenticationRejected,
}

/// A live connection authenticated after exact host-key verification.
///
/// The underlying handle intentionally remains private. A later capability
/// slice must explicitly expose individually bounded channel operations.
pub struct VerifiedConnection {
    #[allow(
        dead_code,
        reason = "the handle preserves the authenticated transport until an admitted channel capability consumes it"
    )]
    handle: russh::client::Handle<crate::ExactHostKeyHandler>,
}

impl VerifiedConnection {
    /// Connects an injected stream and authenticates with the injected signer.
    ///
    /// This is the only transition from a pre-authentication plan to a live
    /// transport. `russh::client::connect_stream` completes host-key checking
    /// before yielding its handle, so the signer is not invoked on a rejected
    /// or changed host key.
    ///
    /// # Errors
    ///
    /// Returns [`VerifiedConnectionError::TransportFailed`] when host-key
    /// verification or the injected stream fails,
    /// [`VerifiedConnectionError::CredentialSigner`] when the signing
    /// capability fails, and [`VerifiedConnectionError::AuthenticationRejected`]
    /// when the remote rejects the public-key proof.
    pub async fn connect_stream<R, S>(
        session: PreAuthenticationSession,
        stream: R,
        username: SshUsername,
        mut signer: CredentialSignerAdapter<S>,
    ) -> Result<Self, VerifiedConnectionError<S::Error>>
    where
        R: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        S: CredentialSigner,
    {
        let (config, handler) = session.into_russh_parts();
        let mut handle = russh::client::connect_stream(config, stream, handler)
            .await
            .map_err(|_| VerifiedConnectionError::TransportFailed)?;

        let result = handle
            .authenticate_publickey_with(
                username.into_inner(),
                signer.public_key(),
                None,
                &mut signer,
            )
            .await
            .map_err(VerifiedConnectionError::CredentialSigner)?;
        if !result.success() {
            return Err(VerifiedConnectionError::AuthenticationRejected);
        }

        Ok(Self { handle })
    }
}

#[cfg(test)]
mod tests {
    use super::{SshUsername, UsernameError};

    #[test]
    fn username_is_bounded_and_protocol_safe() {
        assert_eq!(
            SshUsername::parse("fixture-user"),
            Ok(SshUsername("fixture-user".into()))
        );
        assert_eq!(SshUsername::parse(""), Err(UsernameError::Empty));
        assert_eq!(
            SshUsername::parse("-fixture"),
            Err(UsernameError::InvalidCharacter)
        );
        assert_eq!(
            SshUsername::parse("fixture user"),
            Err(UsernameError::InvalidCharacter)
        );
        assert_eq!(
            SshUsername::parse("fixture\nuser"),
            Err(UsernameError::InvalidCharacter)
        );
        assert_eq!(
            SshUsername::parse("a".repeat(65)),
            Err(UsernameError::TooLong)
        );
    }
}
