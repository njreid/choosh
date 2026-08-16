//! `mise` shell-out plumbing for both tiers of
//! `docs/specs/toolchain-provisioning.md`:
//!
//! - **Host-managed tools** (`zed-remote-server`, `jj`, `zellij`): not
//!   project state — installed/updated against a dedicated,
//!   `choosh-hostd`-owned `mise` data directory that no project's
//!   `mise.toml` can influence. [`ensure_zed_remote_server`] is the
//!   original, narrowly-scoped M6 function for the SSH bridge's per-
//!   connection Zed version check (`docs/specs/ssh-bridge-and-zed.md`'s
//!   "Session handling"); [`ensure_jj`]/[`ensure_zellij`] extend the same
//!   shape to the two tools `serve.rs` checks on daemon start and
//!   periodically thereafter.
//! - **Project-pinned toolchains**: [`ensure_project_toolchain`]/
//!   [`project_env`] run `mise install`/`mise env` against a *workspace's*
//!   own `mise.toml` (if it has one — see [`has_mise_toml`]), for
//!   `rpc::handle_create`/`rpc::handle_item_create` to drive per
//!   `docs/specs/toolchain-provisioning.md`'s "Project-pinned toolchains"
//!   section.
//!
//! Isolation between the two tiers, and between concurrent workspaces
//! within the project-pinned tier (toolchain-provisioning.md: "Host-managed
//! tool resolution MUST be isolated from project-pinned resolution" /
//! "MUST NOT leak into... any other concurrently open workspace"), comes
//! from the same mechanism used throughout this module: every `mise`
//! invocation gets its own `MISE_DATA_DIR`/`MISE_CONFIG_DIR`/
//! `MISE_CACHE_DIR` scoped to a caller-supplied directory, and every
//! resolved environment is handed straight to a specific spawned child
//! (or returned to a caller to do the same) — never written into this
//! process's own environment (no `std::env::set_var` anywhere in this
//! module) and never cached across calls, so nothing here can bleed
//! between workspaces or into `choosh-hostd` itself. Host-managed tools use
//! one shared `host_tools_dir` (a project's `mise.toml` cannot affect it —
//! nothing ever runs there with a workspace as its current directory);
//! project-pinned tools use a separate shared `project_tools_dir` for
//! installed-tool *payloads* (ordinary `mise` behavior: one cache of
//! `node@20`'s actual bytes, reused by every project pinning it — sharing
//! this is not a leak, since it holds no per-project *values*, only
//! installed binaries keyed by tool+version), while each call's *resolved
//! environment* is always computed fresh against that one call's
//! `workspace_root`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

/// Overrides the `mise` binary invoked — production code never sets this
/// (it resolves to `PATH`'s `mise`, per [`DEFAULT_MISE_BIN`]); tests point
/// it at a fixture script that fakes `install`/`where` without needing a
/// real network fetch or a real `zed-remote-server` release.
pub const MISE_BIN_ENV: &str = "CHOOSH_HOSTD_MISE_BIN";
const DEFAULT_MISE_BIN: &str = "mise";

/// Bounded capture for a failed `mise` invocation's stderr — enough to
/// diagnose, not an unbounded blob in logs/error messages.
const MAX_STDERR_BYTES: usize = 4096;

#[derive(Debug)]
pub enum MiseError {
    Spawn(std::io::Error),
    InstallFailed { stderr: String },
    WhereFailed { stderr: String },
    /// `mise where` succeeded but printed nothing useful, or the binary
    /// isn't present at any path this module knows to check under the
    /// directory it printed — per toolchain-provisioning.md's failure
    /// rule, this MUST fail the connection attempt, not fall back to a
    /// stale cached binary.
    BinaryNotFound(PathBuf),
    /// [`project_env`]: `mise env --json` itself exited non-zero.
    EnvFailed { stderr: String },
    /// [`project_env`]: `mise env --json` exited zero but its stdout
    /// wasn't the `{"KEY": "VALUE", ...}` shape `--json` documents —
    /// treated as a distinct failure rather than silently returning an
    /// empty/partial environment to a spawned process.
    EnvParseFailed(String),
}

