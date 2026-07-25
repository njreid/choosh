//! Minimal stable C ABI composition root for Android/Rust lifecycle smoke tests.
//!
//! Every export uses fixed-width integers only. No pointer crosses the ABI, so
//! callers cannot violate Rust aliasing, lifetime, alignment, or ownership rules.

#![allow(unsafe_code)] // Required only for Edition 2024's `no_mangle` ABI attribute.

use choosh_android_transport::{AndroidRpcSession, BlockingAndroidIo, BlockingAndroidStream, StreamChunkLimits};
use choosh_core::ssh_identity::{PublicKeyFingerprint, PublicKeyMetadata, SshPublicKeyAlgorithm};
use choosh_ssh::SshUsername;
use jni::objects::{Global, JByteArray, JClass, JObject, JValue};
use jni::{Env, EnvUnowned, JavaVM, jni_sig, jni_str};
use std::ffi::c_void;
use std::future::Future;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::runtime::Runtime;

const ABI_VERSION: u32 = 3;
const STATUS_OK: i32 = 0;
const STATUS_STALE_GENERATION: i32 = 1;
const STATUS_UNKNOWN_REQUEST: i32 = 2;
const STATUS_CAPACITY: i32 = 3;
const STATUS_INVALID_ARGUMENT: i32 = 4;
const STATUS_TRANSPORT_UNAVAILABLE: i32 = 5;
const AUTHENTICATED_PLAN_STATUS: u32 = 8;
const SLOT_COUNT: usize = 64;
const MAX_RUNTIME_CALLBACK_BYTES: u64 = 65_536;
const MAX_RUNTIME_METADATA_BYTES: usize = 256;
const MAX_RUNTIME_PUBLIC_KEY_BYTES: usize = 8 * 1024;
const RUNTIME_METADATA_VERSION: u8 = 1;
// Request tokens are process-local opaque capabilities, not serialized IDs. Keeping the
// request kind in the atomically stored token prevents a generic bridge request from being
// reinterpreted as an authenticated plan during a slot reuse race.
const TOKEN_KIND_BITS: u32 = 4;
const TOKEN_ID_BITS: u32 = 30;
const TOKEN_GENERATION_BITS: u32 = 30;
const TOKEN_KIND_MASK: u32 = (1 << TOKEN_KIND_BITS) - 1;
const TOKEN_KIND_MASK_U64: u64 = TOKEN_KIND_MASK as u64;
const TOKEN_ID_MASK: u32 = (1 << TOKEN_ID_BITS) - 1;
const TOKEN_GENERATION_MASK: u32 = (1 << TOKEN_GENERATION_BITS) - 1;

static GENERATION: AtomicU32 = AtomicU32::new(1);
static NEXT_REQUEST: AtomicU32 = AtomicU32::new(1);
static REQUESTS: [AtomicU64; SLOT_COUNT] = [const { AtomicU64::new(0) }; SLOT_COUNT];
type RuntimeAllocationSlot = Option<(u64, RuntimeState)>;
type RuntimeAllocationTable = [RuntimeAllocationSlot; SLOT_COUNT];
/// The bridge's token table owns allocations; it is not an ambient callback lookup service.
static RUNTIME_ALLOCATIONS: OnceLock<Mutex<RuntimeAllocationTable>> = OnceLock::new();

/// JNI composition-root state owned by exactly one authenticated plan token.
///
/// The C ABI is necessarily process-addressable, but this table never exposes a lookup service:
/// callers may only advance, execute through, or cancel their exact opaque plan capability.
enum RuntimeState {
    Pending(RuntimeAllocation<JniRuntimeCallbacks>),
    Connected(NativeSessionCapability),
}

/// Cloneable fixed-RPC capability. Cloning it cannot select another plan or gain a raw stream.
#[derive(Clone)]
struct NativeSessionCapability {
    runtime: Arc<Runtime>,
    actor: SessionActor,
    allocation: Arc<RuntimeAllocation<JniRuntimeCallbacks>>,
}

impl NativeSessionCapability {
    fn execute(&self, payload: Vec<u8>) -> Result<Vec<u8>, ()> {
        self.runtime.block_on(self.actor.execute(payload))
    }

    fn close(&self) -> bool {
        self.actor.close();
        self.allocation.close().is_ok()
    }
}

macro_rules! opaque_handle {
    ($name:ident) => {
        /// A non-zero Android-owned registry handle. Its represented value never crosses Rust's
        /// Android ABI and it deliberately has no `Display` implementation.
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct $name(NonZeroU64);

        impl $name {
            /// Validates an Android registry handle without exposing its underlying value.
            pub const fn new(value: u64) -> Option<Self> {
                match NonZeroU64::new(value) {
                    Some(value) => Some(Self(value)),
                    None => None,
                }
            }
        }
    };
}

opaque_handle!(EndpointHandle);
opaque_handle!(UsernameHandle);
opaque_handle!(KnownHostHandle);
opaque_handle!(CredentialReferenceHandle);
opaque_handle!(PublicKeyHandle);
opaque_handle!(SigningCallbackHandle);
opaque_handle!(RuntimeLeaseHandle);

/// A validated, Android-owned connection description before host-key admission.
///
/// This is deliberately separate from the opaque C/JNI request token. It is the typed native
/// composition seam a future stream registry will resolve; it is not a live connection and does
/// not grant signing access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeAuthenticatedPlan {
    endpoint: EndpointHandle,
    username: UsernameHandle,
    known_host: KnownHostHandle,
    credential_reference: CredentialReferenceHandle,
    public_key: PublicKeyHandle,
    signing_callback: SigningCallbackHandle,
    runtime_lease: RuntimeLeaseHandle,
}

impl NativeAuthenticatedPlan {
    /// Creates a plan only when every Android registry reference is non-zero.
    #[must_use]
    pub fn new(
        endpoint: u64,
        username: u64,
        known_host: u64,
        credential_reference: u64,
        public_key: u64,
        signing_callback: u64,
        runtime_lease: u64,
    ) -> Option<Self> {
        Some(Self {
            endpoint: EndpointHandle::new(endpoint)?,
            username: UsernameHandle::new(username)?,
            known_host: KnownHostHandle::new(known_host)?,
            credential_reference: CredentialReferenceHandle::new(credential_reference)?,
            public_key: PublicKeyHandle::new(public_key)?,
            signing_callback: SigningCallbackHandle::new(signing_callback)?,
            runtime_lease: RuntimeLeaseHandle::new(runtime_lease)?,
        })
    }

    /// Performs the required exact host-key admission before the plan can request a signature.
    ///
    /// # Errors
    ///
    /// Returns the injected verifier's error, including a changed or untrusted presented key.
    pub fn admit_exact_host_key<V>(self, verifier: &mut V) -> Result<HostKeyAdmittedPlan, V::Error>
    where
        V: ExactHostKeyAdmission,
    {
        verifier.verify_exact_host_key(self.endpoint, self.known_host)?;
        Ok(HostKeyAdmittedPlan { plan: self })
    }
}

/// Capability implemented at the Android/native outer composition root.
///
/// The implementation owns the bounded stream and must compare its *presented* host key to the
/// exact persisted key resolved from `known_host`. Supplying an opaque handle is not admission;
/// only a successful return may mint [`HostKeyAdmittedPlan`].
pub trait ExactHostKeyAdmission {
    type Error;

    /// Verifies the stream's presented key against exactly this persisted known-host record.
    ///
    /// # Errors
    ///
    /// Returns an implementation-defined admission error when the stream cannot be verified.
    fn verify_exact_host_key(
        &mut self,
        endpoint: EndpointHandle,
        known_host: KnownHostHandle,
    ) -> Result<(), Self::Error>;
}

/// A plan whose stream has completed exact host-key admission.
///
/// This capability has no public constructor. It is the only type accepted by the Keystore
/// public-key-authentication boundary, so an unverified plan cannot request a signature by API
/// shape alone.
#[derive(Debug)]
pub struct HostKeyAdmittedPlan {
    plan: NativeAuthenticatedPlan,
}

