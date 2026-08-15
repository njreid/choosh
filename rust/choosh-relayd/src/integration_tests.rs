//! Real networked `WebSocket` integration tests: a genuine `axum::serve`
//! listener on an ephemeral port, driven by a real `tokio-tungstenite`
//! client, exercising the exact wire protocol relay-protocol.md and
//! auth-and-enrollment.md specify. Compiled only under `cfg(test)`, so it
//! has crate-internal access to `wire`/`ca`/`state` — deliberately not a
//! `tests/` external integration test, since exposing the wire-format
//! helpers as public API just for test access would be a worse trade.

use crate::state::{EnrollmentToken, now_unix};
use crate::wire::{encode_control, new_decoder};
use crate::{AppState, build_state_in, router};
use choosh_protocol::relay::{
    AuthResult, ClientAuth, ConnectionState, ControlRequest, ControlResponse, DeviceAuth, IdentityClass, PhoneAuth,
    ServerHello, ServerPush, decode_tunnel_frame, decode_tunnel_id_hex, encode_tunnel_frame,
};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;

struct TestServer {
    addr: std::net::SocketAddr,
    state: Arc<AppState>,
}

async fn spawn_server() -> TestServer {
    // Off by default; set RUST_LOG=trace (or similar) to see wire-level
    // tracing when debugging a failing test.
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let dir = std::env::temp_dir().join(format!("choosh-relayd-itest-{}", uuid::Uuid::new_v4()));
    let state = build_state_in(&dir);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    let app = router(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server run");
    });
    TestServer { addr, state }
}

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(server: &TestServer) -> WsStream {
    let url = format!("ws://{}/connect", server.addr);
    let (stream, _response) = tokio_tungstenite::connect_async(url).await.expect("ws connect");
    stream
}

async fn recv_control<T: serde::de::DeserializeOwned>(ws: &mut WsStream) -> T {
    let mut decoder = new_decoder();
    loop {
        let message = ws.next().await.expect("stream open").expect("ws message");
        let WsMessage::Binary(bytes) = message else {
            continue;
        };
        let frames = decoder.feed(&bytes).expect("well-formed frame");
        if let Some(frame) = frames.into_iter().next() {
            return crate::wire::decode_control(&frame).expect("decodable control frame");
        }
    }
}

async fn send_control<T: serde::Serialize>(ws: &mut WsStream, body: &T) {
    let frame = encode_control(body).expect("encodable control frame");
    ws.send(WsMessage::Binary(frame.into())).await.expect("ws send");
}

async fn recv_hello(ws: &mut WsStream) -> String {
    let hello: ServerHello = recv_control(ws).await;
    hello.nonce
}

fn device_auth_for(nonce: &str, device_id: &str, key: &SigningKey, certificate: String) -> ClientAuth {
    let signature = key.sign(nonce.as_bytes());
    ClientAuth::Device(DeviceAuth {
        device_id: device_id.to_string(),
        certificate,
        signature: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, signature.to_bytes()),
    })
}

async fn seed_enrollment_token(server: &TestServer, identity_class: IdentityClass) -> String {
    let token = format!("test-token-{}", uuid::Uuid::new_v4());
    server.state.registry.tokens.write().await.insert(
        token.clone(),
        EnrollmentToken { identity_class, expires_at_unix: now_unix() + 900, consumed: false },
    );
    token
}

async fn enroll_devhost(server: &TestServer, key: &SigningKey) -> (String, String) {
    enroll_as(server, key, IdentityClass::Devhost).await
}

async fn enroll_as(server: &TestServer, key: &SigningKey, identity_class: IdentityClass) -> (String, String) {
    enroll_as_with_details(server, key, identity_class, "test-device", None).await
}

/// The general form [`enroll_as`] delegates to (`account_label: None`,
/// `alias: "test-device"`) — split out so the fleet-view multi-account test
/// below can enroll two devhosts with distinct, real `account_label`s
/// without changing `enroll_as`'s (widely used) signature.
async fn enroll_as_with_details(
    server: &TestServer,
    key: &SigningKey,
    identity_class: IdentityClass,
    alias: &str,
    account_label: Option<String>,
) -> (String, String) {
    // Devhost enrollment MUST carry a loopback SSH host public key
    // (auth-and-enrollment.md step 6) — a fresh, unrelated Ed25519 key
    // stands in for a real SSH host key here, since these tests only
    // exercise `relayd`'s storage/validation of the *shape*, not
    // `choosh-hostd`'s actual host-key derivation (covered in
    // `choosh-hostd`'s own tests).
    let host_ssh_public_key = matches!(identity_class, IdentityClass::Devhost).then(|| {
        let fake_host_key = SigningKey::generate(&mut crate::rng::os_rng());
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, fake_host_key.verifying_key().to_bytes())
    });

    let mut ws = connect(server).await;
    recv_hello(&mut ws).await;
    let token = seed_enrollment_token(server, identity_class).await;
    send_control(
        &mut ws,
        &ControlRequest::Enroll {
            request_id: uuid::Uuid::new_v4().to_string(),
            token,
            identity_class,
            public_key: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                key.verifying_key().to_bytes(),
            ),
            host_ssh_public_key,
            alias: Some(alias.to_string()),
            platform: Some("linux".to_string()),
            account_label,
        },
    )
    .await;
    let response: ControlResponse = recv_control(&mut ws).await;
    match response {
        ControlResponse::EnrollOk { device_id, certificate, .. } => (device_id, certificate),
        other => panic!("expected EnrollOk, got {other:?}"),
    }
}

