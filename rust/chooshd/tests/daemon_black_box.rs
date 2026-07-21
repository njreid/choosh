#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use choosh_protocol::envelope::EnvelopeId;
use choosh_protocol::framing::encode_frame;
use choosh_protocol::handshake::{
    PROTOCOL_V1_MAJOR, PeerIdentity, ProtocolLimits, ProtocolVersion,
};
use chooshd::daemon::{DaemonRpc, HandshakeConfig, bind, serve_once_with_handler};
use chooshd::git::{StatusLimits, StatusSnapshot, parse_status};
use chooshd::git_status::{GitStatusError, GitStatusOperation};
use chooshd::socket::SocketPlan;

struct ProcessFixture {
    child: Child,
    root: PathBuf,
}

impl Drop for ProcessFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn composed_daemon_binds_privately_and_negotiates_typed_hello() {
    let root = std::env::current_dir()
        .unwrap()
        .join(format!("chooshd-black-box-{}", std::process::id()));
    let state = root.join("state");
    let socket = state.join("daemon.sock");
    fs::create_dir(&root).unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_chooshd"))
        .args([
            "--state-dir",
            state.to_str().unwrap(),
            "--socket",
            socket.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut fixture = ProcessFixture { child, root };

    let mut stream = connect_bounded(&socket).unwrap_or_else(|| {
        let status = fixture.child.try_wait().unwrap();
        panic!("daemon did not bind; status={status:?}")
    });
    assert_eq!(
        fs::metadata(&state).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let socket_metadata = fs::symlink_metadata(&socket).unwrap();
    assert!(socket_metadata.file_type().is_socket());
    assert_eq!(socket_metadata.permissions().mode() & 0o777, 0o600);

    let hello = br#"{"kind":"hello","protocol":{"major":1,"minor":0},"client":{"name":"black-box","version":"1"},"capabilities":[]}"#;
    stream
        .write_all(&encode_frame(hello, 1024).unwrap())
        .unwrap();
    let welcome = read_frame(&mut stream);
    assert_eq!(
        welcome,
        format!(
            "{{\"capabilities\":[],\"daemon\":{{\"name\":\"chooshd\",\"version\":\"{}\"}},\"host\":{{\"name\":\"local-host\",\"version\":\"unknown\"}},\"kind\":\"welcome\",\"limits\":{{\"max_control_frame_bytes\":1048576,\"max_in_flight_requests\":64}},\"protocol\":{{\"major\":1,\"minor\":0}}}}",
            env!("CARGO_PKG_VERSION")
        )
        .into_bytes()
    );
    let request = br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000001","method":"host.describe","params":{}}"#;
    stream
        .write_all(&encode_frame(request, 1024).unwrap())
        .unwrap();
    let response = read_frame(&mut stream);
    assert!(matches!(
        choosh_protocol::wire::decode_envelope(&response, 1024 * 1024),
        Ok(choosh_protocol::wire::WireEnvelope::Response(_))
    ));
}

#[derive(Clone)]
struct FixedStatus(StatusSnapshot);

impl GitStatusOperation for FixedStatus {
    fn status_snapshot(&self) -> Result<StatusSnapshot, GitStatusError> {
        Ok(self.0.clone())
    }
}

#[test]
fn registered_git_status_crosses_the_private_socket_with_opaque_paths() {
    const WORKSPACE_ID: &str = "00000000-0000-0000-0000-000000000041";
    let root = std::env::temp_dir().join(format!(
        "chooshd-git-status-black-box-{}",
        std::process::id()
    ));
    let state = root.join("state");
    let socket = state.join("daemon.sock");
    fs::create_dir(&root).unwrap();
    let plan = SocketPlan::new(&state, &socket).unwrap();
    let owned = bind(&plan).unwrap();

    let snapshot = parse_status(
        b" M src/\xff.rs\0",
        StatusLimits {
            max_bytes: 64,
            max_entries: 2,
            max_path_bytes: 32,
        },
    )
    .unwrap();
    let mut handler = DaemonRpc::new();
    handler
        .register_git_status(
            EnvelopeId::new(WORKSPACE_ID).unwrap(),
            Arc::new(FixedStatus(snapshot)),
        )
        .unwrap();
    let config = handshake_config();

    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            serve_once_with_handler(owned.listener(), &config, 1024, &handler).unwrap();
        });
        let mut stream = connect_bounded(&socket).expect("daemon accepts private socket client");
        let hello = br#"{"kind":"hello","protocol":{"major":1,"minor":0},"client":{"name":"black-box","version":"1"},"capabilities":[]}"#;
        let request = format!(
            "{{\"kind\":\"request\",\"id\":\"00000000-0000-0000-0000-000000000042\",\"method\":\"git.status\",\"params\":{{\"workspace_id\":\"{WORKSPACE_ID}\"}}}}"
        );
        stream
            .write_all(&encode_frame(hello, 1024).unwrap())
            .unwrap();
        stream
            .write_all(&encode_frame(request.as_bytes(), 1024).unwrap())
            .unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();

        let _welcome = read_frame(&mut stream);
        let response = read_frame(&mut stream);
        server.join().unwrap();
        assert_eq!(
            response,
            format!(
                "{{\"id\":\"00000000-0000-0000-0000-000000000042\",\"kind\":\"response\",\"result\":{{\"entries\":[{{\"new_path_b64\":\"c3JjL_8ucnM\",\"staged\":\"unmodified\",\"unstaged\":\"modified\"}}],\"workspace_id\":\"{WORKSPACE_ID}\"}}}}"
            )
            .into_bytes()
        );
    });
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rpc_stdio_performs_daemon_handshake_before_forwarding_the_ssh_request() {
    let root = std::env::temp_dir().join(format!(
        "chooshd-rpc-stdio-black-box-{}",
        std::process::id()
    ));
    let state = root.join("state");
    let socket = state.join("daemon.sock");
    fs::create_dir(&root).unwrap();
    let plan = SocketPlan::new(&state, &socket).unwrap();
    let owned = bind(&plan).unwrap();
    let config = handshake_config();

    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            serve_once_with_handler(owned.listener(), &config, 1024, &DaemonRpc::new()).unwrap();
        });
        let mut child = Command::new(env!("CARGO_BIN_EXE_chooshd"))
            .args([
                "rpc",
                "--stdio",
                "--state-dir",
                state.to_str().unwrap(),
                "--socket",
                socket.to_str().unwrap(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let request = br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000031","method":"host.describe","params":{}}"#;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&encode_frame(request, 1024).unwrap())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        server.join().unwrap();

        assert!(
            output.status.success(),
            "rpc stdio failed: {:?}",
            output.stderr
        );
        assert_eq!(
            read_frame(&mut output.stdout.as_slice()),
            br#"{"id":"00000000-0000-0000-0000-000000000031","kind":"response","result":{"capabilities":[],"daemon":{"name":"chooshd","version":"test"},"host":{"name":"local-host","version":"test"},"limits":{"max_control_frame_bytes":1024,"max_in_flight_requests":4},"protocol":{"major":1,"minor":0}}}"#
        );
    });
    fs::remove_dir_all(root).unwrap();
}

fn handshake_config() -> HandshakeConfig {
    HandshakeConfig {
        protocol: ProtocolVersion::new(PROTOCOL_V1_MAJOR, 0),
        daemon: PeerIdentity::new("chooshd", "test").unwrap(),
        host: PeerIdentity::new("local-host", "test").unwrap(),
        capabilities: Vec::new(),
        limits: ProtocolLimits::new(1024, 4).unwrap(),
    }
}

fn read_frame(stream: &mut impl Read) -> Vec<u8> {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).unwrap();
    let length = u32::from_be_bytes(header) as usize;
    assert!(length <= 1024 * 1024);
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).unwrap();
    payload
}

fn connect_bounded(path: &std::path::Path) -> Option<UnixStream> {
    for _ in 0..100_000 {
        match UnixStream::connect(path) {
            Ok(stream) => return Some(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                std::thread::yield_now();
            }
            Err(_) => return None,
        }
    }
    None
}
