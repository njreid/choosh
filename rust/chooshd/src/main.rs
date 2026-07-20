#![cfg(unix)]

#[cfg(test)]
use std::path::Path;
use std::{ffi::OsString, io, path::PathBuf};

use choosh_host::bridge::BridgeLimits;
use choosh_host::socket_relay::run_unix_socket_relay;
use choosh_protocol::handshake::{
    PROTOCOL_V1_MAJOR, PeerIdentity, ProtocolLimits, ProtocolVersion,
};
use chooshd::daemon::{DEFAULT_MAX_FRAME_BYTES, HandshakeConfig, bind, serve};
use chooshd::socket::{SocketPlan, verify_running_endpoint};

enum Invocation {
    Serve(SocketPlan),
    RpcStdio(SocketPlan),
}

fn main() {
    if let Err(code) = run(std::env::args_os().skip(1)) {
        eprintln!("{code}");
        std::process::exit(2);
    }
}

fn run(args: impl Iterator<Item = OsString>) -> Result<(), &'static str> {
    match parse_args(args)? {
        Invocation::Serve(plan) => run_daemon(&plan),
        Invocation::RpcStdio(plan) => {
            let stdin = io::stdin();
            let stdout = io::stdout();
            run_rpc_stdio(stdin.lock(), stdout.lock(), &plan)
        }
    }
}

fn run_daemon(plan: &SocketPlan) -> Result<(), &'static str> {
    let owned = bind(plan).map_err(|_| "socket_bind_failed")?;
    let config = HandshakeConfig {
        protocol: ProtocolVersion::new(PROTOCOL_V1_MAJOR, 0),
        daemon: PeerIdentity::new("chooshd", env!("CARGO_PKG_VERSION"))
            .map_err(|_| "invalid_handshake_config")?,
        host: PeerIdentity::new("local-host", "unknown").map_err(|_| "invalid_handshake_config")?,
        capabilities: Vec::new(),
        limits: ProtocolLimits::new(
            u32::try_from(DEFAULT_MAX_FRAME_BYTES).map_err(|_| "invalid_handshake_config")?,
            64,
        )
        .map_err(|_| "invalid_handshake_config")?,
    };
    serve(owned.listener(), &config, DEFAULT_MAX_FRAME_BYTES).map_err(|_| "daemon_serve_failed")
}

/// Relays bounded daemon frames over explicit stdio and socket-plan boundaries.
///
/// This composition root takes an already validated plan; it never reads a
/// socket path from an environment variable, home directory, or current
/// directory. The host helper supplies these values only from its own explicit
/// deployment configuration.
fn run_rpc_stdio<R: io::Read, W: io::Write>(
    input: R,
    output: W,
    plan: &SocketPlan,
) -> Result<(), &'static str> {
    verify_running_endpoint(plan).map_err(|_| "daemon_socket_unavailable")?;
    run_unix_socket_relay(input, output, plan.socket_path(), BridgeLimits::default())
        .map(|_| ())
        .map_err(|_| "rpc_stdio_failed")
}

fn parse_args(args: impl Iterator<Item = OsString>) -> Result<Invocation, &'static str> {
    let args: Vec<OsString> = args.collect();
    let (mode, flags) = match args.as_slice() {
        [command, stdio, tail @ ..] if command == "rpc" && stdio == "--stdio" => {
            (InvocationMode::RpcStdio, tail)
        }
        [command, ..] if command == "rpc" => return Err("invalid_rpc_mode"),
        _ => (InvocationMode::Serve, args.as_slice()),
    };
    let plan = parse_socket_plan(flags.iter().cloned())?;
    Ok(match mode {
        InvocationMode::Serve => Invocation::Serve(plan),
        InvocationMode::RpcStdio => Invocation::RpcStdio(plan),
    })
}

#[derive(Clone, Copy)]
enum InvocationMode {
    Serve,
    RpcStdio,
}

fn parse_socket_plan(mut args: impl Iterator<Item = OsString>) -> Result<SocketPlan, &'static str> {
    let mut state_dir = None;
    let mut socket = None;
    while let Some(flag) = args.next() {
        let value = args.next().ok_or("missing_argument_value")?;
        match flag.to_str() {
            Some("--state-dir") if state_dir.is_none() => state_dir = Some(PathBuf::from(value)),
            Some("--socket") if socket.is_none() => socket = Some(PathBuf::from(value)),
            Some("--state-dir" | "--socket") => return Err("duplicate_argument"),
            _ => return Err("unknown_argument"),
        }
    }
    SocketPlan::new(
        state_dir.ok_or("missing_state_dir")?,
        socket.ok_or("missing_socket")?,
    )
    .map_err(|_| "invalid_socket_plan")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_explicit_order_independent_and_complete() {
        let parsed = parse_args(
            [
                "--socket",
                "/tmp/state/daemon.sock",
                "--state-dir",
                "/tmp/state",
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
        )
        .unwrap();
        assert!(matches!(parsed, Invocation::Serve(plan)
            if plan.state_dir() == Path::new("/tmp/state")
            && plan.socket_path() == Path::new("/tmp/state/daemon.sock")));
        assert!(parse_args(std::iter::empty()).is_err());
    }

    #[test]
    fn rpc_stdio_requires_its_exact_mode_and_explicit_private_socket_plan() {
        let parsed = parse_args(
            [
                "rpc",
                "--stdio",
                "--state-dir",
                "/tmp/state",
                "--socket",
                "/tmp/state/daemon.sock",
            ]
            .into_iter()
            .map(OsString::from),
        )
        .unwrap();
        assert!(matches!(parsed, Invocation::RpcStdio(plan)
            if plan.state_dir() == Path::new("/tmp/state")
            && plan.socket_path() == Path::new("/tmp/state/daemon.sock")));

        for invalid in [
            vec!["rpc", "--stdio", "--socket", "/tmp/state/daemon.sock"],
            vec!["rpc", "--socket", "/tmp/state/daemon.sock"],
            vec![
                "rpc",
                "--stdio",
                "--state-dir",
                "/tmp/state",
                "--socket",
                "/tmp/other.sock",
            ],
        ] {
            assert!(parse_args(invalid.into_iter().map(OsString::from)).is_err());
        }
    }

    #[test]
    fn rpc_stdio_does_not_connect_an_unverified_injected_endpoint() {
        let plan =
            SocketPlan::new("/tmp/chooshd-missing", "/tmp/chooshd-missing/daemon.sock").unwrap();
        assert_eq!(
            run_rpc_stdio(io::empty(), Vec::new(), &plan),
            Err("daemon_socket_unavailable")
        );
    }
}
