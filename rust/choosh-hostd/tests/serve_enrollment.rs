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