impl HostKeyAdmittedPlan {
    /// Starts public-key authentication through the injected Keystore capability.
    ///
    /// The concrete adapter supplies the SSH signing payload to the Keystore and never exposes
    /// private-key bytes to Rust. A successful result is still not a usable session: a later
    /// transport slice must complete protocol authentication and channel admission.
    ///
    /// # Errors
    ///
    /// Returns the injected Keystore adapter's error when authentication cannot begin.
    pub fn begin_public_key_authentication<S>(self, signer: &mut S) -> Result<(), S::Error>
    where
        S: KeystorePublicKeyAuthentication,
    {
        signer.begin_public_key_authentication(
            self.plan.endpoint,
            self.plan.username,
            self.plan.credential_reference,
            self.plan.public_key,
            self.plan.signing_callback,
        )
    }
}

/// Keystore-backed public-key authentication boundary.
///
/// It is intentionally invoked only with typed opaque references. The eventual Russh adapter
/// will adapt this capability to its per-challenge signing callback.
pub trait KeystorePublicKeyAuthentication {
    type Error;

    /// Begins public-key authentication after exact host-key admission.
    ///
    /// # Errors
    ///
    /// Returns an implementation-defined error when the Keystore cannot begin authentication.
    fn begin_public_key_authentication(
        &mut self,
        endpoint: EndpointHandle,
        username: UsernameHandle,
        credential_reference: CredentialReferenceHandle,
        public_key: PublicKeyHandle,
        signing_callback: SigningCallbackHandle,
    ) -> Result<(), Self::Error>;
}

/// Bounded Android runtime callbacks retained by one native plan allocation.
///
/// Concrete JNI code is an outer adapter for this trait. Shared transport never
/// receives a JVM reference: it receives only the stream and signer built from
/// this allocation after exact-host admission.
pub trait RuntimeCallbacks {
    type Error;

    /// Returns the fixed, non-secret identity metadata for this lease.
    fn metadata(&self, lease: RuntimeLeaseHandle) -> Result<Vec<u8>, Self::Error>;

    /// Returns the canonical OpenSSH public key fixed to this signing lease.
    fn public_key(&self, lease: RuntimeLeaseHandle) -> Result<Vec<u8>, Self::Error>;

    /// # Errors
    ///
    /// Returns the callback's content-free transport failure.
    fn read(&self, lease: RuntimeLeaseHandle, output: &mut [u8]) -> Result<usize, Self::Error>;
    /// # Errors
    ///
    /// Returns the callback's content-free transport failure.
    fn write(&self, lease: RuntimeLeaseHandle, input: &[u8]) -> Result<(), Self::Error>;
    /// # Errors
    ///
    /// Returns the callback's content-free signing failure.
    fn sign(&self, lease: RuntimeLeaseHandle, payload: &[u8]) -> Result<Vec<u8>, Self::Error>;
    /// # Errors
    ///
    /// Returns the callback's content-free release failure.
    fn close(&self, lease: RuntimeLeaseHandle) -> Result<(), Self::Error>;
}

/// Plan-owned JNI adapter for the Android runtime callback object.
///
/// The global reference belongs to this value rather than a process-wide callback registry.
/// Calls attach the current worker thread only for their JNI scope and copy every byte array
/// before its local reference is released. The object must implement the narrow methods declared
/// by `AndroidRuntimeCallbackPort` on the Android side.
#[derive(Debug)]
pub struct JniRuntimeCallbacks {
    vm: JavaVM,
    callbacks: Global<JObject<'static>>,
}

impl JniRuntimeCallbacks {
    /// Retains exactly one callback object for the owning native plan.
    ///
    /// # Errors
    ///
    /// Returns a JNI failure when the VM or a global reference cannot be acquired.
    pub fn retain<'local>(
        environment: &mut Env<'local>,
        callbacks: JObject<'local>,
    ) -> jni::errors::Result<Self> {
        Ok(Self {
            vm: environment.get_java_vm()?,
            callbacks: environment.new_global_ref(callbacks)?,
        })
    }

    fn with_environment<T>(
        &self,
        callback: impl FnOnce(&mut Env<'_>) -> jni::errors::Result<T>,
    ) -> jni::errors::Result<T> {
        self.vm.attach_current_thread(callback)
    }
}

impl RuntimeCallbacks for JniRuntimeCallbacks {
    type Error = JniRuntimeCallbackError;

    fn metadata(&self, lease: RuntimeLeaseHandle) -> Result<Vec<u8>, Self::Error> {
        let bytes = self
            .with_environment(|environment| {
                let result = environment
                    .call_method(
                        &self.callbacks,
                        jni_str!("metadata"),
                        jni_sig!("(J)[B"),
                        &[JValue::Long(lease.0.get().cast_signed())],
                    )?
                    .l()?;
                let bytes = JByteArray::cast_local(environment, result)?;
                environment.convert_byte_array(&bytes)
            })
            .map_err(JniRuntimeCallbackError::Jni)?;
        if bytes.is_empty() || bytes.len() > MAX_RUNTIME_METADATA_BYTES {
            return Err(JniRuntimeCallbackError::OversizedResult);
        }
        Ok(bytes)
    }

    fn public_key(&self, lease: RuntimeLeaseHandle) -> Result<Vec<u8>, Self::Error> {
        let bytes = self
            .with_environment(|environment| {
                let result = environment
                    .call_method(
                        &self.callbacks,
                        jni_str!("publicKey"),
                        jni_sig!("(J)[B"),
                        &[JValue::Long(lease.0.get().cast_signed())],
                    )?
                    .l()?;
                let bytes = JByteArray::cast_local(environment, result)?;
                environment.convert_byte_array(&bytes)
            })
            .map_err(JniRuntimeCallbackError::Jni)?;
        if bytes.is_empty() || bytes.len() > MAX_RUNTIME_PUBLIC_KEY_BYTES {
            return Err(JniRuntimeCallbackError::OversizedResult);
        }
        Ok(bytes)
    }

    fn read(&self, lease: RuntimeLeaseHandle, output: &mut [u8]) -> Result<usize, Self::Error> {
        let requested =
            i32::try_from(output.len()).map_err(|_| JniRuntimeCallbackError::OversizedResult)?;
        let bytes = self
            .with_environment(|environment| {
                let result = environment
                    .call_method(
                        &self.callbacks,
                        jni_str!("read"),
                        jni_sig!("(JI)[B"),
                        &[
                            JValue::Long(lease.0.get().cast_signed()),
                            JValue::Int(requested),
                        ],
                    )?
                    .l()?;
                let bytes = JByteArray::cast_local(environment, result)?;
                environment.convert_byte_array(&bytes)
            })
            .map_err(JniRuntimeCallbackError::Jni)?;
        if bytes.len() > output.len() {
            return Err(JniRuntimeCallbackError::OversizedResult);
        }
        output[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }

    fn write(&self, lease: RuntimeLeaseHandle, input: &[u8]) -> Result<(), Self::Error> {
        self.with_environment(|environment| {
            let bytes = environment.byte_array_from_slice(input)?;
            environment.call_method(
                &self.callbacks,
                jni_str!("write"),
                jni_sig!("(J[B)V"),
                &[
                    JValue::Long(lease.0.get().cast_signed()),
                    JValue::Object(&bytes),
                ],
            )?;
            Ok(())
        })
        .map_err(JniRuntimeCallbackError::Jni)
    }

    fn sign(&self, lease: RuntimeLeaseHandle, payload: &[u8]) -> Result<Vec<u8>, Self::Error> {
        self.with_environment(|environment| {
            let bytes = environment.byte_array_from_slice(payload)?;
            let result = environment
                .call_method(
                    &self.callbacks,
                    jni_str!("sign"),
                    jni_sig!("(J[B)[B"),
                    &[
                        JValue::Long(lease.0.get().cast_signed()),
                        JValue::Object(&bytes),
                    ],
                )?
                .l()?;
            let signature = JByteArray::cast_local(environment, result)?;
            environment.convert_byte_array(&signature)
        })
        .map_err(JniRuntimeCallbackError::Jni)
    }

    fn close(&self, lease: RuntimeLeaseHandle) -> Result<(), Self::Error> {
        self.with_environment(|environment| {
            environment.call_method(
                &self.callbacks,
                jni_str!("close"),
                jni_sig!("(J)V"),
                &[JValue::Long(lease.0.get().cast_signed())],
            )?;
            Ok(())
        })
        .map_err(JniRuntimeCallbackError::Jni)
    }
}

/// Content-free JNI callback failure classification.
#[derive(Debug)]
pub enum JniRuntimeCallbackError {
    Jni(jni::errors::Error),
    OversizedResult,
}

/// One-close-only native owner for callbacks associated with an authenticated plan.
pub struct RuntimeAllocation<C> {
    callbacks: C,
    lease: RuntimeLeaseHandle,
    max_operation_bytes: NonZeroU64,
    closed: AtomicBool,
}

impl<C: RuntimeCallbacks> RuntimeAllocation<C> {
    pub fn new(callbacks: C, lease: RuntimeLeaseHandle, max_operation_bytes: u64) -> Option<Self> {
        Some(Self {
            callbacks,
            lease,
            max_operation_bytes: NonZeroU64::new(max_operation_bytes)?,
            closed: AtomicBool::new(false),
        })
    }

