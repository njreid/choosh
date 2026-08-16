//! The managed-subprocess runner for `Resource` re-auth patterns b/c/d, per
//! `docs/specs/resources-and-reauth.md` and this task's own build-time
//! simplification: **patterns b/c/d are always explicitly triggered via
//! `resource.reauth_start`/`resource.reauth_complete`, never passively
//! detected in an arbitrary human-attached PTY.** That means
//! `choosh-hostd` spawns each one as a real child process it fully owns
//! (piped stdin/stdout — `tokio::process::Command`, not a pty) rather than
//! injecting synthetic keystrokes into someone else's already-running
//! terminal session. This sidesteps the spec's flagged "PTY injection"
//! risk (see that doc's "Open questions") entirely: there is no need to
//! identify *which* still-running PTY a value should be typed into, because
//! nothing is ever typed into a PTY here at all.
//!
//! Only pattern a is ever passively detected, via `serve.rs`'s
//! `open_pty_tunnel` + `auth_detect.rs` + `resources.rs::event_for_pattern_a_detection` —
//! none of which this module touches.
//!
//! # Per-pattern shape
//!
//! - **b** (`gcloud`-shaped: prints a URL, blocks on stdin): `start` spawns
//!   `reauth_command`, scans its stdout for the first `https://` URL
//!   (reusing `auth_detect.rs`'s own ANSI-stripping/URL-extraction
//!   helpers), and emits `ResourceReauthRequired { verification_uri: Some(..),
//!   user_code: None, .. }` once found. `complete` writes the phone-supplied
//!   value, newline-terminated, straight into that same still-open child's
//!   stdin (no PTY, no device file — just a `tokio::process::ChildStdin`),
//!   then waits briefly for the process to exit.
//!
//!   **`user_code: None`, not `Some`, for pattern b — a deliberate reading
//!   of a genuine tension in the two source-of-truth documents.**
//!   `choosh-protocol::relay`'s own doc comment on
//!   `WireAgentEvent::ResourceReauthRequired::user_code` says "populated for
//!   patterns a/b." But `resources-and-reauth.md`'s provider survey is
//!   explicit that pattern b's whole shape is the *opposite* of a's: the
//!   CLI never prints a short code at all, it prints a URL and blocks on a
//!   prompt waiting for a value to be typed *in* (that value either comes
//!   from a browser page's own code, per gcloud's `--no-launch-browser`
//!   flow, or is a full remote-bootstrap command line, per its
//!   `--no-browser` flow — either way, nothing pattern b's own CLI process
//!   ever prints is a `user_code` this event's field could hold). This
//!   module follows the survey (verified provider behavior) over the wire
//!   type's doc comment (evidently written before pattern b's "manual
//!   code paste-back" shape was fully distinguished from pattern a's
//!   "self-polling device code" shape) — `user_code` is structurally
//!   meaningless for b, so it's always `None` here.
//! - **c** (static secret paste, no CLI output to scan at all): `start`
//!   spawns nothing and emits `ResourceReauthRequired` immediately, using
//!   the Resource's stored `fetch_instructions`. `complete` spawns
//!   `reauth_command` fresh (deferred exactly to this point, since there
//!   was nothing to wait on before it) and writes the value to its stdin.
//! - **d** (resume via a fresh command): `start` spawns `reauth_command`
//!   the same way b does, scanning for a URL the same way, and emits
//!   `ResourceReauthRequired { verification_uri: Some(..), user_code: None, .. }`.
//!   That original process is left to whatever it does on its own (real
//!   `firebase`, for instance, keeps polling as a documented fallback) —
//!   `complete` does not write to its stdin at all; instead it substitutes
//!   the phone-supplied value for the literal `{code}` placeholder in the
//!   Resource's `resume_command_template` and spawns *that* as a brand-new,
//!   non-interactive process, per the spec's lifecycle diagram. The
//!   original watcher process is killed once `complete` runs, since the
//!   resume command is now authoritative for this attempt — leaving it
//!   running would just be an orphaned process with no one left reading
//!   its output.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use choosh_protocol::relay::{WireAgentEvent, WireResourcePattern};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::auth_detect::{first_https_url, strip_ansi_and_normalize};
use crate::resources::Resource;

