//! Readiness probing for newly created `WebService` items, per
//! `docs/specs/service-tunnels.md`'s "Readiness" section: "`choosh-hostd`
//! MAY probe the declared host loopback port after launch... a plain
//! TCP-connect-succeeds check is a reasonable, simple readiness probe for
//! V1." Deliberately nothing HTTP-aware — "Readiness affects status only;
//! it MUST NOT discover or substitute another port."
//!
//! Spawned once by `rpc::handle_item_create` immediately after a
//! `WebService` item's Zellij tab is confirmed and registered as
//! `starting`. Drives that item to:
//! - `running`, on the first successful TCP connect to its declared port;
//! - `failed`, if the bound below elapses with no successful connect, or
//!   if the item's own Zellij tab disappears first (the launched process
//!   exited before ever accepting a connection — a real, distinct failure
//!   mode from "just slow to start");
//! - `unknown`, if Zellij's own tab state genuinely can't be queried (a
//!   `zellij` spawn failure) — distinct from "queried and the tab is
//!   gone," which is `failed` above. Note `zellij_ops::list_tabs` already
//!   collapses "no such session" into an empty tab list rather than an
//!   error (see its own doc comment), so in practice `unknown` here is
//!   narrow: it only fires when the `zellij` binary itself can't be
//!   spawned, not on the ordinary "session/tab isn't there (any more)"
//!   path, which reads as `failed` instead. That's a deliberate,
//!   documented choice, not an oversight: this module cannot tell "isn't
//!   there" apart from "never was" using `list_tabs` alone, and either one
//!   reasonably reads as `failed` for a freshly-created item.
//!
//! A user-issued `item.stop` (or any other status write) racing with this
//! probe always wins — [`finish`] only writes its outcome if the item is
//! still `starting` when it gets there.

use std::sync::Arc;
use std::time::{Duration, Instant};

use choosh_protocol::host_rpc::ItemStatus;
use tokio::sync::Mutex;

use crate::registry::Registry;
use crate::zellij_ops;

/// Total wall-clock budget for a `WebService` item to become reachable.
/// Long enough to ride out a cold start of a typical dev-server toolchain
/// (Vite/webpack/Next.js first-run compiles have been observed to take
/// several seconds, occasionally longer on a loaded machine), short enough
/// that a genuinely dead service doesn't sit reported as `starting`
/// indefinitely, and short enough to keep this module's own tests (which
/// wait out the full bound to observe a `failed` transition) reasonably
/// fast. Not configurable — a fixed, documented number beats a knob no one
/// will tune correctly.
pub(crate) const READINESS_TIMEOUT: Duration = Duration::from_secs(8);

/// Exponential backoff between probe attempts, starting here...
const INITIAL_POLL_DELAY: Duration = Duration::from_millis(100);
/// ...and capped here, so a long `starting` window still probes at a
/// bounded rate (never more often than once per second) rather than
/// tightening forever.
const MAX_POLL_DELAY: Duration = Duration::from_secs(1);

/// Spawns the probe as a detached background task. Callers don't await it
/// or hold its `JoinHandle` — `registry` is an `Arc` specifically so the
/// task can outlive the `item.create` RPC call that spawned it (an
/// `RpcContext` borrow would not be `'static`, and this genuinely needs to
/// keep running after `handle_item_create` returns its response).
pub fn spawn(registry: Arc<Mutex<Registry>>, item_id: String, session_name: String, tab_name: String, port: u16) {
    tokio::spawn(run(registry, item_id, session_name, tab_name, port));
}

/// The probe loop itself, exposed separately from [`spawn`] so a caller
/// that genuinely wants to wait for the outcome (`choosh-hostd service
/// run`'s CLI form, which is a one-shot process rather than a resident
/// daemon) can `.await` it directly instead of racing a detached task
/// against process exit.
pub async fn run(registry: Arc<Mutex<Registry>>, item_id: String, session_name: String, tab_name: String, port: u16) {
    let deadline = Instant::now() + READINESS_TIMEOUT;
    let mut delay = INITIAL_POLL_DELAY;

    loop {
        match zellij_ops::list_tabs(&session_name).await {
            Ok(tabs) if !tabs.iter().any(|t| t == &tab_name) => {
                tracing::warn!(item_id, "WebService tab disappeared before becoming ready; marking failed");
                finish(&registry, &item_id, ItemStatus::Failed).await;
                return;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, item_id, "could not confirm WebService tab state while probing readiness");
                finish(&registry, &item_id, ItemStatus::Unknown).await;
                return;
            }
        }

        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            finish(&registry, &item_id, ItemStatus::Running).await;
            return;
        }

        if Instant::now() >= deadline {
            finish(&registry, &item_id, ItemStatus::Failed).await;
            return;
        }

        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_POLL_DELAY);
    }
}

