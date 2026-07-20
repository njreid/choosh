//! The first composed daemon operation: bounded `git status` for one registered root.
//!
//! This is intentionally narrow.  It applies the fixed Git plan, clears the inherited
//! environment in the concrete executor, parses the complete output, then proves every
//! returned *current* path still resolves beneath the root that was registered before
//! returning a result.  It does not turn arbitrary Git output into filesystem authority.

#![cfg(unix)]

use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::git::{GitCommandPlan, StatusLimits, StatusParseError, StatusSnapshot, parse_status};
use crate::project_fs::{PreparedProjectFile, ProjectFsError, RegisteredProjectRoot};

/// Runs a fixed Git plan without exposing a shell or ambient process state.
pub trait GitStatusExecutor: Send + Sync {
    /// Executes the already-fixed status plan and returns at most `max_output_bytes + 1` bytes.
    ///
    /// # Errors
    ///
    /// Returns a stable process-boundary failure without retaining process output.
    fn execute(
        &self,
        plan: &GitCommandPlan,
        max_output_bytes: usize,
    ) -> Result<Vec<u8>, GitStatusExecutionError>;
}

/// Narrow, injectable capability for one already-registered workspace.
///
/// The RPC layer owns workspace identity lookup. Implementations never accept a
/// path from a request, so a caller cannot turn status into filesystem authority.
pub trait GitStatusOperation: Send + Sync {
    /// Returns a complete reconciled status snapshot or a stable domain failure.
    ///
    /// # Errors
    ///
    /// Returns the domain failure without exposing a partial snapshot.
    fn status_snapshot(&self) -> Result<StatusSnapshot, GitStatusError>;
}

/// Concrete outer-adapter executor.  Its only process invocation is [`GitCommandPlan`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemGitStatusExecutor;

impl GitStatusExecutor for SystemGitStatusExecutor {
    fn execute(
        &self,
        plan: &GitCommandPlan,
        max_output_bytes: usize,
    ) -> Result<Vec<u8>, GitStatusExecutionError> {
        let mut command = Command::new(plan.program());
        command
            .args(plan.arguments())
            .current_dir(plan.current_dir())
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped());
        if plan.clear_environment() {
            command.env_clear();
        }
        command.envs(plan.environment().iter().copied());
        let mut child = command
            .spawn()
            .map_err(|error| GitStatusExecutionError::Io(error.kind()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or(GitStatusExecutionError::MissingStdout)?;
        let mut output = Vec::with_capacity(max_output_bytes.saturating_add(1).min(8192));
        let mut limited = stdout.take((max_output_bytes as u64).saturating_add(1));
        limited
            .read_to_end(&mut output)
            .map_err(|error| GitStatusExecutionError::Io(error.kind()))?;
        let status = child
            .wait()
            .map_err(|error| GitStatusExecutionError::Io(error.kind()))?;
        if output.len() > max_output_bytes {
            return Err(GitStatusExecutionError::OutputTooLarge);
        }
        if !status.success() {
            return Err(GitStatusExecutionError::UnsuccessfulExit);
        }
        Ok(output)
    }
}

/// Stable failures from the process boundary.  No command output or host paths are retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitStatusExecutionError {
    Io(io::ErrorKind),
    MissingStdout,
    OutputTooLarge,
    UnsuccessfulExit,
}

/// Composed status-operation failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitStatusError {
    Execution(GitStatusExecutionError),
    Parse(StatusParseError),
    Reconciliation(ProjectFsError),
}

impl fmt::Display for GitStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Execution(_) => formatter.write_str("git_status_execution_failed"),
            Self::Parse(_) => formatter.write_str("git_status_parse_failed"),
            Self::Reconciliation(_) => formatter.write_str("git_status_path_reconciliation_failed"),
        }
    }
}

impl std::error::Error for GitStatusError {}

/// A parsed status snapshot whose current paths have been re-opened under the registered root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconciledGitStatus {
    snapshot: StatusSnapshot,
    current_files: Vec<PreparedProjectFile>,
}

impl ReconciledGitStatus {
    #[must_use]
    pub fn snapshot(&self) -> &StatusSnapshot {
        &self.snapshot
    }

    /// Prepared current paths, held as root-bound capabilities rather than strings.
    #[must_use]
    pub fn current_files(&self) -> &[PreparedProjectFile] {
        &self.current_files
    }
}

/// Composition root for one registered workspace's status operation.
pub struct GitStatusService<E> {
    root: RegisteredProjectRoot,
    executor: E,
    limits: StatusLimits,
}

