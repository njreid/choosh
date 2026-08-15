//! Real, end-to-end coverage for `choosh-hostd`'s self-update mechanism
//! (`docs/specs/host-deployment.md`'s "Self-update" and "Rollback"
//! subsections), driven against the real compiled `choosh-hostd` binary
//! under a real `systemd --user` unit — not mocked, per this crate's
//! established testing discipline (`zellij_ops.rs`, `credential.rs`,
//! `frame_channel.rs` all exercise the real thing rather than a fake).
//!
//! Two properties this proves for real, not by inspection:
//!
//! 1. A restart triggered by `choosh-hostd`'s own update-apply path does
//!    NOT disturb a real Zellij session or the long-running process
//!    inside it — host-deployment.md's Self-update section, and M8's
//!    exit criterion ("completes with no dropped Zellij session").
//! 2. A pushed binary that never becomes healthy is rolled back to the
//!    previous binary within the bounded window, the service comes back
//!    up healthy with it, and an `agent_status` failure event is reported
//!    upstream — host-deployment.md's Rollback subsection, and M8's exit
//!    criterion ("a clean rollback when the pushed binary is deliberately
//!    broken in a test").
//!
//! Requires a real `systemd --user` session (confirmed present in this
//! environment) and the real `zellij` binary on `$PATH`.
//!
//! ## Why not a hand-spawned Zellij session moved into the unit's cgroup
//!
//! An earlier draft of this test tried to create the Zellij session from
//! the test process itself and move it into the target unit's cgroup via
//! `cgroup.procs`, to avoid driving the real `host-rpc.md` RPC path.
//! Confirmed by direct experiment that this fails with `EPERM`: this test
//! process's own cgroup (`user-<uid>.slice/session-N.scope`, a login
//! session scope) is not part of the `user@<uid>.service` delegated
//! subtree the target unit's cgroup lives under, and cgroup v2 requires
//! write access to a common delegated ancestor to move a process across
//! subtrees. So this test instead drives the exact real path a phone
//! would: it plays `relayd`'s role well enough to open an `rpc`-purpose
//! tunnel and send real `workspace.create`/`item.create` requests, which
//! is what actually makes `choosh-hostd` itself spawn the `zellij attach`
//! client (and therefore land the Zellij server in the unit's own cgroup
//! by ordinary fork inheritance — no cgroup surgery needed).

use choosh_hostd::credential::{self, Credential};
use choosh_protocol::framing::{FrameDecoder, FrameLimits, encode_frame};
use choosh_protocol::host_rpc::{ItemType, ProjectSource, RpcRequest, RpcResponse};
use choosh_protocol::relay::{
    AuthOk, AuthResult, ClientAuth, FRAME_CLASS_CONTROL, IdentityClass, MAX_CONTROL_FRAME_BYTES, MAX_TUNNEL_FRAME_BYTES,
    ServerHello, ServerPush, TUNNEL_ID_BYTES, WireAgentEvent, WireAgentStatus, decode_tunnel_frame, encode_tunnel_frame,
};
use ed25519_dalek::SigningKey;
use futures_util::{SinkExt, StreamExt};
use serde::{Serialize, de::DeserializeOwned};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

type WsStream = WebSocketStream<TcpStream>;

// ---------------------------------------------------------------------
// Low-level fake-relayd wire helpers, matching tests/serve_enrollment.rs's
// established hand-rolled framing style (no `FrameChannel` — that type is
// crate-private, and this is a separate integration-test crate).
// ---------------------------------------------------------------------

async fn send_control<T: Serialize>(ws: &mut WsStream, value: &T) {
    let mut payload = vec![FRAME_CLASS_CONTROL];
    payload.extend(serde_json::to_vec(value).unwrap());
    let wire = encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).unwrap();
    ws.send(Message::Binary(wire.into())).await.unwrap();
}