    /// Returns the validated fixed identity associated with this one lease.
    ///
    /// # Errors
    ///
    /// Returns a callback, malformed-capsule, or released-allocation failure.
    pub fn metadata(&self) -> Result<RuntimeConnectionMetadata, RuntimeAllocationError<C::Error>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(RuntimeAllocationError::Closed);
        }
        let bytes = self
            .callbacks
            .metadata(self.lease)
            .map_err(RuntimeAllocationError::Callback)?;
        RuntimeConnectionMetadata::parse(&bytes).map_err(RuntimeAllocationError::Metadata)
    }

    /// Resolves the public key only if it matches the fixed lease metadata.
    ///
    /// # Errors
    ///
    /// Returns a callback, invalid-key, identity-mismatch, or released-allocation failure.
    pub fn public_key(
        &self,
        metadata: &RuntimeConnectionMetadata,
    ) -> Result<russh::keys::PublicKey, RuntimeAllocationError<C::Error>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(RuntimeAllocationError::Closed);
        }
        let bytes = self
            .callbacks
            .public_key(self.lease)
            .map_err(RuntimeAllocationError::Callback)?;
        if bytes.is_empty() || bytes.len() > MAX_RUNTIME_PUBLIC_KEY_BYTES {
            return Err(RuntimeAllocationError::PublicKey(
                RuntimePublicKeyError::InvalidEncoding,
            ));
        }
        let encoded = std::str::from_utf8(&bytes).map_err(|_| {
            RuntimeAllocationError::PublicKey(RuntimePublicKeyError::InvalidEncoding)
        })?;
        let key = encoded.parse().map_err(|_| {
            RuntimeAllocationError::PublicKey(RuntimePublicKeyError::InvalidEncoding)
        })?;
        if choosh_ssh::presented_fingerprint(&key) != metadata.public_key().fingerprint().as_str() {
            return Err(RuntimeAllocationError::PublicKey(
                RuntimePublicKeyError::FingerprintMismatch,
            ));
        }
        Ok(key)
    }

    /// # Errors
    ///
    /// Returns a bounds, closed-allocation, or callback failure.
    pub fn read(&self, output: &mut [u8]) -> Result<usize, RuntimeAllocationError<C::Error>> {
        self.validate(output.len())?;
        self.callbacks
            .read(self.lease, output)
            .map_err(RuntimeAllocationError::Callback)
    }

    /// # Errors
    ///
    /// Returns a bounds, closed-allocation, or callback failure.
    pub fn write(&self, input: &[u8]) -> Result<(), RuntimeAllocationError<C::Error>> {
        self.validate(input.len())?;
        self.callbacks
            .write(self.lease, input)
            .map_err(RuntimeAllocationError::Callback)
    }

    /// # Errors
    ///
    /// Returns a bounds, closed-allocation, or callback failure.
    pub fn sign(&self, payload: &[u8]) -> Result<Vec<u8>, RuntimeAllocationError<C::Error>> {
        self.validate(payload.len())?;
        let signature = self
            .callbacks
            .sign(self.lease, payload)
            .map_err(RuntimeAllocationError::Callback)?;
        self.validate(signature.len())?;
        Ok(signature)
    }

    /// # Errors
    ///
    /// Returns the callback's content-free release failure.
    pub fn close(&self) -> Result<(), RuntimeAllocationError<C::Error>> {
        if self
            .closed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        self.callbacks
            .close(self.lease)
            .map_err(RuntimeAllocationError::Callback)
    }

    fn validate(&self, length: usize) -> Result<(), RuntimeAllocationError<C::Error>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(RuntimeAllocationError::Closed);
        }
        if length == 0 || (length as u64) > self.max_operation_bytes.get() {
            return Err(RuntimeAllocationError::Bounds);
        }
        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum RuntimeAllocationError<E> {
    Bounds,
    Closed,
    Metadata(RuntimeMetadataError),
    PublicKey(RuntimePublicKeyError),
    Callback(E),
}

/// Validated non-secret identity fixed to one Android runtime lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeConnectionMetadata {
    username: SshUsername,
    expected_host: PublicKeyFingerprint,
    public_key: PublicKeyMetadata,
}

impl RuntimeConnectionMetadata {
    /// Parses the versioned Android runtime metadata capsule.
    ///
    /// # Errors
    ///
    /// Returns only a stable classification for malformed or unsupported metadata.
    pub fn parse(bytes: &[u8]) -> Result<Self, RuntimeMetadataError> {
        if bytes.len() > MAX_RUNTIME_METADATA_BYTES
            || bytes.first() != Some(&RUNTIME_METADATA_VERSION)
        {
            return Err(RuntimeMetadataError::InvalidEncoding);
        }
        let mut cursor = 1;
        let username = read_metadata_field(bytes, &mut cursor)?;
        let expected_host = read_metadata_field(bytes, &mut cursor)?;
        let algorithm = read_metadata_field(bytes, &mut cursor)?;
        let fingerprint = read_metadata_field(bytes, &mut cursor)?;
        if cursor != bytes.len() {
            return Err(RuntimeMetadataError::InvalidEncoding);
        }
        let username =
            std::str::from_utf8(username).map_err(|_| RuntimeMetadataError::InvalidEncoding)?;
        let expected_host = std::str::from_utf8(expected_host)
            .map_err(|_| RuntimeMetadataError::InvalidEncoding)?;
        let algorithm = match std::str::from_utf8(algorithm)
            .map_err(|_| RuntimeMetadataError::InvalidEncoding)?
        {
            "ED25519" => SshPublicKeyAlgorithm::Ed25519,
            "ECDSA" => SshPublicKeyAlgorithm::Ecdsa,
            "RSA" => SshPublicKeyAlgorithm::Rsa,
            _ => return Err(RuntimeMetadataError::InvalidEncoding),
        };
        let fingerprint =
            std::str::from_utf8(fingerprint).map_err(|_| RuntimeMetadataError::InvalidEncoding)?;
        Ok(Self {
            username: SshUsername::parse(username)
                .map_err(|_| RuntimeMetadataError::InvalidEncoding)?,
            expected_host: PublicKeyFingerprint::parse(expected_host)
                .map_err(|_| RuntimeMetadataError::InvalidEncoding)?,
            public_key: PublicKeyMetadata::new(
                algorithm,
                PublicKeyFingerprint::parse(fingerprint)
                    .map_err(|_| RuntimeMetadataError::InvalidEncoding)?,
            ),
        })
    }

    #[must_use]
    pub const fn username(&self) -> &SshUsername {
        &self.username
    }

    #[must_use]
    pub const fn expected_host(&self) -> &PublicKeyFingerprint {
        &self.expected_host
    }

    #[must_use]
    pub const fn public_key(&self) -> &PublicKeyMetadata {
        &self.public_key
    }
}