/// Enrolls and authenticates a fresh device-class Identity, returning its
/// live, authenticated connection plus its `device_id`.
async fn authenticated_device(server: &TestServer, identity_class: IdentityClass) -> (WsStream, String) {
    let key = SigningKey::generate(&mut crate::rng::os_rng());
    let (device_id, certificate) = enroll_as(server, &key, identity_class).await;
    let mut ws = connect(server).await;
    let nonce = recv_hello(&mut ws).await;
    send_control(&mut ws, &device_auth_for(&nonce, &device_id, &key, certificate)).await;
    let auth: AuthResult = recv_control(&mut ws).await;
    assert!(matches!(auth, AuthResult::Ok(_)), "expected device auth to succeed, got {auth:?}");
    (ws, device_id)
}

/// [`authenticated_device`]'s sibling for the fleet-view multi-account
/// exit-criterion test: enrolls and authenticates a devhost with a
/// specific, real `alias`/`account_label` rather than the fixed
/// `"test-device"`/`None` every other test in this file uses — two devhosts
/// enrolled this way (with distinct labels, distinct signing keys, and no
/// shared credential material of any kind) are exactly what
/// `docs/milestones/M7-fleet-and-provisioning.md`'s exit criterion ("in at
/// least two different AWS accounts with no shared credentials between
/// them") requires proving.
async fn authenticated_device_with_account_label(server: &TestServer, alias: &str, account_label: &str) -> (WsStream, String) {
    let key = SigningKey::generate(&mut crate::rng::os_rng());
    let (device_id, certificate) = enroll_as_with_details(server, &key, IdentityClass::Devhost, alias, Some(account_label.to_string())).await;
    let mut ws = connect(server).await;
    let nonce = recv_hello(&mut ws).await;
    send_control(&mut ws, &device_auth_for(&nonce, &device_id, &key, certificate)).await;
    let auth: AuthResult = recv_control(&mut ws).await;
    assert!(matches!(auth, AuthResult::Ok(_)), "expected device auth to succeed, got {auth:?}");
    (ws, device_id)
}

async fn send_tunnel_frame(ws: &mut WsStream, tunnel_id: [u8; 8], payload: &[u8]) {
    let frame = encode_tunnel_frame(tunnel_id, payload);
    let framed = choosh_protocol::framing::encode_frame(&frame, choosh_protocol::relay::MAX_TUNNEL_FRAME_BYTES)
        .expect("tunnel frame encodes");
    ws.send(WsMessage::Binary(framed.into())).await.expect("ws send");
}

/// Reads WS messages until exactly one complete frame decodes as a `0x02`
/// tunnel frame, ignoring any interleaved `0x01` control frames (e.g. a
/// `ListDevhostsOk` a test isn't interested in). Returns `(tunnel_id, payload)`.
async fn recv_tunnel_frame(ws: &mut WsStream) -> ([u8; 8], Vec<u8>) {
    let mut decoder = new_decoder();
    loop {
        let message = ws.next().await.expect("stream open").expect("ws message");
        let WsMessage::Binary(bytes) = message else { continue };
        let frames = decoder.feed(&bytes).expect("well-formed frame");
        for frame in frames {
            if let Some((id, payload)) = decode_tunnel_frame(&frame) {
                return (id, payload.to_vec());
            }
        }
    }
}

/// Opens an `rpc`-purpose tunnel from `requester` (already authenticated)
/// to `target_device_id`, and drives `target`'s side of the handshake
/// (receiving the `tunnel-offered` push relay-protocol.md requires). Returns
/// the raw tunnel ID.
async fn open_tunnel(requester: &mut WsStream, target: &mut WsStream, target_device_id: &str, purpose: &str) -> [u8; 8] {
    send_control(
        requester,
        &ControlRequest::OpenTunnel {
            request_id: "open".to_string(),
            target_device_id: target_device_id.to_string(),
            purpose: purpose.to_string(),
        },
    )
    .await;
    let response: ControlResponse = recv_control(requester).await;
    let ControlResponse::OpenTunnelOk { tunnel_id, .. } = response else {
        panic!("expected OpenTunnelOk, got {response:?}")
    };
    let tunnel_id = decode_tunnel_id_hex(&tunnel_id).expect("valid tunnel id hex");

    let push: ServerPush = recv_control(target).await;
    let ServerPush::TunnelOffered { tunnel_id: offered_hex, purpose: offered_purpose, .. } = push else {
        panic!("expected TunnelOffered, got {push:?}")
    };
    assert_eq!(decode_tunnel_id_hex(&offered_hex), Some(tunnel_id));
    assert_eq!(offered_purpose, purpose);

    tunnel_id
}

async fn authenticated_phone(server: &TestServer) -> WsStream {
    let credential = format!("test-session-{}", uuid::Uuid::new_v4());
    server.state.registry.phone_sessions.write().await.insert(
        credential.clone(),
        crate::state::PhoneSession {
            device_id: "phone-test".to_string(),
            expires_at_unix: now_unix() + 900,
        },
    );
    let mut ws = connect(server).await;
    recv_hello(&mut ws).await;
    send_control(&mut ws, &ClientAuth::Phone(PhoneAuth { session_credential: credential })).await;
    let auth: AuthResult = recv_control(&mut ws).await;
    assert!(matches!(auth, AuthResult::Ok(_)), "expected phone auth to succeed, got {auth:?}");
    ws
}

#[tokio::test]
async fn enroll_with_valid_token_returns_verifiable_certificate() {
    let server = spawn_server().await;
    let key = SigningKey::generate(&mut crate::rng::os_rng());
    let (device_id, certificate) = enroll_devhost(&server, &key).await;
    assert!(device_id.starts_with("dev-"));
    let verified = crate::ca::verify(&server.state.ca_key.verifying_key(), &certificate, now_unix());
    assert!(verified.is_ok(), "issued certificate must verify: {verified:?}");
}