async fn recv_control<T: DeserializeOwned>(ws: &mut WsStream) -> Option<T> {
    let Some(Ok(Message::Binary(bytes))) = ws.next().await else { return None };
    let mut decoder = FrameDecoder::new(FrameLimits::new(MAX_CONTROL_FRAME_BYTES, 1).unwrap());
    let frames = decoder.feed(&bytes).ok()?;
    let (class, body) = frames.first()?.split_first()?;
    assert_eq!(*class, FRAME_CLASS_CONTROL, "expected a control frame");
    serde_json::from_slice(body).ok()
}

async fn send_rpc_request(ws: &mut WsStream, tunnel_id: [u8; TUNNEL_ID_BYTES], request: &RpcRequest) {
    let mut inner = vec![FRAME_CLASS_CONTROL];
    inner.extend(serde_json::to_vec(request).unwrap());
    let tunnel_payload = encode_tunnel_frame(tunnel_id, &inner);
    let wire = encode_frame(&tunnel_payload, MAX_TUNNEL_FRAME_BYTES).unwrap();
    ws.send(Message::Binary(wire.into())).await.unwrap();
}

/// Reads WS messages until one decodes as a tunnel-frame response for
/// `tunnel_id` (skipping anything else — `choosh-hostd` may interleave
/// other tunnel/control traffic, though in this test's fixed scenarios it
/// never legitimately does).
async fn recv_rpc_response(ws: &mut WsStream, tunnel_id: [u8; TUNNEL_ID_BYTES]) -> RpcResponse {
    loop {
        let Some(Ok(Message::Binary(bytes))) = ws.next().await else { panic!("connection ended before an rpc response arrived") };
        let mut decoder = FrameDecoder::new(FrameLimits::new(MAX_TUNNEL_FRAME_BYTES, 1).unwrap());
        let Ok(frames) = decoder.feed(&bytes) else { continue };
        let Some(frame) = frames.first() else { continue };
        let Some((id, payload)) = decode_tunnel_frame(frame) else { continue };
        if id != tunnel_id {
            continue;
        }
        let (inner_class, inner_body) = payload.split_first().expect("non-empty rpc tunnel payload");
        assert_eq!(*inner_class, FRAME_CLASS_CONTROL);
        return serde_json::from_slice(inner_body).expect("valid RpcResponse JSON");
    }
}

async fn accept_and_authenticate(listener: &TcpListener) -> WsStream {
    let (stream, _addr) = listener.accept().await.expect("accept a devhost connection");
    let mut ws = tokio_tungstenite::accept_async(stream).await.expect("ws handshake");
    send_control(&mut ws, &ServerHello { nonce: "test-nonce".to_string() }).await;
    let auth: ClientAuth = recv_control(&mut ws).await.expect("expected ClientAuth");
    let device_id = match auth {
        ClientAuth::Device(device_auth) => device_auth.device_id,
        ClientAuth::Phone(_) => panic!("expected device auth, not phone auth"),
    };
    send_control(&mut ws, &AuthResult::Ok(AuthOk { identity_class: IdentityClass::Devhost, device_id })).await;
    ws
}

/// Serves exactly one HTTP GET request with a fixed byte body, matching
/// `update.rs`'s own test helper (duplicated rather than shared across
/// crate boundaries — this is a separate integration-test binary).
async fn serve_one_download(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _addr) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buf).await;
        let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        tokio::io::AsyncWriteExt::write_all(&mut stream, header.as_bytes()).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, &body).await.unwrap();
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
    });
    format!("http://{addr}/choosh-hostd-test-download")
}

fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

// ---------------------------------------------------------------------
// Real systemd --user test harness
// ---------------------------------------------------------------------

struct TestHost {
    root: tempfile::TempDir,
    unit_name: String,
    unit_path: std::path::PathBuf,
    workspaces_dir: std::path::PathBuf,
    socket_path: std::path::PathBuf,
    /// The isolated `XDG_RUNTIME_DIR` this host's `choosh-hostd` runs
    /// with — any Zellij session it creates lives under
    /// `<this>/zellij/...`, not this test process's own default runtime
    /// dir, so cleanup (`zellij_cleanup`) must target this same directory
    /// or it silently no-ops against the wrong Zellij runtime and leaks
    /// the real `zellij --server` process.
    xdg_runtime_dir: std::path::PathBuf,
}

