//! Fail-closed launcher for the fixed SSH stdio dispatcher.
//!
//! The dispatcher decoder accepts a bounded argv-shaped request. This module
//! admits only the one operation presently owned by `choosh-host`:
//! `chooshd rpc --stdio`. It passes bytes to an injected direct-process
//! capability; it never constructs shell text, a path, an environment, or a
//! working directory.

use crate::exec_stdio::FixedExecRequest;

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
/// `rpc --stdio` argument vector and enforce the supplied output bounds while
/// capturing its pipes. Implementations must not invoke a shell.
pub trait ChooshdRpcProcess {
    /// Runs the fixed daemon RPC operation with bounded standard I/O.
    ///
    /// # Errors
    ///
    /// Returns a sanitized process outcome without host diagnostics.
    fn run_rpc_stdio(
        &mut self,
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
    pub fn new(process: P, limits: DirectExecLimits) -> Result<Self, DirectExecError> {
        if limits.max_stdout_bytes == 0 || limits.max_stderr_bytes == 0 {
            return Err(DirectExecError::InvalidLimits);
        }
        Ok(Self { process, limits })
    }

    /// Admits and launches exactly `chooshd rpc --stdio`.
    ///
    /// The supplied request must already have passed the versioned
    /// length-delimited decoder. Any other executable or argument vector is
    /// rejected before the injected process capability is called.
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
            .run_rpc_stdio(request.stdin(), self.limits)
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
        calls: Vec<(Vec<u8>, DirectExecLimits)>,
        result: Option<Result<DirectExecOutput, DirectExecCapabilityError>>,
    }

    impl ChooshdRpcProcess for FakeProcess {
        fn run_rpc_stdio(
            &mut self,
            stdin: &[u8],
            limits: DirectExecLimits,
        ) -> Result<DirectExecOutput, DirectExecCapabilityError> {
            self.calls.push((stdin.to_vec(), limits));
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
            DirectExecLimits {
                max_stdout_bytes: 8,
                max_stderr_bytes: 8,
            },
        )
        .unwrap()
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
        assert_eq!(process.calls[0].0, b"frame");
        assert_eq!(
            process.calls[0].1,
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
                DirectExecLimits {
                    max_stdout_bytes: 0,
                    max_stderr_bytes: 1,
                }
            ),
            Err(DirectExecError::InvalidLimits)
        ));
    }
}