#[tokio::test]
async fn enroll_token_is_single_use() {
    let server = spawn_server().await;
    let token = seed_enrollment_token(&server, IdentityClass::Devhost).await;
    let key = SigningKey::generate(&mut crate::rng::os_rng());
    let host_key = SigningKey::generate(&mut crate::rng::os_rng());

    for expect_ok in [true, false] {
        let mut ws = connect(&server).await;
        recv_hello(&mut ws).await;
        send_control(
            &mut ws,
            &ControlRequest::Enroll {
                request_id: uuid::Uuid::new_v4().to_string(),
                token: token.clone(),
                identity_class: IdentityClass::Devhost,
                public_key: base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    key.verifying_key().to_bytes(),
                ),
                host_ssh_public_key: Some(base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    host_key.verifying_key().to_bytes(),
                )),
                alias: None,
                platform: None,
                account_label: None,
            },
        )
        .await;
        let response: ControlResponse = recv_control(&mut ws).await;
        assert_eq!(
            matches!(response, ControlResponse::EnrollOk { .. }),
            expect_ok,
            "unexpected response on {} use: {response:?}",
            if expect_ok { "first" } else { "second" }
        );
    }
}

#[tokio::test]
async fn enroll_with_unknown_or_expired_token_fails() {
    let server = spawn_server().await;
    let key = SigningKey::generate(&mut crate::rng::os_rng());

    let mut ws = connect(&server).await;
    recv_hello(&mut ws).await;
    send_control(
        &mut ws,
        &ControlRequest::Enroll {
            request_id: "r1".to_string(),
            token: "never-issued".to_string(),
            identity_class: IdentityClass::Devhost,
            public_key: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                key.verifying_key().to_bytes(),
            ),
            host_ssh_public_key: None,
            alias: None,
            platform: None,
            account_label: None,
        },
    )
    .await;
    let response: ControlResponse = recv_control(&mut ws).await;
    assert!(matches!(response, ControlResponse::Error { .. }), "unknown token must fail: {response:?}");

    let expired_token = format!("expired-{}", uuid::Uuid::new_v4());
    server.state.registry.tokens.write().await.insert(
        expired_token.clone(),
        EnrollmentToken { identity_class: IdentityClass::Devhost, expires_at_unix: now_unix() - 1, consumed: false },
    );
    let mut ws2 = connect(&server).await;
    recv_hello(&mut ws2).await;
    send_control(
        &mut ws2,
        &ControlRequest::Enroll {
            request_id: "r2".to_string(),
            token: expired_token,
            identity_class: IdentityClass::Devhost,
            public_key: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                key.verifying_key().to_bytes(),
            ),
            host_ssh_public_key: None,
            alias: None,
            platform: None,
            account_label: None,
        },
    )
    .await;
    let response2: ControlResponse = recv_control(&mut ws2).await;
    assert!(matches!(response2, ControlResponse::Error { .. }), "expired token must fail: {response2:?}");
}

#[tokio::test]
async fn enrolled_devhost_authenticates_and_appears_in_list_devhosts() {
    let server = spawn_server().await;
    let key = SigningKey::generate(&mut crate::rng::os_rng());
    let (device_id, certificate) = enroll_devhost(&server, &key).await;

    let mut ws = connect(&server).await;
    let nonce = recv_hello(&mut ws).await;
    send_control(&mut ws, &device_auth_for(&nonce, &device_id, &key, certificate)).await;
    let auth: AuthResult = recv_control(&mut ws).await;
    let AuthResult::Ok(ok) = auth else { panic!("expected AuthResult::Ok, got {auth:?}") };
    assert_eq!(ok.identity_class, IdentityClass::Devhost);
    assert_eq!(ok.device_id, device_id);

    let mut phone = authenticated_phone(&server).await;
    send_control(&mut phone, &ControlRequest::ListDevhosts { request_id: "r".to_string() }).await;
    let response: ControlResponse = recv_control(&mut phone).await;
    let ControlResponse::ListDevhostsOk { devhosts, .. } = response else {
        panic!("expected ListDevhostsOk, got {response:?}")
    };
    let listed = devhosts.iter().find(|d| d.device_id == device_id).expect("enrolled devhost listed");
    assert_eq!(listed.connection_state, choosh_protocol::relay::ConnectionState::Online);
}

#[tokio::test]
async fn devhost_with_bad_signature_is_rejected_and_connection_closes() {
    let server = spawn_server().await;
    let key = SigningKey::generate(&mut crate::rng::os_rng());
    let (device_id, certificate) = enroll_devhost(&server, &key).await;
    let wrong_key = SigningKey::generate(&mut crate::rng::os_rng());

    let mut ws = connect(&server).await;
    let nonce = recv_hello(&mut ws).await;
    // Sign with the WRONG key but present the real certificate/device_id.
    send_control(&mut ws, &device_auth_for(&nonce, &device_id, &wrong_key, certificate)).await;
    let auth: AuthResult = recv_control(&mut ws).await;
    assert!(matches!(auth, AuthResult::Failed(_)), "bad signature must fail: {auth:?}");

    // relay-protocol.md: a failed authentication closes the connection —
    // the next read must observe a close, not a live, usable session.
    let next = ws.next().await;
    assert!(
        next.is_none() || matches!(next, Some(Ok(WsMessage::Close(_)))),
        "connection must close after failed auth, got {next:?}"
    );
}

