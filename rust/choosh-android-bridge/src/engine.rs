//! The testable core behind the JNI boundary: `WebAuthn` HTTP calls against
//! `relayd` and the phone relay connection, with no `jni`-crate types
//! anywhere in this module. `lib.rs`'s `extern "system"` functions are thin
//! marshaling wrappers around this.

use choosh_android_transport::{CallError, PhoneConnection, PtyTunnelHandle};
use choosh_protocol::host_rpc::{RpcRequest, RpcResponse};
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

    /// Registers the phone's FCM token with `relayd` over the live
    /// connection. `false` if not connected or the call fails — the caller
    /// is expected to retry after the next successful `connect`, not
    /// treat this as fatal.
    pub async fn register_fcm_token(&self, fcm_token: &str) -> bool {
        let mut guard = self.connection.lock().await;
        let Some(connection) = guard.as_mut() else {
            return false;
        };
        match connection.register_fcm_token(fcm_token).await {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "register_fcm_token failed");
                false
            }
        }
    }

    /// Sends one M3 jj RPC ([`RpcRequest::WorkspaceDiff`],
    /// `WorkspaceLog`, `WorkspaceOpLog`, `WorkspaceOpUndo`,
    /// `WorkspaceOpRestore`, `WorkspaceStatus`) over `target_device_id`'s
    /// tunnel and returns the raw [`RpcResponse`], or `Err` with a
    /// human-readable message covering both "not connected" and a
    /// [`CallError`] — the caller (each typed `workspace_*` method below)
    /// turns either into this module's shared `{"error": ...}` JSON shape,
    /// and an `Ok(RpcResponse::Error {...})` application-level failure into
    /// the same shape by matching on it itself, since only the caller knows
    /// which response variant it expected to see on success.
    async fn call_jj_rpc(&self, target_device_id: &str, request: RpcRequest) -> Result<RpcResponse, String> {
        let mut guard = self.connection.lock().await;
        let Some(connection) = guard.as_mut() else {
            return Err("not connected: call nativeConnect first".to_string());
        };
        match connection.call_rpc(target_device_id, request).await {
            Ok(response) => Ok(response),
            Err(error) => {
                // As with list_devhosts: drop a connection that just failed
                // a call rather than let the next attempt reuse it.
                *guard = None;
                Err(format!("rpc call failed: {error}"))
            }
        }
    }

    /// Returns `workspace.diff`'s `files` array (`Vec<DiffFileEntry>`) as a
    /// bare JSON array — `from`/`to` empty means the RPC's own default
    /// (`from = "@-"`, `to = "@"`), matching this module's other
    /// caller-friendly wrapper style over the raw wire types.
    pub async fn workspace_diff(&self, target_device_id: &str, workspace_id: &str, from: &str, to: &str) -> String {
        let request = RpcRequest::WorkspaceDiff {
            request_id: new_request_id(),
            workspace_id: workspace_id.to_string(),
            from: none_if_empty(from),
            to: none_if_empty(to),
        };
        match self.call_jj_rpc(target_device_id, request).await {
            Ok(RpcResponse::WorkspaceDiffOk { files, .. }) => to_json(&files),
            Ok(other) => unexpected_or_error_json("workspace.diff", &other),
            Err(message) => error_json(&message),
        }
    }

    /// Returns `workspace.log`'s `changes` array (`Vec<ChangeGraphNode>`) —
    /// the `JjChangeGraph` item's node/edge data (edges are each node's own
    /// `parent_change_ids`) — as a bare JSON array. Empty `revset` means
    /// `jj log`'s own default.
    pub async fn workspace_log(&self, target_device_id: &str, workspace_id: &str, revset: &str, limit: i64) -> String {
        let request = RpcRequest::WorkspaceLog {
            request_id: new_request_id(),
            workspace_id: workspace_id.to_string(),
            revset: none_if_empty(revset),
            limit: clamp_limit(limit),
        };
        match self.call_jj_rpc(target_device_id, request).await {
            Ok(RpcResponse::WorkspaceLogOk { changes, .. }) => to_json(&changes),
            Ok(other) => unexpected_or_error_json("workspace.log", &other),
            Err(message) => error_json(&message),
        }
    }

    /// Returns `workspace.op.log`'s `operations` array
    /// (`Vec<OperationLogEntry>`) as a bare JSON array, most recent first.
    pub async fn workspace_op_log(&self, target_device_id: &str, workspace_id: &str, limit: i64) -> String {
        let request = RpcRequest::WorkspaceOpLog {
            request_id: new_request_id(),
            workspace_id: workspace_id.to_string(),
            limit: clamp_limit(limit),
        };
        match self.call_jj_rpc(target_device_id, request).await {
            Ok(RpcResponse::WorkspaceOpLogOk { operations, .. }) => to_json(&operations),
            Ok(other) => unexpected_or_error_json("workspace.op.log", &other),
            Err(message) => error_json(&message),
        }
    }

    /// `workspace.op.undo` — reverses `op_id`'s effect. Returns
    /// `{"new_op_id": "..."}`: per `jj-integration.md`, this is the id of
    /// the *new* operation-log entry the undo itself created, never `op_id`.
    /// The caller (Kotlin) is expected to re-fetch `workspace.log`/
    /// `workspace.op.log` afterward to observe the reversal — this call
    /// does not do that itself.
    pub async fn workspace_op_undo(&self, target_device_id: &str, workspace_id: &str, op_id: &str) -> String {
        let request = RpcRequest::WorkspaceOpUndo {
            request_id: new_request_id(),
            workspace_id: workspace_id.to_string(),
            op_id: op_id.to_string(),
        };
        match self.call_jj_rpc(target_device_id, request).await {
            Ok(RpcResponse::WorkspaceOpUndoOk { new_op_id, .. }) => json!({ "new_op_id": new_op_id }).to_string(),
            Ok(other) => unexpected_or_error_json("workspace.op.undo", &other),
            Err(message) => error_json(&message),
        }
    }

    /// `workspace.op.restore` — resets the repo to `op_id`'s state. Returns
    /// `{"new_op_id": "..."}`, same "id of the new entry, not the one
    /// restored to" contract as [`Self::workspace_op_undo`].
    pub async fn workspace_op_restore(&self, target_device_id: &str, workspace_id: &str, op_id: &str) -> String {
        let request = RpcRequest::WorkspaceOpRestore {
            request_id: new_request_id(),
            workspace_id: workspace_id.to_string(),
            op_id: op_id.to_string(),
        };
        match self.call_jj_rpc(target_device_id, request).await {
            Ok(RpcResponse::WorkspaceOpRestoreOk { new_op_id, .. }) => json!({ "new_op_id": new_op_id }).to_string(),
            Ok(other) => unexpected_or_error_json("workspace.op.restore", &other),
            Err(message) => error_json(&message),
        }
    }

    /// Returns `workspace.status`'s `{changed, conflicted}` shape verbatim,
    /// backing the explorer's changed-files section.
    pub async fn workspace_status(&self, target_device_id: &str, workspace_id: &str) -> String {
        let request = RpcRequest::WorkspaceStatus { request_id: new_request_id(), workspace_id: workspace_id.to_string() };
        match self.call_jj_rpc(target_device_id, request).await {
            Ok(RpcResponse::WorkspaceStatusOk { changed, conflicted, .. }) => {
                to_json(&json!({ "changed": changed, "conflicted": conflicted }))
            }
            Ok(other) => unexpected_or_error_json("workspace.status", &other),
            Err(message) => error_json(&message),
        }
    }

    /// Opens a document per editor-protocol.md's "Opening a document":
    /// `workspace.file.read { workspace_id, path }` (no `revision`/`range`
    /// — defaults to the live working copy `@`, whole file). The returned
    /// JSON is one of:
    ///
    /// - `{"type":"ok","content_base64":...,"revision":...,"total_size":...}`
    /// - `{"type":"error","code":...,"message":...}` — a `hostd`-side
    ///   rejection (e.g. binary/oversized, per editor-protocol.md's "Limits").
    /// - `{"type":"offline","message":...}` — a transport-level failure
    ///   (not connected, or `call_rpc` itself failed) — editor-protocol.md's
    ///   `offline` save state, not an application error.
    pub async fn open_document(&self, target_device_id: &str, workspace_id: &str, path: &str) -> String {
        let mut guard = self.connection.lock().await;
        let Some(connection) = guard.as_mut() else {
            return offline_json("not connected: call nativeConnect first");
        };
        let request = RpcRequest::WorkspaceFileRead {
            request_id: new_request_id(),
            workspace_id: workspace_id.to_string(),
            path: path.to_string(),
            revision: None,
            range: None,
        };
        match connection.call_rpc(target_device_id, request).await {
            Ok(RpcResponse::WorkspaceFileReadOk { content_base64, total_size, revision, .. }) => {
                json!({ "type": "ok", "content_base64": content_base64, "revision": revision, "total_size": total_size }).to_string()
            }
            Ok(RpcResponse::Error { code, message, .. }) => error_json_with_code(&code, &message),
            Ok(other) => error_json_with_code("internal", &format!("unexpected response to workspace.file.read: {other:?}")),
            Err(error) => {
                // Per relay-protocol.md's reconnect-discontinuity rule and
                // this module's existing `list_devhosts` precedent: a failed
                // call on a stale/dropped connection drops it so the next
                // attempt doesn't reuse a known-broken channel.
                if matches!(error, CallError::Transport(_)) {
                    *guard = None;
                }
                offline_json(&format!("workspace.file.read failed: {error}"))
            }
        }
    }

    /// Saves a document per editor-protocol.md's "Persistence":
    /// `workspace.file.write { workspace_id, path, base_revision,
    /// content_base64 }`. `content_base64` is always the document's full
    /// current content (this crate's V1 scope, per
    /// `choosh_protocol::host_rpc::RpcRequest::WorkspaceFileWrite`'s doc
    /// comment) — callers debounce Sora's edit stream themselves before
    /// calling this. The returned JSON:
    ///
    /// - `{"type":"ok","revision":...}` — becomes the next `base_revision`.
    /// - `{"type":"stale","current_revision":...,"current_content_base64":...}`
    ///   — a real conflict (editor-protocol.md's `conflicted` state): the
    ///   caller MUST NOT silently overwrite in either direction, it must
    ///   surface this to the user for an explicit "keep mine"/"take theirs"
    ///   resolution.
    /// - `{"type":"error","code":...,"message":...}` — a `hostd`-side
    ///   rejection.
    /// - `{"type":"offline","message":...}` — a transport-level failure;
    ///   the caller keeps its local edits and gets a chance to retry once
    ///   connectivity returns, per editor-protocol.md's `offline` state.
    pub async fn save_document(
        &self,
        target_device_id: &str,
        workspace_id: &str,
        path: &str,
        base_revision: &str,
        content_base64: &str,
    ) -> String {
        let mut guard = self.connection.lock().await;
        let Some(connection) = guard.as_mut() else {
            return offline_json("not connected: call nativeConnect first");
        };
        let request = RpcRequest::WorkspaceFileWrite {
            request_id: new_request_id(),
            workspace_id: workspace_id.to_string(),
            path: path.to_string(),
            base_revision: base_revision.to_string(),
            content_base64: content_base64.to_string(),
        };
        match connection.call_rpc(target_device_id, request).await {
            Ok(RpcResponse::WorkspaceFileWriteOk { revision, .. }) => json!({ "type": "ok", "revision": revision }).to_string(),
            Ok(RpcResponse::WorkspaceFileWriteStale { current_revision, current_content_base64, .. }) => json!({
                "type": "stale",
                "current_revision": current_revision,
                "current_content_base64": current_content_base64,
            })
            .to_string(),
            Ok(RpcResponse::Error { code, message, .. }) => error_json_with_code(&code, &message),
            Ok(other) => error_json_with_code("internal", &format!("unexpected response to workspace.file.write: {other:?}")),
            Err(error) => {
                if matches!(error, CallError::Transport(_)) {
                    *guard = None;
                }
                offline_json(&format!("workspace.file.write failed: {error}"))
            }
        }
    }

    /// Opens a `"pty:<item_id>"`-purpose tunnel for the terminal renderer.
    ///
    /// # Errors
    ///
    /// A message describing why: not connected yet, or the tunnel-open
    /// call itself failed (see [`CallError`]).
    ///
    /// Only called from `terminal_jni.rs`, which is
    /// `#[cfg(target_os = "android")]`-gated — see `crate::with_connection_engine`
    /// and `terminal_renderer.rs`'s module doc for why a host build
    /// legitimately never calls this despite it being real, tested (via
    /// this crate's Android-target build) production code.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub async fn open_pty_tunnel(&self, target_device_id: &str, item_id: &str) -> Result<PtyTunnelHandle, String> {
        let mut guard = self.connection.lock().await;
        let Some(connection) = guard.as_mut() else {
            return Err("not connected: call nativeConnect first".to_string());
        };
        connection.open_pty_tunnel(target_device_id, item_id).await.map_err(|error| {
            tracing::warn!(%error, "open_pty_tunnel failed");
            format!("open_pty_tunnel failed: {error}")
        })
    }

    pub async fn close(&self) {
        *self.connection.lock().await = None;
    }
}

