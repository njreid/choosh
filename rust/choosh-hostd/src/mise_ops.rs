//! The one `mise` call this crate needs for the SSH bridge's Zed
//! version-check/update path: ensuring a `mise`-managed `zed-remote-server`
//! install matches the version a connecting Zed client declares, per
//! `docs/specs/toolchain-provisioning.md`'s "Host-managed tools" section
//! and the `ubi` backend it names. This is deliberately narrow — a single
//! focused function, not a general toolchain manager — because that's all
//! `docs/specs/ssh-bridge-and-zed.md`'s "Session handling" section needs.
//!
//! Isolation from project-pinned `mise.toml` resolution
//! (toolchain-provisioning.md: "Host-managed tool resolution MUST be
//! isolated from project-pinned resolution") comes from two things
//! together: `mise`'s data directory is pointed at a dedicated,
//! `choosh-hostd`-owned directory via `MISE_DATA_DIR` (never a workspace's
//! own state), and the command runs with that same directory as its
//! current working directory, which is guaranteed to contain no
//! `mise.toml` of its own — so there is nothing for `mise` to pick up from
//! any project.

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
}

impl std::fmt::Display for MiseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "failed to run mise: {error}"),
            Self::InstallFailed { stderr } => write!(f, "mise install failed: {stderr}"),
            Self::WhereFailed { stderr } => write!(f, "mise where failed: {stderr}"),
            Self::BinaryNotFound(dir) => write!(f, "zed-remote-server binary not found under {}", dir.display()),
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
    tokio::fs::create_dir_all(host_tools_dir).await.map_err(MiseError::Spawn)?;
    let spec = tool_spec(version);

    let install_output = run_mise(mise_bin, host_tools_dir, &["install", "--yes", &spec]).await?;
    if !install_output.status.success() {
        return Err(MiseError::InstallFailed { stderr: bounded_stderr(&install_output.stderr) });
    }

    let where_output = run_mise(mise_bin, host_tools_dir, &["where", &spec]).await?;
    if !where_output.status.success() {
        return Err(MiseError::WhereFailed { stderr: bounded_stderr(&where_output.stderr) });
    }
    let install_dir = PathBuf::from(String::from_utf8_lossy(&where_output.stdout).trim());

    for candidate in [install_dir.join("zed-remote-server"), install_dir.join("bin").join("zed-remote-server")] {
        if tokio::fs::metadata(&candidate).await.is_ok() {
            return Ok(candidate);
        }
    }
    Err(MiseError::BinaryNotFound(install_dir))
}

async fn run_mise(mise_bin: &str, host_tools_dir: &Path, args: &[&str]) -> Result<std::process::Output, MiseError> {
    Command::new(mise_bin)
        .args(args)
        .current_dir(host_tools_dir)
        .env("MISE_DATA_DIR", host_tools_dir.join("data"))
        .env("MISE_CONFIG_DIR", host_tools_dir.join("config"))
        .env("MISE_CACHE_DIR", host_tools_dir.join("cache"))
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(MiseError::Spawn)
}

fn bounded_stderr(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    if text.len() > MAX_STDERR_BYTES { format!("{}... (truncated)", &text[..MAX_STDERR_BYTES]) } else { text.into_owned() }
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
}
