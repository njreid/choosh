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

/// The port `zellij web` listens on when started with no explicit `--port`
/// — confirmed against the real binary (`zellij web --help`: "defaults to
/// 8082"), and also the only port `zellij web --status` itself checks
/// (also confirmed empirically: `--status` takes no `--port` argument and
/// always probes this fixed port). [`ensure_web_server_running`]
/// deliberately never overrides it — using a different port would make
/// `--status`'s own check meaningless for confirming "is it up".
pub const ZELLIJ_WEB_DEFAULT_PORT: u16 = 8082;

#[derive(Debug)]
pub enum ZellijError {
    Spawn(std::io::Error),
    /// The session didn't appear in `list-sessions` shortly after
    /// attempting creation — the actual success/failure signal this module
    /// relies on; `zellij attach --create-background`'s own exit status
    /// isn't inspected (see [`create_session`]'s doc comment for why).
    NotConfirmed,
    /// [`list_tabs`] specifically: the `zellij action list-tabs` client
    /// never exited within its bound. Confirmed by direct experiment
    /// (`env ZELLIJ_SESSION_NAME=<a name nothing created> zellij action
    /// list-tabs --json`, real binary, real timeout) that this is a real,
    /// reachable case — targeting a session name that was never created at
    /// all hangs the client indefinitely, unlike targeting a *real*
    /// session for a tab that just isn't in it (which returns quickly,
    /// successfully, with an empty/partial list) — so this is genuinely
    /// distinct from "queried successfully and the tab isn't there".
    Timeout,
}

impl std::fmt::Display for ZellijError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "failed to spawn zellij: {error}"),
            Self::NotConfirmed => write!(f, "zellij session was not confirmed via list-sessions after creation"),
            Self::Timeout => write!(f, "zellij client did not respond within the timeout"),
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
/// Bounded with the same 5s timeout [`wait_bounded`] uses elsewhere in this
/// module, for the same class of reason but a distinct, specifically
/// confirmed failure mode: unlike the `spawn`-then-`wait_bounded` commands
/// elsewhere in this file (whose actual success signal is always a
/// separate `list-sessions`/`list-tabs` poll, so a hung/killed spawned
/// child there was never itself a problem), this function's result *is*
/// the signal a caller needs — so a hang here can't just be shrugged off
/// the same way; it has to surface as [`ZellijError::Timeout`] instead so
/// callers ([`close_tab`], `readiness::run`) can tell "genuinely couldn't
/// find out" apart from "asked, and there's nothing there".
///
/// # Errors
///
/// Returns [`ZellijError::Spawn`] if `zellij` can't be spawned, or
/// [`ZellijError::Timeout`] if it doesn't respond within 5s (confirmed by
/// direct experiment: a `session_name` that was never created at all hangs
/// the client indefinitely rather than failing fast — see
/// [`ZellijError::Timeout`]'s own doc comment).
pub async fn list_tabs(session_name: &str) -> Result<Vec<String>, ZellijError> {
    Ok(list_tabs_info(session_name).await?.into_iter().map(|t| t.name).collect())
}

