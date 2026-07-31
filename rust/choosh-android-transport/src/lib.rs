//! Android outer composition root for admitted native SSH transport.
//!
//! This crate is the only permitted dependency join between opaque Android
//! handles and the Russh adapter. Concrete JNI socket and Keystore callbacks
//! remain injected capabilities; no credential bytes are represented here.

#![forbid(unsafe_code)]

use choosh_protocol::envelope::Response;
use choosh_protocol::wire::{WireEnvelope, WireError, decode_envelope, encode_response};
use std::io;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::task::JoinHandle;

/// Marker for the outer Android/Russh composition root.
///
/// The generated-key acceptance tests this boundary waited on now exist in this
/// crate: they drive exact-host admission, Keystore-shaped signing, and a fixed
/// `git.status` RPC through a real `chooshd` private socket. The concrete JNI
/// runtime adapter still lives in `choosh-android-bridge`, never here.
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

/// Opaque failure of one Android callback operation.
///
/// The Android side deliberately reports no message, path, errno, or byte
/// content across this boundary. The type carries no payload so a callback
/// cannot widen the boundary by attaching diagnostic text to a failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AndroidIoFailure;

/// Thread-safe, bounded byte capability supplied by an Android runtime lease.
///
/// Implementors may block while entering their platform socket API. The
/// adapter below runs each operation on Tokio's blocking pool so a Russh worker
/// is never held by an Android socket read. Read and write are intentionally
/// separate operations, allowing TCP's independent directions to make progress.
pub trait BlockingAndroidIo: Send + Sync + 'static {
    /// Performs one bounded read, returning zero for EOF.
    ///
    /// # Errors
    ///
    /// Returns [`AndroidIoFailure`] when the Android callback cannot complete
    /// the read. The failure is deliberately opaque.
    fn read(&self, output: &mut [u8]) -> Result<usize, AndroidIoFailure>;

    /// Performs one bounded write.
    ///
    /// # Errors
    ///
    /// Returns [`AndroidIoFailure`] when the Android callback cannot complete
    /// the write. The failure is deliberately opaque.
    fn write(&self, input: &[u8]) -> Result<(), AndroidIoFailure>;
}

/// Asynchronous stream adapter for one Android-owned blocking socket lease.
///
/// The adapter retains no endpoint, credential, or JVM reference. It copies
/// each bounded operation into a blocking task and converts all callback
/// failures into a content-free `io::Error`.
pub struct BlockingAndroidStream<C> {
    callbacks: Arc<C>,
    limits: StreamChunkLimits,
    read_task: Option<JoinHandle<Result<Vec<u8>, AndroidIoFailure>>>,
    write_task: Option<JoinHandle<Result<usize, AndroidIoFailure>>>,
}

impl<C: BlockingAndroidIo> BlockingAndroidStream<C> {
    /// Creates an async stream over one thread-safe Android callback capability.
    #[must_use]
    pub const fn new(callbacks: Arc<C>, limits: StreamChunkLimits) -> Self {
        Self {
            callbacks,
            limits,
            read_task: None,
            write_task: None,
        }
    }

    fn poll_pending_write(&mut self, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let Some(task) = &mut self.write_task else {
            return Poll::Ready(Ok(()));
        };
        match Pin::new(task).poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(Ok(_))) => {
                self.write_task = None;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Ok(Err(AndroidIoFailure)) | Err(_)) => {
                self.write_task = None;
                Poll::Ready(Err(callback_io_error()))
            }
        }
    }
}

impl<C: BlockingAndroidIo> AsyncRead for BlockingAndroidStream<C> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.read_task.is_none() {
            let requested = output.remaining().min(this.limits.read_bytes());
            if requested == 0 {
                return Poll::Ready(Ok(()));
            }
            let callbacks = Arc::clone(&this.callbacks);
            this.read_task = Some(tokio::task::spawn_blocking(move || {
                let mut bytes = vec![0; requested];
                let length = callbacks.read(&mut bytes)?;
                if length > bytes.len() {
                    return Err(AndroidIoFailure);
                }
                bytes.truncate(length);
                Ok(bytes)
            }));
        }
        let Some(task) = &mut this.read_task else {
            return Poll::Ready(Err(callback_io_error()));
        };
        match Pin::new(task).poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(Ok(bytes))) => {
                this.read_task = None;
                output.put_slice(&bytes);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Ok(Err(AndroidIoFailure)) | Err(_)) => {
                this.read_task = None;
                Poll::Ready(Err(callback_io_error()))
            }
        }
    }
}

