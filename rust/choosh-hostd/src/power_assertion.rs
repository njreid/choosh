//! macOS power-assertion management —
//! [`docs/specs/host-deployment.md`](../../../docs/specs/host-deployment.md)'s
//! "Power assertions" requirement under "macOS service lifecycle": while
//! `choosh-hostd serve` is running and trying to stay connected to
//! `relayd`, hold an `IOPMAssertionCreateWithName` assertion of type
//! `kIOPMAssertionTypePreventUserIdleSystemSleep` so macOS idle sleep can't
//! silently sever the outbound `relayd` connection mid-task, and release it
//! cleanly on shutdown.
//!
//! **Scope note, reported rather than silently narrowed**: the spec's exact
//! condition is "whenever the host has at least one attached PTY ... or a
//! running registered service/build process, MUST release it as soon as
//! none of those conditions hold" — a liveness signal that would need to be
//! threaded through from `pty.rs`'s attach/detach points,
//! `agent_launch.rs`'s session lifecycle, and service-tunnel/build-process
//! tracking. That's a materially larger, cross-cutting change than this
//! module's scope (a new file plus small, additive `serve.rs` wiring, per
//! this task's own boundary). What's implemented instead: the assertion is
//! held for `connect_loop`'s entire lifetime — i.e. the whole time
//! `choosh-hostd` is trying to stay connected to `relayd`, across every
//! reconnect, for the daemon's whole run after startup. This is a
//! conservative superset of the spec's condition: it never *under-holds*
//! relative to what the spec requires (every PTY-attached/build-active
//! moment is a subset of "the daemon is running"), so the connection-drop
//! risk the spec is actually guarding against is fully covered; the
//! tradeoff is holding the assertion (and so blocking idle sleep) during
//! genuinely idle stretches too, which the spec's finer-grained condition
//! would have avoided. Narrowing this to the spec's exact condition is a
//! real, scoped follow-up, not done here.
//!
//! On any non-macOS target (this sandbox is Linux-only, and cannot build or
//! run the macOS path at all) [`PowerAssertion`] is a real, harmless no-op:
//! same public API, no IOKit/CoreFoundation calls, so `serve.rs`'s call
//! sites never need their own `#[cfg(target_os = "macos")]` branching.

/// Whether a power assertion is currently held, independent of the
/// platform mechanism used to acquire/release it — a pure state machine
/// with no syscalls, unit-tested directly below regardless of host
/// platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Released,
    Held,
}

impl State {
    /// Returns the new state and whether the caller should actually do the
    /// (fallible, platform-specific) work of creating an assertion: `false`
    /// means "already held," an idempotent no-op that must not create a
    /// second, leaked platform assertion.
    fn on_acquire(self) -> (Self, bool) {
        match self {
            State::Released => (State::Held, true),
            State::Held => (State::Held, false),
        }
    }

    /// Returns the new state and whether the caller should actually
    /// release the platform assertion: `false` means "already released,"
    /// an idempotent no-op that must not double-release a platform
    /// assertion ID.
    fn on_release(self) -> (Self, bool) {
        match self {
            State::Held => (State::Released, true),
            State::Released => (State::Released, false),
        }
    }
}

/// A held-or-not power assertion. `acquire`/`release` are both idempotent
/// (safe to call repeatedly or out of order); dropping a still-held
/// instance releases it, so holders don't strictly need to call `release`
/// on every exit path, though `serve.rs`'s shutdown path calls it
/// explicitly for clarity and so its own tracing log fires exactly at
/// shutdown rather than at whatever later point the value happens to drop.
pub struct PowerAssertion {
    state: State,
    #[cfg(target_os = "macos")]
    id: Option<macos::AssertionId>,
}

impl PowerAssertion {
    pub const fn new() -> Self {
        Self {
            state: State::Released,
            #[cfg(target_os = "macos")]
            id: None,
        }
    }

