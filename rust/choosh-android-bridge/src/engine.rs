//! The testable core behind the JNI boundary: `WebAuthn` HTTP calls against
//! `relayd` and the phone relay connection, with no `jni`-crate types
//! anywhere in this module. `lib.rs`'s `extern "system"` functions are thin
//! marshaling wrappers around this.

use choosh_android_transport::PhoneConnection;
use serde::Serialize;
use serde_json::json;
use tokio::sync::Mutex;

/// Every fallible call here returns this shape as its `Ok` payload — never
/// a Rust `Err` that would surface as a thrown Java exception, per the
/// directive: JNI callers get a JSON string back either way and branch on
/// an `"error"` key, exactly like `relayd`'s own HTTP error responses
/// already do (see `rust/choosh-relayd/src/webauthn.rs`), so Kotlin has one
/// error shape to handle across both the HTTP and native-bridge layers.
fn error_json(message: &str) -> String {
    json!({ "error": message }).to_string()
}

pub struct Engine {
    http: reqwest::Client,
    http_base_url: String,
    ws_url: String,
    connection: Mutex<Option<PhoneConnection>>,
}

impl Engine {
    #[must_use]
    pub fn new(http_base_url: String, ws_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            http_base_url,
            ws_url,
            connection: Mutex::new(None),
        }
    }

    async fn post_passthrough(&self, path: &str, body: Option<&str>) -> String {
        let url = format!("{}{path}", self.http_base_url);
        let mut request = self.http.post(&url);
        request = match body {
            Some(body) => request.header("content-type", "application/json").body(body.to_string()),
            None => request.header("content-type", "application/json").body("{}"),
        };
        match request.send().await {
            Ok(response) => match response.text().await {
                // Passed through verbatim whether relayd's own body was a
                // success or an `{"error": ...}` shape — relayd already
                // uses that exact shape (see webauthn.rs), so there's
                // nothing to reshape here, only a genuine transport
                // failure below needs this crate's own error_json.
                Ok(text) => text,
                Err(error) => error_json(&format!("failed to read relayd response body: {error}")),
            },
            Err(error) => error_json(&format!("failed to reach relayd: {error}")),
        }
    }

    pub async fn webauthn_register_start(&self) -> String {
        self.post_passthrough("/webauthn/register/start", None).await
    }

    pub async fn webauthn_register_finish(&self, credential_json: &str) -> String {
        self.post_passthrough("/webauthn/register/finish", Some(credential_json)).await
    }

    pub async fn webauthn_login_start(&self) -> String {
        self.post_passthrough("/webauthn/login/start", None).await
    }

    pub async fn webauthn_login_finish(&self, credential_json: &str) -> String {
        self.post_passthrough("/webauthn/login/finish", Some(credential_json)).await
    }

    /// Establishes the persistent relay connection, per auth-and-enrollment.md's
    /// phone-reuse path. `true` on `AuthResult::Ok`; `false` on any
    /// rejection or transport failure — the caller (Kotlin) is expected to
    /// force a fresh `WebAuthn` ceremony on `false`, not retry blindly.
    pub async fn connect(&self, session_credential: &str) -> bool {
        match PhoneConnection::connect(&self.ws_url, session_credential).await {
            Ok(connection) => {
                *self.connection.lock().await = Some(connection);
                true
            }
            Err(error) => {
                tracing::warn!(%error, "phone connect failed");
                *self.connection.lock().await = None;
                false
            }
        }
    }

    pub async fn list_devhosts(&self) -> String {
        let mut guard = self.connection.lock().await;
        let Some(connection) = guard.as_mut() else {
            return error_json("not connected: call nativeConnect first");
        };
        match connection.list_devhosts().await {
            Ok(devhosts) => devhosts_json(&devhosts),
            Err(error) => {
                // A failed call on a stale/dropped connection: drop it so
                // the next attempt doesn't reuse a known-broken channel,
                // and report the failure rather than silently retrying —
                // reconnecting is `nativeConnect`'s job, not implicit here.
                *guard = None;
                error_json(&format!("list_devhosts failed: {error}"))
            }
        }
    }

    pub async fn close(&self) {
        *self.connection.lock().await = None;
    }
}

