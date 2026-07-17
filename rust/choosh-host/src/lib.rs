//! Shell-free command-line parsing for the Choosh host helper.

pub mod bridge;
pub mod dispatch;
#[cfg(unix)]
pub mod socket_relay;

use std::fmt;

const MAX_VALUE_BYTES: usize = 1_024;
const MAX_CAPABILITY_BYTES: usize = 4_096;
const MAX_COMMAND_ARGS: usize = 256;
const MAX_COMMAND_BYTES: usize = 64 * 1_024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    RpcStdio,
    EmitStdin,
    Stream { capability: Capability },
    ServiceRun(ServiceRun),
}

/// An opaque bearer capability. Its contents are deliberately excluded from
/// `Debug` and `Display` output.
#[derive(Clone, Eq, PartialEq)]
pub struct Capability(String);

impl Capability {
    /// Returns the capability for the transport boundary that consumes it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Capability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Capability([REDACTED])")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceRun {
    pub workspace: String,
    pub name: String,
    pub protocol: ServiceProtocol,
    pub port: u16,
    /// Exact argv supplied after `--`; it is never interpreted as shell text.
    pub argv: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceProtocol {
    Http,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseError {
    MissingCommand,
    UnknownCommand,
    MissingSubcommand,
    UnknownSubcommand,
    MissingFlag,
    DuplicateFlag,
    UnknownFlag,
    MissingValue,
    UnexpectedArgument,
    MissingSeparator,
    EmptyCommand,
    InvalidValue,
    UnsupportedProtocol,
    InvalidPort,
    LimitExceeded,
}

impl ParseError {
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingCommand => "missing_command",
            Self::UnknownCommand => "unknown_command",
            Self::MissingSubcommand => "missing_subcommand",
            Self::UnknownSubcommand => "unknown_subcommand",
            Self::MissingFlag => "missing_flag",
            Self::DuplicateFlag => "duplicate_flag",
            Self::UnknownFlag => "unknown_flag",
            Self::MissingValue => "missing_value",
            Self::UnexpectedArgument => "unexpected_argument",
            Self::MissingSeparator => "missing_separator",
            Self::EmptyCommand => "empty_command",
            Self::InvalidValue => "invalid_value",
            Self::UnsupportedProtocol => "unsupported_protocol",
            Self::InvalidPort => "invalid_port",
            Self::LimitExceeded => "limit_exceeded",
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for ParseError {}

/// Parses host-helper arguments without invoking or emulating a shell.
///
/// # Errors
///
/// Returns a stable [`ParseError`] when the command grammar, value bounds, or
/// supported service settings are invalid.
pub fn parse<I, S>(args: I) -> Result<Command, ParseError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let Some((head, tail)) = args.split_first() else {
        return Err(ParseError::MissingCommand);
    };
    match head.as_str() {
        "rpc" => exact_switch(tail, "--stdio", Command::RpcStdio),
        "emit" => exact_switch(tail, "--stdin", Command::EmitStdin),
        "stream" => parse_stream(tail),
        "service" => parse_service(tail),
        _ => Err(ParseError::UnknownCommand),
    }
}

fn exact_switch(args: &[String], required: &str, command: Command) -> Result<Command, ParseError> {
    match args {
        [] => Err(ParseError::MissingFlag),
        [flag] if flag == required => Ok(command),
        [flag] if flag.starts_with('-') => Err(ParseError::UnknownFlag),
        [first, second, ..] if first == required && second == required => {
            Err(ParseError::DuplicateFlag)
        }
        _ => Err(ParseError::UnexpectedArgument),
    }
}

fn parse_stream(args: &[String]) -> Result<Command, ParseError> {
    if args.is_empty() {
        return Err(ParseError::MissingFlag);
    }
    let mut capability = None;
    let mut index = 0;
    while index < args.len() {
        if args[index] != "--capability" {
            return Err(if args[index].starts_with('-') {
                ParseError::UnknownFlag
            } else {
                ParseError::UnexpectedArgument
            });
        }
        if capability.is_some() {
            return Err(ParseError::DuplicateFlag);
        }
        let value = args.get(index + 1).ok_or(ParseError::MissingValue)?;
        if value.starts_with("--") {
            return Err(ParseError::MissingValue);
        }
        validate(value, MAX_CAPABILITY_BYTES)?;
        capability = Some(Capability(value.clone()));
        index += 2;
    }
    Ok(Command::Stream {
        capability: capability.ok_or(ParseError::MissingFlag)?,
    })
}

fn parse_service(tokens: &[String]) -> Result<Command, ParseError> {
    let Some((subcommand, tokens)) = tokens.split_first() else {
        return Err(ParseError::MissingSubcommand);
    };
    if subcommand != "run" {
        return Err(ParseError::UnknownSubcommand);
    }

    let separator = tokens
        .iter()
        .position(|arg| arg == "--")
        .ok_or(ParseError::MissingSeparator)?;
    let (flags, command_with_separator) = tokens.split_at(separator);
    let command_argv = &command_with_separator[1..];
    if command_argv.is_empty() || command_argv[0].is_empty() {
        return Err(ParseError::EmptyCommand);
    }
    if command_argv.len() > MAX_COMMAND_ARGS {
        return Err(ParseError::LimitExceeded);
    }
    let command_bytes = command_argv.iter().try_fold(0usize, |total, arg| {
        validate_no_controls(arg)?;
        total
            .checked_add(arg.len())
            .and_then(|value| value.checked_add(1))
            .ok_or(ParseError::LimitExceeded)
    })?;
    if command_bytes > MAX_COMMAND_BYTES {
        return Err(ParseError::LimitExceeded);
    }

    let mut workspace = None;
    let mut name = None;
    let mut protocol = None;
    let mut port = None;
    let mut index = 0;
    while index < flags.len() {
        let flag = flags[index].as_str();
        if !matches!(flag, "--workspace" | "--name" | "--protocol" | "--port") {
            return Err(if flag.starts_with('-') {
                ParseError::UnknownFlag
            } else {
                ParseError::UnexpectedArgument
            });
        }
        let value = flags.get(index + 1).ok_or(ParseError::MissingValue)?;
        if value.starts_with("--") {
            return Err(ParseError::MissingValue);
        }
        match flag {
            "--workspace" => set_once(&mut workspace, validated(value)?)?,
            "--name" => set_once(&mut name, validated(value)?)?,
            "--protocol" => {
                let parsed = if value == "http" {
                    ServiceProtocol::Http
                } else {
                    return Err(ParseError::UnsupportedProtocol);
                };
                set_once(&mut protocol, parsed)?;
            }
            "--port" => {
                validate(value, 5)?;
                let parsed = value.parse::<u16>().map_err(|_| ParseError::InvalidPort)?;
                if parsed == 0 {
                    return Err(ParseError::InvalidPort);
                }
                set_once(&mut port, parsed)?;
            }
            _ => unreachable!(),
        }
        index += 2;
    }

    Ok(Command::ServiceRun(ServiceRun {
        workspace: workspace.ok_or(ParseError::MissingFlag)?,
        name: name.ok_or(ParseError::MissingFlag)?,
        protocol: protocol.ok_or(ParseError::MissingFlag)?,
        port: port.ok_or(ParseError::MissingFlag)?,
        argv: command_argv.to_vec(),
    }))
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), ParseError> {
    if slot.replace(value).is_some() {
        Err(ParseError::DuplicateFlag)
    } else {
        Ok(())
    }
}

fn validated(value: &str) -> Result<String, ParseError> {
    validate(value, MAX_VALUE_BYTES)?;
    Ok(value.to_owned())
}

fn validate(value: &str, max_bytes: usize) -> Result<(), ParseError> {
    if value.is_empty() {
        return Err(ParseError::InvalidValue);
    }
    if value.len() > max_bytes {
        return Err(ParseError::LimitExceeded);
    }
    validate_no_controls(value)
}

fn validate_no_controls(value: &str) -> Result<(), ParseError> {
    if value.chars().any(char::is_control) {
        Err(ParseError::InvalidValue)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixed_commands_exactly() {
        assert_eq!(parse(["rpc", "--stdio"]), Ok(Command::RpcStdio));
        assert_eq!(parse(["emit", "--stdin"]), Ok(Command::EmitStdin));
        assert_eq!(parse(["rpc"]), Err(ParseError::MissingFlag));
        assert_eq!(
            parse(["rpc", "--stdio", "extra"]),
            Err(ParseError::UnexpectedArgument)
        );
    }

    #[test]
    fn capability_is_parsed_but_redacted_from_diagnostics() {
        let parsed = parse(["stream", "--capability", "top-secret"]).unwrap();
        let Command::Stream { capability } = parsed else {
            panic!("wrong command")
        };
        assert_eq!(capability.expose(), "top-secret");
        assert_eq!(format!("{capability:?}"), "Capability([REDACTED])");
        assert!(!format!("{capability:?}").contains("top-secret"));
        assert_eq!(
            parse(["stream", "--capability", "first", "--capability", "second",]),
            Err(ParseError::DuplicateFlag)
        );
    }

    #[test]
    fn service_preserves_exact_argv_without_shell_reconstruction() {
        let parsed = parse([
            "service",
            "run",
            "--port",
            "3000",
            "--name",
            "web app",
            "--protocol",
            "http",
            "--workspace",
            "app",
            "--",
            "sh",
            "-c",
            "printf '$HOME'",
            "a b",
        ])
        .unwrap();
        let Command::ServiceRun(service) = parsed else {
            panic!("wrong command")
        };
        assert_eq!(service.workspace, "app");
        assert_eq!(service.name, "web app");
        assert_eq!(service.port, 3000);
        assert_eq!(service.argv, ["sh", "-c", "printf '$HOME'", "a b"]);
    }

    #[test]
    fn service_rejects_missing_duplicate_and_unknown_flags() {
        let base = [
            "service",
            "run",
            "--workspace",
            "app",
            "--name",
            "web",
            "--protocol",
            "http",
            "--port",
            "3000",
            "--",
            "server",
        ];
        assert!(parse(base).is_ok());
        assert_eq!(
            parse(["service", "run", "--workspace", "app", "--", "server"]),
            Err(ParseError::MissingFlag)
        );
        assert_eq!(
            parse([
                "service",
                "run",
                "--workspace",
                "app",
                "--workspace",
                "other",
                "--name",
                "web",
                "--protocol",
                "http",
                "--port",
                "3000",
                "--",
                "server",
            ]),
            Err(ParseError::DuplicateFlag)
        );
        assert_eq!(
            parse(["service", "run", "--bogus", "x", "--", "server"]),
            Err(ParseError::UnknownFlag)
        );
    }

    #[test]
    fn service_rejects_invalid_protocol_ports_and_commands() {
        assert_eq!(
            parse([
                "service",
                "run",
                "--workspace",
                "app",
                "--name",
                "web",
                "--protocol",
                "https",
                "--port",
                "3000",
                "--",
                "server",
            ]),
            Err(ParseError::UnsupportedProtocol)
        );
        for port in ["0", "65536", "-1", "abc"] {
            assert_eq!(
                parse([
                    "service",
                    "run",
                    "--workspace",
                    "app",
                    "--name",
                    "web",
                    "--protocol",
                    "http",
                    "--port",
                    port,
                    "--",
                    "server",
                ]),
                Err(ParseError::InvalidPort)
            );
        }
        assert_eq!(
            parse([
                "service",
                "run",
                "--workspace",
                "app",
                "--name",
                "web",
                "--protocol",
                "http",
                "--port",
                "3000",
                "--",
            ]),
            Err(ParseError::EmptyCommand)
        );
    }

    #[test]
    fn rejects_controls_and_bounded_values() {
        assert_eq!(
            parse(["stream", "--capability", "secret\nleak"]),
            Err(ParseError::InvalidValue)
        );
        let oversized = "x".repeat(MAX_VALUE_BYTES + 1);
        assert_eq!(
            parse([
                "service",
                "run",
                "--workspace",
                oversized.as_str(),
                "--name",
                "web",
                "--protocol",
                "http",
                "--port",
                "3000",
                "--",
                "server",
            ]),
            Err(ParseError::LimitExceeded)
        );
    }
}