impl std::fmt::Display for MiseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "failed to run mise: {error}"),
            Self::InstallFailed { stderr } => write!(f, "mise install failed: {stderr}"),
            Self::WhereFailed { stderr } => write!(f, "mise where failed: {stderr}"),
            Self::BinaryNotFound(dir) => write!(f, "zed-remote-server binary not found under {}", dir.display()),
            Self::EnvFailed { stderr } => write!(f, "mise env failed: {stderr}"),
            Self::EnvParseFailed(reason) => write!(f, "could not parse mise env --json output: {reason}"),
        }
    }
}

impl std::error::Error for MiseError {}

/// Reads [`MISE_BIN_ENV`], defaulting to `"mise"` (resolved via `PATH`) if
/// unset — the production call site's way of picking up a test override
/// without every call site reading the environment itself.
#[must_use]
pub fn mise_bin_from_env() -> String {
    std::env::var(MISE_BIN_ENV).unwrap_or_else(|_| DEFAULT_MISE_BIN.to_string())
}

/// The `ubi` tool spec for a given `zed-remote-server` version, per
/// toolchain-provisioning.md's documented (if not yet independently
/// verified against a live `mise use ubi:...` call) syntax.
fn tool_spec(version: &str) -> String {
    format!("ubi:zed-industries/zed[exe=zed-remote-server]@{version}")
}

/// Ensures `zed-remote-server@<version>` is installed under `host_tools_dir`
/// (a `choosh-hostd`-owned, workspace-independent `mise` data directory —
/// see this module's doc comment) via `mise`'s `ubi` backend, and returns
/// the resolved binary's path.
///
/// # Errors
///
/// Returns [`MiseError::InstallFailed`]/[`MiseError::WhereFailed`] if the
/// underlying `mise install`/`mise where` invocation exits non-zero,
/// [`MiseError::Spawn`] if `mise_bin` cannot even be executed, and
/// [`MiseError::BinaryNotFound`] if `mise where` succeeds but no
/// `zed-remote-server` executable exists at the path it reports.
pub async fn ensure_zed_remote_server(mise_bin: &str, version: &str, host_tools_dir: &Path) -> Result<PathBuf, MiseError> {
    resolve_host_managed_tool(mise_bin, host_tools_dir, &tool_spec(version), "zed-remote-server").await
}

/// Shared by [`ensure_zed_remote_server`] and [`ensure_host_tool`] (in turn
/// [`ensure_jj`]/[`ensure_zellij`]): `mise install --yes <spec>` then
/// `mise where <spec>`, resolving the installed binary the same
/// two-candidate-path way for both — some `mise` backends lay a tool's
/// binary directly under its install directory, others under a `bin/`
/// subdirectory (confirmed empirically for `jj`/`zellij`'s aqua backend to
/// be the former, but checking both keeps this resilient to a backend
/// change rather than silently breaking). `spec` is the full `mise`
/// tool-spec string (`ubi:...@<version>` for zed, `<tool>@latest` for a
/// host-managed tool); `binary_name` is the executable's own file name to
/// look for under the resolved install directory, which is not always the
/// same string as `spec` names (zed's `ubi` spec always resolves to a
/// `zed-remote-server` binary regardless of the declared version).
///
/// # Errors
///
/// Returns [`MiseError::InstallFailed`]/[`MiseError::WhereFailed`] if the
/// underlying `mise install`/`mise where` invocation exits non-zero,
/// [`MiseError::Spawn`] if `mise_bin` cannot even be executed, and
/// [`MiseError::BinaryNotFound`] if `mise where` succeeds but no
/// `binary_name` executable exists at the path it reports.
async fn resolve_host_managed_tool(mise_bin: &str, host_tools_dir: &Path, spec: &str, binary_name: &str) -> Result<PathBuf, MiseError> {
    tokio::fs::create_dir_all(host_tools_dir).await.map_err(MiseError::Spawn)?;

    let install_output = run_mise(mise_bin, host_tools_dir, host_tools_dir, &["install", "--yes", spec]).await?;
    if !install_output.status.success() {
        return Err(MiseError::InstallFailed { stderr: bounded_stderr(&install_output.stderr) });
    }

    let where_output = run_mise(mise_bin, host_tools_dir, host_tools_dir, &["where", spec]).await?;
    if !where_output.status.success() {
        return Err(MiseError::WhereFailed { stderr: bounded_stderr(&where_output.stderr) });
    }
    let install_dir = PathBuf::from(String::from_utf8_lossy(&where_output.stdout).trim());

    for candidate in [install_dir.join(binary_name), install_dir.join("bin").join(binary_name)] {
        if tokio::fs::metadata(&candidate).await.is_ok() {
            return Ok(candidate);
        }
    }
    Err(MiseError::BinaryNotFound(install_dir))
}

