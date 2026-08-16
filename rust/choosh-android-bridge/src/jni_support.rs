//! Small helpers shared by this crate's Android-only JNI-facing modules
//! (`terminal_jni.rs`, `gateway_jni.rs`) — factored out to replace what used
//! to be independent, hand-duplicated copies in each: a `JString` decoder
//! with the same "give up cleanly rather than propagate a JNI error"
//! fallback, and the `LazyLock<Mutex<HashMap<i64, T>>>` + `AtomicI64`
//! opaque-handle registry pattern each module's own session/gateway table
//! used.
//!
//! `lib.rs` keeps its own, deliberately distinct `jstring_to_string` (it
//! returns a `Result`, propagating a real JNI error rather than silently
//! defaulting to `""`) — every `NativeBridge` native method already threads
//! a `Result` back through `native_method!`, so it can afford the stricter
//! behavior; `terminal_jni.rs`/`gateway_jni.rs`'s native methods mostly
//! don't, hence the lossy-default version here instead.

use jni::Env;
use jni::objects::JString;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

/// Decodes a `JString` to a Rust `String`, defaulting to `""` on a decode
/// failure — see this module's doc comment for why this (rather than
/// `lib.rs`'s stricter, `Result`-returning version) is what
/// `terminal_jni.rs`/`gateway_jni.rs` both want.
pub(crate) fn jstring_to_string(env: &Env<'_>, value: &JString<'_>) -> String {
    value.mutf8_chars(env).map(|chars| chars.to_string()).unwrap_or_default()
}

/// An opaque-`i64`-handle-keyed table: [`Self::insert`] mints a fresh,
/// monotonically increasing handle and stores a value under it;
/// [`Self::get`]/[`Self::remove`] look up (or take) an entry by handle.
/// Every method recovers from a poisoned lock rather than propagating the
/// panic — one handle's entry panicking mid-access must not wedge every
/// other handle lookup for the lifetime of the process, matching what each
/// of this registry's three call sites (`terminal_jni::SESSIONS`,
/// `gateway_jni::WEB_GATEWAYS`, `gateway_jni::MARKDOWN_GATEWAYS`) already
/// did by hand before this.
pub(crate) struct HandleRegistry<T> {
    entries: Mutex<HashMap<i64, T>>,
    next_handle: AtomicI64,
}

impl<T> Default for HandleRegistry<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> HandleRegistry<T> {
    pub(crate) fn new() -> Self {
        Self { entries: Mutex::new(HashMap::new()), next_handle: AtomicI64::new(1) }
    }

    /// Stores `value` under a fresh handle and returns that handle.
    pub(crate) fn insert(&self, value: T) -> i64 {
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(handle, value);
        handle
    }

    /// Looks up `handle`, applying `present` to a reference to its entry if
    /// found; returns `absent` otherwise (unknown handle, or an
    /// already-removed one).
    pub(crate) fn get<R>(&self, handle: i64, absent: R, present: impl FnOnce(&T) -> R) -> R {
        let entries = self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.get(&handle).map_or(absent, present)
    }

    /// Removes and returns `handle`'s entry, if any.
    pub(crate) fn remove(&self, handle: i64) -> Option<T> {
        self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&handle)
    }
}

// `jstring_to_string` is intentionally not covered here: it takes a real
// `jni::objects::JString` backed by a live `jni::Env`, and this crate has no
// path to construct either without an actual JVM (no `JavaVM`/testing
// harness in `[dev-dependencies]`, and no other module in the crate —
// including `lib.rs`'s own near-identical, stricter `jstring_to_string` —
// unit-tests one either). Faking that with a hand-rolled stand-in would
// exercise the stand-in, not this function's real decode/error path, so it's
// left untested here rather than given a vacuous test.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_get_round_trips_the_value() {
        let registry = HandleRegistry::new();
        let handle = registry.insert("hello".to_string());
        let seen = registry.get(handle, None, |value| Some(value.clone()));
        assert_eq!(seen, Some("hello".to_string()));
    }

    #[test]
    fn insert_then_get_round_trips_a_reference_without_cloning() {
        let registry = HandleRegistry::new();
        let handle = registry.insert(vec![1, 2, 3]);
        let len = registry.get(handle, 0, Vec::len);
        assert_eq!(len, 3);
    }

    #[test]
    fn distinct_inserts_get_distinct_monotonic_handles() {
        let registry = HandleRegistry::new();
        let a = registry.insert("a".to_string());
        let b = registry.insert("b".to_string());
        assert_ne!(a, b);
        assert!(b > a);
        // Each handle still resolves to its own value, not the other's.
        assert_eq!(registry.get(a, String::new(), String::clone), "a");
        assert_eq!(registry.get(b, String::new(), String::clone), "b");
    }

    #[test]
    fn remove_returns_the_entry_and_a_later_get_reports_it_absent() {
        let registry = HandleRegistry::new();
        let handle = registry.insert(42);

        let removed = registry.remove(handle);
        assert_eq!(removed, Some(42));

        let seen_after_remove = registry.get(handle, -1, |value| *value);
        assert_eq!(seen_after_remove, -1);
        // Removing again is a clean no-op, not a panic.
        assert_eq!(registry.remove(handle), None);
    }

    #[test]
    fn get_and_remove_on_an_unknown_handle_report_absent_without_panicking() {
        let registry: HandleRegistry<String> = HandleRegistry::new();
        assert_eq!(registry.get(123, "absent".to_string(), String::clone), "absent");
        assert_eq!(registry.remove(123), None);
    }

    #[test]
    fn a_poisoned_lock_is_recovered_rather_than_propagated() {
        use std::sync::Arc;

        let registry = Arc::new(HandleRegistry::new());
        let handle = registry.insert("still here".to_string());

        // Poison the internal mutex by panicking while it's held.
        let poisoner = Arc::clone(&registry);
        let panicked = std::thread::spawn(move || {
            let _guard = poisoner.entries.lock().unwrap();
            panic!("deliberately poisoning the HandleRegistry lock");
        })
        .join();
        assert!(panicked.is_err(), "the spawned thread was expected to panic");

        // Every operation recovers via `PoisonError::into_inner` rather than
        // panicking itself, and the data from before the poisoning survives.
        assert_eq!(registry.get(handle, String::new(), String::clone), "still here");
        let other = registry.insert("inserted after poisoning".to_string());
        assert_eq!(registry.get(other, String::new(), String::clone), "inserted after poisoning");
        assert_eq!(registry.remove(handle), Some("still here".to_string()));
    }
}