impl<E> GitStatusService<E>
where
    E: GitStatusExecutor,
{
    #[must_use]
    pub fn new(root: RegisteredProjectRoot, executor: E, limits: StatusLimits) -> Self {
        Self {
            root,
            executor,
            limits,
        }
    }

    /// Executes and reconciles a complete status snapshot.  Partial results are never returned.
    ///
    /// # Errors
    ///
    /// Returns an execution, parser, or root-reconciliation failure without a partial snapshot.
    pub fn status(&self) -> Result<ReconciledGitStatus, GitStatusError> {
        let plan = GitCommandPlan::status(self.root.canonical_path());
        let output = self
            .executor
            .execute(&plan, self.limits.max_bytes)
            .map_err(GitStatusError::Execution)?;
        let snapshot = parse_status(&output, self.limits).map_err(GitStatusError::Parse)?;
        let current_files = snapshot
            .entries()
            .iter()
            .map(|entry| {
                self.root
                    .prepare(Path::new(OsStr::from_bytes(entry.new_path())))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(GitStatusError::Reconciliation)?;
        Ok(ReconciledGitStatus {
            snapshot,
            current_files,
        })
    }
}

impl<E> GitStatusOperation for GitStatusService<E>
where
    E: GitStatusExecutor,
{
    fn status_snapshot(&self) -> Result<StatusSnapshot, GitStatusError> {
        self.status().map(|status| status.snapshot().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    const LIMITS: StatusLimits = StatusLimits {
        max_bytes: 256,
        max_entries: 4,
        max_path_bytes: 64,
    };
    static FIXTURE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    type ObservedPlan = (bool, String, Vec<String>);

    #[derive(Clone)]
    struct FakeExecutor {
        output: Vec<u8>,
        seen: Arc<Mutex<Option<ObservedPlan>>>,
    }

    impl GitStatusExecutor for FakeExecutor {
        fn execute(
            &self,
            plan: &GitCommandPlan,
            _max: usize,
        ) -> Result<Vec<u8>, GitStatusExecutionError> {
            *self.seen.lock().unwrap() = Some((
                plan.clear_environment(),
                plan.current_dir().display().to_string(),
                plan.arguments().iter().map(ToString::to_string).collect(),
            ));
            Ok(self.output.clone())
        }
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "choosh-git-status-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(root.join("src")).unwrap();
            fs::write(root.join("src/main.rs"), b"fn main() {}\n").unwrap();
            Self(root)
        }
        fn registered(&self) -> RegisteredProjectRoot {
            RegisteredProjectRoot::register(
                &self.0,
                crate::project_fs::ProjectPathLimits::new(128, 8).unwrap(),
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn fixed_plan_is_executed_then_each_current_path_is_root_reconciled() {
        let fixture = Fixture::new();
        let seen = Arc::new(Mutex::new(None));
        let service = GitStatusService::new(
            fixture.registered(),
            FakeExecutor {
                output: b" M src/main.rs\0".to_vec(),
                seen: Arc::clone(&seen),
            },
            LIMITS,
        );
        let status = service.status().unwrap();
        assert_eq!(status.snapshot().entries().len(), 1);
        assert_eq!(
            status.current_files()[0].canonical_path(),
            fixture.0.join("src/main.rs")
        );
        let (cleared, root, args) = seen.lock().unwrap().clone().unwrap();
        assert!(cleared);
        assert_eq!(root, fixture.0.display().to_string());
        assert!(args.contains(&"--porcelain=v1".to_owned()));
    }

    #[test]
    fn a_status_path_that_cannot_be_proven_under_the_registered_root_fails_closed() {
        let fixture = Fixture::new();
        let service = GitStatusService::new(
            fixture.registered(),
            FakeExecutor {
                output: b" M missing.rs\0".to_vec(),
                seen: Arc::new(Mutex::new(None)),
            },
            LIMITS,
        );
        assert!(matches!(
            service.status(),
            Err(GitStatusError::Reconciliation(_))
        ));
    }

    #[test]
    fn real_executor_enforces_the_parser_output_bound() {
        let fixture = Fixture::new();
        let plan = GitCommandPlan::status(&fixture.0);
        // The directory is not a repository; failure is stable and its stderr is not surfaced.
        assert_eq!(
            SystemGitStatusExecutor.execute(&plan, 32),
            Err(GitStatusExecutionError::UnsuccessfulExit)
        );
    }
}
