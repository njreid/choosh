//! Minimal stable C ABI composition root for Android/Rust lifecycle smoke tests.
//!
//! Every export uses fixed-width integers only. No pointer crosses the ABI, so
//! callers cannot violate Rust aliasing, lifetime, alignment, or ownership rules.

#![allow(unsafe_code)] // Required only for Edition 2024's `no_mangle` ABI attribute.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const ABI_VERSION: u32 = 1;
const STATUS_OK: i32 = 0;
const STATUS_STALE_GENERATION: i32 = 1;
const STATUS_UNKNOWN_REQUEST: i32 = 2;
const STATUS_CAPACITY: i32 = 3;
const STATUS_INVALID_ARGUMENT: i32 = 4;
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
}
