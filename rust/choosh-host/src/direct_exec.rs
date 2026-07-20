//! Fail-closed launcher for the fixed SSH stdio dispatcher.
//!
//! The dispatcher decoder accepts a bounded argv-shaped request. This module
//! admits only the one operation presently owned by `choosh-host`:
//! `chooshd rpc --stdio`. It passes bytes and an already validated daemon
//! socket plan to an injected direct-process capability; it never constructs
//! shell text or discovers a path, environment, or working directory.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::exec_stdio::FixedExecRequest;

const MAX_DAEMON_PATH_BYTES: usize = 4_096;

/// Explicit daemon paths held by the host composition root.
///
/// These paths never originate in the SSH request. They are deliberately
/// excluded from `Debug` output because a local path may be sensitive.
#[derive(Clone, Eq, PartialEq)]
pub struct DaemonRpcPlan {
    state_dir: PathBuf,
    socket_path: PathBuf,
}

impl DaemonRpcPlan {
    /// Validates a bounded absolute plan whose socket is the direct state child.
    ///
    /// # Errors
    ///
    /// Rejects relative, non-normal, oversized, or mismatched paths without
    /// including either path in the error.
    pub fn new(
        state_dir: impl Into<PathBuf>,
        socket_path: impl Into<PathBuf>,
    ) -> Result<Self, DaemonRpcPlanError> {
        let state_dir = state_dir.into();
        let socket_path = socket_path.into();
        validate_daemon_path(&state_dir).map_err(|()| DaemonRpcPlanError::InvalidStateDirectory)?;
        validate_daemon_path(&socket_path).map_err(|()| DaemonRpcPlanError::InvalidSocketPath)?;
        if socket_path.parent() != Some(state_dir.as_path()) || socket_path.file_name().is_none() {
            return Err(DaemonRpcPlanError::InvalidSocketLayout);
        }
        Ok(Self {
            state_dir,
            socket_path,
        })
    }

    /// Returns the validated state directory only to the process adapter.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Returns the validated socket path only to the process adapter.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl fmt::Debug for DaemonRpcPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DaemonRpcPlan([REDACTED])")
    }
}

fn validate_daemon_path(path: &Path) -> Result<(), ()> {
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().is_empty()
        || path.as_os_str().as_encoded_bytes().len() > MAX_DAEMON_PATH_BYTES
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(());
    }
    Ok(())
}

/// Stable invalid-plan classifications without local path diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonRpcPlanError {
    InvalidStateDirectory,
    InvalidSocketPath,
    InvalidSocketLayout,
}

impl DaemonRpcPlanError {
    /// Returns a stable machine-readable outcome code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidStateDirectory => "direct_exec_invalid_state_directory",
            Self::InvalidSocketPath => "direct_exec_invalid_socket_path",
            Self::InvalidSocketLayout => "direct_exec_invalid_socket_layout",
        }
    }
}

/// Upper bounds the launcher gives to its direct-process capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectExecLimits {
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
}

/// Bounded completed output from a direct child process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_status: u32,
}

/// Sanitized process-launch outcomes that do not carry host diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectExecCapabilityError {
    Unavailable,
    Cancelled,
    Failed,
}

/// Narrow outer-boundary capability for the allowlisted daemon invocation.
///
/// A concrete implementation must launch `chooshd` directly with the fixed
/// `rpc --stdio --state-dir <plan> --socket <plan>` argument vector and
/// enforce the supplied output bounds while capturing its pipes.
/// Implementations must not invoke a shell or replace the injected plan with
/// configuration discovered from the environment.
pub trait ChooshdRpcProcess {
    /// Runs the fixed daemon RPC operation with bounded standard I/O.
    ///
    /// # Errors
    ///
    /// Returns a sanitized process outcome without host diagnostics.
    fn run_rpc_stdio(
        &mut self,
        plan: &DaemonRpcPlan,
        stdin: &[u8],
        limits: DirectExecLimits,
    ) -> Result<DirectExecOutput, DirectExecCapabilityError>;
}

/// Stable direct-dispatch outcomes, without request bytes or host errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectExecError {
    InvalidLimits,
    ExecutableNotAllowed,
    ArgumentsNotAllowed,
    OutputLimitExceeded,
    Unavailable,
    Cancelled,
    LaunchFailed,
}