/// Shared by every `mise` invocation in this module (both tiers —
/// `resolve_host_managed_tool` for the host-managed tier,
/// [`ensure_project_toolchain`]/[`project_env`] for the project-pinned
/// tier): same three `MISE_DATA_DIR`/`MISE_CONFIG_DIR`/`MISE_CACHE_DIR`
/// overrides scoped to `tools_dir`, same `stdin(Stdio::null())`. The two
/// tiers differ only in what `cwd` and `tools_dir` actually are: the
/// host-managed tier passes the same `host_tools_dir` for both (nothing
/// ever runs there with a workspace as its current directory, per this
/// module's doc comment), while the project-pinned tier passes a
/// workspace's own `workspace_root` as `cwd` (so `mise` discovers *that*
/// directory's `mise.toml` via its ordinary upward config search —
/// confirmed against the real binary that `MISE_CONFIG_DIR`, which governs
/// `mise`'s *global* config, does not affect this local discovery) and the
/// shared `project_tools_dir` as `tools_dir`. This pair of facts — `cwd`
/// possibly distinct from `tools_dir`, and `tools_dir` always one of two
/// entirely separate trees — is what gives project-pinned resolution its
/// isolation from host-managed resolution (see this module's doc comment).
async fn run_mise(mise_bin: &str, cwd: &Path, tools_dir: &Path, args: &[&str]) -> Result<std::process::Output, MiseError> {
    Command::new(mise_bin)
        .args(args)
        .current_dir(cwd)
        .env("MISE_DATA_DIR", tools_dir.join("data"))
        .env("MISE_CONFIG_DIR", tools_dir.join("config"))
        .env("MISE_CACHE_DIR", tools_dir.join("cache"))
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(MiseError::Spawn)
}

fn bounded_stderr(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    if text.len() > MAX_STDERR_BYTES { format!("{}... (truncated)", &text[..MAX_STDERR_BYTES]) } else { text.into_owned() }
}

// ---------------------------------------------------------------------
// Project-pinned toolchains (toolchain-provisioning.md's first tier).
// ---------------------------------------------------------------------

/// Whether `workspace_root` declares a Project-pinned `mise.toml`, per
/// toolchain-provisioning.md: "Each Project MAY declare a `mise.toml`...
/// at its repository root." A workspace with none of these MUST NOT have
/// `mise install` attempted against it and MUST NOT get any injected
/// environment beyond the host's own ambient environment — this is the
/// single gate callers (`rpc::handle_create`/`rpc::handle_item_create`)
/// use to decide whether to touch `mise` for a given workspace at all.
#[must_use]
pub fn has_mise_toml(workspace_root: &Path) -> bool {
    workspace_root.join("mise.toml").is_file()
}

