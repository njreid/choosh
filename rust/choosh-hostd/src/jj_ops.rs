//! `jj` operations for workspace creation and status, per
//! `docs/specs/jj-integration.md` and `docs/milestones/M1-workspace-and-jj.md`.
//!
//! ## A deliberate, reported deviation from `jj-lib`
//!
//! `DESIGN.md` and `jj-integration.md` prefer linking `jj-lib` directly
//! over shelling out to the `jj` CLI, for the same anti-string-parsing
//! reason the pre-relay design gave for `git`. `jj-lib = "0.44.0"` is a
//! real dependency of this crate and does compile cleanly here. This
//! module does not use its programmatic API, though: `jj-lib`'s public
//! surface (`Workspace`, `RepoLoader`, `MergedTree`, etc.) is oriented
//! around building a CLI like `jj` itself — clone-from-remote, workspace
//! creation, and diff-summary are all reachable through it in principle,
//! but assembling that correctly (remote transport setup, working-copy
//! snapshotting, tree-diff traversal) is substantially more work than a
//! single-pass increment can responsibly cover and verify. This directive
//! explicitly permits a CLI fallback for exactly this situation ("if
//! `jj-lib`'s public API doesn't cleanly support that specific operation
//! ... report clearly which approach you used and why") — this module
//! takes that fallback for every `jj`-touching operation, not just clone.
//! What's still worth the tradeoff: every invocation here is fixed
//! executable + a fully-encoded argv (never a shell string, per
//! `host-rpc.md`'s "Command construction"), and `jj diff --summary`'s
//! output format (verified against the real `jj 0.44.0` binary installed
//! in this environment) is a small, single-character-status-plus-path
//! format per line — not the free-form human diff output the pre-relay
//! design correctly avoided parsing. Replacing this module's internals
//! with real `jj-lib` calls behind the same function signatures is a
//! reasonable, scoped follow-up that wouldn't need to touch any caller.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

#[derive(Debug)]
pub enum JjError {
    Spawn(std::io::Error),
    CommandFailed { argv: Vec<String>, stderr: String },
    UnparseableOutput(String),
}

impl std::fmt::Display for JjError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "failed to spawn jj: {error}"),
            // Deliberately not including `stderr` verbatim in the Display
            // impl a caller might surface to an RPC error message — jj's
            // stderr can echo back paths/URLs the caller supplied, and
            // host-rpc.md requires error messages never leak that; callers
            // that need the detail for their own logs use the `stderr`
            // field directly, not this trait.
            Self::CommandFailed { argv, .. } => write!(f, "jj command failed: {}", argv.join(" ")),
            Self::UnparseableOutput(reason) => write!(f, "could not parse jj output: {reason}"),
        }
    }
}

impl std::error::Error for JjError {}

