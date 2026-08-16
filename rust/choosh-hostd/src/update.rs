//! `choosh-hostd` self-update, per `docs/specs/host-deployment.md`'s
//! "Self-update" section and its "Rollback" subsection, triggered by
//! `relayd`'s `ServerPush::UpdateBinary` push
//! (`docs/specs/relay-protocol.md`'s `update_binary`).
//!
//! ## Procedure
//!
//! [`handle_update_binary_push`] runs entirely inside the *currently
//! running* (known-good) process, per host-deployment.md's ordered steps:
//! download to a sibling `<binary>.new` path, verify its SHA-256 digest
//! before proceeding (a mismatch is rejected outright — the running
//! binary is never touched), `chmod +x`, back up the currently-running
//! binary to a sibling `<binary>.previous`, and atomically `rename()` the
//! new binary over the running one's own path. It then spawns the
//! post-restart [`run_monitor`] (see below) as a detached child *before*
//! asking the service manager to restart the unit — this process is the
//! one about to be killed by that very restart, so anything that needs to
//! observe what happens next must already be running independently by
//! the time it's triggered.
//!
//! ## Why a separate monitor process, and why it survives the restart
//!
//! `systemctl --user restart`/`launchctl kickstart -k` is asked for, not
//! a self-`exec`, per host-deployment.md — the service manager's own
//! supervision stays authoritative for process lifecycle. That restart
//! kills this process (the unit's tracked main PID) as part of its stop
//! phase, so no in-process logic here can observe what happens
//! afterward — including whether the new binary ever becomes healthy at
//! all (it might not even be a valid `choosh-hostd` binary). A separate,
//! already-running child process is the only thing that can watch across
//! that boundary.
//!
//! For that child to survive the restart, the unit's `KillMode` matters:
//! confirmed by direct experiment (a real `systemd --user` unit, a real
//! `zellij attach --create-background` session, a real long-running
//! process inside it) that the *default* `KillMode=control-group` SIGKILLs
//! every process in the unit's cgroup on `systemctl restart` — including
//! Zellij's own server and everything attached to it, regardless of
//! reparenting. `scripts/install.sh` sets `KillMode=process` (Linux) /
//! `AbandonProcessGroup` (macOS) specifically so only the unit's tracked
//! main PID is signaled — confirmed by the same kind of experiment to
//! leave a spawned child (no `setsid`/double-fork needed) running
//! straight through the restart. This is the same property
//! host-deployment.md's Self-update section requires for Zellij sessions,
//! not a separate mechanism.
//!
//! The monitor is invoked as `<previous-binary> update-monitor ...`
//! (never `<current-binary>`, which may be the untested new binary) so
//! it's guaranteed to be running known-good code even when the pushed
//! binary is completely broken.
//!
//! ## Rollback bound
//!
//! Per host-deployment.md's Rollback subsection ("a binary that
//! repeatedly fails health-check after rollback MUST stop retrying"):
//! [`run_monitor`] performs **at most one** rollback-and-restart cycle per
//! pushed update. The previous binary was, by definition, healthy and
//! running immediately before this update was applied — if swapping back
//! to it and restarting *also* fails its own health check, the cause is
//! very unlikely to be that binary itself (it worked a moment ago) and
//! much more likely something environmental (disk full, a permissions
//! change, the local socket path itself now unwritable) that a second or
//! third identical rollback attempt would not fix. So this stops after
//! exactly one rollback attempt, records the outcome, and leaves the host
//! in whatever state that attempt reached, rather than looping forever.
//!
//! The health check itself is [`crate::local_ipc::probe`] against the
//! same local IPC socket `serve` already binds early in its own startup
//! ([`crate::local_ipc::default_socket_path`]) — "the local RPC socket
//! `choosh-hostd` already exposes" per this task's brief, not a new
//! listener invented for this purpose.
//!
//! ## Reporting failure upstream
//!
//! Neither this process (about to die) nor the monitor (no relay
//! credential/connection of its own) can reliably deliver an
//! `agent-event` control frame the moment a failure is detected. Instead,
//! the monitor persists a pending failure report to
//! [`default_state_path`]'s JSON file, and [`crate::serve::run`] checks
//! for one on every startup (via [`take_pending_failure_event`]) — once
//! *some* `choosh-hostd` process (old or rolled-back) re-establishes its
//! relayd connection, it drains and sends the queued
//! `WireAgentEvent::AgentStatus` the same way every other locally-emitted
//! agent event already flows (`serve.rs`'s `agent_event_tx`), rather than
//! this module inventing a second delivery path.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use choosh_protocol::relay::{WireAgentEvent, WireAgentStatus};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::local_ipc;

/// How long [`run_monitor`] waits for the restarted binary to prove
/// itself healthy (accept a connection on its local IPC socket) before
/// concluding it failed. `serve::run` binds that socket as one of its
/// very first actions — a working binary reaches that point in well
/// under a second in this environment; 15s leaves generous headroom for a
/// slow disk/cold-cache first start without leaving a genuinely broken
/// binary unnoticed for long, given this window sits on the critical path
/// of restoring service.
pub const HEALTH_CHECK_WINDOW: Duration = Duration::from_secs(15);

