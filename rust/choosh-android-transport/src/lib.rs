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
    fn take_pre_authentication_session(
        &mut self,
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
        .take_pre_authentication_session(plan.known_host)
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
    use super::*;
    use choosh_core::ssh_identity::PublicKeyFingerprint;
    use choosh_ssh::{CredentialSigner, SessionLimits, presented_fingerprint};
    use russh::keys::agent::AgentIdentity;
    use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
    use russh::server::{self, Auth};
    use signature::Signer as _;
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn keeps_the_platform_composition_boundary_explicit() {
        assert_eq!(COMPOSITION_BOUNDARY, "android-opaque-handles-to-russh");
        assert!(super::AndroidHandle::new(0).is_none());
        assert!(super::AndroidHandle::new(1).is_some());
    }

    struct FixtureServer;
    impl server::Handler for FixtureServer {
        type Error = russh::Error;
    }

    struct AcceptingServer {
        expected_credential: PublicKeyFingerprint,
    }
    impl server::Handler for AcceptingServer {
        type Error = russh::Error;

        async fn auth_publickey_offered(
            &mut self,
            _: &str,
            key: &PublicKey,
        ) -> Result<Auth, Self::Error> {
            Ok(
                if presented_fingerprint(key) == self.expected_credential.as_str() {
                    Auth::Accept
                } else {
                    Auth::reject()
                },
            )
        }

        async fn auth_publickey(&mut self, _: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
            Ok(
                if presented_fingerprint(key) == self.expected_credential.as_str() {
                    Auth::Accept
                } else {
                    Auth::reject()
                },
            )
        }
    }

    #[derive(Debug)]
    struct FixtureSignerError;
    impl From<russh::SendError> for FixtureSignerError {
        fn from(_: russh::SendError) -> Self {
            Self
        }
    }

    struct CountingSigner {
        key: PrivateKey,
        calls: Arc<AtomicUsize>,
    }
    impl CredentialSigner for CountingSigner {
        type Error = FixtureSignerError;
        fn public_key(&self) -> PublicKey {
            self.key.public_key().clone()
        }
        async fn sign(
            &mut self,
            _: &AgentIdentity,
            _: Option<HashAlg>,
            mut payload: Vec<u8>,
        ) -> Result<Vec<u8>, Self::Error> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let signature = self
                .key
                .try_sign(&payload)
                .expect("generated Ed25519 fixture key signs SSH authentication payloads");
            let signature = Vec::try_from(signature)
                .expect("generated Ed25519 fixture signature has SSH wire encoding");
            payload.extend_from_slice(
                &u32::try_from(signature.len())
                    .expect("generated Ed25519 fixture signature is bounded")
                    .to_be_bytes(),
            );
            payload.extend_from_slice(&signature);
            Ok(payload)
        }
    }

    struct FixtureRuntime {
        session: Option<choosh_ssh::PreAuthenticationSession>,
        stream: Option<tokio::io::DuplexStream>,
        signer: Option<CountingSigner>,
    }
    impl AndroidSshRuntime for FixtureRuntime {
        type Stream = tokio::io::DuplexStream;
        type Error = Infallible;
        fn open_stream(&mut self, _: AndroidHandle) -> Result<Self::Stream, Self::Error> {
            Ok(self.stream.take().expect("one stream"))
        }
        fn username(&self, _: AndroidHandle) -> Result<choosh_ssh::SshUsername, Self::Error> {
            Ok(choosh_ssh::SshUsername::parse("fixture-user").unwrap())
        }
    }
    impl ExactHostSessionResolver for FixtureRuntime {
        type Error = Infallible;
        fn take_pre_authentication_session(
            &mut self,
            _: AndroidHandle,
        ) -> Result<choosh_ssh::PreAuthenticationSession, Self::Error> {
            Ok(self.session.take().expect("one session"))
        }
    }
    impl KeystoreSignerResolver for FixtureRuntime {
        type Signer = CountingSigner;
        type Error = Infallible;
        fn signer(
            &mut self,
            _: AndroidHandle,
            _: AndroidHandle,
        ) -> Result<Self::Signer, Self::Error> {
            Ok(self.signer.take().expect("one signer"))
        }
    }

    fn generated_key() -> PrivateKey {
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
    }

    #[tokio::test]
    async fn changed_host_key_rejects_before_android_signer_is_called() {
        let presented = generated_key();
        let different = generated_key();
        let credential = generated_key();
        let calls = Arc::new(AtomicUsize::new(0));
        let (client, server_stream) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(server::run_stream(
            Arc::new(server::Config {
                keys: vec![presented],
                ..server::Config::default()
            }),
            server_stream,
            FixtureServer,
        ));
        let expected =
            PublicKeyFingerprint::parse(presented_fingerprint(different.public_key())).unwrap();
        let mut runtime = FixtureRuntime {
            session: Some(choosh_ssh::PreAuthenticationSession::new(
                expected,
                SessionLimits::admission_default(),
            )),
            stream: Some(client),
            signer: Some(CountingSigner {
                key: credential,
                calls: Arc::clone(&calls),
            }),
        };
        let result = connect_verified(
            &mut runtime,
            AndroidConnectionPlan::new(1, 2, 3, 4, 5).unwrap(),
        )
        .await;
        assert!(matches!(
            result,
            Err(AndroidTransportError::Connection(
                choosh_ssh::VerifiedConnectionError::TransportFailed
            ))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn exact_host_key_reaches_android_signer_and_authenticates() {
        let host = generated_key();
        let credential = generated_key();
        let expected =
            PublicKeyFingerprint::parse(presented_fingerprint(host.public_key())).unwrap();
        let expected_credential =
            PublicKeyFingerprint::parse(presented_fingerprint(credential.public_key())).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (client, server_stream) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(server::run_stream(
            Arc::new(server::Config {
                keys: vec![host],
                ..server::Config::default()
            }),
            server_stream,
            AcceptingServer {
                expected_credential,
            },
        ));
        let mut runtime = FixtureRuntime {
            session: Some(choosh_ssh::PreAuthenticationSession::new(
                expected,
                SessionLimits::admission_default(),
            )),
            stream: Some(client),
            signer: Some(CountingSigner {
                key: credential,
                calls: Arc::clone(&calls),
            }),
        };

        let connection = connect_verified(
            &mut runtime,
            AndroidConnectionPlan::new(1, 2, 3, 4, 5).unwrap(),
        )
        .await
        .expect("the exact generated host key and credential authenticate");

        assert!(calls.load(Ordering::SeqCst) > 0);
        drop(connection);
        server.abort();
    }
}