/// Bounded wait for a spawned pattern-b/d process to print a verification
/// URL before giving up — long enough for a real CLI's own startup/network
/// round trip, short enough that a genuinely hung/wrong `reauth_command`
/// doesn't leave a `resource.reauth_start` caller waiting forever for an
/// event that will never come. Mirrors `readiness.rs`'s
/// `READINESS_TIMEOUT`'s "long enough to ride out a cold start, short
/// enough to not hang" reasoning, scaled up for a real network-backed CLI
/// login rather than a local TCP connect.
const REAUTH_URL_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a spawned pattern-b/d child is kept alive awaiting
/// `resource.reauth_complete` before this module gives up, kills it, and
/// forgets it — the "reasonable timeout" this task's brief asks for.
/// Generous relative to [`REAUTH_URL_WAIT_TIMEOUT`]: once a URL is showing
/// on the phone, completing it depends on a human noticing a notification
/// and finishing a browser flow, not on network/CLI startup latency.
const REAUTH_IDLE_TIMEOUT: Duration = Duration::from_mins(10);

/// Bounded wait, after writing a value to a child's stdin (pattern b/c) or
/// spawning a fresh resume command (pattern d), for that process to exit
/// before this module reports `verified` — "wait briefly," per this task's
/// brief, not indefinitely: a real CLI normally exits within a couple of
/// seconds of a successful auth completion, and this must not block an RPC
/// response on a hung process.
const REAUTH_COMPLETE_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum ReauthError {
    /// The Resource has no `pattern` (a non-reauth Resource, e.g. a second
    /// test host) or `pattern == Some(A)` — pattern a is never explicitly
    /// started, only passively detected (see this module's own doc
    /// comment).
    NotApplicable,
    /// The Resource is missing the command this pattern needs
    /// (`reauth_command` for b/c, `resume_command_template` for d).
    MissingCommand,
    Spawn(std::io::Error),
    /// `resource.reauth_complete` named a `resource_id` with no
    /// in-progress pattern-b/d subprocess (`resource.reauth_start` was
    /// never called, already completed, or timed out).
    NoActiveReauth,
    /// `resource.reauth_start` named a `resource_id` that already has a
    /// pattern-b/d subprocess in flight. Rejected, not silently replaced —
    /// see [`ReauthProcesses::start`]'s own doc comment for why a second,
    /// concurrent start for the same resource is a genuine caller error
    /// (a double-tap, a naive RPC retry after a slow response, two phone
    /// sessions racing) rather than something safe to let overwrite the
    /// map entry a first, still-in-progress attempt is relying on.
    AlreadyInProgress,
}

impl std::fmt::Display for ReauthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotApplicable => write!(f, "this resource's pattern does not support resource.reauth_start/complete"),
            Self::MissingCommand => write!(f, "this resource has no command configured for its pattern"),
            Self::Spawn(error) => write!(f, "failed to spawn the resource's reauth command: {error}"),
            Self::NoActiveReauth => write!(f, "no in-progress reauth subprocess for this resource"),
            Self::AlreadyInProgress => write!(f, "a reauth is already in progress for this resource; call resource.reauth_complete or wait for it to time out"),
        }
    }
}

impl std::error::Error for ReauthError {}

struct Entry {
    child: Child,
}

/// Shared, `Clone`-cheap (an `Arc` internally) table of in-flight pattern-b/d
/// subprocesses, keyed by `resource_id`. One instance lives for the whole
/// `choosh-hostd serve` process lifetime, held on `RpcContext` the same way
/// `RpcContext::registry`/`RpcContext::resources` are — see `rpc.rs`.
#[derive(Clone)]
pub struct ReauthProcesses {
    inner: Arc<Mutex<HashMap<String, Entry>>>,
}

