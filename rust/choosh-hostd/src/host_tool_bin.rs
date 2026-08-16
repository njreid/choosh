//! Process-wide record of the directories containing the `jj`/`zellij`
//! binaries most recently proven current by `serve.rs`'s
//! `check_host_tool_currency_once` (`mise_ops::ensure_jj`/`ensure_zellij`),
//! consulted by every `jj`/`zellij`-spawning `Command` in `jj_ops.rs`,
//! `zellij_ops.rs`, and `pty.rs` so the binary `mise` resolved as current is
//! the one that actually runs — not just whatever an ordinary `$PATH`
//! lookup would have found first.
//!
//! **Why `PATH`-prepend, not a resolved path threaded through every call
//! site's signature** (see `check_host_tool_currency_once`'s own doc
//! comment for the two options it originally deferred between): `jj_ops`'s
//! and `zellij_ops`'s functions are each called from dozens of places —
//! `rpc.rs`'s own test module alone has ~50 `zellij_ops::kill_session(&name)`
//! cleanup call sites, none of which have any reason to know or care about a
//! resolved binary path. Prepending the resolved directory onto the child's
//! own `PATH` (mirroring the `env KEY=VALUE ...` argv-prefix mechanism
//! `agent_launch.rs` already uses to reach a differently-shaped set of
//! spawned processes — that one shapes an *argv* because the target process
//! is spawned indirectly, by the Zellij session server, not directly by
//! this crate; here every call site spawns `jj`/`zellij` directly via
//! `tokio::process::Command`, so an ordinary `Command::env("PATH", ..)`
//! override reaches it with no argv change at all) keeps every one of those
//! call sites' signatures completely unchanged: `Command::new("jj")`/
//! `Command::new("zellij")` still does an ordinary `$PATH` search, it just
//! searches a `$PATH` with the mise-resolved directory listed first.
//!
//! `Mutex<Option<PathBuf>>`, not `OnceLock`: `check_host_tool_currency_once`
//! re-resolves on every `HOST_TOOL_RECHECK_INTERVAL` tick for as long as
//! `serve` runs, so the value genuinely changes over a process's lifetime —
//! unlike `mise_ops::MISE_BIN_ENV`'s fixed-for-the-process test-only
//! override. `None` (the initial value, and the value on a currency-check
//! failure — see `check_host_tool_currency_once`) means "no resolved
//! directory to prefer yet"; every call site falls back to this process's
//! own unmodified inherited `PATH`, exactly as before this module existed.
//!
//! **Test-safety note**: because this is genuine process-wide state and
//! `cargo test` runs many tests concurrently in one process, any test that
//! points this at a fake directory (`jj_ops.rs`'s and `zellij_ops.rs`'s own
//! `resolved_..._dir_is_actually_used...` tests) does so with a *transparent
//! forwarding* fake binary — one that records its own invocation and then
//! execs the real binary with the same argv — specifically so a concurrently
//! running, unrelated test that happens to spawn `jj`/`zellij` while the
//! override is active still gets fully correct real behavior, just via one
//! extra logged hop. See those tests' own fixture (`write_fake_jj_proxy`/
//! `write_fake_zellij_proxy`) for the mechanism.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Process-wide record of the most recently mise-resolved directory for
/// each host-managed tool this module tracks (`"jj"`, `"zellij"` — just two
/// entries ever, hence a plain `Vec` rather than a `HashMap`), keyed by tool
/// name rather than one static per tool. `set_jj_dir`/`set_zellij_dir` and
/// `apply_jj_path`/`apply_zellij_path` stay as the small, tool-specific
/// public surface every other module's call sites already use; only the
/// storage behind them is shared.
static RESOLVED_TOOL_DIRS: Mutex<Vec<(&'static str, PathBuf)>> = Mutex::new(Vec::new());

fn set_resolved_dir(tool: &'static str, dir: Option<PathBuf>) {
    let mut entries = RESOLVED_TOOL_DIRS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    entries.retain(|(name, _)| *name != tool);
    if let Some(dir) = dir {
        entries.push((tool, dir));
    }
}

fn resolved_dir(tool: &str) -> Option<PathBuf> {
    RESOLVED_TOOL_DIRS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(name, _)| *name == tool)
        .map(|(_, dir)| dir.clone())
}

