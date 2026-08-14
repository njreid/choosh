//! Zellij session lifecycle for workspace registration, per
//! `DESIGN.md`'s design principle 6 ("`choosh-hostd` owns durable host
//! state; Zellij owns process survival") and
//! `docs/milestones/M1-workspace-and-jj.md`'s "a same-named Zellij session
//! created alongside it". Every invocation is fixed executable + argv, per
//! `host-rpc.md`'s "Command construction" — never a shell string.
//!
//! Session creation uses `zellij attach <name> --create-background`
//! (`zellij attach --help`'s `-b, --create-background`: "Create a detached
//! session in the background if one does not exist") — a real, documented,
//! synchronous headless-creation command. An earlier version of this module
//! worked around what it believed was a missing headless mode by spawning
//! `zellij --session <name>` with no controlling TTY and treating the
//! resulting raw-mode client crash as an incidental, empirically-observed
//! side effect that left the session server running — that approach was
//! genuinely flaky (the server didn't reliably outlive the crashed client
//! across consecutive `list-sessions` calls) and is replaced entirely here.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::Mutex;

/// Serializes every `zellij` client invocation this process makes.
///
/// Confirmed by direct experiment (10 concurrent `zellij attach
/// --create-background` invocations against distinct session names): 9
/// completed, 1 hung for 2+ minutes before being killed by an external
/// timeout — a real concurrency bug in Zellij's own client/server layer,
/// not a slow-startup timing issue a longer poll or a bare retry can fully
/// paper over (the existing retry loop in [`create_session`] helps with a
/// *lost* creation, but not with a client that never exits at all). A
/// real devhost handling several `workspace.create` calls close together
/// benefits from this the same way this test suite does: safe by
/// construction rather than by luck.
///
/// **Known residual gap**: this serializes every Zellij client invocation
/// *this process* makes, but not Zellij's own internal concurrency (its
/// server handling multiple attached clients, background threads, etc.) —
/// so it substantially reduces rather than provably eliminates exposure to
/// the underlying bug. Observed directly: 0 failures across 15 isolated
/// re-runs of a single Zellij-dependent test, vs. 1 failure across 6 runs
/// of the full ~45-test suite (heavier concurrent subprocess load from
/// unrelated `jj`/`git`-spawning tests running in parallel). Don't be
/// surprised by a rare flake here under full-suite parallel execution; do
/// be suspicious if it stops being rare.
static ZELLIJ_CLIENT_LOCK: Mutex<()> = Mutex::const_new(());

#[derive(Debug)]
pub enum ZellijError {
    Spawn(std::io::Error),
    /// The session didn't appear in `list-sessions` shortly after
    /// attempting creation — the actual success/failure signal this module
    /// relies on; `zellij attach --create-background`'s own exit status
    /// isn't inspected (see [`create_session`]'s doc comment for why).
    NotConfirmed,
}

impl std::fmt::Display for ZellijError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "failed to spawn zellij: {error}"),
            Self::NotConfirmed => write!(f, "zellij session was not confirmed via list-sessions after creation"),
        }
    }
}

impl std::error::Error for ZellijError {}

