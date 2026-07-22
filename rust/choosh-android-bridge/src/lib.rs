//! Minimal stable C ABI composition root for Android/Rust lifecycle smoke tests.
//!
//! Every export uses fixed-width integers only. No pointer crosses the ABI, so
//! callers cannot violate Rust aliasing, lifetime, alignment, or ownership rules.

#![allow(unsafe_code)] // Required only for Edition 2024's `no_mangle` ABI attribute.

use std::ffi::c_void;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const ABI_VERSION: u32 = 3;
const STATUS_OK: i32 = 0;
const STATUS_STALE_GENERATION: i32 = 1;
const STATUS_UNKNOWN_REQUEST: i32 = 2;
const STATUS_CAPACITY: i32 = 3;
const STATUS_INVALID_ARGUMENT: i32 = 4;
const STATUS_TRANSPORT_UNAVAILABLE: i32 = 5;
const AUTHENTICATED_PLAN_STATUS: u32 = 8;
const SLOT_COUNT: usize = 64;
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
    choosh_bridge_request_cancel(generation, plan)
}

/// Attempts to advance an admitted plan into the native transport.
///
/// This deliberately fails closed until a later composition root can resolve
/// the opaque Android handles into an injected stream, exact-host-key verifier,
/// and Keystore-backed signer. In particular it never asks Android to provide
/// credential material and it never treats plan creation as authentication.
/// The request remains owned by the Java plan lifecycle and must be cancelled
/// by its caller after this result.
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
    if REQUESTS
        .iter()
        .any(|slot| slot.load(Ordering::Acquire) == plan)
    {
        STATUS_TRANSPORT_UNAVAILABLE
    } else {
        STATUS_UNKNOWN_REQUEST
    }
}

// JNI wrappers intentionally accept only primitive values. The Java side
// resolves typed profile metadata to opaque handles before this boundary, so
// Rust never dereferences a JVM object or receives credential material.
#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_choosh_RustNativeConnectorJni_nativeAbiVersion(
    _environment: *mut c_void,
    _class: *mut c_void,
) -> i32 {
    choosh_bridge_abi_version().cast_signed()
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_ai_choosh_RustNativeConnectorJni_nativeBeginAuthenticatedPlan(
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

#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_choosh_RustNativeConnectorJni_nativeCancelAuthenticatedPlan(
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
pub extern "system" fn Java_ai_choosh_RustNativeConnectorJni_nativeOpenAuthenticatedPlan(
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
    STATUS_OK
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
}
