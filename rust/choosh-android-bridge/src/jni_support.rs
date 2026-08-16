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