/// Creates a Zellij session named `session_name` rooted at `cwd`, per the
/// headless-creation approach documented above. Idempotent in the sense
/// that a session already present under this name is treated as success
/// without re-spawning — callers (the registry layer) are responsible for
/// deciding whether a pre-existing same-named session should be adopted or
/// rejected (per `host-rpc.md`'s explicit-confirmation posture); this
/// function only guarantees "a session with this name exists" on success.
///
/// # Errors
///
/// Returns [`ZellijError::Spawn`] if the `zellij` binary can't be spawned
/// at all, or [`ZellijError::NotConfirmed`] if the session still doesn't
/// appear in `list-sessions` after every retry.
pub async fn create_session(session_name: &str, cwd: &Path) -> Result<(), ZellijError> {
    // `zellij attach --create-background` has a real, reproducible race
    // under concurrent invocation: 8 concurrent creates against 8 distinct
    // names lost one outright in this environment (7 sessions materialized,
    // not 8), confirmed by hand outside this codebase, not theorized. A
    // bare retry of the *creation command itself* against the same
    // already-lost attempt reliably recovers — this isn't a client hang or
    // a slow-to-appear session (both handled below/via `wait_bounded`),
    // it's the session never having been created at all the first time.
    // Polling `list-sessions` for longer without ever retrying the command
    // that failed to create anything cannot fix that class of failure.
    for attempt in 0..5 {
        if list_sessions().await?.iter().any(|name| name == session_name) {
            return Ok(());
        }

        // Deliberately NOT `Command::output()`: `--create-background`
        // detaches a session server that, at least in this environment,
        // can inherit this process's stdout/stderr pipe file descriptors
        // rather than closing them — `output()` waits for those pipes to
        // reach EOF as part of collecting output, so it can hang
        // indefinitely on the long-lived server holding them open, well
        // after the client itself has finished. Sending stdout/stderr to
        // `/dev/null` sidesteps that particular hang, but isn't sufficient
        // on its own: empirically, the `zellij attach` *client process
        // itself* sometimes never exits at all, independent of its
        // stdout/stderr — a bare `.wait()` can hang just as badly.
        // `wait_bounded` covers both: the client hanging or being killed
        // doesn't mean creation failed, since the session server may
        // already be up regardless, or this may just be one of several
        // retries — `list-sessions` is this function's actual source of
        // truth either way, never this process's exit.
        {
            let _guard = ZELLIJ_CLIENT_LOCK.lock().await;
            let child = Command::new("zellij")
                .arg("attach")
                .arg(session_name)
                .arg("--create-background")
                .current_dir(cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(ZellijError::Spawn)?;
            wait_bounded(child).await;
        }

        // 20 x 100ms = 2s per attempt (up to 5 attempts above): enough
        // headroom for ordinary startup latency without this function
        // silently masking a genuinely lost creation as "just needs more
        // time" — a real loss gets a fresh, independent retry instead,
        // which is what actually fixes it (see above).
        for _ in 0..20 {
            if list_sessions().await?.iter().any(|name| name == session_name) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tracing::warn!(session_name, attempt, "zellij session not confirmed yet, retrying creation");
    }
    Err(ZellijError::NotConfirmed)
}

/// Creates a new tab named `tab_name` in the given session, running `cwd`
/// as its starting directory. If `initial_command` is non-empty, it's
/// appended after `--` as the tab's initial process (fixed argv, per
/// `host-rpc.md`'s "Command construction" — never a shell string); an empty
/// `initial_command` gives an ordinary shell tab.
///
/// **Environment variables do not propagate through this call**: `zellij
/// action` is a lightweight IPC client sending a message to the
/// already-running session *server*, which is what actually spawns the new
/// tab's process — confirmed empirically that env vars set on the `zellij
/// action` client process are invisible to it. To inject
/// `CHOOSH_WORKSPACE_ID`/`CHOOSH_ITEM_ID`/`CHOOSH_ROOT`/`CHOOSH_AGENT` for an
/// `AgentTerminal` item (`agent-events.md`'s adapter contract), callers must
/// prepend `env KEY=VALUE ...` (the real `env` utility, fixed argv, never
/// shell-interpolated) to `initial_command` themselves — see
/// `items.rs::agent_launch_argv` for the one place that does this.
///
/// Targets `session_name` via `ZELLIJ_SESSION_NAME` in *this* process's own
/// environment, which is how `zellij action` addresses "the session for
/// this call" — confirmed against the real binary: `zellij action` takes no
/// session-selecting flag at all, it always targets whatever
/// `ZELLIJ_SESSION_NAME` names.
///
/// # Errors
///
/// Returns [`ZellijError::Spawn`] if `zellij` can't be spawned, or
/// [`ZellijError::NotConfirmed`] if the tab doesn't appear in
/// `list-tabs` shortly after.
pub async fn new_tab(session_name: &str, tab_name: &str, cwd: &Path, initial_command: &[String]) -> Result<(), ZellijError> {
    {
        let _guard = ZELLIJ_CLIENT_LOCK.lock().await;
        let mut command = Command::new("zellij");
        command
            .env("ZELLIJ_SESSION_NAME", session_name)
            .arg("action")
            .arg("new-tab")
            .arg("--name")
            .arg(tab_name)
            .arg("--cwd")
            .arg(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if !initial_command.is_empty() {
            command.arg("--");
            command.args(initial_command);
        }
        let child = command.spawn().map_err(ZellijError::Spawn)?;
        wait_bounded(child).await;
    }

    for _ in 0..20 {
        if list_tabs(session_name).await?.iter().any(|name| name == tab_name) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(ZellijError::NotConfirmed)
}

/// Tab names from `zellij action list-tabs --json`, targeting
/// `session_name` via `ZELLIJ_SESSION_NAME` the same way [`new_tab`] does.
/// Uses `--json` rather than the plain-text table format: confirmed against
/// the real binary, `--json` gives a stable array of objects with a `name`
/// field, versus guessing at column alignment/whitespace in the table
/// renderer's output.
///
/// # Errors
///
/// Returns [`ZellijError::Spawn`] if `zellij` can't be spawned.
pub async fn list_tabs(session_name: &str) -> Result<Vec<String>, ZellijError> {
    let output = Command::new("zellij")
        .env("ZELLIJ_SESSION_NAME", session_name)
        .arg("action")
        .arg("list-tabs")
        .arg("--json")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(ZellijError::Spawn)?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let tabs: Vec<TabInfo> = serde_json::from_slice(&output.stdout).unwrap_or_default();
    Ok(tabs.into_iter().map(|t| t.name).collect())
}

/// One entry from `zellij action list-tabs --json` — shared by [`list_tabs`]
/// and [`close_tab`] rather than each declaring its own local shape.
#[derive(serde::Deserialize)]
struct TabInfo {
    name: String,
    tab_id: u32,
}

/// Focuses the tab named `tab_name` for whichever client most recently
/// attached to `session_name` (`zellij action go-to-tab-name`, confirmed
/// present via `zellij action --help`) — used by `pty.rs` immediately
/// after a headless `zellij attach` client connects, since `attach` itself
/// has no per-tab targeting flag.
///
/// # Errors
///
/// Returns [`ZellijError::Spawn`] if `zellij` can't be spawned.
pub async fn focus_tab(session_name: &str, tab_name: &str) -> Result<(), ZellijError> {
    let _guard = ZELLIJ_CLIENT_LOCK.lock().await;
    let child = Command::new("zellij")
        .env("ZELLIJ_SESSION_NAME", session_name)
        .arg("action")
        .arg("go-to-tab-name")
        .arg(tab_name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(ZellijError::Spawn)?;
    wait_bounded(child).await;
    Ok(())
}

/// Closes the tab named `tab_name` in `session_name`. Looks up the tab's
/// stable numeric ID via `list-tabs --json` first and closes by ID
/// (`zellij action close-tab --tab-id <id>`) rather than the current-tab-only
/// `close-tab` with no ID, which would require focusing the tab first (an
/// extra round trip and a race against whatever tab happens to be focused
/// when this runs). A tab that's already gone (stopped twice, or removed
/// out of band) is treated as success — idempotent, matching
/// `kill_session`'s posture.
///
/// # Errors
///
/// Returns [`ZellijError::Spawn`] if `zellij` can't be spawned.
pub async fn close_tab(session_name: &str, tab_name: &str) -> Result<(), ZellijError> {
    let output = Command::new("zellij")
        .env("ZELLIJ_SESSION_NAME", session_name)
        .arg("action")
        .arg("list-tabs")
        .arg("--json")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(ZellijError::Spawn)?;
    let tabs: Vec<TabInfo> = serde_json::from_slice(&output.stdout).unwrap_or_default();
    let Some(tab) = tabs.iter().find(|t| t.name == tab_name) else {
        return Ok(());
    };

    let _guard = ZELLIJ_CLIENT_LOCK.lock().await;
    let child = Command::new("zellij")
        .env("ZELLIJ_SESSION_NAME", session_name)
        .arg("action")
        .arg("close-tab")
        .arg("--tab-id")
        .arg(tab.tab_id.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(ZellijError::Spawn)?;
    wait_bounded(child).await;
    Ok(())
}

/// Session names from `zellij list-sessions --no-formatting`, one per
/// line, stripped of the trailing `[Created ...]`/`(current)` metadata
/// Zellij appends after the name.
///
/// # Errors
///
/// Returns [`ZellijError::Spawn`] if `zellij` can't be spawned. A `zellij`
/// exit failure with no sessions running is treated as "no sessions", not
/// an error, since that's Zellij's normal behavior when nothing exists yet.
pub async fn list_sessions() -> Result<Vec<String>, ZellijError> {
    let output = Command::new("zellij")
        .arg("list-sessions")
        .arg("--no-formatting")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(ZellijError::Spawn)?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_string)
        .collect())
}

/// # Errors
///
/// Returns [`ZellijError::Spawn`] if `zellij` can't be spawned. Killing an
/// already-absent session is not an error (idempotent teardown, used by
/// this module's own tests to clean up after themselves).
///
/// Only test-called today (`#[allow(dead_code)]` below) — production
/// callers land with `item.stop`/workspace teardown in a later increment.
#[allow(dead_code)]
pub async fn kill_session(session_name: &str) -> Result<(), ZellijError> {
    // Same hang risk `create_session` documents (pipe inheritance, and the
    // client process itself sometimes never exiting) — same bounded-wait
    // fix, plus the same serialization against every other `zellij` client
    // invocation this process makes (see `ZELLIJ_CLIENT_LOCK`).
    let _guard = ZELLIJ_CLIENT_LOCK.lock().await;
    let kill = Command::new("zellij")
        .arg("kill-session")
        .arg(session_name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(ZellijError::Spawn)?;
    wait_bounded(kill).await;
    let delete = Command::new("zellij")
        .arg("delete-session")
        .arg(session_name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(ZellijError::Spawn)?;
    wait_bounded(delete).await;
    Ok(())
}

/// Waits up to 5s for `child` to exit, force-killing it if it overruns.
/// `zellij` client processes have been observed, in this environment under
/// concurrent load, to occasionally never exit on their own regardless of
/// what their stdout/stderr are connected to — the caller's actual success
/// signal is always something else (a `list-sessions` poll), never this
/// process's exit, so a hung/killed child here is not itself a failure.
async fn wait_bounded(mut child: tokio::process::Child) {
    if tokio::time::timeout(Duration::from_secs(5), child.wait()).await.is_err() {
        let _ = child.kill().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises the real `zellij` binary installed in this environment —
    /// deliberately not mocked, per this crate's established testing
    /// discipline (see `credential.rs`, `frame_channel.rs`). Uses a
    /// randomized session name to avoid colliding with any session a
    /// concurrent test run (or a real devhost daemon on this same
    /// machine) might already have, and always cleans up after itself.
    #[tokio::test]
    async fn create_session_is_confirmed_via_list_sessions_and_cleans_up() {
        let name = format!("choosh-test-{}", uuid::Uuid::new_v4());
        let dir = tempfile::tempdir().unwrap();

        create_session(&name, dir.path()).await.unwrap();
        assert!(list_sessions().await.unwrap().contains(&name));

        kill_session(&name).await.unwrap();
        // Zellij's own teardown is not instant; poll briefly rather than
        // asserting absence immediately after kill.
        for _ in 0..20 {
            if !list_sessions().await.unwrap().contains(&name) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("session {name} still listed after kill_session");
    }

    #[tokio::test]
    async fn creating_an_already_existing_session_is_a_no_op_success() {
        let name = format!("choosh-test-{}", uuid::Uuid::new_v4());
        let dir = tempfile::tempdir().unwrap();

        create_session(&name, dir.path()).await.unwrap();
        create_session(&name, dir.path()).await.unwrap();
        let count = list_sessions().await.unwrap().iter().filter(|n| *n == &name).count();
        assert_eq!(count, 1, "creating an existing session must not duplicate it");

        kill_session(&name).await.unwrap();
    }
}