/// A request id unique per in-flight RPC, generated by this crate rather
/// than `choosh-android-transport` (its pre-M1 control methods generate
/// their own internally, but `PhoneConnection::call_rpc` takes a
/// fully-formed [`RpcRequest`] — including its `request_id` — from the
/// caller). Same shape as that crate's own private `new_request_id`
/// (process id plus a monotonic counter — unique per in-flight request, not
/// globally unique or ordered, and no UUID dependency this crate has no
/// other need for).
fn new_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("req-{}-{count}", std::process::id())
}

fn offline_json(message: &str) -> String {
    json!({ "type": "offline", "message": message }).to_string()
}

fn error_json_with_code(code: &str, message: &str) -> String {
    json!({ "type": "error", "code": code, "message": message }).to_string()
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|error| error_json(&format!("failed to encode response: {error}")))
}

fn none_if_empty(value: &str) -> Option<String> {
    if value.is_empty() { None } else { Some(value.to_string()) }
}

/// Clamps a Kotlin-supplied `Long` limit to a sane, positive `usize` —
/// negative/zero/absurdly large inputs from the JNI boundary get a safe
/// default rather than panicking on the `i64`->`usize` conversion.
fn clamp_limit(limit: i64) -> usize {
    usize::try_from(limit).unwrap_or(50).clamp(1, 500)
}

/// An `Ok(RpcResponse::Error {...})` or any other unexpected response
/// variant, turned into this module's shared error JSON shape — every
/// `workspace_*` method's fallback arm for "the RPC succeeded at the
/// transport level but wasn't the response we expected."
fn unexpected_or_error_json(rpc_name: &str, response: &RpcResponse) -> String {
    match response {
        RpcResponse::Error { code, message, .. } => json!({ "error": format!("{code}: {message}") }).to_string(),
        other => error_json(&format!("unexpected response to {rpc_name}: {other:?}")),
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
            let request: ControlRequest = recv(&mut ws).await;
            let ControlRequest::ListDevhosts { request_id } = request else { panic!("expected list-devhosts") };
            send(
                &mut ws,
                &ControlResponse::ListDevhostsOk {
                    request_id,
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