async fn run(args: &[&str], cwd: Option<&Path>) -> Result<String, JjError> {
    let mut command = Command::new("jj");
    command.args(args).arg("--no-pager").arg("--color=never").stdin(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command.output().await.map_err(JjError::Spawn)?;
    if !output.status.success() {
        return Err(JjError::CommandFailed {
            argv: args.iter().map(|s| (*s).to_string()).collect(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    String::from_utf8(output.stdout).map_err(|error| JjError::UnparseableOutput(error.to_string()))
}

fn path_arg(path: &Path) -> Result<&str, JjError> {
    path.to_str()
        .ok_or_else(|| JjError::UnparseableOutput(format!("path is not valid UTF-8: {}", path.display())))
}

/// `jj git clone <url> <dest>`, for `workspace.create`'s fresh-`clone_url`
/// path. `dest`'s parent MUST already exist and be writable; `dest` itself
/// MUST NOT already exist (jj creates it).
///
/// # Errors
///
/// See [`JjError`].
pub async fn clone(clone_url: &str, dest: &Path) -> Result<(), JjError> {
    run(&["git", "clone", clone_url, path_arg(dest)?], None).await?;
    Ok(())
}

/// Colocates an existing plain Git repo at `path` with a `jj` repo
/// (`jj git init`), for `workspace.create`'s `existing_path` adoption
/// path. A no-op (not an error) if `path` is already a `jj` repo.
///
/// # Errors
///
/// See [`JjError`].
pub async fn ensure_colocated(path: &Path) -> Result<(), JjError> {
    if path.join(".jj").is_dir() {
        return Ok(());
    }
    run(&["git", "init"], Some(path)).await?;
    Ok(())
}

/// `jj workspace rename <name>`, run against the repo at `path` — used
/// after [`clone`]/[`ensure_colocated`] when the registered
/// `workspace_name` differs from `jj`'s default workspace name (the
/// destination directory's basename).
///
/// # Errors
///
/// See [`JjError`].
pub async fn rename_workspace(path: &Path, new_name: &str) -> Result<(), JjError> {
    run(&["workspace", "rename", "-R", path_arg(path)?, new_name], None).await?;
    Ok(())
}

/// `jj workspace add <dest> --name <name> -R <existing_repo_root>`, for the
/// "one `jj workspace` per agent" mechanism (`jj-integration.md`) when
/// `workspace.create`'s `parent_workspace_id` is set.
///
/// # Errors
///
/// See [`JjError`].
pub async fn workspace_add(existing_repo_root: &Path, dest: &Path, name: &str) -> Result<(), JjError> {
    run(&["workspace", "add", path_arg(dest)?, "--name", name, "-R", path_arg(existing_repo_root)?], None).await?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusEntry {
    pub kind: ChangeKind,
    pub path: String,
}

/// Changed paths for `@` vs `@-`, via `jj diff --summary -r @`.
///
/// Conflict flagging is a deliberate gap in this pass: `jj diff --summary`
/// does not surface conflict state per-path in an easily-machine-parsed
/// way the way it does add/modify/delete, and getting that right (matching
/// `jj-integration.md`'s "structural, not text markers" requirement)
/// deserves its own verified increment rather than a guess here — this
/// function always returns an empty conflicted set; `workspace.status`'s
/// caller in `rpc.rs` reports that honestly rather than papering over it.
///
/// # Errors
///
/// Returns [`JjError::UnparseableOutput`] if a line doesn't match the
/// expected `<CHAR> <path>` shape (a real format change upstream, not
/// something to silently ignore).
pub async fn status(workspace_root: &Path) -> Result<Vec<StatusEntry>, JjError> {
    // `-R <path>` selects which repo `jj` operates on, but does NOT make it
    // print paths relative to that repo — paths in `diff --summary` output
    // are always relative to the process's actual working directory. With
    // `cwd: None` and only `-R`, this printed `../../../tmp/xyz/a.txt`
    // instead of `a.txt` whenever the caller's cwd wasn't the repo itself,
    // which `parse_diff_summary` correctly refused to match against a bare
    // path — the fix is running *in* the workspace root, not pointing at it.
    let output = run(&["diff", "--summary", "-r", "@"], Some(workspace_root)).await?;
    parse_diff_summary(&output)
}

fn parse_diff_summary(text: &str) -> Result<Vec<StatusEntry>, JjError> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (marker, path) = line
                .split_once(' ')
                .ok_or_else(|| JjError::UnparseableOutput(format!("no status/path separator in {line:?}")))?;
            let kind = match marker {
                "A" => ChangeKind::Added,
                "M" => ChangeKind::Modified,
                "D" => ChangeKind::Deleted,
                other => {
                    return Err(JjError::UnparseableOutput(format!("unrecognized status marker {other:?} in {line:?}")));
                }
            };
            Ok(StatusEntry { kind, path: path.to_string() })
        })
        .collect()
}

#[must_use]
pub fn default_workspace_name(dest: &Path) -> String {
    dest.file_name().and_then(|n| n.to_str()).unwrap_or("workspace").to_string()
}

/// Resolves `dest` to an absolute, canonical path suitable for passing to
/// `jj`/registering as a Workspace's `root_path` — `dest` itself need not
/// exist yet (a fresh `clone` destination doesn't), so this canonicalizes
/// the deepest existing ancestor and rejoins the remainder rather than
/// requiring the whole path to exist first.
///
/// # Errors
///
/// Returns [`JjError::UnparseableOutput`] if no ancestor of `dest` exists
/// (not even the filesystem root, which should be unreachable in practice).
pub fn canonicalize_prospective(dest: &Path) -> Result<PathBuf, JjError> {
    let mut existing = dest;
    let mut trailing = Vec::new();
    loop {
        if existing.exists() {
            let mut resolved = existing
                .canonicalize()
                .map_err(|error| JjError::UnparseableOutput(format!("cannot canonicalize {}: {error}", existing.display())))?;
            for component in trailing.iter().rev() {
                resolved.push(component);
            }
            return Ok(resolved);
        }
        let Some(parent) = existing.parent() else {
            return Err(JjError::UnparseableOutput(format!("no existing ancestor of {}", dest.display())));
        };
        if let Some(name) = existing.file_name() {
            trailing.push(name.to_os_string());
        }
        existing = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_git_repo(dir: &Path) {
        std::process::Command::new("git").arg("init").arg("-q").current_dir(dir).status().unwrap();
        std::process::Command::new("git").args(["config", "user.email", "a@b.c"]).current_dir(dir).status().unwrap();
        std::process::Command::new("git").args(["config", "user.name", "a"]).current_dir(dir).status().unwrap();
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        std::process::Command::new("git").args(["add", "a.txt"]).current_dir(dir).status().unwrap();
        std::process::Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(dir).status().unwrap();
    }

    #[test]
    fn parses_a_real_diff_summary_shape() {
        let text = "D a.txt\nA b.txt\nA sub/c.txt\n";
        let entries = parse_diff_summary(text).unwrap();
        assert_eq!(
            entries,
            vec![
                StatusEntry { kind: ChangeKind::Deleted, path: "a.txt".to_string() },
                StatusEntry { kind: ChangeKind::Added, path: "b.txt".to_string() },
                StatusEntry { kind: ChangeKind::Added, path: "sub/c.txt".to_string() },
            ]
        );
    }

    #[test]
    fn rejects_an_unrecognized_marker_instead_of_guessing() {
        assert!(matches!(parse_diff_summary("? weird.txt\n"), Err(JjError::UnparseableOutput(_))));
    }

    #[tokio::test]
    async fn ensure_colocated_then_status_reflects_real_changes() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        ensure_colocated(dir.path()).await.unwrap();

        std::fs::write(dir.path().join("b.txt"), "new\n").unwrap();
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();

        let entries = status(dir.path()).await.unwrap();
        assert!(entries.contains(&StatusEntry { kind: ChangeKind::Added, path: "b.txt".to_string() }));
        assert!(entries.contains(&StatusEntry { kind: ChangeKind::Deleted, path: "a.txt".to_string() }));
    }

    #[tokio::test]
    async fn ensure_colocated_is_a_no_op_on_an_already_colocated_repo() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        ensure_colocated(dir.path()).await.unwrap();
        ensure_colocated(dir.path()).await.unwrap();
    }

    #[tokio::test]
    async fn workspace_add_creates_a_second_working_copy_sharing_the_store() {
        let parent_dir = tempfile::tempdir().unwrap();
        init_git_repo(parent_dir.path());
        ensure_colocated(parent_dir.path()).await.unwrap();

        let agent_dest = parent_dir.path().parent().unwrap().join(format!("agent-{}", uuid::Uuid::new_v4()));
        workspace_add(parent_dir.path(), &agent_dest, "agent-b").await.unwrap();
        assert!(agent_dest.join("a.txt").exists(), "new workspace should check out the shared repo's content");

        std::fs::remove_dir_all(&agent_dest).ok();
    }

    #[test]
    fn canonicalize_prospective_resolves_a_not_yet_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("fresh-clone-dest");
        let resolved = canonicalize_prospective(&dest).unwrap();
        assert_eq!(resolved, dir.path().canonicalize().unwrap().join("fresh-clone-dest"));
    }
}