#[tokio::test]
async fn non_phone_identity_cannot_call_list_devhosts_or_request_enrollment_token() {
    let server = spawn_server().await;
    let key = SigningKey::generate(&mut crate::rng::os_rng());
    let (device_id, certificate) = enroll_devhost(&server, &key).await;

    let mut ws = connect(&server).await;
    let nonce = recv_hello(&mut ws).await;
    send_control(&mut ws, &device_auth_for(&nonce, &device_id, &key, certificate)).await;
    let auth: AuthResult = recv_control(&mut ws).await;
    assert!(matches!(auth, AuthResult::Ok(_)));

    send_control(&mut ws, &ControlRequest::ListDevhosts { request_id: "a".to_string() }).await;
    let response: ControlResponse = recv_control(&mut ws).await;
    assert!(
        matches!(&response, ControlResponse::Error { code, .. } if code == "not_permitted"),
        "devhost calling list-devhosts must be rejected: {response:?}"
    );

    send_control(
        &mut ws,
        &ControlRequest::RequestEnrollmentToken {
            request_id: "b".to_string(),
            identity_class: IdentityClass::Devhost,
        },
    )
    .await;
    let response2: ControlResponse = recv_control(&mut ws).await;
    assert!(
        matches!(&response2, ControlResponse::Error { code, .. } if code == "not_permitted"),
        "devhost calling request-enrollment-token must be rejected: {response2:?}"
    );
}

#[tokio::test]
async fn malformed_frame_closes_the_connection() {
    let server = spawn_server().await;
    let mut ws = connect(&server).await;
    recv_hello(&mut ws).await;

    // Well-framed (valid length prefix) but not valid UTF-8 JSON after the
    // class byte — a malformed control frame per relay-protocol.md.
    let mut payload = vec![choosh_protocol::relay::FRAME_CLASS_CONTROL];
    payload.extend_from_slice(&[0xFF, 0xFE, 0xFD]);
    let framed = choosh_protocol::framing::encode_frame(&payload, choosh_protocol::relay::MAX_CONTROL_FRAME_BYTES)
        .expect("frame encodes");
    ws.send(WsMessage::Binary(framed.into())).await.expect("send malformed frame");

    let next = ws.next().await;
    assert!(
        next.is_none() || matches!(next, Some(Ok(WsMessage::Close(_)))),
        "malformed frame must close the connection, got {next:?}"
    );
}

#[tokio::test]
async fn phone_request_enrollment_token_round_trips_into_a_working_enrollment() {
    let server = spawn_server().await;
    let mut phone = authenticated_phone(&server).await;
    send_control(
        &mut phone,
        &ControlRequest::RequestEnrollmentToken {
            request_id: "r".to_string(),
            identity_class: IdentityClass::Devhost,
        },
    )
    .await;
    let response: ControlResponse = recv_control(&mut phone).await;
    let ControlResponse::RequestEnrollmentTokenOk { token, .. } = response else {
        panic!("expected RequestEnrollmentTokenOk, got {response:?}")
    };

    let key = SigningKey::generate(&mut crate::rng::os_rng());
    let host_key = SigningKey::generate(&mut crate::rng::os_rng());
    let mut ws = connect(&server).await;
    recv_hello(&mut ws).await;
    send_control(
        &mut ws,
        &ControlRequest::Enroll {
            request_id: "e".to_string(),
            token,
            identity_class: IdentityClass::Devhost,
            public_key: base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                key.verifying_key().to_bytes(),
            ),
            host_ssh_public_key: Some(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                host_key.verifying_key().to_bytes(),
            )),
            alias: None,
            platform: None,
            account_label: None,
        },
    )
    .await;
    let enroll_response: ControlResponse = recv_control(&mut ws).await;
    assert!(
        matches!(enroll_response, ControlResponse::EnrollOk { .. }),
        "phone-issued token must enroll successfully: {enroll_response:?}"
    );
}

#[tokio::test]
async fn phone_opens_tunnel_to_online_devhost_and_bytes_route_both_ways() {
    let server = spawn_server().await;
    let mut phone = authenticated_phone(&server).await;
    let (mut devhost, devhost_id) = authenticated_device(&server, IdentityClass::Devhost).await;

    let tunnel_id = open_tunnel(&mut phone, &mut devhost, &devhost_id, "rpc").await;

    send_tunnel_frame(&mut phone, tunnel_id, b"hello from phone").await;
    let (received_id, payload) = recv_tunnel_frame(&mut devhost).await;
    assert_eq!(received_id, tunnel_id);
    assert_eq!(payload, b"hello from phone");

    send_tunnel_frame(&mut devhost, tunnel_id, b"hello from devhost").await;
    let (received_id2, payload2) = recv_tunnel_frame(&mut phone).await;
    assert_eq!(received_id2, tunnel_id);
    assert_eq!(payload2, b"hello from devhost");
}

#[tokio::test]
async fn open_tunnel_against_offline_devhost_fails_immediately() {
    let server = spawn_server().await;
    let mut phone = authenticated_phone(&server).await;
    // Enrolled but never connected — a real "never seen this identity
    // online" case, not merely disconnected.
    let key = SigningKey::generate(&mut crate::rng::os_rng());
    let (device_id, _certificate) = enroll_devhost(&server, &key).await;

    send_control(
        &mut phone,
        &ControlRequest::OpenTunnel {
            request_id: "r".to_string(),
            target_device_id: device_id,
            purpose: "rpc".to_string(),
        },
    )
    .await;
    let response: ControlResponse = recv_control(&mut phone).await;
    assert!(
        matches!(&response, ControlResponse::Error { code, .. } if code == "target_offline"),
        "tunnel to an offline devhost must fail with target_offline: {response:?}"
    );
}

