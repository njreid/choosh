//! Shared relayd dial/authenticate logic for `choosh-hostd`'s two
//! client-only WebSocket surfaces: `proxy.rs` (`proxy connect`/`proxy
//! sync`) and `dev_exec.rs` (`dev-exec`). Both dial `relayd`'s public
//! WebSocket endpoint and authenticate with an already-persisted device
//! credential in exactly the same three steps — dial, receive
//! `ServerHello`, sign the nonce and send `ClientAuth::Device`, receive
//! `AuthResult` — so that one shared shape lives here instead of twice.
//!
//! **Deliberately not shared with `serve.rs`.** `serve.rs`'s own
//! `dial`/`run_one_connection` are private to that module and built
//! around `serve`'s own devhost-specific state (an `RpcContext`, a
//! long-lived reconnect loop, `pty:`/`web:` tunnel bookkeeping) that
//! neither `proxy.rs` nor `dev_exec.rs` has any of. A prior review judged
//! pulling a shared abstraction out of that already-tested, working
//! module a worse trade than the small amount of connect+auth logic
//! duplicated there — this module only unifies the two *other*,
//! genuinely-identical client-only call sites with each other.

use base64::Engine;
use choosh_protocol::relay::{AuthResult, ClientAuth, DeviceAuth, FRAME_CLASS_CONTROL, ServerHello};

use crate::credential::{Credential, CredentialError};
use crate::frame_channel::{ChannelError, FrameChannel};

const DEFAULT_RELAYD_URL: &str = "ws://127.0.0.1:7443/connect";

/// `relayd`'s WebSocket URL, overridable via `CHOOSH_RELAYD_URL` — both
/// `proxy.rs`'s and `dev_exec.rs`'s tests rely on this override to point
/// at a fake relayd instead of a real one.
pub(crate) fn relay_url() -> String {
    std::env::var("CHOOSH_RELAYD_URL").unwrap_or_else(|_| DEFAULT_RELAYD_URL.to_string())
}

pub(crate) type WsChannel = FrameChannel<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>;

/// Failure modes shared by [`dial`]/[`connect_authenticated`]. Each caller
/// (`proxy::ProxyError`/`dev_exec::DevExecError`) converts this into its
/// own error type via `From`, so both keep their own module-specific error
/// enum and `Display` wording rather than this module dictating either.
#[derive(Debug)]
pub(crate) enum RelayClientError {
    Transport(String),
    AuthFailed(String),
    Credential(CredentialError),
}

impl From<ChannelError> for RelayClientError {
    fn from(error: ChannelError) -> Self {
        Self::Transport(error.to_string())
    }
}

impl From<CredentialError> for RelayClientError {
    fn from(error: CredentialError) -> Self {
        Self::Credential(error)
    }
}

/// # Errors
///
/// [`RelayClientError::Transport`] if the WebSocket connection itself
/// cannot be established.
pub(crate) async fn dial(relay_url: &str) -> Result<WsChannel, RelayClientError> {
    let (stream, _response) =
        tokio_tungstenite::connect_async(relay_url).await.map_err(|error| RelayClientError::Transport(error.to_string()))?;
    Ok(FrameChannel::new(stream))
}

/// Dials `relay_url` and authenticates with `credential` — the shared
/// step 1 for `proxy connect`/`proxy sync` (ssh-bridge-and-zed.md) and
/// `dev-exec` (auth-and-enrollment.md's capability table) alike.
///
/// # Errors
///
/// [`RelayClientError::Transport`] for a failed dial or a transport
/// failure mid-handshake, [`RelayClientError::Credential`] if the stored
/// credential's key material is corrupt, [`RelayClientError::AuthFailed`]
/// if `relayd` rejects the signed challenge.
pub(crate) async fn connect_authenticated(relay_url: &str, credential: &Credential) -> Result<WsChannel, RelayClientError> {
    let mut channel = dial(relay_url).await?;
    let hello: ServerHello = channel.recv().await?;
    let signature = credential.sign(hello.nonce.as_bytes())?;
    let auth = ClientAuth::Device(DeviceAuth {
        device_id: credential.device_id.clone(),
        certificate: credential.certificate.clone(),
        signature: base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
    });
    channel.send(FRAME_CLASS_CONTROL, &auth).await?;
    match channel.recv().await? {
        AuthResult::Ok(_) => Ok(channel),
        AuthResult::Failed(failed) => Err(RelayClientError::AuthFailed(failed.reason)),
    }
}