fn read_metadata_field<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
) -> Result<&'a [u8], RuntimeMetadataError> {
    let Some(&length) = bytes.get(*cursor) else {
        return Err(RuntimeMetadataError::InvalidEncoding);
    };
    *cursor += 1;
    let end = cursor
        .checked_add(usize::from(length))
        .ok_or(RuntimeMetadataError::InvalidEncoding)?;
    let field = bytes
        .get(*cursor..end)
        .ok_or(RuntimeMetadataError::InvalidEncoding)?;
    if field.is_empty() {
        return Err(RuntimeMetadataError::InvalidEncoding);
    }
    *cursor = end;
    Ok(field)
}

/// Stable classification for runtime metadata rejected at the JNI edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeMetadataError {
    InvalidEncoding,
}

/// Stable classification for an untrusted public key returned by a runtime lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimePublicKeyError {
    InvalidEncoding,
    FingerprintMismatch,
}

/// Content-free signer failure from a bounded Android runtime lease.
#[derive(Debug)]
pub enum RuntimeLeaseSignerError {
    Callback,
    Send(russh::SendError),
}

impl From<russh::SendError> for RuntimeLeaseSignerError {
    fn from(value: russh::SendError) -> Self {
        Self::Send(value)
    }
}

/// Russh signer whose only secret-bearing operation is the bound lease callback.
pub struct RuntimeLeaseSigner<C> {
    allocation: Arc<RuntimeAllocation<C>>,
    public_key: russh::keys::PublicKey,
}

impl<C> RuntimeLeaseSigner<C> {
    /// Creates a signer after the lease's canonical public key has been validated.
    #[must_use]
    pub fn new(allocation: Arc<RuntimeAllocation<C>>, public_key: russh::keys::PublicKey) -> Self {
        Self {
            allocation,
            public_key,
        }
    }
}

impl<C> choosh_ssh::CredentialSigner for RuntimeLeaseSigner<C>
where
    C: RuntimeCallbacks + Send + Sync + 'static,
{
    type Error = RuntimeLeaseSignerError;

    fn public_key(&self) -> russh::keys::PublicKey {
        self.public_key.clone()
    }

    async fn sign(
        &mut self,
        _identity: &russh::keys::agent::AgentIdentity,
        _hash_alg: Option<russh::keys::HashAlg>,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, Self::Error> {
        self.allocation
            .sign(&payload)
            .map_err(|_| RuntimeLeaseSignerError::Callback)
    }
}

impl<C> BlockingAndroidIo for RuntimeAllocation<C>
where
    C: RuntimeCallbacks + Send + Sync + 'static,
{
    fn read(&self, output: &mut [u8]) -> Result<usize, ()> {
        self.read(output).map_err(|_| ())
    }

    fn write(&self, input: &[u8]) -> Result<(), ()> {
        self.write(input).map_err(|_| ())
    }
}

/// Validated Android lease parts ready for exact-host SSH admission.
pub struct RuntimeLeaseTransport<C> {
    session: choosh_ssh::PreAuthenticationSession,
    username: SshUsername,
    stream: BlockingAndroidStream<RuntimeAllocation<C>>,
    signer: RuntimeLeaseSigner<C>,
}

impl<C> RuntimeLeaseTransport<C>
where
    C: RuntimeCallbacks + Send + Sync + 'static,
{
    /// Resolves one lease into typed stream, host session, username, and signer capabilities.
    ///
    /// # Errors
    ///
    /// Returns only stable validation failures before any SSH network operation or signature.
    pub fn new(
        allocation: Arc<RuntimeAllocation<C>>,
        limits: StreamChunkLimits,
    ) -> Result<Self, RuntimeLeaseCompositionError> {
        let metadata = allocation
            .metadata()
            .map_err(|_| RuntimeLeaseCompositionError::Metadata)?;
        let public_key = allocation
            .public_key(&metadata)
            .map_err(|_| RuntimeLeaseCompositionError::PublicKey)?;
        Ok(Self {
            session: choosh_ssh::PreAuthenticationSession::new(
                metadata.expected_host().clone(),
                choosh_ssh::SessionLimits::admission_default(),
            ),
            username: metadata.username().clone(),
            stream: BlockingAndroidStream::new(Arc::clone(&allocation), limits),
            signer: RuntimeLeaseSigner::new(allocation, public_key),
        })
    }

    /// Connects through exact-host admission before the signer can be invoked.
    ///
    /// # Errors
    ///
    /// Returns only the verified SSH connection's typed failure.
    pub async fn connect(
        self,
    ) -> Result<
        choosh_ssh::VerifiedConnection,
        choosh_ssh::VerifiedConnectionError<RuntimeLeaseSignerError>,
    > {
        choosh_ssh::VerifiedConnection::connect_stream(
            self.session,
            self.stream,
            self.username,
            choosh_ssh::CredentialSignerAdapter::new(self.signer),
        )
        .await
    }
}

/// Stable lease-to-transport composition failures before SSH begins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeLeaseCompositionError {
    Metadata,
    PublicKey,
}

/// Explicit bounded owner for live sessions transferred from native plans.
///
/// This is deliberately an ordinary constructor-injected value rather than a
/// global lookup service. The JNI outer root will own one instance and clear it
/// before it invalidates the associated Android lease generation.
pub struct SessionRegistry<S> {
    slots: [Option<(u64, S)>; SLOT_COUNT],
}

impl<S> SessionRegistry<S> {
    /// Creates an empty bounded session owner.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [const { None }; SLOT_COUNT],
        }
    }

    /// Inserts one plan-owned session without replacing another plan's session.
    pub fn insert(&mut self, plan: u64, session: S) -> Result<(), SessionRegistryError> {
        if plan == 0
            || self
                .slots
                .iter()
                .any(|slot| slot.as_ref().is_some_and(|(owned, _)| *owned == plan))
        {
            return Err(SessionRegistryError::InvalidPlan);
        }
        let Some(slot) = self.slots.iter_mut().find(|slot| slot.is_none()) else {
            return Err(SessionRegistryError::Capacity);
        };
        *slot = Some((plan, session));
        Ok(())
    }

    /// Removes the sole session owned by a plan.
    pub fn remove(&mut self, plan: u64) -> Option<S> {
        self.slots.iter_mut().find_map(|slot| {
            (slot.as_ref().is_some_and(|(owned, _)| *owned == plan))
                .then(|| slot.take())
                .flatten()
                .map(|(_, session)| session)
        })
    }

    /// Executes one fixed capability operation against the session owned by a plan.
    ///
    /// The closure receives no token table or registry access, so it cannot select another
    /// plan's session or insert a replacement while an operation is in flight.
    pub fn with_session<T>(
        &mut self,
        plan: u64,
        operation: impl FnOnce(&mut S) -> T,
    ) -> Result<T, SessionRegistryError> {
        let Some(Some((_, session))) = self
            .slots
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|(owned, _)| *owned == plan))
        else {
            return Err(SessionRegistryError::InvalidPlan);
        };
        Ok(operation(session))
    }

    /// Clones one plan-owned operation capability without retaining a registry borrow.
    pub fn capability(&self, plan: u64) -> Result<S, SessionRegistryError>
    where
        S: Clone,
    {
        self.slots
            .iter()
            .find_map(|slot| {
                slot.as_ref()
                    .filter(|(owned, _)| *owned == plan)
                    .map(|(_, session)| session.clone())
            })
            .ok_or(SessionRegistryError::InvalidPlan)
    }

    /// Drops every session before its owning generation's leases are released.
    pub fn clear(&mut self) {
        self.slots = [const { None }; SLOT_COUNT];
    }
}

