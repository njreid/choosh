// See `lib.rs`'s matching attribute: this integration-test crate root calls
// into `choosh_hostd::serve::run()`, whose future nesting (deepened by
// `jj_ops.rs`'s real `jj-lib` calls) overflows rustc's default query-depth
// recursion limit — verified against a real build.
#![recursion_limit = "512"]
//! Integration coverage for `choosh-hostd serve`'s enrollment path against a
//! hand-rolled fake `relayd`, per `docs/specs/auth-and-enrollment.md`'s
//! devhost enrollment sequence. Deliberately does not depend on the real
//! `choosh-relayd` binary — a fake server here is faster and more reliable,
//! and the two sides only need to agree on the shared
//! `choosh_protocol::relay` wire types, which this test exercises directly.
//!
//! Env-var mutation goes through `temp_env::async_with_vars` rather than
//! `std::env::set_var` directly: the workspace forbids `unsafe_code`
//! (`set_var`/`remove_var` require an `unsafe` block in this edition), and
//! `temp_env` also serializes concurrently-running tests on a shared lock
//! and restores the prior environment afterward, which a hand-rolled
//! `set_var`/`remove_var` pair would not do safely under parallel tests.

use choosh_hostd::credential;
use choosh_hostd::serve::{self, ServeConfig};
use choosh_protocol::framing::{FrameDecoder, FrameLimits, encode_frame};
use choosh_protocol::relay::{
    ControlRequest, ControlResponse, FRAME_CLASS_CONTROL, MAX_CONTROL_FRAME_BYTES, ServerHello,
};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Accepts exactly one WebSocket connection, sends the `ServerHello` real
/// `relayd` sends unconditionally as the first frame on every connection
/// (enrollment included — see `docs/specs/relay-protocol.md`'s transport
/// section), reads one `enroll` control frame, and replies with either a
/// canned `EnrollOk` or a canned error — enough of `relayd`'s enrollment
/// behavior to drive `choosh-hostd`'s client side through a real (if
/// simplified) exchange. Omitting the `ServerHello` here previously let a
/// real bug (the client misparsing it as the enroll response) pass this
/// suite while failing against the real binary — see the fix alongside this
/// change.
async fn run_fake_relayd_enroll_once(listener: TcpListener, respond: impl FnOnce(ControlRequest) -> ControlResponse + Send + 'static) {
    let (stream, _addr) = listener.accept().await.expect("accept");
    let mut ws = tokio_tungstenite::accept_async(stream).await.expect("ws handshake");

    let hello = ServerHello { nonce: "test-nonce".to_string() };
    let mut hello_payload = vec![FRAME_CLASS_CONTROL];
    hello_payload.extend(serde_json::to_vec(&hello).unwrap());
    let hello_wire = encode_frame(&hello_payload, MAX_CONTROL_FRAME_BYTES).unwrap();
    ws.send(Message::Binary(hello_wire.into())).await.unwrap();

    let Some(Ok(Message::Binary(bytes))) = ws.next().await else {
        panic!("expected a binary enroll frame");
    };
    let mut decoder = FrameDecoder::new(FrameLimits::new(MAX_CONTROL_FRAME_BYTES, 1).unwrap());
    let frames = decoder.feed(&bytes).unwrap();
    assert_eq!(frames.len(), 1, "test client is expected to send exactly one frame per WS message");
    let (class, body) = frames[0].split_first().unwrap();
    assert_eq!(*class, FRAME_CLASS_CONTROL);
    let request: ControlRequest = serde_json::from_slice(body).unwrap();

    let response = respond(request);
    let mut payload = vec![FRAME_CLASS_CONTROL];
    payload.extend(serde_json::to_vec(&response).unwrap());
    let wire_bytes = encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).unwrap();
    ws.send(Message::Binary(wire_bytes.into())).await.unwrap();
    // Close cleanly so the client's subsequent read sees a clean EOF rather
    // than hanging — the client only needs this one response for `enroll`.
    let _ = ws.close(None).await;
}

async fn bind_fake_relayd() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, format!("ws://{addr}/connect"))
}