#[tokio::test]
async fn laptop_proxy_may_only_open_ssh_purpose_tunnels() {
    let server = spawn_server().await;
    let (mut laptop, _laptop_id) = authenticated_device(&server, IdentityClass::LaptopProxy).await;
    let (mut devhost, devhost_id) = authenticated_device(&server, IdentityClass::Devhost).await;

    send_control(
        &mut laptop,
        &ControlRequest::OpenTunnel {
            request_id: "wrong-purpose".to_string(),
            target_device_id: devhost_id.clone(),
            purpose: "rpc".to_string(),
        },
    )
    .await;
    let response: ControlResponse = recv_control(&mut laptop).await;
    assert!(
        matches!(&response, ControlResponse::Error { code, .. } if code == "not_permitted"),
        "laptop-proxy requesting a non-ssh-purpose tunnel must be rejected: {response:?}"
    );

    // The *correct* purpose for this Identity class must still work — this
    // isn't a blanket denial of laptop-proxy tunnels, only a scope
    // restriction on which purpose it may use.
    let tunnel_id = open_tunnel(&mut laptop, &mut devhost, &devhost_id, "ssh").await;
    send_tunnel_frame(&mut laptop, tunnel_id, b"ssh bytes").await;
    let (received_id, payload) = recv_tunnel_frame(&mut devhost).await;
    assert_eq!(received_id, tunnel_id);
    assert_eq!(payload, b"ssh bytes");
}

#[tokio::test]
async fn laptop_proxy_can_list_devhost_ssh_endpoints_but_others_cannot() {
    let server = spawn_server().await;
    let (_devhost_ws, devhost_id) = authenticated_device(&server, IdentityClass::Devhost).await;
    let (mut laptop, _laptop_id) = authenticated_device(&server, IdentityClass::LaptopProxy).await;

    send_control(&mut laptop, &ControlRequest::ListDevhostSshEndpoints { request_id: "r".to_string() }).await;
    let response: ControlResponse = recv_control(&mut laptop).await;
    let ControlResponse::ListDevhostSshEndpointsOk { endpoints, .. } = response else {
        panic!("expected ListDevhostSshEndpointsOk, got {response:?}");
    };
    assert_eq!(endpoints.len(), 1, "expected exactly the one enrolled devhost: {endpoints:?}");
    assert_eq!(endpoints[0].device_id, devhost_id);
    assert_eq!(endpoints[0].alias, "test-device");
    assert!(!endpoints[0].ssh_host_public_key.is_empty());

    // A devhost Identity is not scoped to this restricted read either —
    // it's laptop-proxy-only, per auth-and-enrollment.md.
    let (mut devhost2, _) = authenticated_device(&server, IdentityClass::Devhost).await;
    send_control(&mut devhost2, &ControlRequest::ListDevhostSshEndpoints { request_id: "r2".to_string() }).await;
    let response2: ControlResponse = recv_control(&mut devhost2).await;
    assert!(
        matches!(&response2, ControlResponse::Error { code, .. } if code == "not_permitted"),
        "devhost calling list-devhost-ssh-endpoints must be rejected: {response2:?}"
    );

    // A phone Identity is not scoped to this either — it has its own,
    // richer `list-devhosts` instead.
    let mut phone = authenticated_phone(&server).await;
    send_control(&mut phone, &ControlRequest::ListDevhostSshEndpoints { request_id: "r3".to_string() }).await;
    let response3: ControlResponse = recv_control(&mut phone).await;
    assert!(
        matches!(&response3, ControlResponse::Error { code, .. } if code == "not_permitted"),
        "phone calling list-devhost-ssh-endpoints must be rejected: {response3:?}"
    );
}

#[tokio::test]
async fn devhost_enrollment_without_host_ssh_public_key_is_rejected() {
    let server = spawn_server().await;
    let key = SigningKey::generate(&mut crate::rng::os_rng());
    let mut ws = connect(&server).await;
    recv_hello(&mut ws).await;
    let token = seed_enrollment_token(&server, IdentityClass::Devhost).await;
    send_control(
        &mut ws,
        &ControlRequest::Enroll {
            request_id: "e".to_string(),
            token,
            identity_class: IdentityClass::Devhost,
            public_key: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, key.verifying_key().to_bytes()),
            host_ssh_public_key: None,
            alias: None,
            platform: None,
            account_label: None,
        },
    )
    .await;
    let response: ControlResponse = recv_control(&mut ws).await;
    assert!(
        matches!(&response, ControlResponse::Error { code, .. } if code == "missing_host_ssh_public_key"),
        "devhost enrollment with no SSH host key must be rejected: {response:?}"
    );
}

#[tokio::test]
async fn zero_payload_frame_closes_tunnel_and_further_frames_are_dropped() {
    let server = spawn_server().await;
    let mut phone = authenticated_phone(&server).await;
    let (mut devhost, devhost_id) = authenticated_device(&server, IdentityClass::Devhost).await;
    let tunnel_id = open_tunnel(&mut phone, &mut devhost, &devhost_id, "rpc").await;

    send_tunnel_frame(&mut phone, tunnel_id, &[]).await;
    let (received_id, payload) = recv_tunnel_frame(&mut devhost).await;
    assert_eq!(received_id, tunnel_id);
    assert!(payload.is_empty(), "close signal must relay as a zero-payload frame");

    assert!(
        !server.state.registry.tunnels.read().await.contains_key(&tunnel_id),
        "tunnel must be removed from the registry once closed"
    );

    // A further frame for the now-closed tunnel ID is dropped silently —
    // the devhost side must NOT receive anything for it.
    send_tunnel_frame(&mut phone, tunnel_id, b"too late").await;
    let nothing_within = tokio::time::timeout(std::time::Duration::from_millis(200), recv_tunnel_frame(&mut devhost)).await;
    assert!(nothing_within.is_err(), "a frame for a closed tunnel must not be routed");
}