async fn finish(registry: &Arc<Mutex<Registry>>, item_id: &str, status: ItemStatus) {
    let mut registry = registry.lock().await;
    if registry.find_item(item_id).is_some_and(|item| item.status == ItemStatus::Starting) {
        let _ = registry.set_item_status(item_id, status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use choosh_protocol::host_rpc::ItemType;

    fn registry_with_starting_item(item_id: &str, workspace_id: &str, port: Option<u16>) -> (tempfile::TempDir, Arc<Mutex<Registry>>) {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::load(&dir.path().join("registry.json")).unwrap();
        registry
            .register_item(
                item_id.to_string(),
                workspace_id.to_string(),
                ItemType::WebService,
                "web".to_string(),
                "web".to_string(),
                None,
                port,
                ItemStatus::Starting,
            )
            .unwrap();
        (dir, Arc::new(Mutex::new(registry)))
    }

    /// A real Zellij session (the realistic production shape: the
    /// workspace's session always exists by the time a `WebService` item
    /// is created in it) with no tab of the target name in it — the "the
    /// launched process's tab exited/never existed" case, confirmed to
    /// return quickly and successfully from `list_tabs` (unlike a wholly
    /// nonexistent session — see the test below, and
    /// `ZellijError::Timeout`'s doc comment for the real, confirmed-by-
    /// experiment distinction between the two).
    #[tokio::test]
    async fn missing_tab_in_a_real_session_fails_fast_without_waiting_out_the_full_bound() {
        let session_name = format!("readiness-test-{}", uuid::Uuid::new_v4());
        let dir = tempfile::tempdir().unwrap();
        crate::zellij_ops::create_session(&session_name, dir.path()).await.unwrap();

        let item_id = "item-missing-tab";
        let (_registry_dir, registry) = registry_with_starting_item(item_id, "ws-1", Some(1));

        let start = Instant::now();
        run(registry.clone(), item_id.to_string(), session_name.clone(), "no-such-tab".to_string(), 1).await;
        let elapsed = start.elapsed();

        crate::zellij_ops::kill_session(&session_name).await.ok();
        assert!(elapsed < READINESS_TIMEOUT, "expected the missing-tab case to fail fast, took {elapsed:?}");
        let status = registry.lock().await.find_item(item_id).unwrap().status;
        assert_eq!(status, ItemStatus::Failed);
    }

    /// A session name nothing ever created: confirmed by direct experiment
    /// (`ZellijError::Timeout`'s doc comment) that `zellij action
    /// list-tabs --json` hangs rather than failing fast against a
    /// genuinely nonexistent session — this is the "Zellij pane state is
    /// unreachable" case this module's own doc comment calls out, and it
    /// must land on `unknown`, not `failed` (which is reserved for
    /// "asked, and confirmed the tab really isn't there").
    #[tokio::test]
    async fn wholly_unreachable_session_yields_unknown_not_failed() {
        let item_id = "item-unreachable-session";
        let (_dir, registry) = registry_with_starting_item(item_id, "ws-1", Some(1));

        let start = Instant::now();
        run(registry.clone(), item_id.to_string(), format!("no-such-session-{}", uuid::Uuid::new_v4()), "no-such-tab".to_string(), 1).await;
        let elapsed = start.elapsed();

        assert!(elapsed < READINESS_TIMEOUT, "expected the unreachable-session case to fail fast, took {elapsed:?}");
        let status = registry.lock().await.find_item(item_id).unwrap().status;
        assert_eq!(status, ItemStatus::Unknown);
    }

    #[tokio::test]
    async fn a_status_no_longer_starting_is_never_overwritten() {
        let item_id = "item-already-stopped";
        let (_dir, registry) = registry_with_starting_item(item_id, "ws-1", Some(1));
        registry.lock().await.set_item_status(item_id, ItemStatus::Stopped).unwrap();

        finish(&registry, item_id, ItemStatus::Running).await;

        let status = registry.lock().await.find_item(item_id).unwrap().status;
        assert_eq!(status, ItemStatus::Stopped, "a concurrent write must not be clobbered by a late probe outcome");
    }
}