impl DirectExecError {
    /// Returns a stable machine-readable outcome code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "direct_exec_invalid_limits",
            Self::ExecutableNotAllowed => "direct_exec_executable_not_allowed",
            Self::ArgumentsNotAllowed => "direct_exec_arguments_not_allowed",
            Self::OutputLimitExceeded => "direct_exec_output_limit_exceeded",
            Self::Unavailable => "direct_exec_unavailable",
            Self::Cancelled => "direct_exec_cancelled",
            Self::LaunchFailed => "direct_exec_launch_failed",
        }
    }
}

/// Direct launcher for the sole currently allowlisted executable operation.
///
/// It is intentionally generic over a narrow process capability so the host
/// binary owns concrete process wiring and tests can use deterministic fakes.
#[derive(Debug)]
pub struct DirectExecLauncher<P> {
    process: P,
    daemon_plan: DaemonRpcPlan,
    limits: DirectExecLimits,
}

impl<P> DirectExecLauncher<P>
where
    P: ChooshdRpcProcess,
{
    /// Creates a launcher with non-zero independent output limits.
    ///
    /// # Errors
    ///
    /// Returns [`DirectExecError::InvalidLimits`] if either output stream has
    /// no bounded capture capacity.
    pub fn new(
        process: P,
        daemon_plan: DaemonRpcPlan,
        limits: DirectExecLimits,
    ) -> Result<Self, DirectExecError> {
        if limits.max_stdout_bytes == 0 || limits.max_stderr_bytes == 0 {
            return Err(DirectExecError::InvalidLimits);
        }
        Ok(Self {
            process,
            daemon_plan,
            limits,
        })
    }

    /// Admits and launches exactly `chooshd rpc --stdio`.
    ///
    /// The supplied request must already have passed the versioned
    /// length-delimited decoder. Its state directory and socket are not part of
    /// that untrusted request: they are supplied only by the injected plan.
    /// Any other executable or argument vector is rejected before the injected
    /// process capability is called.
    ///
    /// # Errors
    ///
    /// Returns a stable, content-free [`DirectExecError`]. The capability is
    /// also rechecked so a faulty implementation cannot return oversized
    /// captured output to the SSH boundary.
    pub fn launch(
        &mut self,
        request: &FixedExecRequest,
    ) -> Result<DirectExecOutput, DirectExecError> {
        if request.executable() != "chooshd" {
            return Err(DirectExecError::ExecutableNotAllowed);
        }
        if request.arguments() != ["rpc", "--stdio"] {
            return Err(DirectExecError::ArgumentsNotAllowed);
        }
        let output = self
            .process
            .run_rpc_stdio(&self.daemon_plan, request.stdin(), self.limits)
            .map_err(map_capability_error)?;
        if output.stdout.len() > self.limits.max_stdout_bytes
            || output.stderr.len() > self.limits.max_stderr_bytes
        {
            return Err(DirectExecError::OutputLimitExceeded);
        }
        Ok(output)
    }

    /// Returns the injected process capability to the outer composition root.
    #[must_use]
    pub fn into_process(self) -> P {
        self.process
    }
}