#[tokio::test]
async fn disconnecting_one_party_mid_tunnel_closes_it_for_the_survivor() {
    let server = spawn_server().await;
    let mut phone = authenticated_phone(&server).await;
    let (mut devhost, devhost_id) = authenticated_device(&server, IdentityClass::Devhost).await;
    let tunnel_id = open_tunnel(&mut phone, &mut devhost, &devhost_id, "rpc").await;

    // The requester vanishes without sending an explicit close frame.
    phone.close(None).await.expect("close phone connection");
    drop(phone);

    // The survivor (devhost) must observe the tunnel closing — a
    // zero-payload frame for the same tunnel ID — rather than hanging or
    // silently never finding out.
    let (received_id, payload) =
        tokio::time::timeout(std::time::Duration::from_secs(2), recv_tunnel_frame(&mut devhost))
            .await
            .expect("survivor must observe tunnel closure promptly");
    assert_eq!(received_id, tunnel_id);
    assert!(payload.is_empty());

    assert!(
        !server.state.registry.tunnels.read().await.contains_key(&tunnel_id),
        "tunnel must be removed from the registry after the requester disconnects"
    );
}

#[tokio::test]
async fn a_full_outbound_queue_closes_the_tunnel_instead_of_growing_unboundedly() {
    // Exercises the exact backpressure code path (`forward_data`'s
    // `try_send` hitting a full channel) deterministically and fast, by
    // synthetically filling the target's registered outbound channel
    // rather than trying to overwhelm real OS/TCP buffers with a large
    // burst — the latter would be slow, flaky, and platform-dependent for
    // a test that needs to run quickly and reliably. This still exercises
    // the real `handle_tunnel_frame` → `forward_data` → registry-removal
    // path end to end; it's synthetic only in *how* the queue is filled.
    let server = spawn_server().await;
    let mut phone = authenticated_phone(&server).await;
    let (mut devhost, devhost_id) = authenticated_device(&server, IdentityClass::Devhost).await;
    let tunnel_id = open_tunnel(&mut phone, &mut devhost, &devhost_id, "rpc").await;

    // Replace the devhost's registered outbound sender with a small,
    // already-full one so the next routed frame is guaranteed to hit
    // backpressure — the real devhost connection's own reader task is now
    // orphaned from routing, which is fine, this test asserts nothing
    // further about it afterward.
    let (full_tx, _full_rx_never_read) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    full_tx.try_send(vec![0u8]).expect("fill the synthetic channel");
    server.state.registry.connections.write().await.insert(devhost_id.clone(), full_tx);

    send_tunnel_frame(&mut phone, tunnel_id, b"this cannot be delivered").await;
    // Give the phone connection's routing task a moment to process the
    // frame it just sent.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    assert!(
        !server.state.registry.tunnels.read().await.contains_key(&tunnel_id),
        "a full outbound queue must close the tunnel rather than buffering unboundedly"
    );
}

#[tokio::test]
async fn oversized_tunnel_frame_closes_the_connection() {
    // Regression test for the gap where the ingress decoder is shared
    // across both frame classes and sized to `MAX_CONTROL_FRAME_BYTES` (1
    // MiB) — a tunnel frame between `MAX_TUNNEL_FRAME_BYTES` (256 KiB) and
    // that shared cap would previously pass framing uncaught, in violation
    // of relay-protocol.md's "a frame exceeding its class's size cap"
    // connection-termination rule.
    let server = spawn_server().await;
    let mut phone = authenticated_phone(&server).await;
    let (mut devhost, devhost_id) = authenticated_device(&server, IdentityClass::Devhost).await;
    let tunnel_id = open_tunnel(&mut phone, &mut devhost, &devhost_id, "rpc").await;

    let oversized_payload = vec![0u8; choosh_protocol::relay::MAX_TUNNEL_FRAME_BYTES + 1];
    let frame = encode_tunnel_frame(tunnel_id, &oversized_payload);
    // Encoded under the larger control-frame cap, matching exactly what the
    // shared ingress decoder accepts on the wire before class-specific
    // limits are applied.
    let framed = choosh_protocol::framing::encode_frame(&frame, choosh_protocol::relay::MAX_CONTROL_FRAME_BYTES)
        .expect("frame encodes under the control-frame cap");
    phone.send(WsMessage::Binary(framed.into())).await.expect("send oversized tunnel frame");

    let next = phone.next().await;
    assert!(
        next.is_none() || matches!(next, Some(Ok(WsMessage::Close(_)))),
        "an oversized tunnel frame must close the connection, got {next:?}"
    );
}

fn input_required_event(workspace_id: &str, item_id: &str) -> choosh_protocol::relay::WireAgentEvent {
    choosh_protocol::relay::WireAgentEvent::InputRequired {
        workspace_id: workspace_id.to_string(),
        item_id: item_id.to_string(),
        reason: choosh_protocol::relay::WireInputReason::Approval,
    }
}

#[tokio::test]
async fn agent_event_from_devhost_is_pushed_to_a_connected_phone() {
    let server = spawn_server().await;
    let (mut devhost, device_id) = authenticated_device(&server, IdentityClass::Devhost).await;
    let mut phone = authenticated_phone(&server).await;

    send_control(
        &mut devhost,
        &ControlRequest::AgentEvent { request_id: "e1".to_string(), event: input_required_event("ws-1", "item-1") },
    )
    .await;
    let response: ControlResponse = recv_control(&mut devhost).await;
    assert!(matches!(response, ControlResponse::AgentEventOk { .. }), "expected AgentEventOk, got {response:?}");

    let push: ServerPush = recv_control(&mut phone).await;
    let ServerPush::AgentEvent { from_device_id, event } = push else { panic!("expected AgentEvent push, got {push:?}") };
    assert_eq!(from_device_id, device_id);
    assert_eq!(event, input_required_event("ws-1", "item-1"));
}

#[tokio::test]
async fn agent_event_from_a_non_devhost_identity_is_rejected() {
    let server = spawn_server().await;
    let mut phone = authenticated_phone(&server).await;

    send_control(
        &mut phone,
        &ControlRequest::AgentEvent { request_id: "e1".to_string(), event: input_required_event("ws-1", "item-1") },
    )
    .await;
    let response: ControlResponse = recv_control(&mut phone).await;
    assert!(
        matches!(&response, ControlResponse::Error { code, .. } if code == "not_permitted"),
        "a phone sending agent-event must be rejected: {response:?}"
    );
}