impl<S> Default for SessionRegistry<S> {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable session-owner failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionRegistryError {
    InvalidPlan,
    Capacity,
}

/// Fixed-RPC capability owned by one session actor.
pub trait FixedRpcExecutor: Send + 'static {
    fn execute(
        &mut self,
        payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ()>> + Send + '_>>;
}

/// Bounded command sender for one plan-owned fixed-RPC session actor.
#[derive(Clone)]
pub struct SessionActor {
    sender: tokio::sync::mpsc::Sender<SessionCommand>,
}

enum SessionCommand {
    Execute {
        payload: Vec<u8>,
        reply: tokio::sync::oneshot::Sender<Result<Vec<u8>, ()>>,
    },
    Close,
}

impl SessionActor {
    pub fn spawn<S: FixedRpcExecutor>(mut session: S) -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                match command {
                    SessionCommand::Execute { payload, reply } => {
                        let _ = reply.send(session.execute(payload).await);
                    }
                    SessionCommand::Close => break,
                }
            }
        });
        Self { sender }
    }

    pub async fn execute(&self, payload: Vec<u8>) -> Result<Vec<u8>, ()> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.sender
            .try_send(SessionCommand::Execute { payload, reply })
            .map_err(|_| ())?;
        response.await.map_err(|_| ())?
    }

    pub fn close(&self) {
        let _ = self.sender.try_send(SessionCommand::Close);
    }
}

impl SessionRegistry<SessionActor> {
    /// Executes fixed RPC after cloning the owner capability and dropping the registry borrow.
    pub async fn execute_rpc(&self, plan: u64, payload: Vec<u8>) -> Result<Vec<u8>, ()> {
        self.capability(plan)
            .map_err(|_| ())?
            .execute(payload)
            .await
    }
}

impl FixedRpcExecutor for AndroidRpcSession {
    fn execute(
        &mut self,
        payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ()>> + Send + '_>> {
        Box::pin(async move { AndroidRpcSession::execute(self, &payload).await.map_err(|_| ()) })
    }
}

fn runtime_allocations() -> &'static Mutex<RuntimeAllocationTable> {
    RUNTIME_ALLOCATIONS.get_or_init(|| Mutex::new([const { None }; SLOT_COUNT]))
}

fn retain_runtime_allocation(
    plan: u64,
    allocation: RuntimeAllocation<JniRuntimeCallbacks>,
) -> bool {
    let Ok(mut allocations) = runtime_allocations().lock() else {
        return false;
    };
    for slot in allocations.iter_mut() {
        if slot.is_none() {
            *slot = Some((plan, RuntimeState::Pending(allocation)));
            return true;
        }
    }
    false
}

/// Releases the plan-owned global reference and asks Android to invalidate its lease.
///
/// The allocation is removed even if the Java close callback fails, so a failed close cannot
/// retain a callback object or make a later token reuse invoke it.
fn release_runtime_allocation(plan: u64) -> bool {
    let allocation = {
        let Ok(mut allocations) = runtime_allocations().lock() else {
            return false;
        };
        let Some(slot) = allocations
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|(owned, _)| *owned == plan))
        else {
            return true;
        };
        slot.take().map(|(_, allocation)| allocation)
    };
    allocation.is_none_or(|allocation| match allocation {
        RuntimeState::Pending(allocation) => allocation.close().is_ok(),
        RuntimeState::Connected(session) => session.close(),
    })
}

fn release_all_runtime_allocations() -> bool {
    let allocations = {
        let Ok(mut slots) = runtime_allocations().lock() else {
            return false;
        };
        std::mem::replace(&mut *slots, [const { None }; SLOT_COUNT])
    };
    allocations
        .into_iter()
        .flatten()
        .all(|(_, allocation)| match allocation {
            RuntimeState::Pending(allocation) => allocation.close().is_ok(),
            RuntimeState::Connected(session) => session.close(),
        })
}

/// Takes a pending allocation so an SSH handshake never holds the registry mutex.
fn take_pending_runtime_allocation(plan: u64) -> Option<RuntimeAllocation<JniRuntimeCallbacks>> {
    let Ok(mut allocations) = runtime_allocations().lock() else {
        return None;
    };
    let slot = allocations
        .iter_mut()
        .find(|slot| slot.as_ref().is_some_and(|(owned, _)| *owned == plan))?;
    match slot.take() {
        Some((_, RuntimeState::Pending(allocation))) => Some(allocation),
        Some(entry) => {
            *slot = Some(entry);
            None
        }
        None => None,
    }
}

fn retain_connected_session(plan: u64, session: NativeSessionCapability) -> bool {
    let Ok(mut allocations) = runtime_allocations().lock() else {
        return false;
    };
    let Some(slot) = allocations.iter_mut().find(|slot| slot.is_none()) else {
        return false;
    };
    *slot = Some((plan, RuntimeState::Connected(session)));
    true
}

fn session_capability(plan: u64) -> Option<NativeSessionCapability> {
    let Ok(allocations) = runtime_allocations().lock() else {
        return None;
    };
    allocations.iter().find_map(|slot| match slot {
        Some((owned, RuntimeState::Connected(session))) if *owned == plan => Some(session.clone()),
        _ => None,
    })
}

/// Returns the stable ABI contract version.
#[unsafe(no_mangle)]
pub extern "C" fn choosh_bridge_abi_version() -> u32 {
    ABI_VERSION
}

/// Returns the process-local bridge generation.
#[unsafe(no_mangle)]
pub extern "C" fn choosh_bridge_generation() -> u32 {
    GENERATION.load(Ordering::Acquire)
}

/// Begins a bounded request, returning zero and a typed status on failure.
#[unsafe(no_mangle)]
pub extern "C" fn choosh_bridge_request_begin(generation: u32, status: u32) -> u64 {
    if generation == 0
        || generation > TOKEN_GENERATION_MASK
        || status == 0
        || status > TOKEN_KIND_MASK
        || generation != GENERATION.load(Ordering::Acquire)
    {
        return 0;
    }
    let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
    if id == 0 || id > TOKEN_ID_MASK {
        return 0;
    }
    let key = encode(generation, id, status);
    for slot in &REQUESTS {
        if slot
            .compare_exchange(0, key, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return key;
        }
    }
    0
}

/// Begins an opaque Android-to-Rust authenticated-connection plan.
///
/// Every input is a non-zero opaque handle owned by Android. `runtime_lease` is the
/// per-attempt callback registration which owns the socket and signer callback. In particular,
/// this ABI accepts neither a private key nor any pointer to credential,
/// hostname, user name, or fingerprint bytes. A plan is not a verified SSH
/// session and cannot perform an operation; a later bridge slice must consume
/// it only after exact host-key admission and public-key authentication.
#[unsafe(no_mangle)]
pub extern "C" fn choosh_bridge_authenticated_plan_begin(
    generation: u32,
    endpoint: u64,
    username: u64,
    known_host: u64,
    credential_ref: u64,
    public_key: u64,
    signing_callback: u64,
    runtime_lease: u64,
) -> u64 {
    if NativeAuthenticatedPlan::new(
        endpoint,
        username,
        known_host,
        credential_ref,
        public_key,
        signing_callback,
        runtime_lease,
    )
    .is_none()
    {
        return 0;
    }
    choosh_bridge_request_begin(generation, AUTHENTICATED_PLAN_STATUS)
}

/// Cancels one opaque authenticated-connection plan.
#[unsafe(no_mangle)]
pub extern "C" fn choosh_bridge_authenticated_plan_cancel(generation: u32, plan: u64) -> i32 {
    let status = choosh_bridge_request_cancel(generation, plan);
    if status == STATUS_OK && !release_runtime_allocation(plan) {
        return STATUS_TRANSPORT_UNAVAILABLE;
    }
    status
}