/// The actual bounded `zellij action list-tabs --json` invocation, shared
/// by [`list_tabs`] (names only) and [`close_tab`] (needs each tab's
/// numeric `tab_id` too, to close by ID rather than by currently-focused
/// tab — see [`close_tab`]'s own doc comment).
async fn list_tabs_info(session_name: &str) -> Result<Vec<TabInfo>, ZellijError> {
    let child = Command::new("zellij")
        .env("ZELLIJ_SESSION_NAME", session_name)
        .arg("action")
        .arg("list-tabs")
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A timed-out `wait_with_output` future (below) drops the `Child`
        // without ever cleanly `.wait()`-ing it — `kill_on_drop` is what
        // actually reaps the hung process in that case rather than
        // leaking it as an orphan; `wait_bounded`'s callers elsewhere in
        // this file instead call `.kill()` explicitly after their own
        // timeout because they still hold `child` at that point, but
        // `wait_with_output(self)` below consumes it by value, so there's
        // nothing left to call `.kill()` on if the timeout wins the race.
        .kill_on_drop(true)
        .spawn()
        .map_err(ZellijError::Spawn)?;

    let output = match tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(ZellijError::Spawn(error)),
        Err(_elapsed) => return Err(ZellijError::Timeout),
    };
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&output.stdout).unwrap_or_default())
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
    run_zellij_client(&["action", "go-to-tab-name", tab_name], Some(session_name)).await
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
    // Reuses `list_tabs_info` rather than duplicating its own
    // `list-tabs --json` invocation (an earlier version of this function
    // did exactly that, with its own unbounded `.output()` call — the same
    // real hang risk `list_tabs_info`'s bounded timeout now guards
    // against; see `ZellijError::Timeout`'s doc comment). `rpc.rs`'s
    // `handle_item_stop` already only treats `ZellijError::Spawn`
    // specially and swallows every other `close_tab` error as best-effort
    // (a stop request still records the item as stopped even if this part
    // of it couldn't be confirmed in time), so surfacing `Timeout` here
    // rather than the old code's silent-empty-list behavior is a strict
    // improvement, not a behavior change callers need to handle specially.
    let tabs = list_tabs_info(session_name).await?;
    let Some(tab) = tabs.iter().find(|t| t.name == tab_name) else {
        return Ok(());
    };

    let tab_id = tab.tab_id.to_string();
    run_zellij_client(&["action", "close-tab", "--tab-id", &tab_id], Some(session_name)).await
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
    // invocation this process makes (see `ZELLIJ_CLIENT_LOCK`, both via
    // `run_zellij_client`).
    run_zellij_client(&["kill-session", session_name], None).await?;
    run_zellij_client(&["delete-session", session_name], None).await
}

