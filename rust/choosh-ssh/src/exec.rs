//! Bounded fixed-command execution over an authenticated SSH transport.
//!
//! SSH's `exec` request is conventionally interpreted by a remote shell.
//! Choosh therefore sends only this constant dispatcher command and places a
//! length-delimited argv/stdin request on the channel. The remote
//! `choosh-host` dispatcher must decode it and invoke the requested executable
//! directly; no caller-controlled text is interpolated into the SSH command.

use std::io::Cursor;

use crate::VerifiedConnection;

const DISPATCHER_COMMAND: &[u8] = b"choosh-host --exec-stdio-v1";
const MAX_EXECUTABLE_BYTES: usize = 96;
const MAX_ARGUMENTS: usize = 32;
const MAX_ARGUMENT_BYTES: usize = 4_096;
const MAX_STDIN_BYTES: usize = 256 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// A directly invoked executable and separately encoded argument vector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixedCommand {
    executable: String,
    arguments: Vec<String>,
    stdin: Vec<u8>,
}

impl FixedCommand {
    /// Validates a bounded executable, argument vector, and standard input.
    ///
    /// # Errors
    ///
    /// Returns a stable error when a value is oversized or cannot be encoded
    /// by the fixed dispatcher protocol.
    pub fn new(
        executable: impl Into<String>,
        arguments: Vec<String>,
        stdin: Vec<u8>,
    ) -> Result<Self, FixedCommandError> {
        let executable = executable.into();
        if executable.is_empty() || executable.len() > MAX_EXECUTABLE_BYTES {
            return Err(FixedCommandError::InvalidExecutable);
        }
        if !executable
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(FixedCommandError::InvalidExecutable);
        }
        if arguments.len() > MAX_ARGUMENTS {
            return Err(FixedCommandError::TooManyArguments);
        }
        if arguments.iter().any(|argument| {
            argument.is_empty()
                || argument.len() > MAX_ARGUMENT_BYTES
                || argument
                    .bytes()
                    .any(|byte| byte == 0 || byte.is_ascii_control())
        }) {
            return Err(FixedCommandError::InvalidArgument);
        }
        if stdin.len() > MAX_STDIN_BYTES {
            return Err(FixedCommandError::StdinTooLarge);
        }

        Ok(Self {
            executable,
            arguments,
            stdin,
        })
    }

    fn encode(&self) -> Vec<u8> {
        let size = 1
            + 2
            + self.executable.len()
            + 2
            + self
                .arguments
                .iter()
                .map(|argument| 2 + argument.len())
                .sum::<usize>()
            + 4
            + self.stdin.len();
        let mut encoded = Vec::with_capacity(size);
        encoded.push(1);
        encode_u16(&mut encoded, self.executable.len());
        encoded.extend_from_slice(self.executable.as_bytes());
        encode_u16(&mut encoded, self.arguments.len());
        for argument in &self.arguments {
            encode_u16(&mut encoded, argument.len());
            encoded.extend_from_slice(argument.as_bytes());
        }
        let stdin_length = u32::try_from(self.stdin.len()).expect("validated stdin fits u32");
        encoded.extend_from_slice(&stdin_length.to_be_bytes());
        encoded.extend_from_slice(&self.stdin);
        encoded
    }
}

fn encode_u16(target: &mut Vec<u8>, value: usize) {
    let value = u16::try_from(value).expect("validated field fits u16");
    target.extend_from_slice(&value.to_be_bytes());
}

/// Stable local validation failures for a fixed dispatcher request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixedCommandError {
    InvalidExecutable,
    TooManyArguments,
    InvalidArgument,
    StdinTooLarge,
}

/// Bounded standard-output, standard-error, and exit result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixedExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_status: u32,
}

/// Typed failures while executing the fixed dispatcher request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixedExecError {
    TransportFailed,
    OutputLimitExceeded,
    MissingExitStatus,
}

impl VerifiedConnection {
    /// Executes a length-delimited fixed dispatcher request.
    ///
    /// The only SSH command text is the constant `choosh-host
    /// --exec-stdio-v1`; executable and arguments are sent as binary channel
    /// input and never shell-quoted or concatenated.
    ///
    /// # Errors
    ///
    /// Returns [`FixedExecError::TransportFailed`] for a rejected/lost channel,
    /// [`FixedExecError::OutputLimitExceeded`] when combined stdout/stderr
    /// exceeds the fixed bound, and [`FixedExecError::MissingExitStatus`] when
    /// the dispatcher closes without a status.
    pub async fn execute_fixed(
        &self,
        command: FixedCommand,
    ) -> Result<FixedExecOutput, FixedExecError> {
        let mut channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|_| FixedExecError::TransportFailed)?;
        channel
            .exec(true, DISPATCHER_COMMAND)
            .await
            .map_err(|_| FixedExecError::TransportFailed)?;
        channel
            .data(Cursor::new(command.encode()))
            .await
            .map_err(|_| FixedExecError::TransportFailed)?;
        channel
            .eof()
            .await
            .map_err(|_| FixedExecError::TransportFailed)?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_status = None;
        while let Some(message) = channel.wait().await {
            match message {
                russh::ChannelMsg::Data { data } => append_output(&mut stdout, &stderr, &data)?,
                russh::ChannelMsg::ExtendedData { data, .. } => {
                    append_output(&mut stderr, &stdout, &data)?;
                }
                russh::ChannelMsg::ExitStatus {
                    exit_status: status,
                } => exit_status = Some(status),
                _ => {}
            }
        }

        let exit_status = exit_status.ok_or(FixedExecError::MissingExitStatus)?;
        Ok(FixedExecOutput {
            stdout,
            stderr,
            exit_status,
        })
    }
}

fn append_output(target: &mut Vec<u8>, other: &[u8], bytes: &[u8]) -> Result<(), FixedExecError> {
    if target
        .len()
        .saturating_add(other.len())
        .saturating_add(bytes.len())
        > MAX_OUTPUT_BYTES
    {
        return Err(FixedExecError::OutputLimitExceeded);
    }
    target.extend_from_slice(bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FixedCommand, FixedCommandError};

    #[test]
    fn fixed_command_uses_length_delimited_argv_and_stdin() {
        let command = FixedCommand::new(
            "chooshd",
            vec!["--workspace".into(), "fixture-root".into()],
            vec![1, 2, 3],
        )
        .unwrap();

        assert_eq!(
            command.encode(),
            [
                1, 0, 7, b'c', b'h', b'o', b'o', b's', b'h', b'd', 0, 2, 0, 11, b'-', b'-', b'w',
                b'o', b'r', b'k', b's', b'p', b'a', b'c', b'e', 0, 12, b'f', b'i', b'x', b't',
                b'u', b'r', b'e', b'-', b'r', b'o', b'o', b't', 0, 0, 0, 3, 1, 2, 3,
            ]
        );
    }

    #[test]
    fn command_rejects_shell_like_executable_and_unbounded_inputs() {
        assert_eq!(
            FixedCommand::new("sh -c", Vec::new(), Vec::new()),
            Err(FixedCommandError::InvalidExecutable)
        );
        assert_eq!(
            FixedCommand::new("tool", vec!["line\nbreak".into()], Vec::new()),
            Err(FixedCommandError::InvalidArgument)
        );
        assert_eq!(
            FixedCommand::new("tool", vec!["x".into(); 33], Vec::new()),
            Err(FixedCommandError::TooManyArguments)
        );
    }
}