/// Advances a plan-owned Android lease through verified SSH and into one fixed-RPC actor.
///
/// The callback stream presents the SSH host key to `RuntimeLeaseTransport`, which admits it
/// against the exact persisted fingerprint before the Keystore signing callback is reachable.
/// A successful return means the opaque plan now owns exactly one live actor; Java transfers the
/// same token to `SessionLease` immediately afterwards. Every other outcome remains fail-closed.
#[unsafe(no_mangle)]
pub extern "C" fn choosh_bridge_authenticated_plan_open(generation: u32, plan: u64) -> i32 {
    if generation == 0 || generation != GENERATION.load(Ordering::Acquire) {
        return STATUS_STALE_GENERATION;
    }
    if plan == 0
        || generation_of(plan) != generation
        || kind_of(plan) != u64::from(AUTHENTICATED_PLAN_STATUS)
    {
        return STATUS_INVALID_ARGUMENT;
    }
    if !REQUESTS.iter().any(|slot| slot.load(Ordering::Acquire) == plan) {
        return STATUS_UNKNOWN_REQUEST;
    }
    let Some(allocation) = take_pending_runtime_allocation(plan) else {
        return STATUS_TRANSPORT_UNAVAILABLE;
    };
    let allocation = Arc::new(allocation);
    let runtime = match Runtime::new() {
        Ok(runtime) => Arc::new(runtime),
        Err(_) => {
            let _ = allocation.close();
            return STATUS_TRANSPORT_UNAVAILABLE;
        }
    };
    let transport = match RuntimeLeaseTransport::new(
        Arc::clone(&allocation),
        StreamChunkLimits::new(16 * 1024, 16 * 1024).expect("constant chunk limits are valid"),
    ) {
        Ok(transport) => transport,
        Err(_) => {
            let _ = allocation.close();
            return STATUS_TRANSPORT_UNAVAILABLE;
        }
    };
    let connection = match runtime.block_on(transport.connect()) {
        Ok(connection) => connection,
        Err(_) => {
            let _ = allocation.close();
            return STATUS_TRANSPORT_UNAVAILABLE;
        }
    };
    let actor = runtime.block_on(async { SessionActor::spawn(AndroidRpcSession::new(connection)) });
    let capability = NativeSessionCapability {
        runtime,
        actor,
        allocation,
    };
    if !REQUESTS.iter().any(|slot| slot.load(Ordering::Acquire) == plan)
        || !retain_connected_session(plan, capability.clone())
    {
        let _ = capability.close();
        return STATUS_TRANSPORT_UNAVAILABLE;
    }
    STATUS_OK
}

