//! Decoder for the fixed `choosh-host --exec-stdio-v1` SSH dispatcher input.
//!
//! The SSH command itself is constant. This decoder accepts the versioned,
//! length-delimited request written to its standard input and never interprets
//! any field as shell text.

use std::fmt;

const WIRE_VERSION: u8 = 1;
const MAX_EXECUTABLE_BYTES: usize = 96;
const MAX_ARGUMENTS: usize = 32;
const MAX_ARGUMENT_BYTES: usize = 4_096;
const MAX_STDIN_BYTES: usize = 256 * 1024;

/// One decoded request for a directly invoked, policy-approved executable.
#[derive(Clone, Eq, PartialEq)]
pub struct FixedExecRequest {
    executable: String,
    arguments: Vec<String>,
    stdin: Vec<u8>,
}

impl FixedExecRequest {
    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    /// Returns opaque command input only to the injected direct-exec adapter.
    #[must_use]
    pub fn stdin(&self) -> &[u8] {
        &self.stdin
    }
}

impl fmt::Debug for FixedExecRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixedExecRequest")
            .field("executable", &self.executable)
            .field("arguments", &self.arguments)
            .field("stdin", &"[REDACTED]")
            .finish()
    }
}

/// Stable malformed-input classifications for dispatcher protocol version 1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixedExecDecodeError {
    Truncated,
    UnsupportedVersion,
    InvalidUtf8,
    InvalidExecutable,
    TooManyArguments,
    InvalidArgument,
    StdinTooLarge,
    TrailingBytes,
}

impl FixedExecDecodeError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Truncated => "exec_stdio_truncated",
            Self::UnsupportedVersion => "exec_stdio_unsupported_version",
            Self::InvalidUtf8 => "exec_stdio_invalid_utf8",
            Self::InvalidExecutable => "exec_stdio_invalid_executable",
            Self::TooManyArguments => "exec_stdio_too_many_arguments",
            Self::InvalidArgument => "exec_stdio_invalid_argument",
            Self::StdinTooLarge => "exec_stdio_stdin_too_large",
            Self::TrailingBytes => "exec_stdio_trailing_bytes",
        }
    }
}

/// Decodes exactly one bounded dispatcher request.
///
/// # Errors
///
/// Returns a stable [`FixedExecDecodeError`] without embedding untrusted input
/// in diagnostics. The caller must still apply its executable allowlist before
/// invoking a process.
pub fn decode_fixed_exec_request(input: &[u8]) -> Result<FixedExecRequest, FixedExecDecodeError> {
    let mut reader = Reader::new(input);
    if reader.byte()? != WIRE_VERSION {
        return Err(FixedExecDecodeError::UnsupportedVersion);
    }
    let executable_length = usize::from(reader.u16()?);
    let executable = reader.utf8(executable_length)?;
    validate_executable(&executable)?;

    let argument_count = usize::from(reader.u16()?);
    if argument_count > MAX_ARGUMENTS {
        return Err(FixedExecDecodeError::TooManyArguments);
    }
    let mut arguments = Vec::with_capacity(argument_count);
    for _ in 0..argument_count {
        let argument_length = usize::from(reader.u16()?);
        let argument = reader.utf8(argument_length)?;
        validate_argument(&argument)?;
        arguments.push(argument);
    }

    let stdin_length =
        usize::try_from(reader.u32()?).map_err(|_| FixedExecDecodeError::StdinTooLarge)?;
    if stdin_length > MAX_STDIN_BYTES {
        return Err(FixedExecDecodeError::StdinTooLarge);
    }
    let stdin = reader.bytes(stdin_length)?.to_vec();
    if !reader.is_empty() {
        return Err(FixedExecDecodeError::TrailingBytes);
    }

    Ok(FixedExecRequest {
        executable,
        arguments,
        stdin,
    })
}