impl<C: BlockingAndroidIo> AsyncWrite for BlockingAndroidStream<C> {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        input: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.write_task.is_none() {
            let length = input.len().min(this.limits.write_bytes());
            if length == 0 {
                return Poll::Ready(Ok(0));
            }
            let bytes = input[..length].to_vec();
            let callbacks = Arc::clone(&this.callbacks);
            this.write_task = Some(tokio::task::spawn_blocking(move || {
                callbacks.write(&bytes)?;
                Ok(bytes.len())
            }));
        }
        let Some(task) = &mut this.write_task else {
            return Poll::Ready(Err(callback_io_error()));
        };
        match Pin::new(task).poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(Ok(length))) => {
                this.write_task = None;
                Poll::Ready(Ok(length))
            }
            Poll::Ready(Ok(Err(AndroidIoFailure)) | Err(_)) => {
                this.write_task = None;
                Poll::Ready(Err(callback_io_error()))
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().poll_pending_write(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().poll_pending_write(context)
    }
}

fn callback_io_error() -> io::Error {
    io::Error::other("android runtime callback unavailable")
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

/// Android outer runtime with a bounded native stream at its only I/O exit.
///
/// `R` remains responsible for resolving opaque Android handles and for the
/// JNI socket implementation. This adapter is the composition step that puts
/// [`BoundedAndroidStream`] between that socket and Russh, while forwarding
/// only the existing narrow metadata, host-session, and signing capabilities.
/// It does not expose a JVM reference, a host path, or key material.
pub struct BoundedAndroidRuntime<R> {
    inner: R,
    stream_limits: StreamChunkLimits,
}

impl<R> BoundedAndroidRuntime<R> {
    /// Creates one outer runtime whose native stream callbacks are bounded.
    #[must_use]
    pub const fn new(inner: R, stream_limits: StreamChunkLimits) -> Self {
        Self {
            inner,
            stream_limits,
        }
    }

    /// Releases the Android-owned runtime after its connection attempt ends.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R> AndroidSshRuntime for BoundedAndroidRuntime<R>
where
    R: AndroidSshRuntime,
{
    type Stream = BoundedAndroidStream<R::Stream>;
    type Error = R::Error;

    fn open_stream(&mut self, endpoint: AndroidHandle) -> Result<Self::Stream, Self::Error> {
        self.inner
            .open_stream(endpoint)
            .map(|stream| BoundedAndroidStream::new(stream, self.stream_limits))
    }

    fn username(&self, username: AndroidHandle) -> Result<choosh_ssh::SshUsername, Self::Error> {
        self.inner.username(username)
    }
}

impl<R> ExactHostSessionResolver for BoundedAndroidRuntime<R>
where
    R: ExactHostSessionResolver,
{
    type Error = R::Error;

    fn take_pre_authentication_session(
        &mut self,
        known_host: AndroidHandle,
    ) -> Result<choosh_ssh::PreAuthenticationSession, Self::Error> {
        self.inner.take_pre_authentication_session(known_host)
    }
}

impl<R> KeystoreSignerResolver for BoundedAndroidRuntime<R>
where
    R: KeystoreSignerResolver,
{
    type Signer = R::Signer;
    type Error = R::Error;

    fn signer(
        &mut self,
        credential_reference: AndroidHandle,
        public_key: AndroidHandle,
    ) -> Result<Self::Signer, Self::Error> {
        self.inner.signer(credential_reference, public_key)
    }
}

/// Typed failures while composing Android-owned capabilities into Russh.
#[derive(Debug)]
pub enum AndroidTransportError<RuntimeError, SessionError, SignerError> {
    Runtime(RuntimeError),
    Session(SessionError),
    Signer(SignerError),
    Connection(choosh_ssh::VerifiedConnectionError<SignerError>),
}

const MAX_ANDROID_RPC_BYTES: usize = 256 * 1024 - 4;

/// The opaque post-authentication session exposed to the Android JNI boundary.
///
/// It accepts one validated Android envelope payload at a time and retains the
/// live Russh handle privately. This is intentionally narrower than a raw
/// byte stream: callers cannot choose an SSH command, argv, channel type, or
/// socket path.
pub struct AndroidRpcSession {
    connection: choosh_ssh::VerifiedConnection,
}

impl AndroidRpcSession {
    /// Wraps a connection which has already completed exact-host admission and
    /// public-key authentication.
    #[must_use]
    pub const fn new(connection: choosh_ssh::VerifiedConnection) -> Self {
        Self { connection }
    }

    /// Carries one Android protocol request through the fixed SSH RPC bridge.
    ///
    /// The input and output are unframed JSON envelope payloads, matching the
    /// Java `GitStatusRpc` boundary. Framing, fixed command selection, and
    /// SSH-channel lifetime remain owned by `choosh-ssh`.
    ///
    /// # Errors
    ///
    /// Returns a content-free classification for malformed Android request
    /// payloads, non-request envelopes, SSH RPC failures, or a response that
    /// exceeds the Android protocol bound.
    pub async fn execute(&self, payload: &[u8]) -> Result<Vec<u8>, AndroidRpcError> {
        let request = decode_android_rpc_request(payload)?;
        let response = self
            .connection
            .request_rpc(request)
            .await
            .map_err(AndroidRpcError::Rpc)?;
        encode_response(
            &Response {
                id: response.id,
                terminal: response.terminal,
            },
            MAX_ANDROID_RPC_BYTES,
        )
        .map_err(AndroidRpcError::Response)
    }
}

/// Stable classifications for the Android-to-fixed-RPC capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AndroidRpcError {
    Request(WireError),
    RequestKind,
    Rpc(choosh_ssh::RpcError),
    Response(WireError),
}

fn decode_android_rpc_request(payload: &[u8]) -> Result<choosh_ssh::RpcRequest, AndroidRpcError> {
    let WireEnvelope::Request(request) =
        decode_envelope(payload, MAX_ANDROID_RPC_BYTES).map_err(AndroidRpcError::Request)?
    else {
        return Err(AndroidRpcError::RequestKind);
    };
    Ok(choosh_ssh::RpcRequest::new(
        request.id,
        request.method,
        request.params,
    ))
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

/// Opens one authenticated Android connection with bounded native callbacks.
///
/// This is the smallest real native runtime composition: callers supply the
/// Android-owned resolver and opaque plan, while the transport crate ensures
/// the raw stream is bounded before the verified Russh admission path starts.
/// Concrete JNI socket and Keystore implementations remain outer adapters.
///
/// # Errors
///
/// Returns only the injected runtime or verified-connection failure.
pub async fn connect_verified_bounded<R, E>(
    runtime: R,
    plan: AndroidConnectionPlan,
    stream_limits: StreamChunkLimits,
) -> Result<
    choosh_ssh::VerifiedConnection,
    AndroidTransportError<E, E, <R::Signer as choosh_ssh::CredentialSigner>::Error>,
>
where
    R: AndroidSshRuntime + ExactHostSessionResolver<Error = E> + KeystoreSignerResolver<Error = E>,
    R: AndroidSshRuntime<Error = E>,
    R::Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut runtime = BoundedAndroidRuntime::new(runtime, stream_limits);
    connect_verified(&mut runtime, plan).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_core::ssh_identity::PublicKeyFingerprint;
    use choosh_host::bridge::BridgeLimits;
    use choosh_host::socket_relay::run_unix_socket_relay;
    use choosh_protocol::envelope::{EnvelopeId, Response, Terminal};
    use choosh_protocol::framing::{FrameDecoder, FrameLimits, encode_frame};
    use choosh_protocol::handshake::{
        PeerIdentity, ProtocolLimits, ProtocolVersion, ServerNegotiator,
    };
    use choosh_protocol::wire::{decode_hello, encode_response, encode_server_reply};
    use choosh_ssh::{CredentialSigner, SessionLimits, presented_fingerprint};
    use chooshd::daemon::{DaemonRpc, HandshakeConfig, bind, serve_once_with_handler};
    use chooshd::git::{StatusLimits, StatusSnapshot, parse_status};
    use chooshd::git_status::{GitStatusError, GitStatusOperation};
    use chooshd::socket::SocketPlan;
    use russh::keys::agent::AgentIdentity;
    use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
    use russh::server::{self, Auth, ChannelOpenHandle, Msg, Session};
    use russh::{Channel, ChannelId};
    use signature::Signer as _;
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::io::{self, Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};
    use std::thread;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadBuf};

    #[test]
    fn keeps_the_platform_composition_boundary_explicit() {
        assert_eq!(COMPOSITION_BOUNDARY, "android-opaque-handles-to-russh");
        assert!(super::AndroidHandle::new(0).is_none());
        assert!(super::AndroidHandle::new(1).is_some());
    }

    #[derive(Default)]
    struct RecordingBlockingIo {
        reads: Mutex<VecDeque<Vec<u8>>>,
        writes: Mutex<Vec<Vec<u8>>>,
    }

    impl BlockingAndroidIo for RecordingBlockingIo {
        fn read(&self, output: &mut [u8]) -> Result<usize, AndroidIoFailure> {
            let bytes = self
                .reads
                .lock()
                .map_err(|_| AndroidIoFailure)?
                .pop_front()
                .unwrap_or_default();
            if bytes.len() > output.len() {
                return Err(AndroidIoFailure);
            }
            output[..bytes.len()].copy_from_slice(&bytes);
            Ok(bytes.len())
        }

        fn write(&self, input: &[u8]) -> Result<(), AndroidIoFailure> {
            self.writes
                .lock()
                .map_err(|_| AndroidIoFailure)?
                .push(input.to_vec());
            Ok(())
        }
    }

    #[tokio::test]
    async fn blocking_android_stream_bounds_callbacks_off_the_async_worker() {
        let callbacks = Arc::new(RecordingBlockingIo {
            reads: Mutex::new(VecDeque::from([vec![1, 2]])),
            writes: Mutex::new(Vec::new()),
        });
        let mut stream = BlockingAndroidStream::new(
            Arc::clone(&callbacks),
            StreamChunkLimits::new(2, 2).unwrap(),
        );
        stream.write_all(&[7, 8, 9]).await.unwrap();
        stream.flush().await.unwrap();
        let mut received = [0; 2];
        stream.read_exact(&mut received).await.unwrap();
        assert_eq!(received, [1, 2]);
        assert_eq!(*callbacks.writes.lock().unwrap(), vec![vec![7, 8], vec![9]]);
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

    struct GitStatusServer {
        expected_credential: PublicKeyFingerprint,
        command_accepted: bool,
        input: Vec<u8>,
        daemon_socket: PathBuf,
    }
    impl server::Handler for GitStatusServer {
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

        async fn channel_open_session(
            &mut self,
            _: Channel<Msg>,
            reply: ChannelOpenHandle,
            _: &mut Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }

        async fn exec_request(
            &mut self,
            channel: ChannelId,
            command: &[u8],
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            self.command_accepted = command == b"choosh-host --exec-stdio-v1";
            if self.command_accepted {
                session.channel_success(channel)?;
            } else {
                session.channel_failure(channel)?;
            }
            Ok(())
        }

        async fn data(
            &mut self,
            _: ChannelId,
            data: &[u8],
            _: &mut Session,
        ) -> Result<(), Self::Error> {
            self.input.extend_from_slice(data);
            Ok(())
        }

        async fn channel_eof(
            &mut self,
            channel: ChannelId,
            session: &mut Session,
        ) -> Result<(), Self::Error> {
            assert!(self.command_accepted);
            let input = fixed_command_stdin(&self.input);
            let mut output = Vec::new();
            run_unix_socket_relay(
                input,
                &mut output,
                &self.daemon_socket,
                BridgeLimits::default(),
            )
            .expect("fixed SSH command reaches the private daemon socket");
            session.data(channel, output)?;
            session.exit_status_request(channel, 0)?;
            session.eof(channel)?;
            session.close(channel)?;
            Ok(())
        }
    }

    fn fixed_command_stdin(input: &[u8]) -> &[u8] {
        assert_eq!(input.first(), Some(&1));
        let mut cursor = 1;
        let executable = take_u16_bytes(input, &mut cursor);
        assert_eq!(executable, b"choosh-host");
        assert_eq!(
            u16::from_be_bytes(input[cursor..cursor + 2].try_into().unwrap()),
            2
        );
        cursor += 2;
        assert_eq!(take_u16_bytes(input, &mut cursor), b"rpc");
        assert_eq!(take_u16_bytes(input, &mut cursor), b"--stdio");
        let size = u32::from_be_bytes(input[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4;
        assert_eq!(cursor + size, input.len());
        &input[cursor..]
    }

    fn take_u16_bytes<'a>(input: &'a [u8], cursor: &mut usize) -> &'a [u8] {
        let size = u16::from_be_bytes(input[*cursor..*cursor + 2].try_into().unwrap()) as usize;
        *cursor += 2;
        let value = &input[*cursor..*cursor + size];
        *cursor += size;
        value
    }

    fn daemon_socket_fixture() -> (PathBuf, thread::JoinHandle<()>) {
        let path = std::env::temp_dir().join(format!(
            "choosh-android-rpc-{}-{}.sock",
            std::process::id(),
            NEXT_SOCKET_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let listener = UnixListener::bind(&path).expect("unique private daemon fixture socket");
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("host relay connects");
            let hello = read_single_frame(&mut stream);
            let hello = decode_hello(&hello, 1024).expect("host relay sends typed hello");
            let reply = ServerNegotiator::new(
                ProtocolVersion::new(1, 0),
                PeerIdentity::new("chooshd", "test").unwrap(),
                PeerIdentity::new("fixture-host", "test").unwrap(),
                [],
                ProtocolLimits::new(1024, 4).unwrap(),
            )
            .unwrap()
            .receive_hello(&hello)
            .expect("host relay hello negotiates");
            let reply = encode_server_reply(&reply, 1024).unwrap();
            stream
                .write_all(&encode_frame(&reply, 1024).unwrap())
                .unwrap();

            let request = read_single_frame(&mut stream);
            assert_eq!(
                request,
                br#"{"id":"00000000-0000-0000-0000-000000000052","kind":"request","method":"git.status","params":{"workspace_id":"00000000-0000-0000-0000-000000000051"}}"#
            );
            let response = encode_response(
                &Response {
                    id: EnvelopeId::new("00000000-0000-0000-0000-000000000052").unwrap(),
                    terminal: Terminal::Result(serde_json::json!({
                        "workspace_id": "00000000-0000-0000-0000-000000000051",
                        "entries": [{"staged": "unmodified", "unstaged": "modified", "new_path_b64": "c3JjL_8ucnM"}]
                    })),
                },
                1024,
            )
            .unwrap();
            stream
                .write_all(&encode_frame(&response, 1024).unwrap())
                .unwrap();
        });
        (path, worker)
    }

    #[derive(Clone)]
    struct FixedStatus(StatusSnapshot);

    impl GitStatusOperation for FixedStatus {
        fn status_snapshot(&self) -> Result<StatusSnapshot, GitStatusError> {
            Ok(self.0.clone())
        }
    }

    fn real_daemon_socket_fixture() -> (PathBuf, PathBuf, thread::JoinHandle<()>) {
        let root = std::env::temp_dir().join(format!(
            "choosh-android-real-daemon-{}-{}",
            std::process::id(),
            NEXT_SOCKET_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let state = root.join("state");
        let socket = state.join("daemon.sock");
        std::fs::create_dir(&root).expect("unique daemon fixture root");
        let plan = SocketPlan::new(&state, &socket).expect("valid private daemon plan");
        let owned = bind(&plan).expect("private daemon socket binds");
        let snapshot = parse_status(
            b" M src/\xff.rs\0",
            StatusLimits {
                max_bytes: 64,
                max_entries: 2,
                max_path_bytes: 32,
            },
        )
        .expect("fixed status parses");
        let mut handler = DaemonRpc::new();
        handler
            .register_git_status(
                EnvelopeId::new("00000000-0000-0000-0000-000000000051").unwrap(),
                Arc::new(FixedStatus(snapshot)),
            )
            .expect("registered workspace is unique");
        let config = HandshakeConfig {
            protocol: ProtocolVersion::new(1, 0),
            daemon: PeerIdentity::new("chooshd", "test").unwrap(),
            host: PeerIdentity::new("fixture-host", "test").unwrap(),
            capabilities: Vec::new(),
            limits: ProtocolLimits::new(1024, 4).unwrap(),
        };
        let worker = thread::spawn(move || {
            serve_once_with_handler(owned.listener(), &config, 1024, &handler)
                .expect("real daemon serves one SSH-relayed RPC");
        });
        (root, socket, worker)
    }

    fn read_single_frame(stream: &mut UnixStream) -> Vec<u8> {
        let mut decoder = FrameDecoder::new(FrameLimits::new(1024, 2).unwrap());
        let mut buffer = [0; 128];
        loop {
            let read = stream
                .read(&mut buffer)
                .expect("fixture socket is readable");
            assert_ne!(read, 0, "fixture socket must not end before its frame");
            let frames = decoder
                .feed(&buffer[..read])
                .expect("fixture frame is bounded");
            if let Some(frame) = frames.into_iter().next() {
                return frame;
            }
        }
    }

    static NEXT_SOCKET_ID: AtomicUsize = AtomicUsize::new(1);

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
        let runtime = FixtureRuntime {
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

        let connection = connect_verified_bounded(
            runtime,
            AndroidConnectionPlan::new(1, 2, 3, 4, 5).unwrap(),
            StreamChunkLimits::new(4 * 1024, 4 * 1024).expect("non-zero JNI callback bounds"),
        )
        .await
        .expect("the exact generated host key and credential authenticate");

        assert!(calls.load(Ordering::SeqCst) > 0);
        drop(connection);
        server.abort();
    }

    #[tokio::test]
    async fn android_git_status_payload_crosses_the_admitted_fixed_ssh_rpc_capability() {
        let host = generated_key();
        let credential = generated_key();
        let expected_host =
            PublicKeyFingerprint::parse(presented_fingerprint(host.public_key())).unwrap();
        let expected_credential =
            PublicKeyFingerprint::parse(presented_fingerprint(credential.public_key())).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (daemon_socket, daemon) = daemon_socket_fixture();
        let (client, server_stream) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(server::run_stream(
            Arc::new(server::Config {
                keys: vec![host],
                ..server::Config::default()
            }),
            server_stream,
            GitStatusServer {
                expected_credential,
                command_accepted: false,
                input: Vec::new(),
                daemon_socket: daemon_socket.clone(),
            },
        ));
        let mut runtime = FixtureRuntime {
            session: Some(choosh_ssh::PreAuthenticationSession::new(
                expected_host,
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
        .expect("generated Android fixture authenticates before RPC");
        let session = AndroidRpcSession::new(connection);
        let response = session
            .execute(
                br#"{"id":"00000000-0000-0000-0000-000000000052","kind":"request","method":"git.status","params":{"workspace_id":"00000000-0000-0000-0000-000000000051"}}"#,
            )
            .await
            .expect("fixed SSH RPC returns the terminal git status response");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            response,
            br#"{"id":"00000000-0000-0000-0000-000000000052","kind":"response","result":{"entries":[{"new_path_b64":"c3JjL_8ucnM","staged":"unmodified","unstaged":"modified"}],"workspace_id":"00000000-0000-0000-0000-000000000051"}}"#
        );
        drop(session);
        server.abort();
        daemon.join().expect("daemon fixture completes");
        std::fs::remove_file(daemon_socket).expect("daemon fixture socket is removed");
    }

    #[tokio::test]
    async fn android_git_status_crosses_authenticated_ssh_and_the_real_private_daemon() {
        let host = generated_key();
        let credential = generated_key();
        let expected_host =
            PublicKeyFingerprint::parse(presented_fingerprint(host.public_key())).unwrap();
        let expected_credential =
            PublicKeyFingerprint::parse(presented_fingerprint(credential.public_key())).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let (daemon_root, daemon_socket, daemon) = real_daemon_socket_fixture();
        let (client, server_stream) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(server::run_stream(
            Arc::new(server::Config {
                keys: vec![host],
                ..server::Config::default()
            }),
            server_stream,
            GitStatusServer {
                expected_credential,
                command_accepted: false,
                input: Vec::new(),
                daemon_socket,
            },
        ));
        let mut runtime = FixtureRuntime {
            session: Some(choosh_ssh::PreAuthenticationSession::new(
                expected_host,
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
        .expect("generated Android fixture authenticates before real daemon RPC");
        let response = AndroidRpcSession::new(connection)
            .execute(
                br#"{"id":"00000000-0000-0000-0000-000000000052","kind":"request","method":"git.status","params":{"workspace_id":"00000000-0000-0000-0000-000000000051"}}"#,
            )
            .await
            .expect("real private daemon returns registered git status");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            response,
            br#"{"id":"00000000-0000-0000-0000-000000000052","kind":"response","result":{"entries":[{"new_path_b64":"c3JjL_8ucnM","staged":"unmodified","unstaged":"modified"}],"workspace_id":"00000000-0000-0000-0000-000000000051"}}"#
        );
        server.abort();
        daemon.join().expect("real daemon fixture completes");
        std::fs::remove_dir_all(daemon_root).expect("real daemon fixture root is removed");
    }

    #[test]
    fn android_rpc_session_rejects_non_request_payloads_before_ssh() {
        let error = decode_android_rpc_request(
            br#"{"id":"00000000-0000-0000-0000-000000000052","kind":"response","result":{}}"#,
        )
        .unwrap_err();
        assert_eq!(error, AndroidRpcError::RequestKind);
    }
}