#[tokio::test]
async fn successful_enrollment_persists_a_usable_credential() {
    let (listener, relay_url) = bind_fake_relayd().await;
    let server = tokio::spawn(run_fake_relayd_enroll_once(listener, |request| match request {
        ControlRequest::Enroll { request_id, token, .. } => {
            assert_eq!(token, "one-shot-token");
            ControlResponse::EnrollOk {
                request_id,
                device_id: "dev-abc123".to_string(),
                certificate: "fake-certificate-bytes".to_string(),
            }
        }
        other => panic!("expected Enroll, got {other:?}"),
    }));

    let credential_dir = tempfile::tempdir().unwrap();
    let credential_path = credential_dir.path().join("device-credential.json");
    let config = ServeConfig::for_test(relay_url, credential_path.clone());

    let credential = Box::pin(temp_env::async_with_vars(
        [("CHOOSH_ENROLLMENT_TOKEN", Some("one-shot-token"))],
        serve::enroll(&config),
    ))
    .await
    .expect("enrollment should succeed");

    assert_eq!(credential.device_id, "dev-abc123");
    assert_eq!(credential.certificate, "fake-certificate-bytes");

    // Persisted to disk, not just returned in-memory.
    let reloaded = credential::load(&credential_path).unwrap().expect("credential file should exist");
    assert_eq!(reloaded, credential);

    server.await.unwrap();
}

#[tokio::test]
async fn relayd_rejecting_the_token_fails_closed_with_no_credential_written() {
    let (listener, relay_url) = bind_fake_relayd().await;
    let server = tokio::spawn(run_fake_relayd_enroll_once(listener, |request| match request {
        ControlRequest::Enroll { request_id, .. } => ControlResponse::Error {
            request_id,
            code: "token_invalid".to_string(),
            message: "token already consumed".to_string(),
        },
        other => panic!("expected Enroll, got {other:?}"),
    }));

    let credential_dir = tempfile::tempdir().unwrap();
    let credential_path = credential_dir.path().join("device-credential.json");
    let config = ServeConfig::for_test(relay_url, credential_path.clone());

    let result = Box::pin(temp_env::async_with_vars(
        [("CHOOSH_ENROLLMENT_TOKEN", Some("already-used-token"))],
        serve::enroll(&config),
    ))
    .await;

    assert!(result.is_err(), "a rejected token must not produce a credential");
    assert!(!credential_path.exists(), "no credential file should be written on rejection");

    server.await.unwrap();
}

#[tokio::test]
async fn enrollment_request_carries_a_valid_ssh_host_public_key() {
    // auth-and-enrollment.md step 6: devhost enrollment must include the
    // loopback SSH server's host public key — captured here as the same
    // value `ssh_keys::openssh_public_key_line` would format, proving
    // `serve::enroll` actually derives and sends it rather than leaving
    // the field `None` (its pre-M6 placeholder value).
    let (listener, relay_url) = bind_fake_relayd().await;
    let server = tokio::spawn(run_fake_relayd_enroll_once(listener, |request| match request {
        ControlRequest::Enroll { request_id, host_ssh_public_key, .. } => {
            let encoded = host_ssh_public_key.expect("devhost enrollment must carry a host_ssh_public_key");
            let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &encoded).expect("valid base64");
            assert_eq!(raw.len(), 32, "expected a raw 32-byte Ed25519 public key");
            ControlResponse::EnrollOk { request_id, device_id: "dev-ssh-key-test".to_string(), certificate: "fake-certificate-bytes".to_string() }
        }
        other => panic!("expected Enroll, got {other:?}"),
    }));

    let credential_dir = tempfile::tempdir().unwrap();
    let config = ServeConfig::for_test(relay_url, credential_dir.path().join("device-credential.json"));
    Box::pin(temp_env::async_with_vars([("CHOOSH_ENROLLMENT_TOKEN", Some("one-shot-token"))], serve::enroll(&config)))
        .await
        .expect("enrollment should succeed");

    server.await.unwrap();
}

#[tokio::test]
async fn missing_enrollment_token_fails_fast_with_no_network_attempt() {
    // Deliberately point at a URL nothing is listening on: if `enroll`
    // tried to dial before checking the token, this would hang or error
    // with a connection failure instead of `MissingToken`, and the test
    // would tell them apart.
    let credential_dir = tempfile::tempdir().unwrap();
    let config = ServeConfig::for_test(
        "ws://127.0.0.1:1/connect".to_string(),
        credential_dir.path().join("device-credential.json"),
    );

    let result = Box::pin(temp_env::async_with_vars([("CHOOSH_ENROLLMENT_TOKEN", None::<&str>)], serve::enroll(&config)))
        .await;

    assert!(matches!(result, Err(serve::ServeError::MissingToken)));
}

#[tokio::test]
async fn corrupt_credential_file_fails_serve_run_instead_of_silently_re_enrolling() {
    let credential_dir = tempfile::tempdir().unwrap();
    let credential_path = credential_dir.path().join("device-credential.json");
    std::fs::write(&credential_path, b"not a valid credential file").unwrap();

    let result = Box::pin(temp_env::async_with_vars(
        [
            ("CHOOSH_HOSTD_CREDENTIAL_PATH", Some(credential_path.to_str().unwrap())),
            // Token is set (and would happily "succeed" if this were
            // mistakenly treated as an unenrolled first run) so the test
            // actually proves the corrupt file is what fails the run, not
            // a missing token.
            ("CHOOSH_ENROLLMENT_TOKEN", Some("irrelevant-if-this-test-passes")),
            ("CHOOSH_RELAYD_URL", Some("ws://127.0.0.1:1/connect")),
        ],
        serve::run(),
    ))
    .await;

    assert!(result.is_err(), "a corrupt credential file must fail run(), not silently re-enroll");
}

