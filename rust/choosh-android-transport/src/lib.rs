//! Android outer composition root for admitted native SSH transport.
//!
//! This crate is the only permitted dependency join between opaque Android
//! handles and the Russh adapter. Concrete JNI socket and Keystore callbacks
//! remain injected capabilities; no credential bytes are represented here.

#![forbid(unsafe_code)]

use std::io;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Marker for the outer Android/Russh composition root.
///
/// The runtime adapter is deliberately not implemented until its JNI stream
/// and callback contracts have deterministic generated-key acceptance tests.
pub const COMPOSITION_BOUNDARY: &str = "android-opaque-handles-to-russh";

/// Per-callback byte limits for the Android-owned stream adapter.
///
/// These limits bound a single read or write crossing the eventual JNI
/// boundary. They deliberately do not impose a connection lifetime byte
/// budget: SSH is a long-lived multiplexed protocol and lifetime accounting
/// belongs to the individual RPC, SFTP, or terminal capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamChunkLimits {
    read: NonZeroUsize,
    write: NonZeroUsize,
}

impl StreamChunkLimits {
    /// Creates non-zero read and write callback limits.
    #[must_use]
    pub fn new(read: usize, write: usize) -> Option<Self> {
        Some(Self {
            read: NonZeroUsize::new(read)?,
            write: NonZeroUsize::new(write)?,
        })
    }

    /// Returns the maximum bytes requested from one native read callback.
    #[must_use]
    pub const fn read_bytes(self) -> usize {
        self.read.get()
    }

    /// Returns the maximum bytes supplied to one native write callback.
    #[must_use]
    pub const fn write_bytes(self) -> usize {
        self.write.get()
    }
}

/// Bounded adapter for a byte stream resolved by Android-owned metadata.
///
/// The concrete JNI runtime owns the socket and supplies it as `S`; this
/// wrapper makes it impossible for Russh to make an unbounded callback into
/// that runtime. It contains no JVM object, endpoint, username, or credential
/// material and is therefore safe to exercise in a headless host test.
pub struct BoundedAndroidStream<S> {
    inner: S,
    limits: StreamChunkLimits,
    read_scratch: Box<[u8]>,
}

impl<S> BoundedAndroidStream<S> {
    /// Wraps one Android-owned stream with non-zero callback limits.
    #[must_use]
    pub fn new(inner: S, limits: StreamChunkLimits) -> Self {
        Self {
            inner,
            limits,
            read_scratch: vec![0; limits.read_bytes()].into_boxed_slice(),
        }
    }

    /// Returns the configured callback limits.
    #[must_use]
    pub const fn limits(&self) -> StreamChunkLimits {
        self.limits
    }

    /// Releases the wrapped Android-owned stream after the session ends.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S> AsyncRead for BoundedAndroidStream<S>
where
    S: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let requested = output.remaining().min(this.limits.read_bytes());
        if requested == 0 {
            return Poll::Ready(Ok(()));
        }

        let mut bounded = ReadBuf::new(&mut this.read_scratch[..requested]);
        match Pin::new(&mut this.inner).poll_read(context, &mut bounded) {
            Poll::Ready(Ok(())) => {
                output.put_slice(bounded.filled());
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> AsyncWrite for BoundedAndroidStream<S>
where
    S: AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        input: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let bounded = &input[..input.len().min(this.limits.write_bytes())];
        Pin::new(&mut this.inner).poll_write(context, bounded)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(context)
    }
}

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
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::io;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};
    use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadBuf};

    #[test]
    fn keeps_the_platform_composition_boundary_explicit() {
        assert_eq!(COMPOSITION_BOUNDARY, "android-opaque-handles-to-russh");
        assert!(super::AndroidHandle::new(0).is_none());
        assert!(super::AndroidHandle::new(1).is_some());
    }

    #[derive(Default)]
    struct RecordingStream {
        unread: VecDeque<u8>,
        read_capacities: Vec<usize>,
        writes: Vec<Vec<u8>>,
    }

    impl RecordingStream {
        fn with_read(bytes: impl IntoIterator<Item = u8>) -> Self {
            Self {
                unread: bytes.into_iter().collect(),
                ..Self::default()
            }
        }
    }

    impl AsyncRead for RecordingStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            output: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            self.read_capacities.push(output.remaining());
            let read = output.remaining().min(self.unread.len());
            output.put_slice(&self.unread.drain(..read).collect::<Vec<_>>());
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for RecordingStream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            input: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.writes.push(input.to_vec());
            Poll::Ready(Ok(input.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn bounded_android_stream_never_crosses_callback_chunk_limits() {
        let limits = StreamChunkLimits::new(3, 2).expect("non-zero chunk limits");
        let mut stream = BoundedAndroidStream::new(RecordingStream::with_read(*b"abcdefg"), limits);
        let mut read = Vec::new();
        stream
            .read_to_end(&mut read)
            .await
            .expect("recording stream reads");
        stream
            .write_all(b"12345")
            .await
            .expect("recording stream writes");
        stream
            .shutdown()
            .await
            .expect("recording stream shuts down");

        let recorded = stream.into_inner();
        assert_eq!(read, b"abcdefg");
        assert!(recorded.read_capacities.iter().all(|&size| size <= 3));
        assert_eq!(
            recorded.writes,
            [b"12".to_vec(), b"34".to_vec(), b"5".to_vec()]
        );
    }

    #[test]
    fn stream_chunk_limits_reject_zero_callbacks() {
        assert!(StreamChunkLimits::new(0, 1).is_none());
        assert!(StreamChunkLimits::new(1, 0).is_none());
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
