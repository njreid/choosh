//! Android outer composition root for admitted native SSH transport.
//!
//! This crate is the only permitted dependency join between opaque Android
//! handles and the Russh adapter. Concrete JNI socket and Keystore callbacks
//! remain injected capabilities; no credential bytes are represented here.

#![forbid(unsafe_code)]

use tokio::io::{AsyncRead, AsyncWrite};

/// Marker for the outer Android/Russh composition root.
///
/// The runtime adapter is deliberately not implemented until its JNI stream
/// and callback contracts have deterministic generated-key acceptance tests.
pub const COMPOSITION_BOUNDARY: &str = "android-opaque-handles-to-russh";

/// Opaque Android registry reference used only by the outer composition root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AndroidHandle(u64);

impl AndroidHandle {
    /// Creates a non-zero opaque registry reference.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }
}

/// Five opaque Android registry references needed for one SSH attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AndroidConnectionPlan {
    endpoint: AndroidHandle,
    username: AndroidHandle,
    known_host: AndroidHandle,
    credential_reference: AndroidHandle,
    public_key: AndroidHandle,
}

impl AndroidConnectionPlan {
    /// Creates a plan only when every registry reference is non-zero.
    #[must_use]
    pub fn new(
        endpoint: u64,
        username: u64,
        known_host: u64,
        credential_reference: u64,
        public_key: u64,
    ) -> Option<Self> {
        Some(Self {
            endpoint: AndroidHandle::new(endpoint)?,
            username: AndroidHandle::new(username)?,
            known_host: AndroidHandle::new(known_host)?,
            credential_reference: AndroidHandle::new(credential_reference)?,
            public_key: AndroidHandle::new(public_key)?,
        })
    }
}

/// Android-owned capabilities required to create one real Russh connection.
///
/// Implementations must resolve every opaque handle internally. They never
/// export host strings, private keys, or JVM object references to shared code.
pub trait AndroidSshRuntime {
    type Stream;
    type Error;

    /// Obtains the bounded byte stream for an opaque endpoint registry record.
    ///
    /// # Errors
    ///
    /// Returns the platform adapter's content-free transport failure.
    fn open_stream(&mut self, endpoint: AndroidHandle) -> Result<Self::Stream, Self::Error>;

    /// Resolves a canonical SSH user name from Android-owned metadata.
    ///
    /// # Errors
    ///
    /// Returns the platform adapter's content-free metadata or validation failure.
    fn username(&self, username: AndroidHandle) -> Result<choosh_ssh::SshUsername, Self::Error>;
}

/// Resolves a persisted exact host identity into the Russh pre-authentication plan.
pub trait ExactHostSessionResolver {
    type Error;

    /// # Errors
    ///
    /// Returns a content-free registry or host-identity validation failure.
    fn pre_authentication_session(
        &self,
        known_host: AndroidHandle,
    ) -> Result<choosh_ssh::PreAuthenticationSession, Self::Error>;
}

/// Resolves an opaque Android credential into the Russh signing capability.
///
/// The concrete implementation invokes the Java Keystore callback per SSH
/// challenge; it MUST NOT return private-key bytes.
pub trait KeystoreSignerResolver {
    type Signer: choosh_ssh::CredentialSigner;
    type Error;

    /// # Errors
    ///
    /// Returns a content-free registry or public-metadata validation failure.
    fn signer(
        &mut self,
        credential_reference: AndroidHandle,
        public_key: AndroidHandle,
    ) -> Result<Self::Signer, Self::Error>;
}

/// Typed failures while composing Android-owned capabilities into Russh.
#[derive(Debug)]
pub enum AndroidTransportError<RuntimeError, SessionError, SignerError> {
    Runtime(RuntimeError),
    Session(SessionError),
    Signer(SignerError),
    Connection(choosh_ssh::VerifiedConnectionError<SignerError>),
}

/// Opens and authenticates exactly one injected Android SSH stream.
///
/// The exact-host session is resolved before the signer. Russh then performs
/// its host-key callback before it asks the signer for any challenge proof.
///
/// # Errors
///
/// Returns only typed injected-capability or verified-connection failures.
pub async fn connect_verified<R, E>(
    runtime: &mut R,
    plan: AndroidConnectionPlan,
) -> Result<
    choosh_ssh::VerifiedConnection,
    AndroidTransportError<E, E, <R::Signer as choosh_ssh::CredentialSigner>::Error>,
>
where
    R: AndroidSshRuntime + ExactHostSessionResolver<Error = E> + KeystoreSignerResolver<Error = E>,
    R: AndroidSshRuntime<Error = E>,
    R::Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let session = runtime
        .pre_authentication_session(plan.known_host)
        .map_err(AndroidTransportError::Session)?;
    let username = runtime
        .username(plan.username)
        .map_err(AndroidTransportError::Runtime)?;
    let signer = runtime
        .signer(plan.credential_reference, plan.public_key)
        .map_err(AndroidTransportError::Runtime)?;
    let stream = runtime
        .open_stream(plan.endpoint)
        .map_err(AndroidTransportError::Runtime)?;
    choosh_ssh::VerifiedConnection::connect_stream(
        session,
        stream,
        username,
        choosh_ssh::CredentialSignerAdapter::new(signer),
    )
    .await
    .map_err(AndroidTransportError::Connection)
}

#[cfg(test)]
mod tests {
    use super::COMPOSITION_BOUNDARY;

    #[test]
    fn keeps_the_platform_composition_boundary_explicit() {
        assert_eq!(COMPOSITION_BOUNDARY, "android-opaque-handles-to-russh");
        assert!(super::AndroidHandle::new(0).is_none());
        assert!(super::AndroidHandle::new(1).is_some());
    }
}