/// Polling cadence within [`HEALTH_CHECK_WINDOW`]: frequent enough that
/// the monitor doesn't waste much of its own budget waiting between
/// checks, infrequent enough not to spin.
const HEALTH_CHECK_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Grace period after asking the service manager to restart, before the
/// first health-check attempt — covers the ordinary stop-then-start
/// latency of `systemctl --user restart`/`launchctl kickstart -k`
/// themselves, so the first probe isn't spent against a unit still
/// mid-stop.
const RESTART_SETTLE_DELAY: Duration = Duration::from_secs(1);

/// [`HEALTH_CHECK_WINDOW`], overridable via `CHOOSH_HOSTD_UPDATE_HEALTH_WINDOW_MS`
/// — exists solely so integration tests can exercise a real rollback cycle
/// in well under a second instead of the production ~30s worst case
/// (two health-check windows plus a settle delay), without touching the
/// production default itself.
fn health_check_window() -> Duration {
    duration_override("CHOOSH_HOSTD_UPDATE_HEALTH_WINDOW_MS", HEALTH_CHECK_WINDOW)
}

/// [`RESTART_SETTLE_DELAY`], overridable via `CHOOSH_HOSTD_UPDATE_SETTLE_MS`
/// for the same reason as [`health_check_window`].
fn restart_settle_delay() -> Duration {
    duration_override("CHOOSH_HOSTD_UPDATE_SETTLE_MS", RESTART_SETTLE_DELAY)
}

/// [`HEALTH_CHECK_POLL_INTERVAL`], overridable via
/// `CHOOSH_HOSTD_UPDATE_POLL_MS` — without this, a shrunk
/// [`health_check_window`] shorter than the fixed 500ms poll interval
/// would still take at least one full poll interval to resolve (the
/// deadline is only checked *between* probes), making a test's window
/// override far less precise than intended.
fn health_check_poll_interval() -> Duration {
    duration_override("CHOOSH_HOSTD_UPDATE_POLL_MS", HEALTH_CHECK_POLL_INTERVAL)
}

fn duration_override(env_var: &str, default: Duration) -> Duration {
    std::env::var(env_var).ok().and_then(|value| value.parse::<u64>().ok()).map_or(default, Duration::from_millis)
}

/// Sentinel `workspace_id`/`item_id` for a self-update failure report over
/// the existing `agent-event` channel (`WireAgentEvent::AgentStatus`).
/// This failure is host-level, not attributable to any workspace item —
/// but reusing this exact wire shape rather than inventing a new push
/// type is what host-deployment.md's Rollback subsection and M8's scope
/// bullet ask for ("report an `agent_status`-shaped failure event
/// upstream").
pub const SELF_UPDATE_WORKSPACE_ID: &str = "host";
pub const SELF_UPDATE_ITEM_ID: &str = "self-update";

#[derive(Debug)]
pub enum UpdateError {
    Download(reqwest::Error),
    Io(io::Error),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Download(error) => write!(f, "self-update download failed: {error}"),
            Self::Io(error) => write!(f, "self-update I/O error: {error}"),
        }
    }
}

impl std::error::Error for UpdateError {}

impl From<io::Error> for UpdateError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Persisted across the restart boundary this whole module exists to
/// survive — in-memory state does not, since every rollback path involves
/// at least one full process restart.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct UpdateState {
    /// The most recently applied push's `push_id`, so a redelivery after
    /// reconnect (relay-protocol.md's stated reason `update_binary`
    /// carries one) is recognized and skipped rather than re-applied.
    last_handled_push_id: Option<String>,
    /// Set by [`run_monitor`] when a pushed update fails its health check
    /// (with or without a successful rollback); drained by
    /// [`take_pending_failure_event`] the next time any `choosh-hostd`
    /// process starts up with a live relayd connection.
    #[serde(default)]
    pending_failure: Option<FailureReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FailureReport {
    version: String,
    reason: String,
}

/// `<data dir>/choosh-hostd/update_state.json` — the on-disk record of
/// the last handled push and any pending failure to report, per this
/// module's doc comment.
///
/// # Errors
///
/// Returns an error if no config/data directory can be determined for the
/// current user.
pub fn default_state_path() -> Result<PathBuf, UpdateError> {
    let dirs = directories::ProjectDirs::from("ai", "choosh", "hostd")
        .ok_or_else(|| UpdateError::Io(io::Error::other("no data directory available for choosh-hostd")))?;
    Ok(dirs.data_dir().join("update_state.json"))
}

fn load_state(path: &Path) -> UpdateState {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => UpdateState::default(),
    }
}

fn save_state(path: &Path, state: &UpdateState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(state).unwrap_or_default();
    crate::fs_util::atomic_write(path, &json, None)
}

/// Drains and returns any pending self-update failure report at
/// `state_path`, clearing it in the same call — called once from
/// `serve::run`'s startup, so a report is delivered exactly once (per
/// successful drain), not re-sent on every subsequent restart.
#[must_use]
pub fn take_pending_failure_event(state_path: &Path) -> Option<WireAgentEvent> {
    let mut state = load_state(state_path);
    let report = state.pending_failure.take()?;
    if let Err(error) = save_state(state_path, &state) {
        tracing::warn!(%error, "failed to clear pending self-update failure report; it may be re-reported next start");
    }
    tracing::warn!(version = %report.version, reason = %report.reason, "reporting a prior self-update failure upstream");
    Some(WireAgentEvent::AgentStatus {
        workspace_id: SELF_UPDATE_WORKSPACE_ID.to_string(),
        item_id: SELF_UPDATE_ITEM_ID.to_string(),
        status: WireAgentStatus::Failed,
    })
}

