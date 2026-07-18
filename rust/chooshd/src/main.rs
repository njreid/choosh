#![cfg(unix)]

use std::path::PathBuf;

use choosh_protocol::handshake::{
    PROTOCOL_V1_MAJOR, PeerIdentity, ProtocolLimits, ProtocolVersion,
};
use chooshd::daemon::{DEFAULT_MAX_FRAME_BYTES, HandshakeConfig, bind, serve};
use chooshd::socket::SocketPlan;

fn main() {
    if let Err(code) = run(std::env::args_os().skip(1)) {
        eprintln!("{code}");
        std::process::exit(2);
    }
}

fn run(args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), &'static str> {
    let (state_dir, socket_path) = parse_args(args)?;
    let plan = SocketPlan::new(state_dir, socket_path).map_err(|_| "invalid_socket_plan")?;
    let owned = bind(&plan).map_err(|_| "socket_bind_failed")?;
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

fn parse_args(
    mut args: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(PathBuf, PathBuf), &'static str> {
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
    Ok((
        state_dir.ok_or("missing_state_dir")?,
        socket.ok_or("missing_socket")?,
    ))
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
        assert_eq!(parsed.0, PathBuf::from("/tmp/state"));
        assert_eq!(parsed.1, PathBuf::from("/tmp/state/daemon.sock"));
        assert!(parse_args(std::iter::empty()).is_err());
    }
}
