#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use choosh_host::bridge::BridgeLimits;
use choosh_host::socket_relay::run_unix_socket_relay;

const MAX_SOCKET_PATH_BYTES: usize = 4_096;

fn main() -> ExitCode {
    let socket = match parse_rpc_args(std::env::args_os().skip(1)) {
        Ok(socket) => socket,
        Err(code) => {
            eprintln!("{code}");
            return ExitCode::from(2);
        }
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match run_unix_socket_relay(
        stdin.lock(),
        stdout.lock(),
        &socket,
        BridgeLimits::default(),
    ) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error.code());
            ExitCode::from(1)
        }
    }
}

fn parse_rpc_args<I>(args: I) -> Result<PathBuf, &'static str>
where
    I: IntoIterator<Item = OsString>,
{
    let args: Vec<OsString> = args.into_iter().collect();
    let [rpc, stdio, socket_flag, socket] = args.as_slice() else {
        return Err("invalid_arguments");
    };
    if rpc != "rpc" || stdio != "--stdio" || socket_flag != "--socket" {
        return Err("invalid_arguments");
    }
    validate_socket_path(Path::new(socket))
}

fn validate_socket_path(path: &Path) -> Result<PathBuf, &'static str> {
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().is_empty()
        || path.as_os_str().as_encoded_bytes().len() > MAX_SOCKET_PATH_BYTES
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err("invalid_socket_path");
    }
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn requires_exact_shell_free_rpc_grammar_and_absolute_path() {
        assert_eq!(
            parse_rpc_args(args(&[
                "rpc",
                "--stdio",
                "--socket",
                "/run/user/1000/choosh.sock"
            ])),
            Ok(PathBuf::from("/run/user/1000/choosh.sock"))
        );
        for invalid in [
            args(&["rpc", "--stdio"]),
            args(&["rpc", "--stdio", "--socket", "relative.sock"]),
            args(&["rpc", "--stdio", "--socket", "/tmp/../secret.sock"]),
            args(&["rpc", "--socket", "/tmp/choosh.sock", "--stdio"]),
        ] {
            assert!(parse_rpc_args(invalid).is_err());
        }
    }

    #[test]
    fn errors_are_stable_and_do_not_include_socket_path() {
        let secret = "/tmp/private-token/../choosh.sock";
        let error = parse_rpc_args(args(&["rpc", "--stdio", "--socket", secret])).unwrap_err();
        assert_eq!(error, "invalid_socket_path");
        assert!(!error.contains("private-token"));
    }
}