// JNI wrappers intentionally accept only primitive values. The Java side
// resolves typed profile metadata to opaque handles before this boundary, so
// Rust never dereferences a JVM object or receives credential material.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_choosh_RustNativeConnectorJni_00024JniPlanBridge_nativeAbiVersion(
    _environment: *mut c_void,
    _class: *mut c_void,
) -> i32 {
    choosh_bridge_abi_version().cast_signed()
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_ai_choosh_RustNativeConnectorJni_00024JniPlanBridge_nativeBeginAuthenticatedPlan(
    _environment: *mut c_void,
    _class: *mut c_void,
    generation: i32,
    endpoint: i64,
    username: i64,
    known_host: i64,
    credential_ref: i64,
    public_key: i64,
    signing_callback: i64,
    runtime_lease: i64,
) -> i64 {
    if generation <= 0
        || endpoint <= 0
        || username <= 0
        || known_host <= 0
        || credential_ref <= 0
        || public_key <= 0
        || signing_callback <= 0
        || runtime_lease <= 0
    {
        return 0;
    }
    choosh_bridge_authenticated_plan_begin(
        generation.cast_unsigned(),
        endpoint.cast_unsigned(),
        username.cast_unsigned(),
        known_host.cast_unsigned(),
        credential_ref.cast_unsigned(),
        public_key.cast_unsigned(),
        signing_callback.cast_unsigned(),
        runtime_lease.cast_unsigned(),
    )
    .cast_signed()
}

/// Begins a plan while retaining its callback object in the token-owned allocation.
///
/// The Java object remains confined to this bridge. Failure to retain it cancels the just-minted
/// token and returns zero, preserving the fail-closed Java plan contract.
#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_ai_choosh_RustNativeConnectorJni_00024JniPlanBridge_nativeBeginAuthenticatedPlanWithRuntime<
    'local,
>(
    mut unowned_environment: EnvUnowned<'local>,
    _class: JClass<'local>,
    generation: i32,
    endpoint: i64,
    username: i64,
    known_host: i64,
    credential_ref: i64,
    public_key: i64,
    signing_callback: i64,
    runtime_lease: i64,
    callbacks: JObject<'local>,
) -> i64 {
    if generation <= 0
        || endpoint <= 0
        || username <= 0
        || known_host <= 0
        || credential_ref <= 0
        || public_key <= 0
        || signing_callback <= 0
        || runtime_lease <= 0
        || callbacks.is_null()
    {
        return 0;
    }
    match unowned_environment
        .with_env(|environment| -> jni::errors::Result<i64> {
            let runtime = JniRuntimeCallbacks::retain(environment, callbacks)?;
            let Some(allocation) = RuntimeAllocation::new(
                runtime,
                RuntimeLeaseHandle::new(runtime_lease.cast_unsigned()).ok_or(
                    jni::errors::Error::JniCall(jni::errors::JniError::InvalidArguments),
                )?,
                MAX_RUNTIME_CALLBACK_BYTES,
            ) else {
                return Ok(0);
            };
            let plan = choosh_bridge_authenticated_plan_begin(
                generation.cast_unsigned(),
                endpoint.cast_unsigned(),
                username.cast_unsigned(),
                known_host.cast_unsigned(),
                credential_ref.cast_unsigned(),
                public_key.cast_unsigned(),
                signing_callback.cast_unsigned(),
                runtime_lease.cast_unsigned(),
            );
            if plan == 0 {
                return Ok(0);
            }
            if !retain_runtime_allocation(plan, allocation) {
                let _ = choosh_bridge_request_cancel(generation.cast_unsigned(), plan);
                return Ok(0);
            }
            Ok(plan.cast_signed())
        })
        .into_outcome()
    {
        jni::Outcome::Ok(plan) => plan,
        jni::Outcome::Err(_) | jni::Outcome::Panic(_) => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_choosh_RustNativeConnectorJni_00024JniPlanBridge_nativeCancelAuthenticatedPlan(
    _environment: *mut c_void,
    _class: *mut c_void,
    generation: i32,
    plan: i64,
) -> i32 {
    if generation <= 0 || plan <= 0 {
        return STATUS_INVALID_ARGUMENT;
    }
    choosh_bridge_authenticated_plan_cancel(generation.cast_unsigned(), plan.cast_unsigned())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_choosh_RustNativeConnectorJni_00024JniPlanBridge_nativeOpenAuthenticatedPlan(
    _environment: *mut c_void,
    _class: *mut c_void,
    generation: i32,
    plan: i64,
) -> i32 {
    if generation <= 0 || plan <= 0 {
        return STATUS_INVALID_ARGUMENT;
    }
    choosh_bridge_authenticated_plan_open(generation.cast_unsigned(), plan.cast_unsigned())
}

/// Executes one bounded RPC through the actor owned by exactly this connected plan.
///
/// A null return is the Java boundary's content-free failure signal; Java maps it to its typed
/// bridge exception and never exposes a partial native response.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_choosh_RustNativeConnectorJni_00024JniPlanBridge_nativeExecuteAuthenticatedSession<'local>(
    mut unowned_environment: EnvUnowned<'local>,
    _class: JClass<'local>,
    generation: i32,
    plan: i64,
    request: JByteArray<'local>,
) -> JByteArray<'local> {
    if generation <= 0 || plan <= 0 {
        return JByteArray::default();
    }
    match unowned_environment
        .with_env(|environment| -> jni::errors::Result<JByteArray<'local>> {
            let payload = environment.convert_byte_array(&request)?;
            if payload.is_empty() || payload.len() > 1_048_576 {
                return Ok(JByteArray::default());
            }
            let plan = plan.cast_unsigned();
            if generation.cast_unsigned() != GENERATION.load(Ordering::Acquire)
                || generation_of(plan) != generation.cast_unsigned()
                || !REQUESTS.iter().any(|slot| slot.load(Ordering::Acquire) == plan)
            {
                return Ok(JByteArray::default());
            }
            let Some(session) = session_capability(plan) else {
                return Ok(JByteArray::default());
            };
            let Ok(response) = session.execute(payload) else {
                return Ok(JByteArray::default());
            };
            if response.is_empty() || response.len() > 1_048_576 {
                return Ok(JByteArray::default());
            }
            environment.byte_array_from_slice(&response)
        })
        .into_outcome()
    {
        jni::Outcome::Ok(response) => response,
        jni::Outcome::Err(_) | jni::Outcome::Panic(_) => JByteArray::default(),
    }
}

/// Cancels a request at most once and returns a stable typed status code.
#[unsafe(no_mangle)]
pub extern "C" fn choosh_bridge_request_cancel(generation: u32, request: u64) -> i32 {
    if generation == 0 || generation != GENERATION.load(Ordering::Acquire) {
        return STATUS_STALE_GENERATION;
    }
    if request == 0 || generation_of(request) != generation || kind_of(request) == 0 {
        return STATUS_INVALID_ARGUMENT;
    }
    for slot in &REQUESTS {
        if slot
            .compare_exchange(request, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return STATUS_OK;
        }
    }
    STATUS_UNKNOWN_REQUEST
}

/// Advances process recreation generation and invalidates every old callback ID.
#[unsafe(no_mangle)]
pub extern "C" fn choosh_bridge_recreate(expected_generation: u32) -> i32 {
    if expected_generation == 0 || expected_generation >= TOKEN_GENERATION_MASK {
        return STATUS_INVALID_ARGUMENT;
    }
    let Some(next) = expected_generation.checked_add(1) else {
        return STATUS_INVALID_ARGUMENT;
    };
    if GENERATION
        .compare_exchange(
            expected_generation,
            next,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return STATUS_STALE_GENERATION;
    }
    for slot in &REQUESTS {
        slot.store(0, Ordering::Release);
    }
    if release_all_runtime_allocations() {
        STATUS_OK
    } else {
        STATUS_TRANSPORT_UNAVAILABLE
    }
}

/// Exposes numeric status identities without allocating strings across the ABI.
#[unsafe(no_mangle)]
pub extern "C" fn choosh_bridge_status_capacity() -> i32 {
    STATUS_CAPACITY
}

const fn encode(generation: u32, id: u32, kind: u32) -> u64 {
    ((generation as u64) << (TOKEN_ID_BITS + TOKEN_KIND_BITS))
        | ((id as u64) << TOKEN_KIND_BITS)
        | kind as u64
}

const fn generation_of(request: u64) -> u32 {
    ((request >> (TOKEN_ID_BITS + TOKEN_KIND_BITS)) as u32) & TOKEN_GENERATION_MASK
}

const fn kind_of(request: u64) -> u64 {
    request & TOKEN_KIND_MASK_U64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ABI_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Debug, Eq, PartialEq)]
    enum AdmissionError {
        HostKeyMismatch,
    }

    #[derive(Debug, Eq, PartialEq)]
    enum Event {
        HostKeyChecked,
        SignatureRequested,
    }

    struct RecordingAdmission<'a> {
        events: &'a Mutex<Vec<Event>>,
        accept: bool,
    }

    impl ExactHostKeyAdmission for RecordingAdmission<'_> {
        type Error = AdmissionError;

        fn verify_exact_host_key(
            &mut self,
            _endpoint: EndpointHandle,
            _known_host: KnownHostHandle,
        ) -> Result<(), Self::Error> {
            self.events.lock().unwrap().push(Event::HostKeyChecked);
            self.accept
                .then_some(())
                .ok_or(AdmissionError::HostKeyMismatch)
        }
    }

    struct RecordingSigner<'a> {
        events: &'a Mutex<Vec<Event>>,
    }

    impl KeystorePublicKeyAuthentication for RecordingSigner<'_> {
        type Error = std::convert::Infallible;

        fn begin_public_key_authentication(
            &mut self,
            _endpoint: EndpointHandle,
            _username: UsernameHandle,
            _credential_reference: CredentialReferenceHandle,
            _public_key: PublicKeyHandle,
            _signing_callback: SigningCallbackHandle,
        ) -> Result<(), Self::Error> {
            self.events.lock().unwrap().push(Event::SignatureRequested);
            Ok(())
        }
    }

    #[test]
    fn typed_plan_cannot_request_keystore_authentication_before_exact_host_key_admission() {
        let events = Mutex::new(Vec::new());
        let plan = NativeAuthenticatedPlan::new(1, 2, 3, 4, 5, 6, 7).expect("non-zero handles");
        let error = plan
            .admit_exact_host_key(&mut RecordingAdmission {
                events: &events,
                accept: false,
            })
            .expect_err("changed host key rejects before authentication");
        assert_eq!(error, AdmissionError::HostKeyMismatch);
        assert_eq!(*events.lock().unwrap(), [Event::HostKeyChecked]);
    }

    #[test]
    fn typed_plan_requests_keystore_authentication_only_after_exact_host_key_admission() {
        let events = Mutex::new(Vec::new());
        let plan = NativeAuthenticatedPlan::new(1, 2, 3, 4, 5, 6, 7).expect("non-zero handles");
        let admitted = plan
            .admit_exact_host_key(&mut RecordingAdmission {
                events: &events,
                accept: true,
            })
            .expect("exact host key admitted");
        admitted
            .begin_public_key_authentication(&mut RecordingSigner { events: &events })
            .expect("fake keystore accepts");
        assert_eq!(
            *events.lock().unwrap(),
            [Event::HostKeyChecked, Event::SignatureRequested]
        );
    }

    #[test]
    fn abi_request_cancel_and_recreation_are_typed_and_bounded() {
        let _guard = ABI_TEST_LOCK.lock().unwrap();
        assert_eq!(choosh_bridge_abi_version(), 3);
        let generation = choosh_bridge_generation();
        let request = choosh_bridge_request_begin(generation, 7);
        assert_ne!(request, 0);
        assert_eq!(choosh_bridge_request_cancel(generation, request), STATUS_OK);
        assert_eq!(
            choosh_bridge_request_cancel(generation, request),
            STATUS_UNKNOWN_REQUEST
        );

        let stale = choosh_bridge_request_begin(generation, 7);
        assert_ne!(stale, 0);
        assert_eq!(choosh_bridge_recreate(generation), STATUS_OK);
        assert_eq!(
            choosh_bridge_request_cancel(generation, stale),
            STATUS_STALE_GENERATION
        );
    }

    #[test]
    fn authenticated_plan_accepts_only_opaque_nonzero_handles() {
        let _guard = ABI_TEST_LOCK.lock().unwrap();
        let generation = choosh_bridge_generation();
        assert_eq!(
            choosh_bridge_authenticated_plan_begin(generation, 1, 2, 3, 0, 5, 6, 7),
            0
        );
        assert_eq!(
            choosh_bridge_authenticated_plan_begin(generation, 1, 2, 3, 4, 5, 0, 7),
            0
        );
        assert_eq!(
            choosh_bridge_authenticated_plan_begin(generation, 1, 2, 3, 4, 5, 6, 0),
            0
        );
        let plan = choosh_bridge_authenticated_plan_begin(generation, 1, 2, 3, 4, 5, 6, 7);
        assert_ne!(plan, 0);
        assert_eq!(
            choosh_bridge_authenticated_plan_open(generation, plan),
            STATUS_TRANSPORT_UNAVAILABLE
        );
        assert_eq!(
            choosh_bridge_authenticated_plan_cancel(generation, plan),
            STATUS_OK
        );
    }

    #[test]
    fn unowned_authenticated_plan_cannot_advance_to_transport() {
        let _guard = ABI_TEST_LOCK.lock().unwrap();
        let generation = choosh_bridge_generation();
        assert_eq!(
            choosh_bridge_authenticated_plan_open(
                generation,
                encode(generation, 0x004d, AUTHENTICATED_PLAN_STATUS),
            ),
            STATUS_UNKNOWN_REQUEST
        );
    }

    #[test]
    fn generic_request_cannot_claim_authenticated_plan_admission() {
        let _guard = ABI_TEST_LOCK.lock().unwrap();
        let generation = choosh_bridge_generation();
        let generic = choosh_bridge_request_begin(generation, 7);
        assert_ne!(generic, 0);
        assert_eq!(
            choosh_bridge_authenticated_plan_open(generation, generic),
            STATUS_INVALID_ARGUMENT
        );
        assert_eq!(choosh_bridge_request_cancel(generation, generic), STATUS_OK);
    }

    #[derive(Default)]
    struct RecordingCallbacks {
        closes: std::sync::atomic::AtomicUsize,
    }
    impl RuntimeCallbacks for RecordingCallbacks {
        type Error = ();
        fn metadata(&self, _: RuntimeLeaseHandle) -> Result<Vec<u8>, Self::Error> {
            Ok(runtime_metadata_fixture())
        }
        fn public_key(&self, _: RuntimeLeaseHandle) -> Result<Vec<u8>, Self::Error> {
            Ok(fixture_public_key())
        }
        fn read(&self, _: RuntimeLeaseHandle, output: &mut [u8]) -> Result<usize, Self::Error> {
            output[0] = 7;
            Ok(1)
        }
        fn write(&self, _: RuntimeLeaseHandle, _: &[u8]) -> Result<(), Self::Error> {
            Ok(())
        }
        fn sign(&self, _: RuntimeLeaseHandle, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
            Ok(vec![9])
        }
        fn close(&self, _: RuntimeLeaseHandle) -> Result<(), Self::Error> {
            self.closes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn runtime_allocation_bounds_and_closes_its_lease_once() {
        let allocation = RuntimeAllocation::new(
            RecordingCallbacks::default(),
            RuntimeLeaseHandle::new(9).unwrap(),
            4,
        )
        .unwrap();
        assert_eq!(allocation.read(&mut [0; 1]), Ok(1));
        assert_eq!(
            allocation.metadata().unwrap().username(),
            &SshUsername::parse("fixture-user").unwrap()
        );
        let metadata = allocation.metadata().unwrap();
        assert_eq!(
            choosh_ssh::presented_fingerprint(&allocation.public_key(&metadata).unwrap()),
            metadata.public_key().fingerprint().as_str()
        );
        assert_eq!(allocation.write(&[]), Err(RuntimeAllocationError::Bounds));
        allocation.close().unwrap();
        allocation.close().unwrap();
        assert_eq!(allocation.callbacks.closes.load(Ordering::Relaxed), 1);
        assert_eq!(allocation.sign(&[1]), Err(RuntimeAllocationError::Closed));
    }

    fn runtime_metadata_fixture() -> Vec<u8> {
        let fields = [
            b"fixture-user".as_slice(),
            b"SHA256:0123456789012345678901234567890123456789012".as_slice(),
            b"ED25519".as_slice(),
            b"SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ".as_slice(),
        ];
        let mut bytes = vec![RUNTIME_METADATA_VERSION];
        for field in fields {
            bytes.push(u8::try_from(field.len()).unwrap());
            bytes.extend_from_slice(field);
        }
        bytes
    }

    fn fixture_public_key() -> Vec<u8> {
        b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti".to_vec()
    }

    #[test]
    fn runtime_metadata_rejects_trailing_or_invalid_identity_values() {
        let mut fixture = runtime_metadata_fixture();
        fixture.push(0);
        assert_eq!(
            RuntimeConnectionMetadata::parse(&fixture),
            Err(RuntimeMetadataError::InvalidEncoding)
        );
        assert_eq!(
            RuntimeConnectionMetadata::parse(&[RUNTIME_METADATA_VERSION]),
            Err(RuntimeMetadataError::InvalidEncoding)
        );
    }

    #[test]
    fn runtime_public_key_rejects_identity_mismatch() {
        struct MismatchedCallbacks;
        impl RuntimeCallbacks for MismatchedCallbacks {
            type Error = ();
            fn metadata(&self, _: RuntimeLeaseHandle) -> Result<Vec<u8>, Self::Error> {
                Ok(runtime_metadata_fixture())
            }
            fn public_key(&self, _: RuntimeLeaseHandle) -> Result<Vec<u8>, Self::Error> {
                Ok(b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAID6hVSJVNnvDT3Iy7h+hdMdV40l0oTbXvHhUKeZp30iU".to_vec())
            }
            fn read(&self, _: RuntimeLeaseHandle, _: &mut [u8]) -> Result<usize, Self::Error> {
                Ok(1)
            }
            fn write(&self, _: RuntimeLeaseHandle, _: &[u8]) -> Result<(), Self::Error> {
                Ok(())
            }
            fn sign(&self, _: RuntimeLeaseHandle, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
                Ok(vec![1])
            }
            fn close(&self, _: RuntimeLeaseHandle) -> Result<(), Self::Error> {
                Ok(())
            }
        }
        let allocation =
            RuntimeAllocation::new(MismatchedCallbacks, RuntimeLeaseHandle::new(9).unwrap(), 4)
                .unwrap();
        let metadata = allocation.metadata().unwrap();
        assert_eq!(
            allocation.public_key(&metadata),
            Err(RuntimeAllocationError::PublicKey(
                RuntimePublicKeyError::FingerprintMismatch
            ))
        );
    }

    #[test]
    fn runtime_transport_composition_validates_identity_without_signing() {
        struct CountingCallbacks {
            signs: std::sync::atomic::AtomicUsize,
        }
        impl RuntimeCallbacks for CountingCallbacks {
            type Error = ();
            fn metadata(&self, _: RuntimeLeaseHandle) -> Result<Vec<u8>, Self::Error> {
                Ok(runtime_metadata_fixture())
            }
            fn public_key(&self, _: RuntimeLeaseHandle) -> Result<Vec<u8>, Self::Error> {
                Ok(fixture_public_key())
            }
            fn read(&self, _: RuntimeLeaseHandle, _: &mut [u8]) -> Result<usize, Self::Error> {
                Ok(0)
            }
            fn write(&self, _: RuntimeLeaseHandle, _: &[u8]) -> Result<(), Self::Error> {
                Ok(())
            }
            fn sign(&self, _: RuntimeLeaseHandle, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
                self.signs.fetch_add(1, Ordering::Relaxed);
                Ok(vec![1])
            }
            fn close(&self, _: RuntimeLeaseHandle) -> Result<(), Self::Error> {
                Ok(())
            }
        }
        let callbacks = CountingCallbacks {
            signs: std::sync::atomic::AtomicUsize::new(0),
        };
        let allocation = Arc::new(
            RuntimeAllocation::new(callbacks, RuntimeLeaseHandle::new(9).unwrap(), 65_536).unwrap(),
        );
        let transport = RuntimeLeaseTransport::new(
            Arc::clone(&allocation),
            StreamChunkLimits::new(1_024, 1_024).unwrap(),
        )
        .expect("validated public identity composes without an SSH signature");
        assert_eq!(
            allocation.callbacks.signs.load(Ordering::Relaxed),
            0,
            "composition must not reach the payload-only signer"
        );
        drop(transport);
    }

    #[test]
    fn session_registry_keeps_plan_ownership_explicit_and_bounded() {
        let mut sessions = SessionRegistry::new();
        sessions.insert(11, "first").unwrap();
        assert_eq!(
            sessions.insert(11, "second"),
            Err(SessionRegistryError::InvalidPlan)
        );
        assert_eq!(sessions.remove(11), Some("first"));
        assert_eq!(sessions.remove(11), None);
        sessions.insert(12, "second").unwrap();
        sessions
            .with_session(12, |session| *session = "updated")
            .unwrap();
        assert_eq!(sessions.remove(12), Some("updated"));
        sessions.insert(12, "second").unwrap();
        sessions.clear();
        assert_eq!(sessions.remove(12), None);
    }

    #[test]
    fn session_actor_serializes_fixed_rpc_without_registry_locking() {
        struct Echo;
        impl FixedRpcExecutor for Echo {
            fn execute(
                &mut self,
                payload: Vec<u8>,
            ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ()>> + Send + '_>> {
                Box::pin(async move { Ok(payload) })
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let actor = SessionActor::spawn(Echo);
            let mut sessions = SessionRegistry::new();
            sessions.insert(44, actor.clone()).unwrap();
            assert_eq!(sessions.execute_rpc(44, vec![1, 2]).await, Ok(vec![1, 2]));
            assert_eq!(sessions.execute_rpc(45, vec![1]).await, Err(()));
            actor.close();
            assert_eq!(actor.execute(vec![3]).await, Err(()));
        });
    }
}