fn mark_failed(state_path: &Path, version: &str, reason: &str) {
    let mut state = load_state(state_path);
    state.pending_failure = Some(FailureReport { version: version.to_string(), reason: reason.to_string() });
    if let Err(error) = save_state(state_path, &state) {
        tracing::error!(%error, "failed to persist self-update failure report");
    }
}

fn sibling_path(exe: &Path, suffix: &str) -> PathBuf {
    let file_name = exe.file_name().and_then(|name| name.to_str()).unwrap_or("choosh-hostd");
    exe.with_file_name(format!("{file_name}{suffix}"))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Step 1 (part): downloads `url`'s full body into memory. Bounded by
/// nothing beyond `reqwest`'s own defaults — a `choosh-hostd` binary is a
/// few tens of MB at most, and this runs once per pushed update, not on
/// any hot path.
async fn download(http: &reqwest::Client, url: &str) -> Result<Vec<u8>, UpdateError> {
    let response = http.get(url).send().await.map_err(UpdateError::Download)?;
    let response = response.error_for_status().map_err(UpdateError::Download)?;
    let bytes = response.bytes().await.map_err(UpdateError::Download)?;
    Ok(bytes.to_vec())
}

/// Step 1 (verify) + step 2 (`chmod +x`): writes `bytes` to `path` and
/// makes it executable. Callers MUST have already verified `bytes`'
/// digest — this function does not check it again.
fn stage_new_binary(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(path, bytes)?;
    set_executable(path)
}

/// Backs up the currently-running binary to `previous_path`, populated at
/// exactly the moment a new binary is about to replace it — per this
/// task's requirement, never overwritten again until a *subsequent*
/// successful update is itself about to swap, and never lost in between
/// (a failed/rejected update — digest mismatch, staging failure — returns
/// before this is ever called, so a stale `.previous` from an earlier
/// successful update is left untouched on that path, still valid).
fn backup_current(current_exe: &Path, previous_path: &Path) -> io::Result<()> {
    std::fs::copy(current_exe, previous_path)?;
    set_executable(previous_path)
}

/// Step 3: the atomic swap. A single `std::fs::rename` call on two paths
/// in the same directory (hence the same filesystem) — there is no window
/// where `current_exe`'s path is missing or holds partial content; the
/// directory entry flips from the old inode to the new one in one
/// filesystem operation. A process already running the old binary (this
/// one, until the restart below) keeps executing its already-open/mapped
/// old inode unaffected — this is what makes "rename over your own
/// running binary" safe at all.
fn swap_in(new_path: &Path, current_exe: &Path) -> io::Result<()> {
    std::fs::rename(new_path, current_exe)
}

/// Step 4: asks the service manager to restart the unit — never a
/// self-`exec`, per host-deployment.md, so `Restart=on-failure`/
/// `KeepAlive` stays authoritative for process lifecycle. Unit/label names
/// default to exactly what `scripts/install.sh` writes
/// (`choosh-hostd.service` / `ai.choosh.hostd`), overridable via env vars
/// so tests can target a throwaway unit instead of colliding with a real
/// one on the same machine.
fn restart_service() -> io::Result<()> {
    match std::env::consts::OS {
        "linux" => {
            let unit = std::env::var("CHOOSH_HOSTD_SERVICE_UNIT").unwrap_or_else(|_| "choosh-hostd.service".to_string());
            let status = std::process::Command::new("systemctl").args(["--user", "restart", &unit]).status()?;
            if !status.success() {
                tracing::warn!(?status, unit, "systemctl --user restart did not report success");
            }
        }
        "macos" => {
            let label = std::env::var("CHOOSH_HOSTD_LAUNCHD_LABEL").unwrap_or_else(|_| "ai.choosh.hostd".to_string());
            let uid = nix::unistd::getuid();
            let target = format!("gui/{uid}/{label}");
            let status = std::process::Command::new("launchctl").args(["kickstart", "-k", &target]).status()?;
            if !status.success() {
                tracing::warn!(?status, target, "launchctl kickstart did not report success");
            }
        }
        other => {
            tracing::warn!(os = other, "no service-manager restart mechanism for this OS; binary was swapped but not activated");
        }
    }
    Ok(())
}

/// Spawns the post-restart rollback monitor, invoked as `<previous>
/// update-monitor ...` — deliberately the *known-good* previous binary,
/// not `current_exe` (which is, at this point, the untested new binary),
/// per this module's doc comment. A plain spawn with no `setsid`/
/// double-fork: `scripts/install.sh`'s `KillMode=process`/
/// `AbandonProcessGroup` is what lets this survive the restart triggered
/// right after this call, not process-group tricks here.
fn spawn_rollback_monitor(
    previous: &Path,
    current: &Path,
    socket: &Path,
    state_path: &Path,
    version: &str,
) -> io::Result<std::process::Child> {
    std::process::Command::new(previous)
        .arg("update-monitor")
        .arg("--previous")
        .arg(previous)
        .arg("--current")
        .arg(current)
        .arg("--socket")
        .arg(socket)
        .arg("--state-file")
        .arg(state_path)
        .arg("--version")
        .arg(version)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
}

async fn report_failure(tx: &mpsc::Sender<WireAgentEvent>, version: &str, reason: &str) {
    tracing::error!(version, reason, "self-update failed; reporting upstream");
    let event = WireAgentEvent::AgentStatus {
        workspace_id: SELF_UPDATE_WORKSPACE_ID.to_string(),
        item_id: SELF_UPDATE_ITEM_ID.to_string(),
        status: WireAgentStatus::Failed,
    };
    let _ = tx.send(event).await;
}

/// Entry point invoked from `serve.rs`'s `ServerPush::UpdateBinary`
/// handler, spawned as its own task (never inline in the dispatch loop —
/// it performs a network download and, on success, a restart that kills
/// this whole process). Resolves the running binary's own path and this
/// module's default state/socket paths, then delegates to
/// [`handle_update_binary_push_with_paths`] — split out so tests can drive
/// the exact same logic against a throwaway file instead of this process's
/// own (test-harness) binary, which `swap_in` would otherwise corrupt.
pub async fn handle_update_binary_push(
    push_id: String,
    download_url: String,
    sha256_expected: String,
    version: String,
    agent_event_tx: mpsc::Sender<WireAgentEvent>,
) {
    let Ok(current_exe) = std::env::current_exe() else {
        tracing::error!("cannot self-update: failed to resolve the running binary's own path");
        return;
    };
    let Ok(state_path) = default_state_path() else {
        tracing::error!("cannot self-update: no data directory available to track update state");
        return;
    };
    let socket_path = local_ipc::default_socket_path().ok();
    handle_update_binary_push_with_paths(
        current_exe,
        socket_path,
        state_path,
        push_id,
        download_url,
        sha256_expected,
        version,
        agent_event_tx,
    )
    .await;
}

/// Runs host-deployment.md's Self-update steps 1-4 in order against an
/// explicit `current_exe`/`socket_path`/`state_path` rather than this
/// process's own defaults — the testable core [`handle_update_binary_push`]
/// delegates to. Returns after step 4 (triggering the service-manager
/// restart); nothing further happens in this process afterward (it is
/// likely killed by the restart it just triggered).
///
/// # Errors
///
/// Never returns an error directly — every failure is reported via
/// `agent_event_tx` (a `WireAgentEvent::AgentStatus` failure event) and
/// logged, per this module's "report, don't crash" posture.
#[allow(clippy::too_many_arguments)] // every argument is a distinct, independently-overridden path/value a test needs to control; a struct would just move the count, not reduce it, for this one production call site plus its test callers.
pub async fn handle_update_binary_push_with_paths(
    current_exe: PathBuf,
    socket_path: Option<PathBuf>,
    state_path: PathBuf,
    push_id: String,
    download_url: String,
    sha256_expected: String,
    version: String,
    agent_event_tx: mpsc::Sender<WireAgentEvent>,
) {
    let mut state = load_state(&state_path);
    if state.last_handled_push_id.as_deref() == Some(push_id.as_str()) {
        tracing::info!(push_id, "ignoring redelivered update_binary push (already handled)");
        return;
    }

    let http = reqwest::Client::new();
    let bytes = match download(&http, &download_url).await {
        Ok(bytes) => bytes,
        Err(error) => {
            report_failure(&agent_event_tx, &version, &format!("download failed: {error}")).await;
            return;
        }
    };

    let digest = crate::fs_ops::content_revision(&bytes);
    if !digest.eq_ignore_ascii_case(&sha256_expected) {
        tracing::error!(expected = %sha256_expected, actual = %digest, "self-update digest mismatch; rejecting without applying");
        report_failure(&agent_event_tx, &version, "sha256 digest mismatch").await;
        return;
    }

    let new_path = sibling_path(&current_exe, ".new");
    let previous_path = sibling_path(&current_exe, ".previous");

    if let Err(error) = stage_new_binary(&new_path, &bytes) {
        report_failure(&agent_event_tx, &version, &format!("failed to stage new binary: {error}")).await;
        return;
    }

    if let Err(error) = backup_current(&current_exe, &previous_path) {
        let _ = std::fs::remove_file(&new_path);
        report_failure(&agent_event_tx, &version, &format!("failed to back up current binary: {error}")).await;
        return;
    }

    if let Err(error) = swap_in(&new_path, &current_exe) {
        report_failure(&agent_event_tx, &version, &format!("failed to swap in new binary: {error}")).await;
        return;
    }

    state.last_handled_push_id = Some(push_id);
    if let Err(error) = save_state(&state_path, &state) {
        tracing::warn!(%error, "failed to persist update state after a successful swap");
    }

    tracing::info!(version, "self-update swap applied; spawning rollback monitor and restarting the service");

    if let Some(socket_path) = socket_path {
        if let Err(error) = spawn_rollback_monitor(&previous_path, &current_exe, &socket_path, &state_path, &version) {
            tracing::error!(%error, "failed to spawn rollback monitor; restarting without post-restart health verification");
        }
    } else {
        tracing::error!("no local IPC socket path available; rollback monitor not spawned");
    }

    if let Err(error) = restart_service() {
        tracing::error!(%error, "failed to trigger service-manager restart after self-update swap");
    }
    // This process is expected to be killed by the restart above; nothing
    // further to do even if it isn't (e.g. no service manager on this OS).
}

/// Polls [`local_ipc::probe`] until it succeeds or `window` elapses.
async fn wait_for_health(socket: &Path, window: Duration, poll_interval: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + window;
    loop {
        if local_ipc::probe(socket, poll_interval).await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// The `choosh-hostd update-monitor` subcommand's implementation — see
/// this module's doc comment for the full design. Runs as a detached
/// child of the process that just applied a self-update, using the
/// known-good `previous` binary's own code (this function), so it keeps
/// working even when `current` is completely broken.
pub async fn run_monitor(previous: PathBuf, current: PathBuf, socket: PathBuf, state_path: PathBuf, version: String) {
    let window = health_check_window();
    let poll_interval = health_check_poll_interval();
    tokio::time::sleep(restart_settle_delay()).await;

    if wait_for_health(&socket, window, poll_interval).await {
        tracing::info!(version, "self-update health check passed; new binary is healthy");
        return;
    }

    tracing::warn!(version, ?window, "self-update health check failed within the bounded window; rolling back once");

    if let Err(error) = std::fs::rename(&previous, &current) {
        tracing::error!(%error, "rollback rename failed; giving up without retrying");
        mark_failed(&state_path, &version, &format!("health check failed and rollback rename also failed: {error}"));
        return;
    }

    // The failure MUST be recorded before triggering the restart below,
    // not after waiting to see whether that restart's own health check
    // passes: `restart_service()` brings up a fresh `serve::run` process
    // that checks for a pending failure report once, right at its own
    // startup (`serve.rs`'s `take_pending_failure_event` call) — if this
    // write happened after that restart instead, there would be a real
    // race where the newly-started (recovered) process's startup check
    // runs before this marker exists on disk, and the report would be
    // silently missed rather than merely delayed. The update failed the
    // moment its health check failed, regardless of whether the
    // subsequent rollback itself succeeds — this is unconditional.
    mark_failed(&state_path, &version, "self-update failed its health check; rolling back to the previous binary");

    if let Err(error) = restart_service() {
        tracing::error!(%error, "failed to restart service after rollback; giving up without retrying");
        return;
    }

    if wait_for_health(&socket, window, poll_interval).await {
        tracing::info!(version, "rollback restart succeeded; previous binary is healthy again");
    } else {
        tracing::error!(version, "rollback restart ALSO failed its health check; giving up, not retrying further");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_binary_bytes() -> Vec<u8> {
        b"#!/bin/sh\necho fake-choosh-hostd\n".to_vec()
    }

    /// Serves exactly one HTTP GET request with a fixed byte body, then
    /// stops — enough of real HTTP/1.1 for `reqwest::Client::get` to
    /// succeed against it, with no mocking framework: a real TCP listener,
    /// a real response `download` parses through `reqwest`.
    async fn serve_one_download(body: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _addr) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
            let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            tokio::io::AsyncWriteExt::write_all(&mut stream, header.as_bytes()).await.unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut stream, &body).await.unwrap();
            tokio::io::AsyncWriteExt::shutdown(&mut stream).await.unwrap();
        });
        format!("http://{addr}/choosh-hostd-linux-x86_64")
    }

    #[test]
    fn sibling_path_appends_suffix_to_the_file_name_only() {
        let exe = PathBuf::from("/home/user/.local/bin/choosh-hostd");
        assert_eq!(sibling_path(&exe, ".new"), PathBuf::from("/home/user/.local/bin/choosh-hostd.new"));
        assert_eq!(sibling_path(&exe, ".previous"), PathBuf::from("/home/user/.local/bin/choosh-hostd.previous"));
    }

    #[test]
    fn stage_new_binary_writes_content_and_makes_it_executable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("choosh-hostd.new");
        let bytes = fake_binary_bytes();

        stage_new_binary(&path, &bytes).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755, "staged binary must be executable");
        }
    }

    #[test]
    fn backup_current_copies_the_running_binary_and_leaves_the_original_intact() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("choosh-hostd");
        let previous = dir.path().join("choosh-hostd.previous");
        std::fs::write(&current, b"original-binary-bytes").unwrap();

        backup_current(&current, &previous).unwrap();

        assert_eq!(std::fs::read(&previous).unwrap(), b"original-binary-bytes");
        assert_eq!(std::fs::read(&current).unwrap(), b"original-binary-bytes", "backup must not touch the source");
    }

    /// Confirms the swap is genuinely atomic: while a background task
    /// spins reading `current`'s content in a tight loop, another task
    /// performs the swap — every single read the observer ever sees must
    /// be either the full old content or the full new content, never
    /// empty/missing/truncated (which a copy-then-delete implementation
    /// could expose a window for; a single `rename()` on the same
    /// filesystem cannot).
    #[tokio::test]
    async fn swap_in_is_atomic_with_no_observable_missing_or_partial_window() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("choosh-hostd");
        let new_path = dir.path().join("choosh-hostd.new");
        let old_content = vec![b'O'; 4096];
        let new_content = vec![b'N'; 8192];
        std::fs::write(&current, &old_content).unwrap();
        std::fs::write(&new_path, &new_content).unwrap();

        let observer_current = current.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observer_stop = stop.clone();
        let observations = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observer_observations = observations.clone();
        let observer = tokio::task::spawn_blocking(move || {
            while !observer_stop.load(std::sync::atomic::Ordering::Relaxed) {
                match std::fs::read(&observer_current) {
                    Ok(bytes) => {
                        assert!(
                            bytes == vec![b'O'; 4096] || bytes == vec![b'N'; 8192],
                            "observed a partial/unexpected file content: {} bytes",
                            bytes.len()
                        );
                        observer_observations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(error) => panic!("observed the binary path missing mid-swap: {error}"),
                }
            }
        });

        // Wait for the observer to have actually read the file at least
        // once before racing the swap against it — otherwise a fast
        // scheduler could run the swap before `spawn_blocking`'s task
        // even starts, making this test pass without ever exercising the
        // race it's meant to catch.
        while observations.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        swap_in(&new_path, &current).unwrap();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        observer.await.unwrap();
        assert!(observations.load(std::sync::atomic::Ordering::Relaxed) > 0, "observer never got a chance to run");
        assert_eq!(std::fs::read(&current).unwrap(), new_content);
        assert!(!new_path.exists(), "rename must remove the source path");
    }

    /// `run_monitor`'s rollback path, exercised for real (real rename,
    /// real `local_ipc` socket probing) but with the health-check window
    /// shrunk via env override so this runs in well under a second instead
    /// of the production ~15s. `current` never binds a socket (simulating
    /// a broken pushed binary that hangs without ever starting); after the
    /// bounded window it must roll back `previous` onto `current`'s path,
    /// then succeed on the second check because a real listener is bound
    /// at the socket path shortly after (simulating the rolled-back
    /// binary coming back up) — proving one rollback attempt is enough
    /// when the previous binary is genuinely healthy.
    #[tokio::test]
    async fn run_monitor_rolls_back_once_and_succeeds_when_the_previous_binary_comes_up_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("choosh-hostd");
        let previous = dir.path().join("choosh-hostd.previous");
        let socket = dir.path().join("emit.sock");
        let state_path = dir.path().join("update_state.json");
        std::fs::write(&current, b"broken-new-binary-never-binds-anything").unwrap();
        std::fs::write(&previous, b"known-good-previous-binary").unwrap();

        // Simulates the rolled-back binary coming back up shortly after
        // `restart_service()` is called: with window=300ms/poll=50ms/
        // settle=10ms, the first (doomed) health-check window resolves by
        // roughly t=360ms and the second starts immediately after — so
        // binding the socket at t=500ms lands solidly inside the second
        // window, well clear of the first.
        let bind_socket = socket.clone();
        let bind_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let listener = local_ipc::bind(&bind_socket).unwrap();
            let (tx, _rx) = mpsc::channel(1);
            local_ipc::serve_forever(listener, tx).await;
        });

        temp_env::async_with_vars(
            [
                ("CHOOSH_HOSTD_UPDATE_HEALTH_WINDOW_MS", Some("300")),
                ("CHOOSH_HOSTD_UPDATE_POLL_MS", Some("50")),
                ("CHOOSH_HOSTD_UPDATE_SETTLE_MS", Some("10")),
                ("CHOOSH_HOSTD_SERVICE_UNIT", Some("choosh-hostd-unittest-does-not-exist.service")),
            ],
            run_monitor(previous.clone(), current.clone(), socket.clone(), state_path.clone(), "0.9.0".to_string()),
        )
        .await;

        bind_task.abort();

        assert!(!previous.exists(), "the rollback rename must have moved .previous onto current's path");
        assert_eq!(std::fs::read(&current).unwrap(), b"known-good-previous-binary");

        let event = take_pending_failure_event(&state_path).expect("a failure must still be reported even though rollback succeeded");
        assert_eq!(
            event,
            WireAgentEvent::AgentStatus {
                workspace_id: SELF_UPDATE_WORKSPACE_ID.to_string(),
                item_id: SELF_UPDATE_ITEM_ID.to_string(),
                status: WireAgentStatus::Failed,
            }
        );
    }

    /// The "repeatedly fails after rollback MUST stop retrying" bound: if
    /// the rolled-back binary *also* never becomes healthy (nothing ever
    /// binds the socket, at all, for either attempt), `run_monitor` must
    /// give up after exactly one rollback rather than looping — proven
    /// here by wrapping the whole call in a generous but finite
    /// `tokio::time::timeout`: an infinite retry loop would hang past it
    /// and fail this test, rather than this test merely trusting the
    /// implementation not to loop.
    #[tokio::test]
    async fn run_monitor_gives_up_after_one_rollback_attempt_instead_of_looping_forever() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("choosh-hostd");
        let previous = dir.path().join("choosh-hostd.previous");
        let socket = dir.path().join("emit.sock"); // never bound by anything in this test
        let state_path = dir.path().join("update_state.json");
        std::fs::write(&current, b"broken-new-binary").unwrap();
        std::fs::write(&previous, b"also-never-comes-up-in-this-test").unwrap();

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            temp_env::async_with_vars(
                [
                    ("CHOOSH_HOSTD_UPDATE_HEALTH_WINDOW_MS", Some("100")),
                    ("CHOOSH_HOSTD_UPDATE_POLL_MS", Some("20")),
                    ("CHOOSH_HOSTD_UPDATE_SETTLE_MS", Some("10")),
                    ("CHOOSH_HOSTD_SERVICE_UNIT", Some("choosh-hostd-unittest-does-not-exist.service")),
                ],
                run_monitor(previous.clone(), current.clone(), socket, state_path.clone(), "0.9.0".to_string()),
            ),
        )
        .await;

        assert!(result.is_ok(), "run_monitor must return within the bounded window, not loop forever");
        assert!(!previous.exists(), "exactly one rollback rename must have happened");
        assert_eq!(std::fs::read(&current).unwrap(), b"also-never-comes-up-in-this-test", "the one rollback attempt still applied");

        let event = take_pending_failure_event(&state_path).expect("giving up must still report a failure upstream");
        assert_eq!(
            event,
            WireAgentEvent::AgentStatus {
                workspace_id: SELF_UPDATE_WORKSPACE_ID.to_string(),
                item_id: SELF_UPDATE_ITEM_ID.to_string(),
                status: WireAgentStatus::Failed,
            }
        );
    }

    /// A distinct failure mode from `run_monitor_gives_up_after_one_rollback_attempt_instead_of_looping_forever`
    /// above: there, the rollback `rename()` itself *succeeds* but the
    /// rolled-back binary still never passes its own health check. Here,
    /// `.previous` is deliberately never written at all, so the rollback
    /// `rename()` call fails outright (a real `ENOENT`) — this must still
    /// be recorded as a distinct, reported failure (with its own "rollback
    /// rename also failed" reason, not conflated with an ordinary
    /// health-check failure) rather than panicking or hanging, and must
    /// leave `current` untouched since nothing was ever renamed onto it.
    #[tokio::test]
    async fn run_monitor_reports_a_distinct_failure_when_the_rollback_rename_itself_fails() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("choosh-hostd");
        let previous = dir.path().join("choosh-hostd.previous"); // never written
        let socket = dir.path().join("emit.sock"); // never bound; health check must fail first
        let state_path = dir.path().join("update_state.json");
        std::fs::write(&current, b"broken-new-binary").unwrap();

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            temp_env::async_with_vars(
                [
                    ("CHOOSH_HOSTD_UPDATE_HEALTH_WINDOW_MS", Some("100")),
                    ("CHOOSH_HOSTD_UPDATE_POLL_MS", Some("20")),
                    ("CHOOSH_HOSTD_UPDATE_SETTLE_MS", Some("10")),
                    ("CHOOSH_HOSTD_SERVICE_UNIT", Some("choosh-hostd-unittest-does-not-exist.service")),
                ],
                run_monitor(previous.clone(), current.clone(), socket, state_path.clone(), "0.9.0".to_string()),
            ),
        )
        .await;

        assert!(result.is_ok(), "run_monitor must return promptly rather than hang when the rollback rename itself fails");
        assert!(!previous.exists(), "previous was never created in this test");
        assert_eq!(std::fs::read(&current).unwrap(), b"broken-new-binary", "a failed rename must leave current completely untouched");

        let state = load_state(&state_path);
        let report = state.pending_failure.expect("a failure must still be recorded even when the rollback rename itself fails");
        assert!(
            report.reason.contains("rollback rename also failed"),
            "expected the distinct rename-failure reason, got: {}",
            report.reason
        );

        let event = take_pending_failure_event(&state_path).expect("the failure must still be reported upstream via the normal channel");
        assert_eq!(
            event,
            WireAgentEvent::AgentStatus {
                workspace_id: SELF_UPDATE_WORKSPACE_ID.to_string(),
                item_id: SELF_UPDATE_ITEM_ID.to_string(),
                status: WireAgentStatus::Failed,
            }
        );
    }

    #[test]
    fn state_round_trips_through_disk_and_missing_file_loads_as_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update_state.json");

        assert!(take_pending_failure_event(&path).is_none());

        mark_failed(&path, "0.5.0", "simulated failure");
        let event = take_pending_failure_event(&path).expect("a failure was just recorded");
        match event {
            WireAgentEvent::AgentStatus { workspace_id, item_id, status } => {
                assert_eq!(workspace_id, SELF_UPDATE_WORKSPACE_ID);
                assert_eq!(item_id, SELF_UPDATE_ITEM_ID);
                assert_eq!(status, WireAgentStatus::Failed);
            }
            other => panic!("expected AgentStatus, got {other:?}"),
        }

        // Drained exactly once: a second call finds nothing pending.
        assert!(take_pending_failure_event(&path).is_none());
    }

    /// Real digest verification, wired end to end: a wrong `sha256` must
    /// be rejected before ever touching the running binary — no `.new`,
    /// no `.previous`, no swap, and a failure event on the agent-event
    /// channel — using a real local HTTP server and the real
    /// `handle_update_binary_push_with_paths` production code path (not a
    /// reimplementation of it).
    #[tokio::test]
    async fn wrong_digest_is_rejected_and_the_running_binary_is_left_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("choosh-hostd");
        let state_path = dir.path().join("update_state.json");
        std::fs::write(&current, b"original-running-binary").unwrap();

        let new_bytes = b"a completely different new binary".to_vec();
        let url = serve_one_download(new_bytes).await;
        let (tx, mut rx) = mpsc::channel(8);

        handle_update_binary_push_with_paths(
            current.clone(),
            None,
            state_path.clone(),
            "push-1".to_string(),
            url,
            "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            "9.9.9".to_string(),
            tx,
        )
        .await;

        assert_eq!(std::fs::read(&current).unwrap(), b"original-running-binary", "a digest mismatch must not touch the running binary");
        assert!(!sibling_path(&current, ".new").exists());
        assert!(!sibling_path(&current, ".previous").exists());
        assert!(load_state(&state_path).last_handled_push_id.is_none(), "a rejected push must not be recorded as handled");

        let event = rx.recv().await.expect("a failure event should have been sent");
        assert_eq!(
            event,
            WireAgentEvent::AgentStatus {
                workspace_id: SELF_UPDATE_WORKSPACE_ID.to_string(),
                item_id: SELF_UPDATE_ITEM_ID.to_string(),
                status: WireAgentStatus::Failed,
            }
        );
    }

    /// The happy path: a correct digest is downloaded, staged, backed up,
    /// and atomically swapped in — `current`'s content becomes the new
    /// binary's, and `.previous` holds exactly what `current` held before
    /// the swap. `CHOOSH_HOSTD_SERVICE_UNIT` is pointed at a unit that
    /// doesn't exist so the real `systemctl --user restart` this exercises
    /// can't collide with anything real on this machine — its failure is
    /// logged, not fatal (matches production's "still return, the restart
    /// is best-effort from this process's point of view" posture).
    #[tokio::test]
    async fn a_correct_digest_is_downloaded_verified_and_atomically_swapped_in() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("choosh-hostd");
        let state_path = dir.path().join("update_state.json");
        std::fs::write(&current, b"original-running-binary").unwrap();

        let new_bytes = fake_binary_bytes();
        let expected_digest = crate::fs_ops::content_revision(&new_bytes);
        let url = serve_one_download(new_bytes.clone()).await;
        let (tx, _rx) = mpsc::channel(8);

        temp_env::async_with_vars(
            [("CHOOSH_HOSTD_SERVICE_UNIT", Some("choosh-hostd-unittest-does-not-exist.service"))],
            handle_update_binary_push_with_paths(
                current.clone(),
                None,
                state_path.clone(),
                "push-1".to_string(),
                url,
                expected_digest,
                "1.2.3".to_string(),
                tx,
            ),
        )
        .await;

        assert_eq!(std::fs::read(&current).unwrap(), new_bytes, "current binary must now hold the new content");
        assert_eq!(
            std::fs::read(sibling_path(&current, ".previous")).unwrap(),
            b"original-running-binary",
            ".previous must hold exactly what was running before the swap"
        );
        assert!(!sibling_path(&current, ".new").exists(), "the staging path must not remain after a successful swap (renamed away)");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&current).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755, "the swapped-in binary must be executable");
        }
        assert_eq!(load_state(&state_path).last_handled_push_id.as_deref(), Some("push-1"));
    }

    /// relay-protocol.md: `update_binary` carries a `push_id` specifically
    /// so a redelivery after reconnect is handled idempotently — a second
    /// call with the same `push_id` must not re-download or re-apply
    /// anything (proven here by pointing it at a URL nothing is listening
    /// on: a real re-download attempt would fail/hang against it).
    #[tokio::test]
    async fn a_redelivered_push_id_is_skipped_without_re_downloading() {
        let dir = tempfile::tempdir().unwrap();
        let current = dir.path().join("choosh-hostd");
        let state_path = dir.path().join("update_state.json");
        std::fs::write(&current, b"original-running-binary").unwrap();
        save_state(&state_path, &UpdateState { last_handled_push_id: Some("push-1".to_string()), pending_failure: None }).unwrap();

        let (tx, mut rx) = mpsc::channel(8);
        handle_update_binary_push_with_paths(
            current.clone(),
            None,
            state_path,
            "push-1".to_string(),
            "http://127.0.0.1:1/nothing-listens-here".to_string(),
            "irrelevant".to_string(),
            "1.2.3".to_string(),
            tx,
        )
        .await;

        assert_eq!(std::fs::read(&current).unwrap(), b"original-running-binary");
        assert!(rx.try_recv().is_err(), "a redelivered, already-handled push must not report a failure either");
    }

    #[test]
    fn redelivered_push_id_is_recognized_via_persisted_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update_state.json");
        let mut state = UpdateState { last_handled_push_id: Some("push-1".to_string()), pending_failure: None };
        save_state(&path, &state).unwrap();

        let reloaded = load_state(&path);
        assert_eq!(reloaded.last_handled_push_id.as_deref(), Some("push-1"));

        state.last_handled_push_id = Some("push-2".to_string());
        save_state(&path, &state).unwrap();
        assert_eq!(load_state(&path).last_handled_push_id.as_deref(), Some("push-2"));
    }
}