/// Records `dir` (the parent directory of the binary path
/// [`crate::mise_ops::ensure_jj`] most recently resolved) as the directory
/// every subsequent `jj_ops.rs` `Command::new("jj")` should prefer via
/// `PATH`. `None` reverts to ordinary, unmodified `$PATH` resolution.
pub(crate) fn set_jj_dir(dir: Option<PathBuf>) {
    set_resolved_dir("jj", dir);
}

/// The `zellij` sibling of [`set_jj_dir`].
pub(crate) fn set_zellij_dir(dir: Option<PathBuf>) {
    set_resolved_dir("zellij", dir);
}

fn jj_dir() -> Option<PathBuf> {
    resolved_dir("jj")
}

fn zellij_dir() -> Option<PathBuf> {
    resolved_dir("zellij")
}

/// This process's own `PATH`, with `dir` prepended when `Some` — mirrors
/// ordinary shell-init `PATH`-prepend behavior (e.g. what `mise activate`
/// itself does), not anything cleverer: every other directory already on
/// `$PATH` remains searched, just after `dir`.
fn path_with_prepended(dir: Option<&Path>) -> OsString {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let Some(dir) = dir else { return existing };
    prepend_over(dir, &existing)
}

/// The pure, directly-testable half of [`path_with_prepended`]: `dir`
/// prepended onto an explicitly-given existing `PATH` value, rather than
/// this process's real one — lets this module's own unit test assert the
/// prepend-and-rejoin logic without mutating this process's actual `PATH`
/// (which, per this crate's `unsafe_code`-forbidding workspace lint, would
/// need the now-`unsafe`, edition-2024 `std::env::set_var` anyway).
fn prepend_over(dir: &Path, existing: &std::ffi::OsStr) -> OsString {
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(existing));
    std::env::join_paths(paths).unwrap_or_else(|_| existing.to_os_string())
}

/// Applies [`jj_dir`]'s current value (if any) to `command`'s `PATH` — the
/// one line `jj_ops.rs`'s single `Command::new("jj")` call site
/// ([`crate::jj_ops::spawn`]) needs.
pub(crate) fn apply_jj_path(command: &mut tokio::process::Command) {
    command.env("PATH", path_with_prepended(jj_dir().as_deref()));
}

/// Applies [`zellij_dir`]'s current value (if any) to `command`'s `PATH` —
/// used by `zellij_ops.rs`'s `zellij_command` (in turn every
/// `Command::new("zellij")` call site in that module, plus `pty.rs`'s
/// `PtySession::attach` via that same helper).
pub(crate) fn apply_zellij_path(command: &mut tokio::process::Command) {
    command.env("PATH", path_with_prepended(zellij_dir().as_deref()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_override_leaves_path_unchanged() {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        assert_eq!(path_with_prepended(None), existing);
    }

    #[test]
    fn an_override_is_prepended_ahead_of_the_existing_path() {
        // Use a fixed, self-contained PATH value for this assertion rather
        // than the real ambient `$PATH` — this test only needs to prove the
        // prepend-and-rejoin logic itself, not anything about this
        // process's actual environment (which `set_jj_dir`/`set_zellij_dir`
        // callers separately prove against a real spawned child, see
        // `jj_ops.rs`/`zellij_ops.rs`).
        let existing = std::env::join_paths([Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        let dir = Path::new("/fake/mise/jj-latest");
        let joined = prepend_over(dir, &existing);
        let joined_str = joined.to_str().unwrap();
        assert!(joined_str.starts_with("/fake/mise/jj-latest:"), "expected the override dir first, got {joined_str}");
        assert!(joined_str.contains("/usr/bin"));
        assert!(joined_str.contains("/bin"));
    }
}
