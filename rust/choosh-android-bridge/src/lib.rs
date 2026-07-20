//! Minimal stable C ABI composition root for Android/Rust lifecycle smoke tests.
//!
//! Every export uses fixed-width integers only. No pointer crosses the ABI, so
//! callers cannot violate Rust aliasing, lifetime, alignment, or ownership rules.

#![allow(unsafe_code)] // Required only for Edition 2024's `no_mangle` ABI attribute.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const ABI_VERSION: u32 = 1;
const STATUS_OK: i32 = 0;
const STATUS_STALE_GENERATION: i32 = 1;
const STATUS_UNKNOWN_REQUEST: i32 = 2;
const STATUS_CAPACITY: i32 = 3;
const STATUS_INVALID_ARGUMENT: i32 = 4;
const STATUS_TRANSPORT_UNAVAILABLE: i32 = 5;
const AUTHENTICATED_PLAN_STATUS: u32 = 8;
const SLOT_COUNT: usize = 64;

static GENERATION: AtomicU32 = AtomicU32::new(1);
static NEXT_REQUEST: AtomicU32 = AtomicU32::new(1);
static REQUESTS: [AtomicU64; SLOT_COUNT] = [const { AtomicU64::new(0) }; SLOT_COUNT];

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
    if generation == 0 || status == 0 || generation != GENERATION.load(Ordering::Acquire) {
        return 0;
    }
    let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
    if id == 0 {
        return 0;
    }
    let key = encode(generation, id);
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
/// Every input is a non-zero opaque handle owned by Android. In particular,
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
) -> u64 {
    if endpoint == 0 || username == 0 || known_host == 0 || credential_ref == 0 || public_key == 0 {
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
    if plan == 0 || generation_of(plan) != generation {
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
) -> i64 {
    if generation <= 0
        || endpoint <= 0
        || username <= 0
        || known_host <= 0
        || credential_ref <= 0
        || public_key <= 0
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
    if request == 0 || generation_of(request) != generation {
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
    if expected_generation == 0 {
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

const fn encode(generation: u32, id: u32) -> u64 {
    (generation as u64) << 32 | id as u64
}

const fn generation_of(request: u64) -> u32 {
    (request >> 32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_request_cancel_and_recreation_are_typed_and_bounded() {
        assert_eq!(choosh_bridge_abi_version(), 1);
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
        let generation = choosh_bridge_generation();
        assert_eq!(
            choosh_bridge_authenticated_plan_begin(generation, 1, 2, 3, 0, 5),
            0
        );
        let plan = choosh_bridge_authenticated_plan_begin(generation, 1, 2, 3, 4, 5);
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
        let generation = choosh_bridge_generation();
        assert_eq!(
            choosh_bridge_authenticated_plan_open(generation, u64::from(generation) << 32 | 0x004d),
            STATUS_UNKNOWN_REQUEST
        );
    }
}