impl Drop for TestHost {
    fn drop(&mut self) {
        let _ = std::process::Command::new("systemctl").args(["--user", "stop", &self.unit_name]).status();
        let _ = std::fs::remove_file(&self.unit_path);
        let _ = std::process::Command::new("systemctl").args(["--user", "daemon-reload"]).status();
    }
}

/// Installs a throwaway `systemd --user` unit running the real, compiled
/// `choosh-hostd` binary (`env!("CARGO_BIN_EXE_choosh-hostd")`) — copied
/// to an isolated path first, since this module's own update logic
/// rewrites its own binary's file in place, and overwriting the shared
/// Cargo-built test artifact would be both wrong and dangerous (other
/// tests, and `cargo test`'s own re-invocation, depend on that file being
/// the real thing). `KillMode=process` matches `scripts/install.sh`
/// exactly — this is the property under test, not incidental.
fn start_test_host(relay_url: &str) -> TestHost {
    let root = tempfile::tempdir().unwrap();
    let installed_binary = root.path().join("choosh-hostd");
    std::fs::copy(env!("CARGO_BIN_EXE_choosh-hostd"), &installed_binary).unwrap();

    let home_dir = root.path().join("home");
    let xdg_runtime_dir = root.path().join("runtime");
    let workspaces_dir = root.path().join("workspaces");
    for dir in [&home_dir, &xdg_runtime_dir, &workspaces_dir] {
        std::fs::create_dir_all(dir).unwrap();
    }

    let credential_path = root.path().join("credential.json");
    let signing_key = SigningKey::generate(&mut rand::rng());
    let cred = Credential::new("selftest-device".to_string(), "fake-cert".to_string(), &signing_key);
    credential::save(&credential_path, &cred).unwrap();

    let unit_name = format!("choosh-hostd-selftest-{}.service", uuid::Uuid::new_v4());
    let unit_dir = dirs_config_systemd_user();
    std::fs::create_dir_all(&unit_dir).unwrap();
    let unit_path = unit_dir.join(&unit_name);

    let path_env = std::env::var("PATH").unwrap_or_default();
    let unit_content = format!(
        "[Unit]\nDescription=choosh-hostd self-update test\n\n\
         [Service]\nType=simple\nExecStart={bin} serve\nRestart=on-failure\nKillMode=process\n\
         Environment=CHOOSH_RELAYD_URL={relay_url}\n\
         Environment=CHOOSH_HOSTD_CREDENTIAL_PATH={cred}\n\
         Environment=CHOOSH_HOSTD_WORKSPACES_DIR={workspaces}\n\
         Environment=CHOOSH_HOSTD_REGISTRY_PATH={registry}\n\
         Environment=CHOOSH_HOSTD_MISE_HOST_TOOLS_DIR={mise_host}\n\
         Environment=CHOOSH_HOSTD_MISE_PROJECT_TOOLS_DIR={mise_project}\n\
         Environment=CHOOSH_HOSTD_SERVICE_UNIT={unit_name}\n\
         Environment=CHOOSH_HOSTD_UPDATE_HEALTH_WINDOW_MS=2000\n\
         Environment=CHOOSH_HOSTD_UPDATE_POLL_MS=200\n\
         Environment=CHOOSH_HOSTD_UPDATE_SETTLE_MS=200\n\
         Environment=HOME={home}\n\
         Environment=XDG_RUNTIME_DIR={xdg_runtime}\n\
         Environment=PATH={path}\n",
        bin = installed_binary.display(),
        relay_url = relay_url,
        cred = credential_path.display(),
        workspaces = workspaces_dir.display(),
        registry = root.path().join("registry.json").display(),
        mise_host = root.path().join("mise-host").display(),
        mise_project = root.path().join("mise-project").display(),
        unit_name = unit_name,
        home = home_dir.display(),
        xdg_runtime = xdg_runtime_dir.display(),
        path = path_env,
    );
    std::fs::write(&unit_path, unit_content).unwrap();

    let socket_path = temp_env::with_vars(
        [("HOME", Some(home_dir.to_str().unwrap())), ("XDG_RUNTIME_DIR", Some(xdg_runtime_dir.to_str().unwrap()))],
        choosh_hostd::local_ipc::default_socket_path,
    )
    .unwrap();

    let status = std::process::Command::new("systemctl").args(["--user", "daemon-reload"]).status().unwrap();
    assert!(status.success());
    let status = std::process::Command::new("systemctl").args(["--user", "start", &unit_name]).status().unwrap();
    assert!(status.success(), "systemctl --user start {unit_name} failed");

    TestHost { root, unit_name, unit_path, workspaces_dir, socket_path, xdg_runtime_dir }
}