fn validate_executable(value: &str) -> Result<(), FixedExecDecodeError> {
    if value.is_empty()
        || value.len() > MAX_EXECUTABLE_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(FixedExecDecodeError::InvalidExecutable);
    }
    Ok(())
}

fn validate_argument(value: &str) -> Result<(), FixedExecDecodeError> {
    if value.is_empty()
        || value.len() > MAX_ARGUMENT_BYTES
        || value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(FixedExecDecodeError::InvalidArgument);
    }
    Ok(())
}

struct Reader<'a> {
    remaining: &'a [u8],
}

impl<'a> Reader<'a> {
    const fn new(remaining: &'a [u8]) -> Self {
        Self { remaining }
    }

    fn byte(&mut self) -> Result<u8, FixedExecDecodeError> {
        Ok(*self.bytes(1)?.first().expect("one byte requested"))
    }

    fn u16(&mut self) -> Result<u16, FixedExecDecodeError> {
        let bytes: [u8; 2] = self
            .bytes(2)?
            .try_into()
            .expect("exactly two bytes requested");
        Ok(u16::from_be_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, FixedExecDecodeError> {
        let bytes: [u8; 4] = self
            .bytes(4)?
            .try_into()
            .expect("exactly four bytes requested");
        Ok(u32::from_be_bytes(bytes))
    }

    fn utf8(&mut self, length: usize) -> Result<String, FixedExecDecodeError> {
        let bytes = self.bytes(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| FixedExecDecodeError::InvalidUtf8)
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], FixedExecDecodeError> {
        let (head, tail) = self
            .remaining
            .split_at_checked(length)
            .ok_or(FixedExecDecodeError::Truncated)?;
        self.remaining = tail;
        Ok(head)
    }

    const fn is_empty(&self) -> bool {
        self.remaining.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{FixedExecDecodeError, decode_fixed_exec_request};

    #[test]
    fn decodes_the_client_v1_golden_wire_without_shell_reconstruction() {
        let request = decode_fixed_exec_request(&[
            1, 0, 7, b'c', b'h', b'o', b'o', b's', b'h', b'd', 0, 2, 0, 11, b'-', b'-', b'w', b'o',
            b'r', b'k', b's', b'p', b'a', b'c', b'e', 0, 12, b'f', b'i', b'x', b't', b'u', b'r',
            b'e', b'-', b'r', b'o', b'o', b't', 0, 0, 0, 3, 1, 2, 3,
        ])
        .unwrap();

        assert_eq!(request.executable(), "chooshd");
        assert_eq!(request.arguments(), ["--workspace", "fixture-root"]);
        assert_eq!(request.stdin(), [1, 2, 3]);
        assert!(!format!("{request:?}").contains("[1, 2, 3]"));
    }

    #[test]
    fn malformed_inputs_fail_closed_without_accepting_trailing_data() {
        assert_eq!(
            decode_fixed_exec_request(&[]),
            Err(FixedExecDecodeError::Truncated)
        );
        assert_eq!(
            decode_fixed_exec_request(&[2]),
            Err(FixedExecDecodeError::UnsupportedVersion)
        );
        assert_eq!(
            decode_fixed_exec_request(&[1, 0, 4, b's', b'h', b' ', b'-']),
            Err(FixedExecDecodeError::InvalidExecutable)
        );
        assert_eq!(
            decode_fixed_exec_request(&[1, 0, 4, b't', b'o', b'o', b'l', 0, 0, 0, 0, 0, 9]),
            Err(FixedExecDecodeError::Truncated)
        );
        assert_eq!(
            decode_fixed_exec_request(&[1, 0, 4, b't', b'o', b'o', b'l', 0, 0, 0, 0, 0, 0, 0]),
            Err(FixedExecDecodeError::TrailingBytes)
        );
    }

    #[test]
    fn error_codes_are_stable_and_content_free() {
        let error = FixedExecDecodeError::InvalidExecutable;
        assert_eq!(error.code(), "exec_stdio_invalid_executable");
        assert!(!error.code().contains("secret"));
    }
}