/// Runs `mise install` against `workspace_root`'s own `mise.toml`, for
/// `rpc::handle_create` to call before a `workspace.create` carrying a
/// Project-pinned `mise.toml` is reported ready. `project_tools_dir` is a
/// `choosh-hostd`-owned directory shared across every workspace on this
/// host for installed-tool *payloads* only (see this module's doc comment
/// for why sharing that is not itself a leak) — never the same directory
/// [`ensure_jj`]/[`ensure_zellij`]/[`ensure_zed_remote_server`] use, per
/// toolchain-provisioning.md's tier-isolation requirement.
///
/// Deliberately does not also read back `mise env` — callers that need the
/// resolved environment call [`project_env`] separately (`handle_create`
/// only needs to know install succeeded; `handle_item_create`, run once
/// per spawned process, needs the environment itself).
///
/// # Errors
///
/// Returns [`MiseError::InstallFailed`] if `mise install` exits non-zero
/// (an unresolvable tool name/version, a network hiccup, etc. — per
/// toolchain-provisioning.md's Failure handling, this MUST fail the
/// `workspace.create` it's called from) and [`MiseError::Spawn`] if
/// `mise_bin` can't even be executed.
pub async fn ensure_project_toolchain(mise_bin: &str, workspace_root: &Path, project_tools_dir: &Path) -> Result<(), MiseError> {
    tokio::fs::create_dir_all(project_tools_dir).await.map_err(MiseError::Spawn)?;
    let install_output = run_mise(mise_bin, workspace_root, project_tools_dir, &["install", "--yes"]).await?;
    if !install_output.status.success() {
        return Err(MiseError::InstallFailed { stderr: bounded_stderr(&install_output.stderr) });
    }
    Ok(())
}

/// Resolves `mise env --json` against `workspace_root`'s own `mise.toml`,
/// as `KEY=VALUE` pairs ready to inject into a spawned process — per
/// toolchain-provisioning.md: "Every agent, service, and shell process
/// `choosh-hostd` spawns inside that workspace MUST have `mise env` for
/// that workspace's directory injected into its environment." Only
/// meaningful to call after [`ensure_project_toolchain`] has succeeded at
/// least once for this `workspace_root` (an uninstalled tool has nothing
/// for `mise env` to resolve a useful `PATH` entry against); callers that
/// already gated on [`has_mise_toml`] and a successful
/// [`ensure_project_toolchain`] don't need to re-check anything here.
///
/// Returns an empty `Vec` (not an error) if `workspace_root` has no
/// `mise.toml` at all — `mise env --json` with nothing pinned legitimately
/// resolves to "nothing to add", so callers that skip the [`has_mise_toml`]
/// gate entirely still get correct (if slightly less efficient) behavior.
///
/// # Errors
///
/// Returns [`MiseError::EnvFailed`] if `mise env --json` exits non-zero,
/// [`MiseError::EnvParseFailed`] if its stdout isn't the flat
/// `{"KEY": "VALUE", ...}` object `--json` documents, and
/// [`MiseError::Spawn`] if `mise_bin` can't even be executed.
pub async fn project_env(mise_bin: &str, workspace_root: &Path, project_tools_dir: &Path) -> Result<Vec<(String, String)>, MiseError> {
    let env_output = run_mise(mise_bin, workspace_root, project_tools_dir, &["env", "--json"]).await?;
    if !env_output.status.success() {
        return Err(MiseError::EnvFailed { stderr: bounded_stderr(&env_output.stderr) });
    }
    let map: std::collections::BTreeMap<String, String> =
        serde_json::from_slice(&env_output.stdout).map_err(|error| MiseError::EnvParseFailed(error.to_string()))?;
    Ok(map.into_iter().collect())
}

// ---------------------------------------------------------------------
// Host-managed tools (toolchain-provisioning.md's second tier): jj and
// zellij, checked on daemon start and periodically thereafter by
// `serve.rs`. Both ship real, dedicated `mise` plugins (`mise registry`
// lists `jj` and `zellij`, both aqua-backed — confirmed against the real
// `mise` binary in this environment), so unlike `zed-remote-server` these
// don't need the `ubi` backend: a plain `mise install <tool>@latest`
// resolves and fetches a real prebuilt release binary directly.
// ---------------------------------------------------------------------