fn dirs_config_systemd_user() -> std::path::PathBuf {
    let home = std::env::var("HOME").expect("real $HOME must be set for the test's own systemd --user session");
    std::path::PathBuf::from(home).join(".config/systemd/user")
}

/// Polls [`choosh_hostd::local_ipc::probe`] until the daemon's local IPC
/// socket is reachable (proving `serve::run` completed its early startup)
/// or `timeout` elapses.
async fn wait_for_socket(socket_path: &std::path::Path, timeout: std::time::Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if choosh_hostd::local_ipc::probe(socket_path, std::time::Duration::from_millis(300)).await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

fn counter_script(counter_file: &std::path::Path) -> Vec<String> {
    vec![
        "sh".to_string(),
        "-c".to_string(),
        format!("i=0; while :; do i=$((i+1)); echo \"$i\" > {path}; sleep 1; done", path = counter_file.display()),
    ]
}

fn read_counter(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

// ---------------------------------------------------------------------
// Test 1: a self-update restart must not disturb a real Zellij session.
// ---------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // this is the single most important test in this task's scope (per its own task brief); it stays as one linear, readable narrative end to end rather than being fragmented across helper functions that would each need their own context.
async fn self_update_restart_does_not_disturb_a_real_zellij_session_or_its_process() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_url = format!("ws://{}/connect", listener.local_addr().unwrap());
    let host = start_test_host(&relay_url);

    let mut ws = accept_and_authenticate(&listener).await;
    assert!(wait_for_socket(&host.socket_path, std::time::Duration::from_secs(15)).await, "daemon never came up initially");

    // Drives the real `workspace.create` + `item.create` RPCs — the exact
    // path that makes `choosh-hostd` itself spawn `zellij attach`, so the
    // resulting Zellij server lands in the *unit's own* cgroup by ordinary
    // fork inheritance (see this file's module doc comment for why this
    // is necessary rather than spawning Zellij from the test process).
    let workspace_dir = host.workspaces_dir.join("proj1");
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let tunnel_id = [0xABu8; TUNNEL_ID_BYTES];
    send_control(&mut ws, &ServerPush::TunnelOffered { tunnel_id: hex(tunnel_id), from_device_id: "phone-fake".to_string(), purpose: "rpc".to_string() }).await;

    send_rpc_request(
        &mut ws,
        tunnel_id,
        &RpcRequest::WorkspaceCreate {
            request_id: "req-1".to_string(),
            devhost_id: "selftest-device".to_string(),
            workspace_name: "proj1".to_string(),
            project_source: ProjectSource::ExistingPath { existing_path: "proj1".to_string() },
            parent_workspace_id: None,
        },
    )
    .await;
    let RpcResponse::WorkspaceCreateOk { workspace_id, .. } = recv_rpc_response(&mut ws, tunnel_id).await else {
        panic!("workspace.create failed");
    };

    let counter_file = host.root.path().join("counter.txt");
    send_rpc_request(
        &mut ws,
        tunnel_id,
        &RpcRequest::ItemCreate {
            request_id: "req-2".to_string(),
            workspace_id,
            item_type: ItemType::WebService,
            name: "counter".to_string(),
            agent: None,
            command: Some(counter_script(&counter_file)),
            port: Some(free_tcp_port()),
        },
    )
    .await;
    let RpcResponse::ItemCreateOk { .. } = recv_rpc_response(&mut ws, tunnel_id).await else {
        panic!("item.create failed");
    };

    // Confirm the counter is actually running (not just that the RPC
    // succeeded) before touching anything else.
    let mut first_value = None;
    for _ in 0..50 {
        if let Some(v) = read_counter(&counter_file) {
            first_value = Some(v);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let first_value = first_value.expect("counter file never appeared — the Zellij tab never actually started");
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let before_update = read_counter(&counter_file).expect("counter file disappeared");
    assert!(before_update > first_value, "counter must be genuinely incrementing before the update, not just present once");

    let zellij_server_pid_before = zellij_server_pid("proj1", &host.xdg_runtime_dir);

    // Sample the counter continuously across the whole restart window, in
    // the background, so this test can prove the process never actually
    // stalled — not just "before" vs. "after" snapshots.
    let sampler_file = counter_file.clone();
    let sampler = tokio::spawn(async move {
        let mut samples = Vec::new();
        for _ in 0..100 {
            samples.push((tokio::time::Instant::now(), read_counter(&sampler_file)));
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }
        samples
    });

    // Trigger a real self-update whose "new" binary is byte-identical to
    // the one already running, so it comes up healthy with no rollback —
    // this test is about restart safety, not the digest/rollback paths
    // (covered separately below).
    let new_bytes = std::fs::read(env!("CARGO_BIN_EXE_choosh-hostd")).unwrap();
    let expected_digest = choosh_hostd::fs_ops::content_revision(&new_bytes);
    let download_url = serve_one_download(new_bytes).await;
    send_control(
        &mut ws,
        &ServerPush::UpdateBinary {
            push_id: "push-zellij-survival".to_string(),
            download_url,
            sha256: expected_digest,
            version: "1.0.1".to_string(),
        },
    )
    .await;

    // The old connection ends once the old process is restarted; accept
    // the reconnect to confirm the new instance came back up and
    // re-authenticated for real.
    drop(ws);
    let mut ws2 = accept_and_authenticate(&listener).await;
    assert!(wait_for_socket(&host.socket_path, std::time::Duration::from_secs(20)).await, "daemon never came back up after the update restart");

    let counter_samples = sampler.await.unwrap();
    let observed: Vec<u64> = counter_samples.iter().filter_map(|(_, v)| *v).collect();
    assert!(!observed.is_empty(), "the counter must have been observed running during the restart window");

    // The single most important assertion: the counter genuinely kept
    // incrementing *throughout* — no gap longer than a handful of its own
    // 1s tick, which would indicate the process was killed and (at best)
    // a fresh one started from zero, or (at worst) never came back.
    let mut max_stall = std::time::Duration::ZERO;
    let mut last_change: Option<(tokio::time::Instant, u64)> = None;
    for (t, v) in counter_samples.iter().filter_map(|(t, v)| v.map(|v| (*t, v))) {
        match last_change {
            Some((last_t, last_v)) if last_v == v => {
                max_stall = max_stall.max(t - last_t);
            }
            _ => {}
        }
        last_change = Some((t, v));
    }
    assert!(max_stall < std::time::Duration::from_secs(5), "counter stalled for {max_stall:?} — the process was disturbed by the restart");

    let after_update = read_counter(&counter_file).expect("counter file must still exist after the restart");
    assert!(after_update > before_update, "counter must have kept incrementing across the restart (was {before_update}, now {after_update})");

    let zellij_server_pid_after = zellij_server_pid("proj1", &host.xdg_runtime_dir);
    assert_eq!(
        zellij_server_pid_before, zellij_server_pid_after,
        "the Zellij server's own PID must be unchanged — a new PID would mean a fresh server, not the same session surviving"
    );

    zellij_cleanup("proj1", &host.xdg_runtime_dir);
    let _ = ws2.close(None).await;
}

fn hex(bytes: [u8; TUNNEL_ID_BYTES]) -> String {
    choosh_protocol::relay::encode_tunnel_id_hex(bytes)
}

/// PID of the real `zellij --server .../<session_name>` process, found via
/// a real `ps` scan — the same "trust the OS, not our own bookkeeping"
/// posture this whole test suite uses elsewhere. Matches on the full
/// `xdg_runtime_dir`-qualified socket path, not just the bare session
/// name, so a stale/leaked server from an unrelated run (a different
/// tempdir, hence a different path) can never be mistaken for this test's
/// own.
fn zellij_server_pid(session_name: &str, xdg_runtime_dir: &std::path::Path) -> u32 {
    let output = std::process::Command::new("ps").args(["-eo", "pid,cmd"]).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    let needle = xdg_runtime_dir.join("zellij/contract_version_1").join(session_name);
    let needle = needle.to_str().unwrap();
    for line in text.lines() {
        if line.contains("zellij --server") && line.contains(needle) {
            let pid: u32 = line.split_whitespace().next().unwrap().parse().unwrap();
            return pid;
        }
    }
    panic!("no zellij --server process found for {needle}; ps output:\n{text}");
}

/// Targets `xdg_runtime_dir` explicitly (rather than this test process's
/// own default `$XDG_RUNTIME_DIR`) — that's where the test host's own
/// `choosh-hostd` (and therefore its Zellij server) actually lives, per
/// [`TestHost::xdg_runtime_dir`]'s doc comment. Without this override,
/// `zellij kill-session` looks in the wrong runtime directory, silently
/// finds nothing, and leaks the real server process.
fn zellij_cleanup(session_name: &str, xdg_runtime_dir: &std::path::Path) {
    let _ = std::process::Command::new("zellij")
        .env("XDG_RUNTIME_DIR", xdg_runtime_dir)
        .args(["kill-session", session_name])
        .status();
    let _ = std::process::Command::new("zellij")
        .env("XDG_RUNTIME_DIR", xdg_runtime_dir)
        .args(["delete-session", session_name])
        .status();
}

// ---------------------------------------------------------------------
// Test 2: a broken pushed binary is rolled back and reported.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_broken_pushed_binary_is_rolled_back_and_reported_upstream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_url = format!("ws://{}/connect", listener.local_addr().unwrap());
    let host = start_test_host(&relay_url);

    let mut ws = accept_and_authenticate(&listener).await;
    assert!(wait_for_socket(&host.socket_path, std::time::Duration::from_secs(15)).await, "daemon never came up initially");

    // A "binary" that's really a shell script: it never binds the local
    // IPC socket (or does anything else `choosh-hostd` would), so it must
    // fail the health check on purpose.
    let broken = b"#!/bin/sh\nsleep 300\n".to_vec();
    let digest = choosh_hostd::fs_ops::content_revision(&broken);
    let download_url = serve_one_download(broken).await;
    send_control(
        &mut ws,
        &ServerPush::UpdateBinary {
            push_id: "push-broken".to_string(),
            download_url,
            sha256: digest,
            version: "0.0.0-broken".to_string(),
        },
    )
    .await;
    drop(ws);

    // The rolled-back (real, previously-running) binary reconnects once
    // it's healthy again — accept that reconnect and confirm the failure
    // report arrives over it.
    let mut ws2 = accept_and_authenticate(&listener).await;
    assert!(
        wait_for_socket(&host.socket_path, std::time::Duration::from_secs(30)).await,
        "daemon never came back up after rollback — the rollback itself failed"
    );

    let mut saw_failure_event = false;
    for _ in 0..20 {
        let Some(request) = tokio::time::timeout(std::time::Duration::from_secs(2), recv_control::<choosh_protocol::relay::ControlRequest>(&mut ws2))
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        if let choosh_protocol::relay::ControlRequest::AgentEvent {
            event: WireAgentEvent::AgentStatus { workspace_id, item_id, status: WireAgentStatus::Failed },
            request_id,
        } = &request
        {
            assert_eq!(workspace_id, choosh_hostd::update::SELF_UPDATE_WORKSPACE_ID);
            assert_eq!(item_id, choosh_hostd::update::SELF_UPDATE_ITEM_ID);
            send_control(&mut ws2, &choosh_protocol::relay::ControlResponse::AgentEventOk { request_id: request_id.clone() }).await;
            saw_failure_event = true;
            break;
        }
    }
    assert!(saw_failure_event, "expected an AgentStatus::Failed self-update failure report over the reconnected connection");
}