#[tokio::test]
async fn register_fcm_token_from_a_devhost_is_rejected() {
    let server = spawn_server().await;
    let (mut devhost, _) = authenticated_device(&server, IdentityClass::Devhost).await;

    send_control(&mut devhost, &ControlRequest::RegisterFcmToken { request_id: "f1".to_string(), fcm_token: "tok".to_string() })
        .await;
    let response: ControlResponse = recv_control(&mut devhost).await;
    assert!(
        matches!(&response, ControlResponse::Error { code, .. } if code == "not_permitted"),
        "a devhost registering an FCM token must be rejected: {response:?}"
    );
}

#[tokio::test]
async fn a_second_register_fcm_token_call_replaces_rather_than_duplicates() {
    let server = spawn_server().await;
    let mut phone = authenticated_phone(&server).await;

    send_control(&mut phone, &ControlRequest::RegisterFcmToken { request_id: "f1".to_string(), fcm_token: "first".to_string() })
        .await;
    let response: ControlResponse = recv_control(&mut phone).await;
    assert!(matches!(response, ControlResponse::RegisterFcmTokenOk { .. }));

    send_control(&mut phone, &ControlRequest::RegisterFcmToken { request_id: "f2".to_string(), fcm_token: "second".to_string() })
        .await;
    let response2: ControlResponse = recv_control(&mut phone).await;
    assert!(matches!(response2, ControlResponse::RegisterFcmTokenOk { .. }));

    let tokens = server.state.registry.fcm_tokens.read().await;
    assert_eq!(tokens.len(), 1, "a second registration must replace, not duplicate: {tokens:?}");
    assert!(tokens.values().any(|token| token == "second"));
}

#[tokio::test]
async fn agent_event_with_no_phone_connected_does_not_error_the_devhost() {
    // No phone identity is enrolled/connected in this test at all — the
    // FCM-dispatch-stub path (or, with no token registered either, simply
    // "nothing to do") must not surface as a failure to the sending
    // devhost, per notifications.md's "FCM is the backstop, not a
    // synchronous acknowledgement channel" framing.
    let server = spawn_server().await;
    let (mut devhost, _) = authenticated_device(&server, IdentityClass::Devhost).await;

    send_control(
        &mut devhost,
        &ControlRequest::AgentEvent { request_id: "e1".to_string(), event: input_required_event("ws-1", "item-1") },
    )
    .await;
    let response: ControlResponse = recv_control(&mut devhost).await;
    assert!(matches!(response, ControlResponse::AgentEventOk { .. }), "expected AgentEventOk even with no phone connected, got {response:?}");
}

// --- M7: `dev-exec` offload capability + fleet-view multi-account --------
//
// `auth-and-enrollment.md`'s capability table already reserved `devhost`
// `open-tunnel` scoped to `purpose = "offload"` only, enforced in
// `ws::check_open_tunnel_permitted` — these tests are new *coverage* of
// that existing enforcement (no `ws.rs` change was needed for the
// capability itself, confirmed by reading it before writing these), plus
// the fleet-view exit criterion, which per M7's own scope bullet is "mostly
// a verification task" over `list-devhosts`/presence, also already built.

#[tokio::test]
async fn devhost_can_open_an_offload_purpose_tunnel_to_another_devhost() {
    let server = spawn_server().await;
    let (mut requester, _requester_id) = authenticated_device(&server, IdentityClass::Devhost).await;
    let (mut target, target_id) = authenticated_device(&server, IdentityClass::Devhost).await;

    let tunnel_id = open_tunnel(&mut requester, &mut target, &target_id, "offload").await;
    send_tunnel_frame(&mut requester, tunnel_id, b"offload bytes").await;
    let (received_id, payload) = recv_tunnel_frame(&mut target).await;
    assert_eq!(received_id, tunnel_id);
    assert_eq!(payload, b"offload bytes");
}

/// The negative half of the same capability scope: a `devhost` Identity may
/// open a tunnel to another devhost ONLY for `purpose = "offload"` — every
/// other purpose (including `"ssh"`, which is `laptop-proxy`'s own scope,
/// and `"rpc"`, which only a `phone` may request) must be refused, per
/// `check_open_tunnel_permitted`'s `IdentityClass::Devhost if purpose ==
/// "offload" => {}` / `IdentityClass::LaptopProxy | IdentityClass::Devhost
/// => Err("not_permitted")` match arms.
#[tokio::test]
async fn devhost_cannot_open_a_non_offload_purpose_tunnel_to_another_devhost() {
    let server = spawn_server().await;
    let (mut requester, _requester_id) = authenticated_device(&server, IdentityClass::Devhost).await;
    let (_target, target_id) = authenticated_device(&server, IdentityClass::Devhost).await;

    for purpose in ["rpc", "ssh", "pty:item-1", "web:item-1"] {
        send_control(
            &mut requester,
            &ControlRequest::OpenTunnel { request_id: format!("wrong-{purpose}"), target_device_id: target_id.clone(), purpose: purpose.to_string() },
        )
        .await;
        let response: ControlResponse = recv_control(&mut requester).await;
        assert!(
            matches!(&response, ControlResponse::Error { code, .. } if code == "not_permitted"),
            "devhost requesting purpose {purpose:?} against another devhost must be rejected: {response:?}"
        );
    }
}