/// Spawns `zellij <args>` with stdio silenced, guarded by
/// [`ZELLIJ_CLIENT_LOCK`], and waits up to 5s for it to exit
/// ([`wait_bounded`]) — the common "fire off a zellij client invocation and
/// don't trust its own exit/hang behavior" shape [`focus_tab`],
/// [`close_tab`], [`kill_session`], [`ensure_web_server_running`], and
/// [`stop_web_server`] all need (see [`wait_bounded`]'s own doc comment for
/// why a hung/killed client here is never itself a failure — the real
/// success signal is always a separate poll). `session_name` sets
/// `ZELLIJ_SESSION_NAME` when `Some`, the mechanism `zellij action`
/// commands use to target a session (see [`new_tab`]'s doc comment).
/// [`create_session`] and [`new_tab`] don't use this: they need
/// `current_dir`/dynamic trailing argv this helper doesn't support, plus
/// their own retry-until-confirmed loop around the spawn itself.
///
/// # Errors
///
/// Returns [`ZellijError::Spawn`] if `zellij` can't be spawned.
async fn run_zellij_client(args: &[&str], session_name: Option<&str>) -> Result<(), ZellijError> {
    let _guard = ZELLIJ_CLIENT_LOCK.lock().await;
    let mut command = Command::new("zellij");
    if let Some(session_name) = session_name {
        command.env("ZELLIJ_SESSION_NAME", session_name);
    }
    command.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let child = command.spawn().map_err(ZellijError::Spawn)?;
    wait_bounded(child).await;
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

/// Ensures Zellij's own web-client server (`zellij web`) is running on
/// [`ZELLIJ_WEB_DEFAULT_PORT`], starting it if it isn't, for
/// `serve.rs`'s Zellij-web-client break-glass tunnel path
/// (`docs/specs/service-tunnels.md`'s last section). Idempotent: a server
/// already running is left alone and its port returned immediately.
///
/// Confirmed against the real binary rather than assumed: `zellij web` is
/// a genuine, working HTTP+WebSocket terminal server (`zellij web
/// --help`), not a stub — `--status`'s stdout text is the only reliable
/// online/offline signal (its exit code is always `0` either way,
/// confirmed by direct experiment), so this parses that text rather than
/// trusting the exit status the way [`create_session`]/[`new_tab`] do
/// against `list-sessions`/`list-tabs`. Starting an already-running server
/// on the same port fails loudly (`zellij web --start`: "Address already
/// in use", exit code 2) rather than being itself idempotent, which is
/// exactly why this function checks `--status` first instead of always
/// calling `--start`; the same race that [`create_session`] documents
/// (two callers both seeing "not there yet" and both trying to create it)
/// is handled the same way here too — a failed `--start` from losing that
/// race is not itself treated as fatal, since the polling loop below still
/// confirms the *actual* outcome via `--status` regardless of which
/// caller's `--start` "won".
///
/// # Errors
///
/// Returns [`ZellijError::Spawn`] if `zellij` can't be spawned at all, or
/// [`ZellijError::NotConfirmed`] if the server still isn't reported
/// online after every retry.
pub async fn ensure_web_server_running() -> Result<u16, ZellijError> {
    if web_server_status_online().await? {
        return Ok(ZELLIJ_WEB_DEFAULT_PORT);
    }

    run_zellij_client(&["web", "--start", "-d"], None).await?;

    for _ in 0..20 {
        if web_server_status_online().await? {
            return Ok(ZELLIJ_WEB_DEFAULT_PORT);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(ZellijError::NotConfirmed)
}

async fn web_server_status_online() -> Result<bool, ZellijError> {
    let output =
        Command::new("zellij").arg("web").arg("--status").stdin(Stdio::null()).output().await.map_err(ZellijError::Spawn)?;
    Ok(String::from_utf8_lossy(&output.stdout).contains("online"))
}

/// Stops Zellij's own web-client server. Idempotent — stopping an
/// already-stopped (or never-started) server is not an error, matching
/// [`kill_session`]'s posture. Used by this module's own tests to clean up
/// after themselves; also available for any future graceful-shutdown path.
///
/// # Errors
///
/// Returns [`ZellijError::Spawn`] if `zellij` can't be spawned.
///
/// Only test-called today (`#[allow(dead_code)]` below, matching
/// [`kill_session`]'s own precedent) — a production graceful-shutdown
/// caller is a reasonable future addition, not part of this change's scope.
#[allow(dead_code)]
pub async fn stop_web_server() -> Result<(), ZellijError> {
    run_zellij_client(&["web", "--stop"], None).await
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

    /// Exercises the real `zellij web` server this environment ships —
    /// confirmed present via `zellij web --help` (see `serve.rs`'s
    /// break-glass tunnel doc comment for the fuller investigation this
    /// backs). `zellij web` is a machine-wide singleton, not scoped per
    /// test, so this restores whatever online/offline state it found
    /// rather than unconditionally stopping it (a real daemon on this same
    /// machine — or a concurrently running test — could legitimately want
    /// it left running).
    #[tokio::test]
    async fn ensure_web_server_running_starts_a_real_reachable_server_and_is_idempotent() {
        let was_online = web_server_status_online().await.unwrap();

        let port = ensure_web_server_running().await.unwrap();
        assert_eq!(port, ZELLIJ_WEB_DEFAULT_PORT);
        assert!(web_server_status_online().await.unwrap());

        // A real TCP connect confirms something is actually listening, not
        // just that `--status` claims so.
        tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("zellij web server should be reachable");

        // Idempotent: calling again while already running must not error
        // or report a different port.
        let second = ensure_web_server_running().await.unwrap();
        assert_eq!(second, port);

        if !was_online {
            stop_web_server().await.unwrap();
            for _ in 0..20 {
                if !web_server_status_online().await.unwrap() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("zellij web server still reported online after stop_web_server");
        }
    }
}
