//! At-most-once host command dispatch over injected narrow capabilities.

use crate::{Command, ServiceRun};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DispatchLimits {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cancellation {
    pub cancelled: bool,
}

pub trait RpcCapability {
    fn rpc_stdio(&mut self, input: &[u8], cancellation: Cancellation) -> CapabilityResult;
}

pub trait EventCapability {
    fn emit(&mut self, input: &[u8], cancellation: Cancellation) -> CapabilityResult;
}

pub trait StreamCapability {
    fn stream(&mut self, capability: &str, cancellation: Cancellation) -> CapabilityResult;
}

pub trait ServiceCapability {
    fn run(&mut self, service: &ServiceRun, cancellation: Cancellation) -> CapabilityResult;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityResult {
    Success(Vec<u8>),
    Rejected(&'static str),
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitCode {
    Success = 0,
    Usage = 2,
    Cancelled = 3,
    Rejected = 4,
    Internal = 5,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchRecord {
    pub exit: ExitCode,
    pub stdout: Vec<u8>,
    pub stderr_code: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchError {
    InvalidLimits,
    AlreadyDispatched,
    InputTooLarge,
    OutputTooLarge,
}

#[derive(Debug)]
pub struct Dispatcher<R, E, T, S> {
    rpc: R,
    events: E,
    streams: T,
    services: S,
    limits: DispatchLimits,
    dispatched: bool,
}

impl<R, E, T, S> Dispatcher<R, E, T, S>
where
    R: RpcCapability,
    E: EventCapability,
    T: StreamCapability,
    S: ServiceCapability,
{
    /// Constructs a dispatcher from explicit narrow boundary capabilities.
    ///
    /// # Errors
    ///
    /// Rejects zero byte limits.
    pub fn new(
        rpc: R,
        events: E,
        streams: T,
        services: S,
        limits: DispatchLimits,
    ) -> Result<Self, DispatchError> {
        if limits.max_input_bytes == 0 || limits.max_output_bytes == 0 {
            return Err(DispatchError::InvalidLimits);
        }
        Ok(Self {
            rpc,
            events,
            streams,
            services,
            limits,
            dispatched: false,
        })
    }

    /// Dispatches the parsed command exactly once without reconstructing shell text.
    ///
    /// # Errors
    ///
    /// Rejects repeated invocation and oversized input/output. The invocation is
    /// consumed before calling external capabilities, including on failure.
    pub fn dispatch(
        &mut self,
        command: &Command,
        input: &[u8],
        cancellation: Cancellation,
    ) -> Result<DispatchRecord, DispatchError> {
        if self.dispatched {
            return Err(DispatchError::AlreadyDispatched);
        }
        self.dispatched = true;
        if input.len() > self.limits.max_input_bytes {
            return Err(DispatchError::InputTooLarge);
        }
        if cancellation.cancelled {
            return Ok(cancelled_record());
        }
        let result = match command {
            // The binary dispatcher parser is intentionally not wired to a
            // process launcher until its direct-exec allowlist is selected.
            // Treat it as unavailable rather than falling back to a shell or
            // another capability.
            Command::ExecStdioV1 => return Ok(usage_record("unsupported_command")),
            Command::RpcStdio => self.rpc.rpc_stdio(input, cancellation),
            Command::EmitStdin => self.events.emit(input, cancellation),
            Command::Stream { capability } => {
                if !input.is_empty() {
                    return Ok(usage_record("unexpected_input"));
                }
                self.streams.stream(capability.expose(), cancellation)
            }
            Command::ServiceRun(service) => {
                if !input.is_empty() {
                    return Ok(usage_record("unexpected_input"));
                }
                self.services.run(service, cancellation)
            }
        };
        normalize_result(result, self.limits.max_output_bytes)
    }
}

fn normalize_result(
    result: CapabilityResult,
    max_output: usize,
) -> Result<DispatchRecord, DispatchError> {
    match result {
        CapabilityResult::Success(stdout) if stdout.len() > max_output => {
            Err(DispatchError::OutputTooLarge)
        }
        CapabilityResult::Success(stdout) => Ok(DispatchRecord {
            exit: ExitCode::Success,
            stdout,
            stderr_code: None,
        }),
        CapabilityResult::Rejected(code) => Ok(DispatchRecord {
            exit: ExitCode::Rejected,
            stdout: Vec::new(),
            stderr_code: Some(code),
        }),
        CapabilityResult::Cancelled => Ok(cancelled_record()),
    }
}

fn cancelled_record() -> DispatchRecord {
    DispatchRecord {
        exit: ExitCode::Cancelled,
        stdout: Vec::new(),
        stderr_code: Some("cancelled"),
    }
}

fn usage_record(code: &'static str) -> DispatchRecord {
    DispatchRecord {
        exit: ExitCode::Usage,
        stdout: Vec::new(),
        stderr_code: Some(code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    #[derive(Debug, Default)]
    struct Fake {
        calls: Vec<&'static str>,
        result: Option<CapabilityResult>,
    }

    impl Fake {
        fn invoke(&mut self, name: &'static str) -> CapabilityResult {
            self.calls.push(name);
            self.result
                .take()
                .unwrap_or(CapabilityResult::Success(name.as_bytes().to_vec()))
        }
    }

    impl RpcCapability for Fake {
        fn rpc_stdio(&mut self, _: &[u8], _: Cancellation) -> CapabilityResult {
            self.invoke("rpc")
        }
    }
    impl EventCapability for Fake {
        fn emit(&mut self, _: &[u8], _: Cancellation) -> CapabilityResult {
            self.invoke("emit")
        }
    }
    impl StreamCapability for Fake {
        fn stream(&mut self, _: &str, _: Cancellation) -> CapabilityResult {
            self.invoke("stream")
        }
    }
    impl ServiceCapability for Fake {
        fn run(&mut self, _: &ServiceRun, _: Cancellation) -> CapabilityResult {
            self.invoke("service")
        }
    }

    fn dispatcher() -> Dispatcher<Fake, Fake, Fake, Fake> {
        Dispatcher::new(
            Fake::default(),
            Fake::default(),
            Fake::default(),
            Fake::default(),
            DispatchLimits {
                max_input_bytes: 8,
                max_output_bytes: 8,
            },
        )
        .unwrap()
    }

    #[test]
    fn command_table_calls_exactly_one_narrow_capability() {
        let commands = [
            Command::RpcStdio,
            Command::EmitStdin,
            parse(["stream", "--capability", "secret"]).unwrap(),
            parse([
                "service",
                "run",
                "--workspace",
                "w",
                "--name",
                "n",
                "--protocol",
                "http",
                "--port",
                "8080",
                "--",
                "program",
                ";rm",
            ])
            .unwrap(),
        ];
        for command in commands {
            let mut dispatcher = dispatcher();
            let input = if matches!(command, Command::RpcStdio | Command::EmitStdin) {
                b"payload".as_slice()
            } else {
                b"".as_slice()
            };
            assert_eq!(
                dispatcher
                    .dispatch(&command, input, Cancellation { cancelled: false })
                    .unwrap()
                    .exit,
                ExitCode::Success
            );
            let calls = dispatcher.rpc.calls.len()
                + dispatcher.events.calls.len()
                + dispatcher.streams.calls.len()
                + dispatcher.services.calls.len();
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn dispatch_is_at_most_once_even_after_input_error() {
        let mut dispatcher = dispatcher();
        assert_eq!(
            dispatcher.dispatch(
                &Command::RpcStdio,
                b"oversized",
                Cancellation { cancelled: false }
            ),
            Err(DispatchError::InputTooLarge)
        );
        assert_eq!(
            dispatcher.dispatch(&Command::RpcStdio, b"", Cancellation { cancelled: false }),
            Err(DispatchError::AlreadyDispatched)
        );
    }

    #[test]
    fn cancellation_short_circuits_without_capability_call() {
        let mut dispatcher = dispatcher();
        let record = dispatcher
            .dispatch(&Command::RpcStdio, b"", Cancellation { cancelled: true })
            .unwrap();
        assert_eq!(record.exit, ExitCode::Cancelled);
        assert!(dispatcher.rpc.calls.is_empty());
    }

    #[test]
    fn output_and_rejection_use_stable_secret_free_records() {
        let mut oversized = dispatcher();
        oversized.rpc.result = Some(CapabilityResult::Success(vec![0; 9]));
        assert_eq!(
            oversized.dispatch(&Command::RpcStdio, b"", Cancellation { cancelled: false }),
            Err(DispatchError::OutputTooLarge)
        );

        let mut rejected = dispatcher();
        rejected.events.result = Some(CapabilityResult::Rejected("invalid_event"));
        assert_eq!(
            rejected
                .dispatch(
                    &Command::EmitStdin,
                    b"secret",
                    Cancellation { cancelled: false }
                )
                .unwrap(),
            DispatchRecord {
                exit: ExitCode::Rejected,
                stdout: vec![],
                stderr_code: Some("invalid_event")
            }
        );
    }

    #[test]
    fn stream_and_service_reject_stdin_without_dispatching() {
        let mut dispatcher = dispatcher();
        let command = parse(["stream", "--capability", "secret"]).unwrap();
        let record = dispatcher
            .dispatch(&command, b"x", Cancellation { cancelled: false })
            .unwrap();
        assert_eq!(record.exit, ExitCode::Usage);
        assert!(dispatcher.streams.calls.is_empty());
    }
}