impl Default for ReauthProcesses {
    fn default() -> Self {
        Self::new()
    }
}

impl ReauthProcesses {
    #[must_use]
    pub fn new() -> Self {
        Self { inner: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// Starts `resource`'s re-auth per its pattern. Never blocks on the
    /// eventual URL/instructions reaching the phone — that always happens
    /// asynchronously via `agent_event_tx`, per
    /// `RpcRequest::ResourceReauthStart`'s own doc comment ("The actual
    /// url/code/instructions arrive asynchronously... not in this RPC's own
    /// response").
    ///
    /// # Errors
    ///
    /// See [`ReauthError`]'s variants.
    ///
    /// # Panics
    ///
    /// Never in practice: the internal `expect` on the spawned child's
    /// `stdout` can only fail if `spawn_piped` ever stopped requesting
    /// `Stdio::piped()` for stdout, which it always does.
    pub async fn start(&self, resource: &Resource, agent_event_tx: tokio::sync::mpsc::Sender<WireAgentEvent>) -> Result<(), ReauthError> {
        let pattern = resource.pattern.ok_or(ReauthError::NotApplicable)?;
        match pattern {
            WireResourcePattern::A => Err(ReauthError::NotApplicable),
            WireResourcePattern::C => {
                let event = build_event(resource, None, None, resource.fetch_instructions.clone());
                let _ = agent_event_tx.send(event).await;
                Ok(())
            }
            WireResourcePattern::B | WireResourcePattern::D => {
                // A cheap up-front check so the overwhelmingly common case
                // (no concurrent start in flight) doesn't pay for spawning
                // a process it's just going to reject — the real
                // correctness guarantee is the `Entry` match below, not
                // this check (see its own comment).
                if self.inner.lock().await.contains_key(&resource.resource_id) {
                    return Err(ReauthError::AlreadyInProgress);
                }
                let command = resource.reauth_command.as_deref().ok_or(ReauthError::MissingCommand)?;
                let mut child = spawn_piped(command).map_err(ReauthError::Spawn)?;
                let stdout = child.stdout.take().expect("stdout is always piped by spawn_piped");
                let resource_id = resource.resource_id.clone();
                {
                    let mut map = self.inner.lock().await;
                    match map.entry(resource_id.clone()) {
                        std::collections::hash_map::Entry::Occupied(_) => {
                            // Lost a race against a concurrent `start` for
                            // the same resource_id between the check above
                            // and this insert — kill the child just spawned
                            // rather than silently overwriting the map
                            // entry. Overwriting would be worse than just a
                            // wasted spawn: each `start` also schedules its
                            // own idle-timeout cleanup task keyed on
                            // `resource_id` alone (see below), so a second
                            // `start` racing in and replacing the entry
                            // would leave the *first* call's cleanup task
                            // free to remove/kill whatever now sits under
                            // that key once its own timer fires — including
                            // the second, still-legitimately-in-progress
                            // attempt. Rejecting the loser outright keeps
                            // "one resource_id has at most one live entry,
                            // and its cleanup task is the only one that can
                            // ever remove it" true.
                            drop(map);
                            let _ = child.start_kill();
                            return Err(ReauthError::AlreadyInProgress);
                        }
                        std::collections::hash_map::Entry::Vacant(slot) => {
                            slot.insert(Entry { child });
                        }
                    }
                }

                let watcher_resource = resource.clone();
                let watcher_map = self.inner.clone();
                tokio::spawn(async move {
                    watch_for_url(watcher_resource, stdout, agent_event_tx).await;
                    // Idle cleanup: if `complete` never arrives, this
                    // Resource's slot (and its child) must not live
                    // forever.
                    tokio::time::sleep(REAUTH_IDLE_TIMEOUT).await;
                    if let Some(mut entry) = watcher_map.lock().await.remove(&resource_id) {
                        let _ = entry.child.start_kill();
                        tracing::warn!(resource_id, "reauth subprocess timed out waiting for resource.reauth_complete; killed");
                    }
                });
                Ok(())
            }
        }
    }

    /// Resolves an in-progress (or, for pattern c, deferred) re-auth with
    /// the phone-supplied `value`. Returns whether the underlying command
    /// exited successfully within [`REAUTH_COMPLETE_WAIT`].
    ///
    /// # Errors
    ///
    /// See [`ReauthError`]'s variants — notably
    /// [`ReauthError::NoActiveReauth`] for pattern b if `start` was never
    /// called (or already timed out/completed) for this `resource_id`.
    pub async fn complete(&self, resource: &Resource, value: &str) -> Result<bool, ReauthError> {
        let pattern = resource.pattern.ok_or(ReauthError::NotApplicable)?;
        match pattern {
            WireResourcePattern::A => Err(ReauthError::NotApplicable),
            WireResourcePattern::B => {
                let mut entry = self.inner.lock().await.remove(&resource.resource_id).ok_or(ReauthError::NoActiveReauth)?;
                Ok(write_value_and_await_exit(&mut entry.child, value).await)
            }
            WireResourcePattern::C => {
                let command = resource.reauth_command.as_deref().ok_or(ReauthError::MissingCommand)?;
                let mut child = spawn_piped(command).map_err(ReauthError::Spawn)?;
                Ok(write_value_and_await_exit(&mut child, value).await)
            }
            WireResourcePattern::D => {
                // The original URL-printing watcher process (if `start`
                // spawned one and it's still around) is no longer
                // authoritative for this attempt — the resume command is.
                if let Some(mut entry) = self.inner.lock().await.remove(&resource.resource_id) {
                    let _ = entry.child.start_kill();
                }
                let template = resource.resume_command_template.as_deref().ok_or(ReauthError::MissingCommand)?;
                let command = template.replace("{code}", value);
                // `stdin(Stdio::null())`, not piped: the phone-supplied
                // value is already embedded in `command` via `{code}`, not
                // written to this process's stdin — reusing
                // `write_value_and_await_exit` here is still correct (it
                // only writes when `child.stdin` is `Some`, which it never
                // is for a `Stdio::null()` child) and, per
                // `REAUTH_COMPLETE_WAIT`'s own doc comment ("spawning a
                // fresh resume command (pattern d)"), is what actually
                // bounds this wait — a plain `.status().await` here would
                // let a hung resume command block this RPC indefinitely.
                let mut child = Command::new("sh")
                    .arg("-c")
                    .arg(&command)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(ReauthError::Spawn)?;
                Ok(write_value_and_await_exit(&mut child, value).await)
            }
        }
    }

    /// Test/diagnostic-only: whether a pattern-b/d subprocess is currently
    /// tracked for `resource_id`.
    #[cfg(test)]
    async fn has_entry(&self, resource_id: &str) -> bool {
        self.inner.lock().await.contains_key(resource_id)
    }
}

fn spawn_piped(command: &str) -> std::io::Result<Child> {
    Command::new("sh").arg("-c").arg(command).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn()
}

fn build_event(resource: &Resource, verification_uri: Option<String>, user_code: Option<String>, fetch_instructions: Option<String>) -> WireAgentEvent {
    WireAgentEvent::ResourceReauthRequired {
        resource_id: resource.resource_id.clone(),
        display_name: resource.display_name.clone(),
        resource_kind: resource.resource_kind.clone(),
        pattern: resource.pattern.expect("build_event is only ever called for a reauth-pattern resource"),
        verification_uri,
        user_code,
        fetch_instructions,
        mobile_profile: resource.mobile_profile,
    }
}

/// Reads `stdout` incrementally, looking for the first `https://` URL —
/// reusing `auth_detect.rs`'s own ANSI-stripping and bounds-checked URL
/// extraction rather than re-implementing either. Emits
/// `ResourceReauthRequired` (via `agent_event_tx`) the moment one is found,
/// then returns; gives up (logging a warning, emitting nothing) if
/// [`REAUTH_URL_WAIT_TIMEOUT`] elapses or the process closes its stdout
/// first.
async fn watch_for_url(resource: Resource, mut stdout: tokio::process::ChildStdout, agent_event_tx: tokio::sync::mpsc::Sender<WireAgentEvent>) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + REAUTH_URL_WAIT_TIMEOUT;
    loop {
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()).filter(|d| !d.is_zero()) else {
            tracing::warn!(resource_id = %resource.resource_id, "timed out waiting for a verification URL from the resource's reauth_command output");
            return;
        };
        match tokio::time::timeout(remaining, stdout.read(&mut buf)).await {
            Ok(Ok(0)) => {
                tracing::warn!(resource_id = %resource.resource_id, "reauth subprocess closed stdout before printing a verification URL");
                return;
            }
            Ok(Ok(n)) => {
                raw.extend_from_slice(&buf[..n]);
                let decoded = String::from_utf8_lossy(&raw);
                let cleaned = strip_ansi_and_normalize(&decoded);
                if let Some(url) = first_https_url(&cleaned) {
                    let event = build_event(&resource, Some(url.to_string()), None, None);
                    let _ = agent_event_tx.send(event).await;
                    return;
                }
            }
            Ok(Err(_)) => return,
            Err(_) => {
                tracing::warn!(resource_id = %resource.resource_id, "timed out waiting for a verification URL from the resource's reauth_command output");
                return;
            }
        }
    }
}