    /// Claims the assertion if not already held. `reason` is a short,
    /// human-readable label surfaced in macOS's `pmset -g assertions`
    /// output — never parsed, purely diagnostic.
    ///
    /// Never panics: a platform failure to create the assertion is logged
    /// and leaves this instance in the released state (idle sleep remains
    /// possible, which is the pre-existing, already-documented risk this
    /// module exists to reduce, not a new failure mode) rather than taking
    /// down the whole daemon.
    pub fn acquire(&mut self, reason: &str) {
        let (next, should_create) = self.state.on_acquire();
        self.state = next;
        // A positive-condition guard (not an early `if !should_create {
        // return; }`) deliberately: on a non-macOS build the only thing
        // that would otherwise follow a `return` is this same platform's
        // no-op block, which clippy's `needless_return` correctly flags
        // as pointless *for that platform* — but the equivalent early
        // return is very much needed on macOS, where it must skip the
        // real `IOPMAssertionCreateWithName` call. Keeping one shape that
        // behaves identically (and lints cleanly) on every target beats a
        // `return` whose necessity is platform-dependent.
        if should_create {
            #[cfg(target_os = "macos")]
            {
                match macos::create(reason) {
                    Ok(id) => {
                        self.id = Some(id);
                        tracing::info!("acquired macOS power assertion (prevent idle sleep)");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "failed to acquire macOS power assertion; idle sleep may sever the relayd connection");
                        // Nothing was actually claimed — roll the state
                        // back so a later `acquire` call can retry instead
                        // of treating this as permanently (falsely) held.
                        self.state = State::Released;
                    }
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = reason;
            }
        }
    }

    /// Releases the assertion if held. Idempotent — a no-op if not
    /// currently held.
    pub fn release(&mut self) {
        let (next, should_release) = self.state.on_release();
        self.state = next;
        // Same positive-condition-guard reasoning as `acquire` above.
        if should_release {
            #[cfg(target_os = "macos")]
            if let Some(id) = self.id.take() {
                macos::release(id);
                tracing::info!("released macOS power assertion");
            }
        }
    }

    /// Whether this instance currently holds a power assertion. Exposed
    /// mainly for tests; also useful for a future readiness/status RPC to
    /// report honestly. `#[allow(dead_code)]`: `power_assertion` is a
    /// private module (`lib.rs`), so this `pub` method — genuinely part of
    /// the type's intended API, not vestigial — isn't reachable from
    /// outside the crate and would otherwise warn as unused in a
    /// non-`#[cfg(test)]` build, since `serve.rs`'s only call sites today
    /// are `acquire`/`release`.
    #[allow(dead_code)]
    pub fn is_held(&self) -> bool {
        self.state == State::Held
    }
}

impl Default for PowerAssertion {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PowerAssertion {
    fn drop(&mut self) {
        self.release();
    }
}

/// Real IOKit/CoreFoundation FFI. Deliberately hand-rolled `extern "C"`
/// declarations rather than a dependency on the `core-foundation`/`objc2`
/// crate family: this module calls exactly two IOKit functions plus two
/// CoreFoundation helpers to build the one `CFStringRef` argument they
/// need, a small enough, single-purpose surface that a maintained
/// wrapper crate would be more weight than value for.
///
/// Signatures cross-checked against Apple's published
/// `IOKit/pwr_mgt/IOPMLib.h` (`IOPMAssertionCreateWithName`,
/// `IOPMAssertionRelease`) and `CoreFoundation/CFString.h`
/// (`CFStringCreateWithCString`) declarations — this sandbox has no macOS
/// SDK to link against (see this crate's `power_assertion` module's
/// verification notes), so `cargo check --target x86_64-apple-darwin`
/// (which type-checks and generates code without invoking the linker) is
/// the actual verification these signatures got: a wrong argument type,
/// count, or calling convention here would fail that check.
#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{c_void, CString};
    use std::os::raw::{c_char, c_int};

    pub type AssertionId = u32;

    type CfStringRef = *const c_void;
    type CfAllocatorRef = *const c_void;
    type CfStringEncoding = u32;
    /// `kCFStringEncodingUTF8`, a fixed constant per `CFString.h` (not an
    /// extern symbol — its value is stable ABI, unlike the assertion-type
    /// constant below, which IS an extern symbol).
    const CF_STRING_ENCODING_UTF8: CfStringEncoding = 0x0800_0100;

    type IoPmAssertionLevel = u32;
    type IoReturn = c_int;
    /// `kIOPMAssertionLevelOn`.
    const IO_PM_ASSERTION_LEVEL_ON: IoPmAssertionLevel = 255;
    /// `kIOReturnSuccess` (`KERN_SUCCESS`).
    const K_IO_RETURN_SUCCESS: IoReturn = 0;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(alloc: CfAllocatorRef, c_str: *const c_char, encoding: CfStringEncoding) -> CfStringRef;
        fn CFRelease(cf: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        #[allow(non_upper_case_globals)]
        static kIOPMAssertionTypePreventUserIdleSystemSleep: CfStringRef;
        fn IOPMAssertionCreateWithName(
            assertion_type: CfStringRef,
            assertion_level: IoPmAssertionLevel,
            assertion_name: CfStringRef,
            assertion_id: *mut AssertionId,
        ) -> IoReturn;
        fn IOPMAssertionRelease(assertion_id: AssertionId) -> IoReturn;
    }

