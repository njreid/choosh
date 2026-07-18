#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use choosh_protocol::framing::{FrameDecoder, FrameLimits, encode_frame};

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
    let mut output = Vec::new();
    stream.read_to_end(&mut output).unwrap();
    let mut decoder = FrameDecoder::new(FrameLimits::new(1024 * 1024, 1).unwrap());
    assert_eq!(
        decoder.feed(&output).unwrap(),
        [format!(
            "{{\"capabilities\":[],\"daemon\":{{\"name\":\"chooshd\",\"version\":\"{}\"}},\"host\":{{\"name\":\"local-host\",\"version\":\"unknown\"}},\"kind\":\"welcome\",\"limits\":{{\"max_control_frame_bytes\":1048576,\"max_in_flight_requests\":64}},\"protocol\":{{\"major\":1,\"minor\":0}}}}",
            env!("CARGO_PKG_VERSION")
        )
        .into_bytes()]
    );
    decoder.finish().unwrap();
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