/// Writes `value` (newline-terminated) to `child`'s stdin, then waits up to
/// [`REAUTH_COMPLETE_WAIT`] for it to exit — `verified` is `true` only if
/// the process actually exited with a success status inside that window;
/// a still-running or killed-on-timeout process reports `false` rather than
/// optimistically assuming success.
async fn write_value_and_await_exit(child: &mut Child, value: &str) -> bool {
    if let Some(stdin) = child.stdin.as_mut() {
        let payload = format!("{value}\n");
        if stdin.write_all(payload.as_bytes()).await.is_err() {
            return false;
        }
        let _ = stdin.flush().await;
    }
    match tokio::time::timeout(REAUTH_COMPLETE_WAIT, child.wait()).await {
        Ok(Ok(status)) => status.success(),
        Ok(Err(_)) => false,
        Err(_) => {
            let _ = child.start_kill();
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_protocol::relay::WireMobileProfile;

    fn resource(pattern: WireResourcePattern, reauth_command: Option<&str>, resume_command_template: Option<&str>, fetch_instructions: Option<&str>) -> Resource {
        Resource {
            resource_id: "res-test".to_string(),
            display_name: "Test Resource".to_string(),
            resource_kind: "custom".to_string(),
            pattern: Some(pattern),
            mobile_profile: WireMobileProfile::Ask,
            created_by: "operator".to_string(),
            last_used_at: None,
            last_verified_at: None,
            reauth_command: reauth_command.map(str::to_string),
            resume_command_template: resume_command_template.map(str::to_string),
            fetch_instructions: fetch_instructions.map(str::to_string),
            detect_kind: None,
        }
    }

    /// Pattern b, end to end against a real, trivial shell subprocess (not
    /// a mock): prints a fake URL, blocks reading a line from stdin, then
    /// exits 0 only if that line is the expected value — proving `start`
    /// really detects the URL from a real child's real stdout and
    /// `complete` really writes into that same still-open child's stdin.
    #[tokio::test]
    async fn pattern_b_detects_a_real_subprocess_url_and_completes_via_its_stdin() {
        let script = "echo 'Go to https://example.com/device to continue'; read -r code; if [ \"$code\" = \"SECRET-1\" ]; then exit 0; else exit 1; fi";
        let res = resource(WireResourcePattern::B, Some(script), None, None);
        let processes = ReauthProcesses::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);

        processes.start(&res, tx).await.unwrap();
        assert!(processes.has_entry(&res.resource_id).await, "pattern b must keep the child alive after start");

        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().expect("must receive a ResourceReauthRequired event");
        let WireAgentEvent::ResourceReauthRequired { verification_uri, user_code, pattern, .. } = event else {
            panic!("expected ResourceReauthRequired");
        };
        assert_eq!(verification_uri.as_deref(), Some("https://example.com/device"));
        assert_eq!(user_code, None, "pattern b never has a short code to report — see this module's doc comment");
        assert_eq!(pattern, WireResourcePattern::B);

        let verified = processes.complete(&res, "SECRET-1").await.unwrap();
        assert!(verified, "the real subprocess must report success once fed the expected value on stdin");
        assert!(!processes.has_entry(&res.resource_id).await, "complete must remove the entry");
    }

    #[tokio::test]
    async fn pattern_b_reports_unverified_on_a_wrong_value() {
        let script = "echo 'https://example.com/device'; read -r code; if [ \"$code\" = \"RIGHT\" ]; then exit 0; else exit 1; fi";
        let res = resource(WireResourcePattern::B, Some(script), None, None);
        let processes = ReauthProcesses::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        processes.start(&res, tx).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();

        let verified = processes.complete(&res, "WRONG").await.unwrap();
        assert!(!verified);
    }

    #[tokio::test]
    async fn pattern_c_emits_immediately_with_no_subprocess_and_completes_by_spawning_fresh() {
        let script = "read -r code; if [ \"$code\" = \"MY-SECRET\" ]; then exit 0; else exit 1; fi";
        let res = resource(WireResourcePattern::C, Some(script), None, Some("Fetch your secret from https://example.com/console"));
        let processes = ReauthProcesses::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);

        processes.start(&res, tx).await.unwrap();
        assert!(!processes.has_entry(&res.resource_id).await, "pattern c must not keep any subprocess alive after start");
        let event = rx.try_recv().expect("pattern c must emit immediately, synchronously with start");
        let WireAgentEvent::ResourceReauthRequired { fetch_instructions, verification_uri, user_code, .. } = event else {
            panic!("expected ResourceReauthRequired");
        };
        assert_eq!(fetch_instructions.as_deref(), Some("Fetch your secret from https://example.com/console"));
        assert_eq!(verification_uri, None);
        assert_eq!(user_code, None);

        let verified = processes.complete(&res, "MY-SECRET").await.unwrap();
        assert!(verified, "complete must spawn reauth_command fresh and feed it the value");
    }

    #[tokio::test]
    async fn pattern_d_detects_a_url_and_completes_via_a_fresh_resume_command() {
        let watcher_script = "echo 'https://example.com/device'; sleep 30";
        let marker = std::env::temp_dir().join(format!("choosh-reauth-d-test-{}", uuid::Uuid::new_v4()));
        let resume_template = format!("touch {} && exit 0", marker.display());
        let res = resource(WireResourcePattern::D, Some(watcher_script), Some(&resume_template), None);
        let processes = ReauthProcesses::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);

        processes.start(&res, tx).await.unwrap();
        assert!(processes.has_entry(&res.resource_id).await);
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        let WireAgentEvent::ResourceReauthRequired { verification_uri, user_code, .. } = event else {
            panic!("expected ResourceReauthRequired");
        };
        assert_eq!(verification_uri.as_deref(), Some("https://example.com/device"));
        assert_eq!(user_code, None);

        let verified = processes.complete(&res, "irrelevant-value").await.unwrap();
        assert!(verified, "the resume command's own exit code must drive `verified`");
        assert!(marker.exists(), "complete must have actually spawned the resume_command_template with {{code}} substituted and run it");
        assert!(!processes.has_entry(&res.resource_id).await, "the original watcher entry must be removed on complete");
        let _ = std::fs::remove_file(&marker);
    }

    /// Regression test for a real bug found in review: pattern d's
    /// `complete` used to `.status().await` its freshly-spawned resume
    /// command with no timeout at all, unlike pattern b/c's
    /// `write_value_and_await_exit` — a hung `resume_command_template`
    /// (a real, plausible failure mode: the resumed CLI itself hangs on a
    /// network call) would have blocked the `resource.reauth_complete` RPC
    /// handler indefinitely. `REAUTH_COMPLETE_WAIT`'s own doc comment
    /// already claimed to bound "spawning a fresh resume command (pattern
    /// d)" — this proves that's actually true now, not just documented.
    #[tokio::test]
    async fn pattern_d_resume_command_that_hangs_is_bounded_and_reports_unverified() {
        let watcher_script = "echo 'https://example.com/device'; sleep 30";
        let resume_template = "sleep 30 && exit 0".to_string();
        let res = resource(WireResourcePattern::D, Some(watcher_script), Some(&resume_template), None);
        let processes = ReauthProcesses::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);

        processes.start(&res, tx).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();

        let started = tokio::time::Instant::now();
        let verified = processes.complete(&res, "irrelevant-value").await.unwrap();
        assert!(!verified, "a resume command that never exits within REAUTH_COMPLETE_WAIT must not be reported as verified");
        assert!(
            started.elapsed() < REAUTH_COMPLETE_WAIT + Duration::from_secs(5),
            "a hung resume command must be bounded by REAUTH_COMPLETE_WAIT, not block indefinitely (elapsed: {:?})",
            started.elapsed()
        );
    }

    /// Regression test for a real race found in review: `start` used to
    /// unconditionally `insert` into the shared map, so a second
    /// `resource.reauth_start` for the same `resource_id` while one was
    /// already in flight (a double-tap, a client retry after a slow
    /// response) would silently replace the first entry — and because each
    /// `start` also schedules its own idle-timeout cleanup task keyed on
    /// nothing but `resource_id`, the *first* call's cleanup, once its
    /// timer fired, could then reap whichever entry currently sat under
    /// that key, including the second, still-legitimate attempt. A second
    /// `start` must now be rejected outright, and must not disturb the
    /// first attempt at all.
    #[tokio::test]
    async fn a_second_concurrent_start_for_the_same_resource_id_is_rejected_and_does_not_disturb_the_first() {
        let script = "echo 'https://example.com/device'; read -r code; if [ \"$code\" = \"SECRET-1\" ]; then exit 0; else exit 1; fi";
        let res = resource(WireResourcePattern::B, Some(script), None, None);
        let processes = ReauthProcesses::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);

        processes.start(&res, tx.clone()).await.unwrap();
        assert!(processes.has_entry(&res.resource_id).await);
        tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().expect("first start must emit ResourceReauthRequired");

        let second = processes.start(&res, tx).await;
        assert!(matches!(second, Err(ReauthError::AlreadyInProgress)), "a second start for an in-progress resource must be rejected, got {second:?}");

        // The first attempt must still be the live one, and still
        // completable — the rejected second call must not have touched it.
        assert!(processes.has_entry(&res.resource_id).await, "the first attempt's entry must survive a rejected concurrent start");
        let verified = processes.complete(&res, "SECRET-1").await.unwrap();
        assert!(verified, "the first, legitimate in-progress attempt must still be completable after a rejected concurrent start");
    }

    #[tokio::test]
    async fn pattern_a_is_never_startable_or_completable_here() {
        let res = resource(WireResourcePattern::A, Some("echo hi"), None, None);
        let processes = ReauthProcesses::new();
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        assert!(matches!(processes.start(&res, tx).await, Err(ReauthError::NotApplicable)));
        assert!(matches!(processes.complete(&res, "x").await, Err(ReauthError::NotApplicable)));
    }

    #[tokio::test]
    async fn completing_pattern_b_with_no_prior_start_is_an_error() {
        let res = resource(WireResourcePattern::B, Some("cat"), None, None);
        let processes = ReauthProcesses::new();
        assert!(matches!(processes.complete(&res, "x").await, Err(ReauthError::NoActiveReauth)));
    }
}