/// Ensures the latest `jj` is installed under `host_tools_dir` and returns
/// its resolved binary path — "checked... periodically" per
/// toolchain-provisioning.md's Host-managed tools section.
///
/// # Errors
///
/// See [`ensure_zed_remote_server`]'s doc comment — the same error shapes
/// apply here (this function shares its underlying implementation).
pub async fn ensure_jj(mise_bin: &str, host_tools_dir: &Path) -> Result<PathBuf, MiseError> {
    ensure_host_tool(mise_bin, "jj", host_tools_dir).await
}

/// The `zellij` sibling of [`ensure_jj`] — see its doc comment.
///
/// # Errors
///
/// See [`ensure_jj`].
pub async fn ensure_zellij(mise_bin: &str, host_tools_dir: &Path) -> Result<PathBuf, MiseError> {
    ensure_host_tool(mise_bin, "zellij", host_tools_dir).await
}

/// Shared implementation for [`ensure_jj`]/[`ensure_zellij`], via
/// [`resolve_host_managed_tool`] (also shared with [`ensure_zed_remote_server`]
/// — see that function's doc comment for the parts of the shape all three
/// have in common).
///
/// Always resolves `@latest` — per toolchain-provisioning.md, `jj`/
/// `zellij` "are not project state... kept current by `choosh-hostd`", the
/// opposite currency policy from a project's own pinned `mise.toml`
/// (which this function has no connection to at all: distinct
/// `host_tools_dir` tree, no `workspace_root`/`current_dir` involved).
async fn ensure_host_tool(mise_bin: &str, tool_name: &str, host_tools_dir: &Path) -> Result<PathBuf, MiseError> {
    resolve_host_managed_tool(mise_bin, host_tools_dir, &format!("{tool_name}@latest"), tool_name).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Writes a fake `mise` executable that fakes just the two
    /// subcommands [`ensure_zed_remote_server`] uses: `install` (a no-op
    /// success) and `where` (prints a fixed install directory containing a
    /// fake `zed-remote-server` executable this test also creates) —
    /// enough to prove the real call sequence and argument shape without
    /// a real network fetch or a real Zed release binary.
    fn write_fake_mise(dir: &std::path::Path, install_dir: &std::path::Path, fail_install: bool) -> PathBuf {
        let script_path = dir.join("mise");
        let body = format!(
            "#!/bin/sh\nset -e\ncase \"$1\" in\n  install)\n    echo \"$@\" >> \"{dir}/mise-calls.log\"\n    {install_behavior}\n    ;;\n  where)\n    echo \"$@\" >> \"{dir}/mise-calls.log\"\n    echo '{install_dir}'\n    ;;\n  *)\n    echo \"unknown subcommand: $1\" >&2\n    exit 1\n    ;;\nesac\n",
            dir = dir.display(),
            install_dir = install_dir.display(),
            install_behavior = if fail_install { "echo 'boom' >&2; exit 1" } else { "exit 0" },
        );
        std::fs::write(&script_path, body).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        script_path
    }

    #[tokio::test]
    async fn ensure_zed_remote_server_resolves_the_binary_a_fake_mise_reports() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path().join("installs").join("zed-remote-server-0.190.0");
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::write(install_dir.join("zed-remote-server"), b"#!/bin/sh\necho fake\n").unwrap();
        let mise_bin = write_fake_mise(dir.path(), &install_dir, false);
        let host_tools_dir = dir.path().join("host-tools");

        let resolved = ensure_zed_remote_server(mise_bin.to_str().unwrap(), "0.190.0", &host_tools_dir).await.unwrap();
        assert_eq!(resolved, install_dir.join("zed-remote-server"));

        let calls = std::fs::read_to_string(dir.path().join("mise-calls.log")).unwrap();
        assert!(calls.contains("install --yes ubi:zed-industries/zed[exe=zed-remote-server]@0.190.0"));
        assert!(calls.contains("where ubi:zed-industries/zed[exe=zed-remote-server]@0.190.0"));
    }

    #[tokio::test]
    async fn ensure_zed_remote_server_finds_binary_under_a_bin_subdirectory_too() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path().join("installs").join("zed-remote-server-0.190.0");
        std::fs::create_dir_all(install_dir.join("bin")).unwrap();
        std::fs::write(install_dir.join("bin").join("zed-remote-server"), b"#!/bin/sh\necho fake\n").unwrap();
        let mise_bin = write_fake_mise(dir.path(), &install_dir, false);
        let host_tools_dir = dir.path().join("host-tools");

        let resolved = ensure_zed_remote_server(mise_bin.to_str().unwrap(), "0.190.0", &host_tools_dir).await.unwrap();
        assert_eq!(resolved, install_dir.join("bin").join("zed-remote-server"));
    }

    #[tokio::test]
    async fn install_failure_surfaces_stderr_rather_than_silently_falling_back() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path().join("installs").join("zed-remote-server-0.190.0");
        let mise_bin = write_fake_mise(dir.path(), &install_dir, true);
        let host_tools_dir = dir.path().join("host-tools");

        let result = ensure_zed_remote_server(mise_bin.to_str().unwrap(), "0.190.0", &host_tools_dir).await;
        match result {
            Err(MiseError::InstallFailed { stderr }) => assert!(stderr.contains("boom")),
            other => panic!("expected InstallFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_binary_after_a_successful_where_is_a_distinct_error() {
        let dir = tempfile::tempdir().unwrap();
        // `where` reports a directory, but no zed-remote-server executable
        // is ever placed inside it — simulates a mismatched/unexpected
        // `ubi` layout, which per toolchain-provisioning.md's failure rule
        // MUST fail rather than silently proceed.
        let install_dir = dir.path().join("installs").join("empty");
        std::fs::create_dir_all(&install_dir).unwrap();
        let mise_bin = write_fake_mise(dir.path(), &install_dir, false);
        let host_tools_dir = dir.path().join("host-tools");

        let result = ensure_zed_remote_server(mise_bin.to_str().unwrap(), "0.190.0", &host_tools_dir).await;
        assert!(matches!(result, Err(MiseError::BinaryNotFound(_))), "expected BinaryNotFound, got {result:?}");
    }

    // -------------------------------------------------------------
    // Project-pinned toolchains
    // -------------------------------------------------------------

    #[test]
    fn has_mise_toml_is_a_plain_file_check() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_mise_toml(dir.path()));
        std::fs::write(dir.path().join("mise.toml"), b"[tools]\n").unwrap();
        assert!(has_mise_toml(dir.path()));
    }

    /// A fake `mise` for the project-pinned tier: `install` succeeds unless
    /// a `FAIL_INSTALL` marker file exists in the directory it's invoked
    /// against (i.e. the workspace root, since `run_mise_project` sets
    /// `current_dir` there — this fixture leans on that to simulate an
    /// unresolvable tool/version without needing a real one), and
    /// `env --json` echoes back whatever JSON object is in that same
    /// directory's `env.json` file (a stand-in for "whatever this
    /// workspace's `mise.toml` would actually resolve to").
    fn write_fake_project_mise(dir: &std::path::Path, log_path: &std::path::Path) -> PathBuf {
        let script_path = dir.join("mise");
        let body = format!(
            "#!/bin/sh\nset -e\ncase \"$1\" in\n  install)\n    echo \"install cwd=$(pwd)\" >> \"{log}\"\n    if [ -f \"$(pwd)/FAIL_INSTALL\" ]; then echo 'boom: cannot resolve tool' >&2; exit 1; fi\n    exit 0\n    ;;\n  env)\n    echo \"env cwd=$(pwd)\" >> \"{log}\"\n    cat \"$(pwd)/env.json\"\n    ;;\n  *)\n    echo \"unknown subcommand: $1\" >&2\n    exit 1\n    ;;\nesac\n",
            log = log_path.display(),
        );
        std::fs::write(&script_path, body).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        script_path
    }

    #[tokio::test]
    async fn ensure_project_toolchain_then_project_env_reports_the_fixtures_resolved_environment() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspace-a");
        std::fs::create_dir_all(&workspace_root).unwrap();
        std::fs::write(workspace_root.join("mise.toml"), b"[tools]\nnode = \"20\"\n").unwrap();
        std::fs::write(workspace_root.join("env.json"), br#"{"CHOOSH_MISE_MARKER": "workspace-a-value"}"#).unwrap();
        let log_path = dir.path().join("mise-calls.log");
        let mise_bin = write_fake_project_mise(dir.path(), &log_path);
        let project_tools_dir = dir.path().join("project-tools");

        assert!(has_mise_toml(&workspace_root));
        ensure_project_toolchain(mise_bin.to_str().unwrap(), &workspace_root, &project_tools_dir).await.unwrap();
        let env = project_env(mise_bin.to_str().unwrap(), &workspace_root, &project_tools_dir).await.unwrap();
        assert_eq!(env, vec![("CHOOSH_MISE_MARKER".to_string(), "workspace-a-value".to_string())]);

        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains(&format!("install cwd={}", workspace_root.display())), "install must run with the workspace root as cwd, got: {log}");
        assert!(log.contains(&format!("env cwd={}", workspace_root.display())), "env must run with the workspace root as cwd, got: {log}");
    }

    #[tokio::test]
    async fn unresolvable_project_toolchain_fails_install_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let workspace_root = dir.path().join("workspace-bad");
        std::fs::create_dir_all(&workspace_root).unwrap();
        std::fs::write(workspace_root.join("mise.toml"), b"[tools]\nnode = \"not-a-real-version\"\n").unwrap();
        std::fs::write(workspace_root.join("FAIL_INSTALL"), b"").unwrap();
        let mise_bin = write_fake_project_mise(dir.path(), &dir.path().join("mise-calls.log"));
        let project_tools_dir = dir.path().join("project-tools");

        let result = ensure_project_toolchain(mise_bin.to_str().unwrap(), &workspace_root, &project_tools_dir).await;
        match result {
            Err(MiseError::InstallFailed { stderr }) => assert!(stderr.contains("boom")),
            other => panic!("expected InstallFailed, got {other:?}"),
        }
    }

    /// Two workspaces resolving through the *same* fake `mise` binary and
    /// the *same* shared `project_tools_dir` (exactly as `rpc.rs` does in
    /// production — installed-tool payloads are shared, per this module's
    /// doc comment) still get their own, non-leaking resolved environments,
    /// because each call is scoped to its own `workspace_root` as `cwd`.
    #[tokio::test]
    async fn concurrent_workspaces_resolve_distinct_environments_via_a_shared_mise_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mise_bin = write_fake_project_mise(dir.path(), &dir.path().join("mise-calls.log"));
        let project_tools_dir = dir.path().join("shared-project-tools");

        let workspace_a = dir.path().join("workspace-a");
        std::fs::create_dir_all(&workspace_a).unwrap();
        std::fs::write(workspace_a.join("mise.toml"), b"[tools]\nnode = \"20\"\n").unwrap();
        std::fs::write(workspace_a.join("env.json"), br#"{"CHOOSH_MISE_MARKER": "value-a"}"#).unwrap();

        let workspace_b = dir.path().join("workspace-b");
        std::fs::create_dir_all(&workspace_b).unwrap();
        std::fs::write(workspace_b.join("mise.toml"), b"[tools]\nnode = \"18\"\n").unwrap();
        std::fs::write(workspace_b.join("env.json"), br#"{"CHOOSH_MISE_MARKER": "value-b"}"#).unwrap();

        let (env_a, env_b) = tokio::join!(
            project_env(mise_bin.to_str().unwrap(), &workspace_a, &project_tools_dir),
            project_env(mise_bin.to_str().unwrap(), &workspace_b, &project_tools_dir),
        );
        assert_eq!(env_a.unwrap(), vec![("CHOOSH_MISE_MARKER".to_string(), "value-a".to_string())]);
        assert_eq!(env_b.unwrap(), vec![("CHOOSH_MISE_MARKER".to_string(), "value-b".to_string())]);
    }

    // -------------------------------------------------------------
    // Host-managed tools: jj / zellij
    // -------------------------------------------------------------

    /// A fake `mise` for `ensure_jj`/`ensure_zellij`'s `<tool>@latest`
    /// shape — same `install`/`where` structure `write_fake_mise` uses for
    /// `ensure_zed_remote_server`, just without a version in the spec.
    fn write_fake_host_tool_mise(dir: &std::path::Path, tool_name: &str, install_dir: &std::path::Path) -> PathBuf {
        let script_path = dir.join("mise");
        let body = format!(
            "#!/bin/sh\nset -e\ncase \"$1\" in\n  install)\n    [ \"$3\" = \"{tool}@latest\" ] || {{ echo \"unexpected spec: $3\" >&2; exit 1; }}\n    exit 0\n    ;;\n  where)\n    [ \"$2\" = \"{tool}@latest\" ] || {{ echo \"unexpected spec: $2\" >&2; exit 1; }}\n    echo '{install_dir}'\n    ;;\n  *)\n    echo \"unknown subcommand: $1\" >&2\n    exit 1\n    ;;\nesac\n",
            tool = tool_name,
            install_dir = install_dir.display(),
        );
        std::fs::write(&script_path, body).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        script_path
    }

    #[tokio::test]
    async fn ensure_jj_resolves_the_binary_a_fake_mise_reports_for_jj_at_latest() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path().join("installs").join("jj-latest");
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::write(install_dir.join("jj"), b"#!/bin/sh\necho fake-jj\n").unwrap();
        let mise_bin = write_fake_host_tool_mise(dir.path(), "jj", &install_dir);
        let host_tools_dir = dir.path().join("host-tools");

        let resolved = ensure_jj(mise_bin.to_str().unwrap(), &host_tools_dir).await.unwrap();
        assert_eq!(resolved, install_dir.join("jj"));
    }

    #[tokio::test]
    async fn ensure_zellij_resolves_the_binary_a_fake_mise_reports_for_zellij_at_latest() {
        let dir = tempfile::tempdir().unwrap();
        let install_dir = dir.path().join("installs").join("zellij-latest");
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::write(install_dir.join("zellij"), b"#!/bin/sh\necho fake-zellij\n").unwrap();
        let mise_bin = write_fake_host_tool_mise(dir.path(), "zellij", &install_dir);
        let host_tools_dir = dir.path().join("host-tools");

        let resolved = ensure_zellij(mise_bin.to_str().unwrap(), &host_tools_dir).await.unwrap();
        assert_eq!(resolved, install_dir.join("zellij"));
    }

    /// The one place these tests exercise the *real* `mise` binary
    /// installed in this environment rather than a fixture, per this
    /// task's verification requirement — proves the fixture scripts above
    /// aren't quietly diverging from real `mise install --yes <tool>@latest`
    /// / `mise where <tool>@latest` behavior. Ignored by default (real
    /// network fetch of a real release binary); run explicitly with
    /// `cargo test -- --ignored` when verifying against a live `mise`.
    #[tokio::test]
    #[ignore = "hits the network via a real mise install; run explicitly, not part of the default suite"]
    async fn ensure_jj_against_the_real_mise_binary_installs_a_working_jj() {
        let dir = tempfile::tempdir().unwrap();
        let host_tools_dir = dir.path().join("host-tools");
        let resolved = ensure_jj("mise", &host_tools_dir).await.unwrap();
        let output = std::process::Command::new(&resolved).arg("--version").output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("jj "), "unexpected `jj --version` output: {output:?}");
    }
}