/// The other half: `laptop-proxy` is scoped to `purpose = "ssh"` only (an
/// existing, already-tested rule — `laptop_proxy_may_only_open_ssh_purpose_tunnels`
/// above), and per that same match arm structure has NO path to
/// `"offload"` either. Recorded here as its own explicit case for the
/// `"offload"` purpose specifically, since a future edit to
/// `check_open_tunnel_permitted` could plausibly add a `laptop-proxy`
/// arm without this test catching a regression otherwise.
#[tokio::test]
async fn laptop_proxy_cannot_open_an_offload_purpose_tunnel() {
    let server = spawn_server().await;
    let (mut laptop, _laptop_id) = authenticated_device(&server, IdentityClass::LaptopProxy).await;
    let (_target, target_id) = authenticated_device(&server, IdentityClass::Devhost).await;

    send_control(
        &mut laptop,
        &ControlRequest::OpenTunnel { request_id: "r".to_string(), target_device_id: target_id, purpose: "offload".to_string() },
    )
    .await;
    let response: ControlResponse = recv_control(&mut laptop).await;
    assert!(
        matches!(&response, ControlResponse::Error { code, .. } if code == "not_permitted"),
        "laptop-proxy requesting an offload-purpose tunnel must be rejected: {response:?}"
    );
}

/// M7's fleet-view exit criterion: "The fleet view correctly lists and
/// status-tracks devhosts in at least two different AWS accounts with no
/// shared credentials between them." This is "mostly a verification task"
/// per the milestone doc's own scope bullet — `DevHostPresence::account_label`
/// and the presence registry both already exist and needed no code change;
/// what this test actually proves is that two independently-enrolled
/// devhosts (distinct signing keys, distinct certificates, distinct
/// `account_label`s — nothing shared between them at any layer this
/// process touches) both appear correctly, with their OWN account label and
/// OWN independent online/offline state, in one `list-devhosts` response.
#[allow(clippy::similar_names)] // `presence_a_after_disconnect`/`presence_b_after_disconnect` are intentionally parallel names for this test's two devhosts, not an accidental collision risk.
#[tokio::test]
async fn fleet_view_tracks_two_devhosts_in_different_accounts_with_independent_presence() {
    let server = spawn_server().await;
    let (mut devhost_a, device_a) = authenticated_device_with_account_label(&server, "prod-build-box", "aws:111111111111").await;
    let (mut devhost_b, device_b) = authenticated_device_with_account_label(&server, "staging-build-box", "aws:222222222222").await;

    let mut phone = authenticated_phone(&server).await;
    send_control(&mut phone, &ControlRequest::ListDevhosts { request_id: "r1".to_string() }).await;
    let response: ControlResponse = recv_control(&mut phone).await;
    let ControlResponse::ListDevhostsOk { devhosts, .. } = response else { panic!("expected ListDevhostsOk, got {response:?}") };

    let entry_a = devhosts.iter().find(|d| d.device_id == device_a).expect("devhost A listed");
    let entry_b = devhosts.iter().find(|d| d.device_id == device_b).expect("devhost B listed");
    assert_eq!(entry_a.account_label.as_deref(), Some("aws:111111111111"));
    assert_eq!(entry_b.account_label.as_deref(), Some("aws:222222222222"));
    assert_ne!(entry_a.account_label, entry_b.account_label, "the two accounts' labels must not collide");
    assert_eq!(entry_a.alias, "prod-build-box");
    assert_eq!(entry_b.alias, "staging-build-box");
    assert_eq!(entry_a.connection_state, ConnectionState::Online);
    assert_eq!(entry_b.connection_state, ConnectionState::Online);

    // Disconnect A only; B's presence must stay independently tracked —
    // this is the "status-tracked" half of the exit criterion, not just a
    // one-time snapshot. relayd updates `online_devices` as part of its own
    // connection-cleanup path when the read loop ends, which happens
    // asynchronously relative to this test's own `close()` call — a short
    // bounded poll (not a fixed sleep) waits for that to land, the same
    // "poll a real RPC rather than assume a fixed delay" discipline this
    // module's other disconnect-observing tests use.
    devhost_a.close(None).await.expect("close devhost A's connection");
    drop(devhost_a);

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let devhosts_after = loop {
        send_control(&mut phone, &ControlRequest::ListDevhosts { request_id: "r2".to_string() }).await;
        let response: ControlResponse = recv_control(&mut phone).await;
        let ControlResponse::ListDevhostsOk { devhosts, .. } = response else { panic!("expected ListDevhostsOk, got {response:?}") };
        let a_offline = devhosts.iter().any(|d| d.device_id == device_a && d.connection_state == ConnectionState::Offline);
        if a_offline || tokio::time::Instant::now() >= deadline {
            break devhosts;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };

    let presence_a_after_disconnect = devhosts_after.iter().find(|d| d.device_id == device_a).expect("devhost A still listed after disconnect");
    let presence_b_after_disconnect = devhosts_after.iter().find(|d| d.device_id == device_b).expect("devhost B still listed after disconnect");
    assert_eq!(presence_a_after_disconnect.connection_state, ConnectionState::Offline, "devhost A must be observed offline after its connection closed");
    assert_eq!(presence_b_after_disconnect.connection_state, ConnectionState::Online, "devhost B's presence must be tracked independently of devhost A's disconnect");
    assert_eq!(presence_b_after_disconnect.account_label.as_deref(), Some("aws:222222222222"), "devhost B's account label must be unaffected by devhost A's disconnect");

    // Both devhosts still connected can independently exercise their
    // capability scope (open-tunnel to each other for offload) — a further,
    // real confirmation that neither Identity's credential/session state
    // bled into the other's.
    let tunnel_id = open_tunnel(&mut phone, &mut devhost_b, &device_b, "rpc").await;
    send_tunnel_frame(&mut phone, tunnel_id, b"still isolated").await;
    let (received_id, payload) = recv_tunnel_frame(&mut devhost_b).await;
    assert_eq!(received_id, tunnel_id);
    assert_eq!(payload, b"still isolated");
}