/// `hooks::install_claude_hooks` (merge-safe installation of `choosh-hostd
/// emit` into a Claude Code `settings.json`) is fully implemented and
/// well-tested in `hooks.rs` itself, but was never actually called from
/// anywhere in production — no RPC handler, no CLI subcommand, nothing in
/// `serve::run()`'s own startup sequence. This proves the fix: `run()`'s
/// startup path really does attempt the install, before its connect-retry
/// loop ever starts trying to reach `relayd` — a fresh, valid credential is
/// pre-saved (the same shape `corrupt_credential_file_fails_serve_run_instead_of_silently_re_enrolling`
/// above uses) so `run()` skips enrollment's own network round-trip
/// entirely, and `CHOOSH_RELAYD_URL` deliberately points at nothing —
/// this test's only interest is the step that runs before that loop even
/// starts, so it never needs a working relayd connection to observe it.
///
/// `CHOOSH_HOSTD_CLAUDE_SETTINGS_PATH` is the test-injectable override
/// `serve::claude_settings_path` gained for exactly this: without it,
/// `run()` would target the real, current user's actual
/// `~/.claude/settings.json`, which a test must never touch.
#[tokio::test]
async fn serve_run_installs_claude_code_hooks_at_startup() {
    let root = tempfile::tempdir().unwrap();

    let credential_path = root.path().join("device-credential.json");
    let signing_key = ed25519_dalek::SigningKey::generate(&mut rand::rng());
    let cred = credential::Credential::new("hooks-test-device".to_string(), "fake-cert".to_string(), &signing_key);
    credential::save(&credential_path, &cred).unwrap();

    let settings_path = root.path().join("claude-settings.json");
    let workspaces_dir = root.path().join("workspaces");
    std::fs::create_dir_all(&workspaces_dir).unwrap();

    Box::pin(temp_env::async_with_vars(
        [
            ("CHOOSH_HOSTD_CREDENTIAL_PATH", Some(credential_path.to_str().unwrap())),
            ("CHOOSH_RELAYD_URL", Some("ws://127.0.0.1:1/connect")),
            ("CHOOSH_HOSTD_CLAUDE_SETTINGS_PATH", Some(settings_path.to_str().unwrap())),
            ("CHOOSH_HOSTD_WORKSPACES_DIR", Some(workspaces_dir.to_str().unwrap())),
            ("CHOOSH_HOSTD_REGISTRY_PATH", Some(root.path().join("registry.json").to_str().unwrap())),
            ("CHOOSH_HOSTD_MISE_HOST_TOOLS_DIR", Some(root.path().join("mise-host-tools").to_str().unwrap())),
            ("CHOOSH_HOSTD_MISE_PROJECT_TOOLS_DIR", Some(root.path().join("mise-project-tools").to_str().unwrap())),
        ],
        async {
            let task = tokio::spawn(serve::run());
            // `run()`'s connect-retry loop never returns on its own here
            // (nothing is listening at `CHOOSH_RELAYD_URL`, and no
            // shutdown signal is ever sent) — this test's whole job is the
            // hook-install step that happens BEFORE that loop starts, so
            // it only needs to wait for the settings file to appear, then
            // abort the still-running task rather than await it to
            // completion.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
            while !settings_path.exists() {
                assert!(tokio::time::Instant::now() < deadline, "serve::run() never installed Claude Code hooks within the deadline");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            task.abort();
        },
    ))
    .await;

    let content: serde_json::Value = serde_json::from_slice(&std::fs::read(&settings_path).unwrap()).unwrap();
    // The real running binary's own path, matching `emit_command`'s
    // documented reason for using `std::env::current_exe()` rather than a
    // bare `"choosh-hostd"` — `run()` executed in-process inside this very
    // test binary (via `tokio::spawn` above, not a subprocess), so this
    // test process's own `current_exe()` is exactly what `run()` itself
    // resolved.
    let expected_command = format!("{} emit --surface Stop", std::env::current_exe().unwrap().display());
    let stop_groups = content["hooks"]["Stop"].as_array().expect("Stop surface must have installed hook groups");
    assert!(
        stop_groups
            .iter()
            .any(|group| group["hooks"].as_array().unwrap().iter().any(|hook| hook["command"] == expected_command)),
        "expected the Stop surface to carry a real, running-binary-pathed choosh-hostd emit command, got: {content}"
    );
}