/// A fresh random id for a `ControlRequest`'s `request_id` field.
pub(crate) fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_protocol::relay::{AuthFailed, AuthOk, IdentityClass, MAX_CONTROL_FRAME_BYTES};
    use choosh_protocol::framing::encode_frame;
    use ed25519_dalek::SigningKey;
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    fn fake_credential() -> Credential {
        let signing_key = SigningKey::generate(&mut rand::rng());
        Credential::new("laptop-1".to_string(), "fake-cert".to_string(), &signing_key)
    }

    async fn bind_fake_relayd() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, format!("ws://{addr}/connect"))
    }

    async fn send_control<T: serde::Serialize>(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>, value: &T) {
        let mut payload = vec![FRAME_CLASS_CONTROL];
        payload.extend(serde_json::to_vec(value).unwrap());
        let framed = encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).unwrap();
        ws.send(Message::Binary(framed.into())).await.unwrap();
    }

    /// `dial` against a `127.0.0.1` port nothing is listening on must
    /// surface as a [`RelayClientError::Transport`], not a panic or a hang
    /// — `connect_async` itself performs the TCP connect, so a refused
    /// connection has to unwrap cleanly through our `map_err`.
    #[tokio::test]
    async fn dial_returns_a_transport_error_when_the_connection_is_refused() {
        // Bind a listener just to reserve a free port, then drop it
        // immediately so nothing is listening there once we dial it —
        // guarantees a connection-refused rather than racing a real
        // service that might happen to be on some fixed port.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let result = dial(&format!("ws://{addr}/connect")).await;

        match result {
            Err(RelayClientError::Transport(_)) => {}
            Err(other) => panic!("expected Transport error, got a different error: {other:?}"),
            Ok(_) => panic!("expected Transport error, got Ok(channel) for a connection nothing was listening on"),
        }
    }

    /// When `relayd` answers the signed challenge with `AuthResult::Failed`,
    /// `connect_authenticated` must surface that as
    /// `RelayClientError::AuthFailed` carrying `relayd`'s exact reason
    /// string, not panic or silently return `Ok`.
    #[tokio::test]
    async fn connect_authenticated_surfaces_auth_failed_with_relayds_reason() {
        let (listener, relay_url) = bind_fake_relayd().await;

        let client_fut = async {
            let credential = fake_credential();
            connect_authenticated(&relay_url, &credential).await
        };
        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let hello = ServerHello { nonce: "test-nonce".to_string() };
            send_control(&mut ws, &hello).await;
            let _client_auth: serde_json::Value = {
                let mut decoder = choosh_protocol::framing::FrameDecoder::new(choosh_protocol::framing::FrameLimits::new(MAX_CONTROL_FRAME_BYTES, 4).unwrap());
                loop {
                    let Some(Ok(Message::Binary(bytes))) = futures_util::StreamExt::next(&mut ws).await else { panic!("expected a binary control message") };
                    let frames = decoder.feed(&bytes).unwrap();
                    if let Some(frame) = frames.into_iter().next() {
                        let (_class, body) = frame.split_first().unwrap();
                        break serde_json::from_slice(body).unwrap();
                    }
                }
            };
            send_control(&mut ws, &AuthResult::Failed(AuthFailed { reason: "device revoked".to_string() })).await;
        };

        let (result, ()) = tokio::join!(client_fut, server_fut);

        match result {
            Err(RelayClientError::AuthFailed(reason)) => assert_eq!(reason, "device revoked"),
            Err(other) => panic!("expected AuthFailed(\"device revoked\"), got a different error: {other:?}"),
            Ok(_) => panic!("expected AuthFailed(\"device revoked\"), got Ok(channel) despite relayd rejecting the device"),
        }
    }

    /// If whatever answers first isn't a `ServerHello` at all (some other
    /// control message shape), `connect_authenticated`'s `channel.recv()`
    /// must fail deserializing it and return a
    /// `RelayClientError::Transport`, not panic — this is the "malformed
    /// first message" case: a peer that isn't a well-behaved `relayd`.
    #[tokio::test]
    async fn connect_authenticated_handles_a_malformed_first_message_without_panicking() {
        let (listener, relay_url) = bind_fake_relayd().await;

        let client_fut = async {
            let credential = fake_credential();
            connect_authenticated(&relay_url, &credential).await
        };
        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            // Send something that is neither `ServerHello` nor any other
            // expected shape in place of the handshake's first message.
            send_control(&mut ws, &AuthResult::Ok(AuthOk { identity_class: IdentityClass::LaptopProxy, device_id: "laptop-1".to_string() })).await;
        };

        let (result, ()) = tokio::join!(client_fut, server_fut);

        match result {
            Err(RelayClientError::Transport(_)) => {}
            Err(other) => panic!("expected Transport error on a malformed first message, got a different error: {other:?}"),
            Ok(_) => panic!("expected Transport error on a malformed first message, got Ok(channel)"),
        }
    }

    /// Once past a valid `ServerHello`, a second message that isn't a valid
    /// `AuthResult` (garbage JSON in the right shape to arrive, but not
    /// deserializable as `AuthResult`) must likewise fail cleanly rather
    /// than panicking the client.
    #[tokio::test]
    async fn connect_authenticated_handles_a_malformed_auth_response_without_panicking() {
        let (listener, relay_url) = bind_fake_relayd().await;

        let client_fut = async {
            let credential = fake_credential();
            connect_authenticated(&relay_url, &credential).await
        };
        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let hello = ServerHello { nonce: "test-nonce".to_string() };
            send_control(&mut ws, &hello).await;
            let _client_auth: serde_json::Value = {
                let mut decoder = choosh_protocol::framing::FrameDecoder::new(choosh_protocol::framing::FrameLimits::new(MAX_CONTROL_FRAME_BYTES, 4).unwrap());
                loop {
                    let Some(Ok(Message::Binary(bytes))) = futures_util::StreamExt::next(&mut ws).await else { panic!("expected a binary control message") };
                    let frames = decoder.feed(&bytes).unwrap();
                    if let Some(frame) = frames.into_iter().next() {
                        let (_class, body) = frame.split_first().unwrap();
                        break serde_json::from_slice(body).unwrap();
                    }
                }
            };
            // Not an `AuthResult` shape at all.
            send_control(&mut ws, &serde_json::json!({ "unexpected": "shape" })).await;
        };

        let (result, ()) = tokio::join!(client_fut, server_fut);

        match result {
            Err(RelayClientError::Transport(_)) => {}
            Err(other) => panic!("expected Transport error on a malformed auth response, got a different error: {other:?}"),
            Ok(_) => panic!("expected Transport error on a malformed auth response, got Ok(channel)"),
        }
    }

    /// Not exhaustive uniqueness proof, just a sanity check that repeated
    /// calls don't collide or degenerate to a constant.
    #[test]
    fn new_request_id_produces_distinct_values_across_repeated_calls() {
        let ids: Vec<String> = (0..100).map(|_| new_request_id()).collect();
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "expected all 100 generated ids to be distinct");
    }
}