    /// Creates and claims a `kIOPMAssertionTypePreventUserIdleSystemSleep`
    /// assertion named `reason`. `Err` on any CoreFoundation/IOKit failure
    /// — the caller (`PowerAssertion::acquire`) logs and treats this as
    /// "stayed released," never a panic.
    pub fn create(reason: &str) -> Result<AssertionId, String> {
        // A reason containing an interior NUL can't round-trip through a
        // C string; fall back to a fixed, known-NUL-free label rather than
        // failing the whole acquire over a cosmetic assertion name.
        let c_reason = CString::new(reason).unwrap_or_else(|_| CString::new("choosh-hostd").expect("fixed literal has no interior NUL"));
        unsafe {
            let name_ref = CFStringCreateWithCString(std::ptr::null(), c_reason.as_ptr(), CF_STRING_ENCODING_UTF8);
            if name_ref.is_null() {
                return Err("CFStringCreateWithCString returned NULL".to_string());
            }
            let mut id: AssertionId = 0;
            let result = IOPMAssertionCreateWithName(kIOPMAssertionTypePreventUserIdleSystemSleep, IO_PM_ASSERTION_LEVEL_ON, name_ref, &mut id);
            CFRelease(name_ref);
            if result == K_IO_RETURN_SUCCESS {
                Ok(id)
            } else {
                Err(format!("IOPMAssertionCreateWithName failed with IOReturn {result}"))
            }
        }
    }

    /// Releases a previously-created assertion. Logged, not propagated —
    /// `PowerAssertion::release` has already committed to the released
    /// state by the time this runs; there is no meaningful recovery from
    /// `IOPMAssertionRelease` itself failing beyond reporting it.
    pub fn release(id: AssertionId) {
        let result = unsafe { IOPMAssertionRelease(id) };
        if result != K_IO_RETURN_SUCCESS {
            tracing::warn!(io_return = result, "IOPMAssertionRelease failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PowerAssertion, State};

    // Pure state-machine transitions — independent of PowerAssertion's
    // platform wiring entirely, so these exercise the exact same logic
    // that would gate real IOKit calls on macOS without needing macOS.
    #[test]
    fn state_transitions() {
        assert_eq!(State::Released.on_acquire(), (State::Held, true));
        assert_eq!(State::Held.on_acquire(), (State::Held, false));
        assert_eq!(State::Held.on_release(), (State::Released, true));
        assert_eq!(State::Released.on_release(), (State::Released, false));
    }

    #[test]
    fn starts_released() {
        let assertion = PowerAssertion::new();
        assert!(!assertion.is_held());
    }

    #[test]
    fn default_starts_released() {
        let assertion = PowerAssertion::default();
        assert!(!assertion.is_held());
    }

    #[test]
    fn acquire_marks_held() {
        let mut assertion = PowerAssertion::new();
        assertion.acquire("test");
        assert!(assertion.is_held());
    }

    // On this (non-macOS) sandbox, `acquire` never touches IOKit at all,
    // so this specifically proves the no-op path: repeated acquire never
    // panics, never errors, and settles on "held" — exactly the "genuinely
    // harmless no-op" this module's doc comment promises for non-macOS
    // targets.
    #[test]
    fn acquire_is_idempotent_and_never_panics() {
        let mut assertion = PowerAssertion::new();
        assertion.acquire("test");
        assertion.acquire("test");
        assertion.acquire("test");
        assert!(assertion.is_held());
    }

    #[test]
    fn release_marks_not_held() {
        let mut assertion = PowerAssertion::new();
        assertion.acquire("test");
        assertion.release();
        assert!(!assertion.is_held());
    }

    #[test]
    fn release_without_acquire_is_a_harmless_no_op() {
        let mut assertion = PowerAssertion::new();
        assertion.release();
        assert!(!assertion.is_held());
    }

    #[test]
    fn release_is_idempotent() {
        let mut assertion = PowerAssertion::new();
        assertion.acquire("test");
        assertion.release();
        assertion.release();
        assert!(!assertion.is_held());
    }

    #[test]
    fn drop_releases_without_panicking() {
        let mut assertion = PowerAssertion::new();
        assertion.acquire("test");
        drop(assertion);
    }

    #[test]
    fn reacquire_after_release_holds_again() {
        let mut assertion = PowerAssertion::new();
        assertion.acquire("test");
        assertion.release();
        assertion.acquire("test");
        assert!(assertion.is_held());
    }
}