fn devhosts_json<T: Serialize>(devhosts: &[T]) -> String {
    serde_json::to_string(devhosts).unwrap_or_else(|error| error_json(&format!("failed to encode devhosts: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::routing::post;
    use serde_json::Value;

    async fn bind_fake_relayd_http() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, format!("http://{addr}"))
    }

    #[tokio::test]
    async fn register_start_passes_through_relayd_response_body() {
        let (listener, base_url) = bind_fake_relayd_http().await;
        let app = axum::Router::new().route(
            "/webauthn/register/start",
            post(|| async { Json(json!({ "challenge": "abc123" })) }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let engine = Engine::new(base_url, "ws://unused".to_string());
        let body = engine.webauthn_register_start().await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["challenge"], "abc123");
    }

    #[tokio::test]
    async fn register_finish_posts_the_credential_json_and_returns_the_session() {
        let (listener, base_url) = bind_fake_relayd_http().await;
        let app = axum::Router::new().route(
            "/webauthn/register/finish",
            post(|body: String| async move {
                let received: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(received["id"], "cred-1");
                Json(json!({ "session_credential": "sess-abc", "expires_at": 123 }))
            }),
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let engine = Engine::new(base_url, "ws://unused".to_string());
        let body = engine.webauthn_register_finish(r#"{"id":"cred-1"}"#).await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["session_credential"], "sess-abc");
    }

    #[tokio::test]
    async fn unreachable_relayd_is_a_json_error_not_a_panic() {
        let engine = Engine::new("http://127.0.0.1:1".to_string(), "ws://unused".to_string());
        let body = engine.webauthn_register_start().await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert!(parsed["error"].as_str().is_some());
    }

    #[tokio::test]
    async fn list_devhosts_before_connect_is_a_typed_error() {
        let engine = Engine::new("http://unused".to_string(), "ws://unused".to_string());
        let body = engine.list_devhosts().await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert!(parsed["error"].as_str().unwrap().contains("not connected"));
    }

    #[tokio::test]
    async fn connect_then_list_devhosts_round_trips_over_a_real_websocket() {
        use choosh_protocol::framing::encode_frame;
        use choosh_protocol::relay::{
            AuthOk, AuthResult, ClientAuth, ConnectionState, ControlRequest, ControlResponse,
            DevHostPresence, FRAME_CLASS_CONTROL, MAX_CONTROL_FRAME_BYTES, PhoneAuth, ServerHello,
        };
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        async fn send<T: Serialize>(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>, value: &T) {
            let mut payload = vec![FRAME_CLASS_CONTROL];
            payload.extend(serde_json::to_vec(value).unwrap());
            let wire = encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).unwrap();
            ws.send(Message::Binary(wire.into())).await.unwrap();
        }
        async fn recv<T: serde::de::DeserializeOwned>(
            ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        ) -> T {
            let Some(Ok(Message::Binary(bytes))) = ws.next().await else { panic!("expected binary frame") };
            let mut decoder = choosh_protocol::framing::FrameDecoder::new(
                choosh_protocol::framing::FrameLimits::new(MAX_CONTROL_FRAME_BYTES, 4).unwrap(),
            );
            let frames = decoder.feed(&bytes).unwrap();
            let (_class, body) = frames[0].split_first().unwrap();
            serde_json::from_slice(body).unwrap()
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let ws_url = format!("ws://{addr}/connect");

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

            send(&mut ws, &ServerHello { nonce: "n".to_string() }).await;
            let _auth: ClientAuth = recv(&mut ws).await;
            send(&mut ws, &AuthResult::Ok(AuthOk { identity_class: choosh_protocol::relay::IdentityClass::Phone, device_id: "phone-1".to_string() })).await;
            let _request: ControlRequest = recv(&mut ws).await;
            send(
                &mut ws,
                &ControlResponse::ListDevhostsOk {
                    request_id: "ignored".to_string(),
                    devhosts: vec![DevHostPresence {
                        device_id: "dev-1".to_string(),
                        alias: "build-box".to_string(),
                        platform: "linux".to_string(),
                        account_label: None,
                        connection_state: ConnectionState::Online,
                        last_seen: "now".to_string(),
                    }],
                },
            )
            .await;
            let _ = PhoneAuth { session_credential: String::new() }; // keep import used across cfg
        });

        let engine = Engine::new("http://unused".to_string(), ws_url);
        assert!(engine.connect("good-cred").await, "connect should succeed against a well-behaved fake relayd");
        let body = engine.list_devhosts().await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed[0]["alias"], "build-box");
    }
}