const fn map_capability_error(error: DirectExecCapabilityError) -> DirectExecError {
    match error {
        DirectExecCapabilityError::Unavailable => DirectExecError::Unavailable,
        DirectExecCapabilityError::Cancelled => DirectExecError::Cancelled,
        DirectExecCapabilityError::Failed => DirectExecError::LaunchFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec_stdio::decode_fixed_exec_request;

    #[derive(Debug, Default)]
    struct FakeProcess {
        calls: Vec<(DaemonRpcPlan, Vec<u8>, DirectExecLimits)>,
        result: Option<Result<DirectExecOutput, DirectExecCapabilityError>>,
    }

    impl ChooshdRpcProcess for FakeProcess {
        fn run_rpc_stdio(
            &mut self,
            plan: &DaemonRpcPlan,
            stdin: &[u8],
            limits: DirectExecLimits,
        ) -> Result<DirectExecOutput, DirectExecCapabilityError> {
            self.calls.push((plan.clone(), stdin.to_vec(), limits));
            self.result.take().unwrap_or(Ok(DirectExecOutput {
                stdout: b"ok".to_vec(),
                stderr: Vec::new(),
                exit_status: 0,
            }))
        }
    }

    fn request(executable: &str, arguments: &[&str], stdin: &[u8]) -> FixedExecRequest {
        let mut wire = vec![1];
        wire.extend_from_slice(
            &u16::try_from(executable.len())
                .expect("test executable length fits u16")
                .to_be_bytes(),
        );
        wire.extend_from_slice(executable.as_bytes());
        wire.extend_from_slice(
            &u16::try_from(arguments.len())
                .expect("test argument count fits u16")
                .to_be_bytes(),
        );
        for argument in arguments {
            wire.extend_from_slice(
                &u16::try_from(argument.len())
                    .expect("test argument length fits u16")
                    .to_be_bytes(),
            );
            wire.extend_from_slice(argument.as_bytes());
        }
        wire.extend_from_slice(
            &u32::try_from(stdin.len())
                .expect("test stdin length fits u32")
                .to_be_bytes(),
        );
        wire.extend_from_slice(stdin);
        decode_fixed_exec_request(&wire).unwrap()
    }

    fn launcher() -> DirectExecLauncher<FakeProcess> {
        DirectExecLauncher::new(
            FakeProcess::default(),
            daemon_plan(),
            DirectExecLimits {
                max_stdout_bytes: 8,
                max_stderr_bytes: 8,
            },
        )
        .unwrap()
    }

    fn daemon_plan() -> DaemonRpcPlan {
        DaemonRpcPlan::new("/run/user/1000/choosh", "/run/user/1000/choosh/daemon.sock").unwrap()
    }

    #[test]
    fn admits_only_the_fixed_daemon_rpc_vector_without_shell_text() {
        let mut launcher = launcher();
        let output = launcher
            .launch(&request("chooshd", &["rpc", "--stdio"], b"frame"))
            .unwrap();
        assert_eq!(output.stdout, b"ok");

        let process = launcher.into_process();
        assert_eq!(process.calls.len(), 1);
        assert_eq!(process.calls[0].0, daemon_plan());
        assert_eq!(process.calls[0].1, b"frame");
        assert_eq!(
            process.calls[0].2,
            DirectExecLimits {
                max_stdout_bytes: 8,
                max_stderr_bytes: 8,
            }
        );
    }

    #[test]
    fn alternate_executables_and_arguments_are_rejected_before_launch() {
        for request in [
            request("sh", &["-c", "echo bad"], b""),
            request("chooshd", &["rpc", "--socket"], b""),
            request("chooshd", &["rpc", "--stdio", "extra"], b""),
        ] {
            let mut launcher = launcher();
            assert!(matches!(
                launcher.launch(&request),
                Err(DirectExecError::ExecutableNotAllowed | DirectExecError::ArgumentsNotAllowed)
            ));
            assert!(launcher.into_process().calls.is_empty());
        }
    }

    #[test]
    fn capability_output_is_bounded_even_when_its_implementation_misbehaves() {
        let mut launcher = launcher();
        launcher.process.result = Some(Ok(DirectExecOutput {
            stdout: vec![0; 9],
            stderr: Vec::new(),
            exit_status: 0,
        }));
        assert_eq!(
            launcher.launch(&request("chooshd", &["rpc", "--stdio"], b"")),
            Err(DirectExecError::OutputLimitExceeded)
        );
    }

    #[test]
    fn process_failures_are_typed_and_content_free() {
        let mut launcher = launcher();
        launcher.process.result = Some(Err(DirectExecCapabilityError::Unavailable));
        let error = launcher
            .launch(&request("chooshd", &["rpc", "--stdio"], b""))
            .unwrap_err();
        assert_eq!(error, DirectExecError::Unavailable);
        assert_eq!(error.code(), "direct_exec_unavailable");
        assert!(!error.code().contains("frame"));
    }

    #[test]
    fn zero_stream_bound_is_not_constructible() {
        assert!(matches!(
            DirectExecLauncher::new(
                FakeProcess::default(),
                daemon_plan(),
                DirectExecLimits {
                    max_stdout_bytes: 0,
                    max_stderr_bytes: 1,
                }
            ),
            Err(DirectExecError::InvalidLimits)
        ));
    }

    #[test]
    fn daemon_plan_is_explicit_bounded_and_never_exposes_paths_in_debug_output() {
        let plan = daemon_plan();
        assert_eq!(plan.state_dir(), Path::new("/run/user/1000/choosh"));
        assert_eq!(
            plan.socket_path(),
            Path::new("/run/user/1000/choosh/daemon.sock")
        );
        assert_eq!(format!("{plan:?}"), "DaemonRpcPlan([REDACTED])");

        for (state, socket) in [
            ("relative", "relative/daemon.sock"),
            (
                "/run/user/1000/../secret",
                "/run/user/1000/secret/daemon.sock",
            ),
            ("/run/user/1000/choosh", "/tmp/daemon.sock"),
        ] {
            assert!(DaemonRpcPlan::new(state, socket).is_err());
        }
    }
}
