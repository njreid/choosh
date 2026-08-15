//! The testable core behind the JNI boundary: `WebAuthn` HTTP calls against
//! `relayd` and the phone relay connection, with no `jni`-crate types
//! anywhere in this module. `lib.rs`'s `extern "system"` functions are thin
//! marshaling wrappers around this.

use choosh_android_transport::{AgentEventPush, CallError, PhoneConnection, PtyTunnelHandle, WebTunnelHandle};
use choosh_protocol::host_rpc::{ByteRange, RpcRequest, RpcResponse};
use choosh_protocol::relay::AgentEventsResumeResponse;
use serde::Serialize;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::Arc;
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

/// Bound on how many un-polled live agent-event pushes [`Engine`] holds at
/// once (see [`Engine::agent_events`]) — same order of magnitude as
/// `choosh-hostd::agent_event_spool::MAX_EVENTS_PER_WORKSPACE` (500) and the
/// identical reasoning: these are coarse lifecycle signals, not
/// high-frequency telemetry, so this only needs to comfortably outlast a
/// Kotlin caller that's briefly slow to poll (backgrounded, mid-recompose),
/// not buffer forever. Once full, the oldest un-polled push is dropped to
/// make room for the newest — losing an old, stale-by-now push in favor of
/// current state is the right trade for a live-status queue (unlike, say, a
/// log), and any real gap this causes is still recoverable: the next
/// `agent_events_resume` call replays from the workspace's own
/// last-acknowledged sequence regardless of what this queue held onto.
const AGENT_EVENT_QUEUE_CAPACITY: usize = 500;

pub struct Engine {
    http: reqwest::Client,
    http_base_url: String,
    ws_url: String,
    connection: Mutex<Option<PhoneConnection>>,
    /// Live agent-event pushes received on the current (or most recent)
    /// connection, oldest first, drained by [`Self::poll_agent_events`].
    /// Deliberately its own `Mutex`, separate from `connection` — populated
    /// by a background pump task (spawned in [`Self::connect`]) that reads
    /// off `PhoneConnection::take_agent_event_receiver`'s receiver, which is
    /// itself decoupled from `connection`'s lock for exactly this reason
    /// (see that method's doc comment): polling this queue, or an ordinary
    /// RPC call locking `connection`, never blocks behind the other.
    agent_events: Arc<Mutex<VecDeque<AgentEventPush>>>,
}

impl Engine {
    #[must_use]
    pub fn new(http_base_url: String, ws_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            http_base_url,
            ws_url,
            connection: Mutex::new(None),
            agent_events: Arc::new(Mutex::new(VecDeque::new())),
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
    ///
    /// Also takes ownership of the fresh connection's agent-event push
    /// receiver (`PhoneConnection::take_agent_event_receiver`) and spawns
    /// [`pump_agent_events`] to drain it into [`Self::agent_events`] for the
    /// lifetime of this connection — see that function's doc comment for
    /// why this happens here, before the connection is stored behind
    /// `self.connection`'s lock, rather than lazily on first poll.
    pub async fn connect(&self, session_credential: &str) -> bool {
        match PhoneConnection::connect(&self.ws_url, session_credential).await {
            Ok(mut connection) => {
                if let Some(agent_event_rx) = connection.take_agent_event_receiver() {
                    tokio::spawn(pump_agent_events(agent_event_rx, Arc::clone(&self.agent_events)));
                }
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

    /// Sends one `jj`-backed workspace RPC ([`RpcRequest::WorkspaceDiff`],
    /// `WorkspaceLog`, `WorkspaceOpLog`, `WorkspaceOpUndo`,
    /// `WorkspaceOpRestore`, `WorkspaceStatus` — the diff/log/op-log/undo/
    /// restore group landed in M3, `WorkspaceStatus` earlier in M1; all are
    /// done, this is just which wire types this helper carries) over
    /// `target_device_id`'s tunnel and returns the raw [`RpcResponse`], or
    /// `Err` with a
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
    /// — defaults to the live working copy `@`, whole file). Internally this
    /// may issue *multiple* `workspace.file.read` calls (see
    /// [`read_whole_file`]'s doc comment): a single response can never carry
    /// more than `choosh_protocol::host_rpc::MAX_FILE_READ_RANGE_BYTES` of
    /// content (that bound exists so one request/response fits inside one
    /// `"rpc"`-purpose relay tunnel frame — see that constant's own doc
    /// comment), so a document larger than the bound needs more than one
    /// round trip to fetch in full. Callers of this method still see one
    /// logical "open" and get the document's true, complete content back
    /// regardless of its size. The returned JSON is one of:
    ///
    /// - `{"type":"ok","content_base64":...,"revision":...,"total_size":...}`
    /// - `{"type":"error","code":...,"message":...}` — a `hostd`-side
    ///   rejection (e.g. binary/oversized, per editor-protocol.md's "Limits"),
    ///   or the file changing on disk while this method was still fetching
    ///   its later chunks (a genuine, if narrow, race — see
    ///   [`read_whole_file`]'s revision-stability check).
    /// - `{"type":"offline","message":...}` — a transport-level failure
    ///   (not connected, or `call_rpc` itself failed) — editor-protocol.md's
    ///   `offline` save state, not an application error.
    pub async fn open_document(&self, target_device_id: &str, workspace_id: &str, path: &str) -> String {
        let mut guard = self.connection.lock().await;
        let Some(connection) = guard.as_mut() else {
            return offline_json("not connected: call nativeConnect first");
        };
        match read_whole_file(connection, target_device_id, workspace_id, path).await {
            Ok((content, total_size, revision)) => {
                use base64::Engine as _;
                let content_base64 = base64::engine::general_purpose::STANDARD.encode(content);
                json!({ "type": "ok", "content_base64": content_base64, "revision": revision, "total_size": total_size }).to_string()
            }
            Err(WholeFileReadError::Application { code, message }) => error_json_with_code(&code, &message),
            Err(WholeFileReadError::Other(message)) => error_json_with_code("internal", &message),
            Err(WholeFileReadError::Transport(message)) => {
                // Per relay-protocol.md's reconnect-discontinuity rule and
                // this module's existing `list_devhosts` precedent: a failed
                // call on a stale/dropped connection drops it so the next
                // attempt doesn't reuse a known-broken channel.
                *guard = None;
                offline_json(&format!("workspace.file.read failed: {message}"))
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
    /// Returns `project.list`'s `projects` array (`Vec<ProjectSummary>`) as
    /// a bare JSON array — the fleet drawer's Project-mode data source
    /// (`host-rpc.md`'s `project.list`). Each entry's `active` is computed
    /// entirely by `hostd`; this crate never infers or overrides it. Same
    /// error-shape convention as `workspace_status`/`item_list`.
    pub async fn project_list(&self, target_device_id: &str) -> String {
        let request = RpcRequest::ProjectList { request_id: new_request_id() };
        match self.call_jj_rpc(target_device_id, request).await {
            Ok(RpcResponse::ProjectListOk { projects, .. }) => to_json(&projects),
            Ok(other) => unexpected_or_error_json("project.list", &other),
            Err(message) => error_json(&message),
        }
    }

    /// `project.set_primary_workspace` — changes which Workspace
    /// `project.list` reports as `primary_workspace_id` for `project_id`.
    /// `workspace_id` MUST already belong to `project_id`
    /// (`host-rpc.md`) — `hostd` rejects a mismatched pair rather than
    /// silently applying it, surfaced here the same way every other
    /// `hostd`-side rejection is (`unexpected_or_error_json`'s `Error`
    /// arm). Returns `{"ok":true}` on success, mirroring
    /// `workspace_op_undo`'s "small confirmation payload" style rather
    /// than a bare boolean, so a caller-visible failure reason survives
    /// the JNI boundary instead of collapsing to `false`.
    pub async fn project_set_primary_workspace(&self, target_device_id: &str, project_id: &str, workspace_id: &str) -> String {
        let request = RpcRequest::ProjectSetPrimaryWorkspace {
            request_id: new_request_id(),
            project_id: project_id.to_string(),
            workspace_id: workspace_id.to_string(),
        };
        match self.call_jj_rpc(target_device_id, request).await {
            Ok(RpcResponse::ProjectSetPrimaryWorkspaceOk { .. }) => json!({ "ok": true }).to_string(),
            Ok(other) => unexpected_or_error_json("project.set_primary_workspace", &other),
            Err(message) => error_json(&message),
        }
    }

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

    /// Returns `item.list`'s items as a bare JSON array, backing the
    /// `WebService`/Markdown pinned-item UI's status polling
    /// (service-tunnels.md's "Lifecycle"/"Readiness" — a pinned item's
    /// `starting`/`running`/`stopped`/`failed`/`unknown` status). Same
    /// error-shape convention as `workspace_status` etc.
    pub async fn item_list(&self, target_device_id: &str, workspace_id: &str) -> String {
        let request = RpcRequest::ItemList { request_id: new_request_id(), workspace_id: workspace_id.to_string() };
        match self.call_jj_rpc(target_device_id, request).await {
            Ok(RpcResponse::ItemListOk { items, .. }) => to_json(&items),
            Ok(other) => unexpected_or_error_json("item.list", &other),
            Err(message) => error_json(&message),
        }
    }

    /// Opens a `"web:<item_id>"`-purpose tunnel for [`crate::web_gateway`]'s
    /// loopback gateway — one call per accepted client connection, per
    /// `open_web_tunnel`'s own doc comment on why that's the right
    /// granularity.
    ///
    /// # Errors
    ///
    /// A message describing why: not connected yet, or the tunnel-open
    /// call itself failed (see [`CallError`]).
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub async fn open_web_tunnel(&self, target_device_id: &str, item_id: &str) -> Result<WebTunnelHandle, String> {
        let mut guard = self.connection.lock().await;
        let Some(connection) = guard.as_mut() else {
            return Err("not connected: call nativeConnect first".to_string());
        };
        connection.open_web_tunnel(target_device_id, item_id).await.map_err(|error| {
            tracing::warn!(%error, "open_web_tunnel failed");
            format!("open_web_tunnel failed: {error}")
        })
    }

    /// Fetches `path` (workspace-relative) at an optional byte range, for
    /// [`crate::markdown_gateway`]'s `FileFetcher` — a lower-level sibling
    /// of [`Self::open_document`]/[`Self::save_document`] that returns
    /// decoded raw bytes plus `total_size` rather than a JSON string, since
    /// the markdown gateway feeds the bytes straight into
    /// `choosh_web::markdown::render_markdown` or an HTTP response body,
    /// never into Kotlin/JNI.
    ///
    /// `range: Some(...)` is passed straight through as a single RPC call —
    /// the caller (the `/asset` route's own HTTP-range translation) chose
    /// that exact byte window on purpose and is responsible for keeping its
    /// length within `MAX_FILE_READ_RANGE_BYTES` itself (see
    /// `markdown_gateway::MAX_ASSET_CHUNK_BYTES`), so this method must not
    /// silently redefine what a caller-specified range means. `range: None`
    /// (the `/doc` route's "whole document" fetch) goes through
    /// [`read_whole_file`] instead, since "whole file" can legitimately be
    /// larger than one RPC response can carry.
    ///
    /// # Errors
    ///
    /// `Err("not_found: ...")`-prefixed message on a `hostd`-side
    /// rejection (surfaced to the caller as a 404), or any other message on
    /// a transport-level failure (surfaced as a 502) — see
    /// `crate::markdown_gateway::FetchError`, which callers map this into.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub async fn read_workspace_file_range(
        &self,
        target_device_id: &str,
        workspace_id: &str,
        path: &str,
        range: Option<(u64, u64)>,
    ) -> Result<(Vec<u8>, u64), String> {
        let mut guard = self.connection.lock().await;
        let Some(connection) = guard.as_mut() else {
            return Err("not connected: call nativeConnect first".to_string());
        };

        let Some((offset, length)) = range else {
            return match read_whole_file(connection, target_device_id, workspace_id, path).await {
                Ok((content, total_size, _revision)) => Ok((content, total_size)),
                Err(WholeFileReadError::Application { code, message }) => Err(format!("{code}: {message}")),
                Err(WholeFileReadError::Other(message)) => Err(message),
                Err(WholeFileReadError::Transport(message)) => {
                    *guard = None;
                    Err(format!("workspace.file.read failed: {message}"))
                }
            };
        };

        let request = RpcRequest::WorkspaceFileRead {
            request_id: new_request_id(),
            workspace_id: workspace_id.to_string(),
            path: path.to_string(),
            revision: None,
            range: Some(ByteRange { offset, length }),
        };
        match connection.call_rpc(target_device_id, request).await {
            Ok(RpcResponse::WorkspaceFileReadOk { content_base64, total_size, .. }) => {
                use base64::Engine as _;
                let content = base64::engine::general_purpose::STANDARD
                    .decode(content_base64)
                    .map_err(|error| format!("hostd returned invalid base64: {error}"))?;
                Ok((content, total_size))
            }
            Ok(RpcResponse::Error { code, message, .. }) => Err(format!("{code}: {message}")),
            Ok(other) => Err(format!("unexpected response to workspace.file.read: {other:?}")),
            Err(error) => {
                if matches!(error, CallError::Transport(_)) {
                    *guard = None;
                }
                Err(format!("workspace.file.read failed: {error}"))
            }
        }
    }

    /// Drains every live agent-event push received since the last call,
    /// oldest first, as a bare JSON array of `{"from_device_id", "event",
    /// "sequence"}` objects (`event` is `WireAgentEvent`'s own tagged JSON
    /// shape, verbatim). Kotlin is expected to poll this periodically (see
    /// this crate's design notes on why a polling queue, not a JNI
    /// callback, mirrors this crate's existing `item_list`/`workspace_status`
    /// shape) and apply every returned push to its own per-workspace
    /// sequence cursor, never regressing it. Always succeeds — an empty
    /// array (not an `{"error": ...}` shape) if nothing has arrived, or if
    /// this engine was never connected, so callers can poll unconditionally
    /// without a "not connected" special case.
    pub async fn poll_agent_events(&self) -> String {
        let drained: Vec<AgentEventPush> = {
            let mut guard = self.agent_events.lock().await;
            guard.drain(..).collect()
        };
        let json_ready: Vec<AgentEventPushJson<'_>> = drained
            .iter()
            .map(|push| AgentEventPushJson { from_device_id: &push.from_device_id, event: &push.event, sequence: push.sequence })
            .collect();
        to_json(&json_ready)
    }

    /// `agent-events.md`'s "Delivery and replay" resume request:
    /// `AgentEventsResumeRequest { workspace_id, after_sequence }` sent over
    /// a fresh `"agent-events"`-purpose tunnel to `target_device_id`.
    /// `after_sequence <= 0` means "no prior acknowledged sequence" (a
    /// first-ever subscribe) — real sequence numbers start at 1, so this
    /// mirrors `choosh-hostd::agent_event_spool`'s own "`Some(0)` behaves
    /// identically to `None`" convention rather than needing a second,
    /// JNI-only sentinel value. Returns one of:
    ///
    /// - `{"outcome":"replayed","events":[{"sequence":...,"event":{...}},...],"latest_sequence":...}`
    /// - `{"outcome":"snapshot_required"}` — per agent-events.md, the caller
    ///   MUST refresh via `workspace.status`/`workspace.list` rather than
    ///   treat this as an error or an empty replay.
    /// - `{"error": "..."}` — not connected, or a transport-level failure.
    pub async fn agent_events_resume(&self, target_device_id: &str, workspace_id: &str, after_sequence: i64) -> String {
        let after_sequence = u64::try_from(after_sequence).ok().filter(|sequence| *sequence > 0);
        let mut guard = self.connection.lock().await;
        let Some(connection) = guard.as_mut() else {
            return error_json("not connected: call nativeConnect first");
        };
        match connection.resume_agent_events(target_device_id, workspace_id, after_sequence).await {
            Ok(AgentEventsResumeResponse::Replayed { events, latest_sequence, .. }) => {
                to_json(&json!({ "outcome": "replayed", "events": events, "latest_sequence": latest_sequence }))
            }
            Ok(AgentEventsResumeResponse::SnapshotRequired { .. }) => to_json(&json!({ "outcome": "snapshot_required" })),
            Err(error) => {
                // Same "drop a connection that just failed a call" posture
                // as list_devhosts/call_jj_rpc.
                *guard = None;
                error_json(&format!("agent_events_resume failed: {error}"))
            }
        }
    }

    pub async fn close(&self) {
        *self.connection.lock().await = None;
        // Discard anything un-polled from the connection just closed — a
        // fresh `connect` starts this queue clean rather than handing
        // Kotlin pushes that arrived under a since-closed connection. Safe
        // regardless: Kotlin's own sequence cursor is idempotent/monotonic
        // (never regresses), so even a push that *did* survive this clear
        // would just be re-delivered, never mis-applied, by the next
        // `agent_events_resume`.
        self.agent_events.lock().await.clear();
    }
}

/// Drains [`Engine::agent_events`] into whatever a live [`PhoneConnection`]'s
/// agent-event push receiver yields, for the lifetime of that receiver.
/// Deliberately decoupled from `Engine::connection`'s lock (see
/// `PhoneConnection::take_agent_event_receiver`'s doc comment) — this task
/// never contends with an in-flight RPC, and vice versa. Ends naturally
/// (the `while let` falls through) once every sender for this channel is
/// dropped, which happens when the underlying connection's background I/O
/// task ends (disconnect, `Engine::close`, or the connection being replaced
/// by a later `Engine::connect`) — no explicit cancellation needed.
async fn pump_agent_events(mut agent_event_rx: tokio::sync::mpsc::Receiver<AgentEventPush>, queue: Arc<Mutex<VecDeque<AgentEventPush>>>) {
    while let Some(push) = agent_event_rx.recv().await {
        let mut guard = queue.lock().await;
        if guard.len() >= AGENT_EVENT_QUEUE_CAPACITY {
            // See AGENT_EVENT_QUEUE_CAPACITY's doc comment: drop the
            // oldest un-polled push to make room for the newest.
            guard.pop_front();
        }
        guard.push_back(push);
    }
}

/// JSON shape for one queued push, per [`Engine::poll_agent_events`]'s doc
/// comment — borrowed fields, since this is only ever constructed
/// transiently to serialize a batch already held by value elsewhere.
#[derive(Serialize)]
struct AgentEventPushJson<'a> {
    from_device_id: &'a str,
    event: &'a choosh_protocol::relay::WireAgentEvent,
    sequence: Option<u64>,
}

/// [`read_whole_file`]'s error shape — deliberately distinct from a bare
/// `String` so each of its two callers ([`Engine::open_document`],
/// [`Engine::read_workspace_file_range`]) can apply its own
/// connection-drop/error-JSON convention without this helper having to know
/// which caller it's serving.
enum WholeFileReadError {
    /// A [`CallError`] from `call_rpc` itself — every existing method in
    /// this module drops its connection on this and reports "offline"
    /// (`open_document`) or a bare message (`read_workspace_file_range`);
    /// this variant carries just the formatted message so both call sites
    /// can keep doing exactly that.
    Transport(String),
    /// A well-formed `RpcResponse::Error` from `hostd` — a real
    /// application-level rejection (`not_found`, `bound_exceeded`,
    /// binary content, etc.), never a reason to drop the connection.
    Application { code: String, message: String },
    /// Anything else: an unexpected response variant, a `content_base64`
    /// that failed to decode, or — the case unique to this multi-request
    /// helper — the file's `revision` changing between chunks (see below).
    Other(String),
}

/// Issues repeated `workspace.file.read` calls (offset increasing by
/// `choosh_protocol::host_rpc::MAX_FILE_READ_RANGE_BYTES` each time) and
/// assembles the complete file content, because a single
/// `WorkspaceFileReadOk.content_base64` can never carry more than that
/// bound (it has to fit inside one `"rpc"`-purpose relay tunnel frame — see
/// `MAX_FILE_READ_RANGE_BYTES`'s own doc comment). Passing `range: None`
/// through to `hostd` unmodified would silently return only the file's
/// *first* chunk while still reporting the file's true `total_size` —
/// exactly the "reads `total_size` correctly but the content is truncated"
/// bug this loop exists to close. Shared by [`Engine::open_document`] (the
/// Kotlin-facing editor path) and [`Engine::read_workspace_file_range`]'s
/// own `range: None` case (the Markdown-document-render path) — the two
/// real "I want the whole file" callers in this crate; the asset-serving
/// range path deliberately does NOT go through this, since a caller-chosen
/// range means exactly the bytes it asked for, once, not "keep looping
/// until you have the whole file."
///
/// Every chunk's `revision` is a hash of the file's *entire* current
/// content as of that specific read (`fs_ops::read_file_range`'s own doc
/// comment: a ranged read still reflects the whole file so a stale write
/// can be detected against a part of the file the client never even read).
/// That means a concurrent write between this loop's chunk N and chunk N+1
/// changes the revision chunk N+1 comes back with — assembling those two
/// chunks together would silently stitch bytes from two different points in
/// time into content that was never a real, single snapshot of the file.
/// Detected here (comparing every chunk's revision against the first) and
/// reported as [`WholeFileReadError::Other`] rather than risking that.
async fn read_whole_file(
    connection: &mut PhoneConnection,
    target_device_id: &str,
    workspace_id: &str,
    path: &str,
) -> Result<(Vec<u8>, u64, String), WholeFileReadError> {
    use base64::Engine as _;

    let mut content = Vec::new();
    let mut first_revision: Option<String> = None;
    let mut offset = 0_u64;

    loop {
        let request = RpcRequest::WorkspaceFileRead {
            request_id: new_request_id(),
            workspace_id: workspace_id.to_string(),
            path: path.to_string(),
            revision: None,
            range: Some(ByteRange { offset, length: choosh_protocol::host_rpc::MAX_FILE_READ_RANGE_BYTES }),
        };
        let response = connection
            .call_rpc(target_device_id, request)
            .await
            .map_err(|error| WholeFileReadError::Transport(format!("{error}")))?;
        match response {
            RpcResponse::WorkspaceFileReadOk { content_base64, total_size, revision, .. } => {
                match &first_revision {
                    None => first_revision = Some(revision),
                    Some(seen) if *seen != revision => {
                        return Err(WholeFileReadError::Other(format!(
                            "{path} changed on disk while it was being read in chunks (revision went from {seen} to {revision}) — retry the read"
                        )));
                    }
                    Some(_) => {}
                }
                let chunk = base64::engine::general_purpose::STANDARD
                    .decode(&content_base64)
                    .map_err(|error| WholeFileReadError::Other(format!("hostd returned invalid base64: {error}")))?;
                let chunk_len = chunk.len() as u64;
                content.extend_from_slice(&chunk);
                offset += chunk_len;
                // `chunk_len == 0` guards against an empty response that
                // never advances `offset` (which would otherwise spin
                // forever) — a well-behaved `hostd` never returns one short
                // of `total_size`, but this loop must not trust that blindly.
                if offset >= total_size || chunk_len == 0 {
                    return Ok((content, total_size, first_revision.unwrap_or_default()));
                }
            }
            RpcResponse::Error { code, message, .. } => return Err(WholeFileReadError::Application { code, message }),
            other => return Err(WholeFileReadError::Other(format!("unexpected response to workspace.file.read: {other:?}"))),
        }
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

    // --- not-connected error paths for every remaining method -------------
    //
    // Every `workspace_*`/`item_list` method funnels "not connected" through
    // the shared `call_jj_rpc` helper (already proven once by
    // `list_devhosts_before_connect_is_a_typed_error` for its own,
    // structurally identical not-connected path) into this module's shared
    // `{"error": ...}` shape; `open_document`/`save_document` instead go
    // through the distinct `{"type":"offline", ...}` shape;
    // `open_pty_tunnel`/`open_web_tunnel`/`read_workspace_file_range` return
    // a bare `Result<_, String>`; `register_fcm_token` returns `false`. Each
    // is worth its own test rather than assuming the pattern holds, since
    // that's exactly the kind of "every method wired the same way" claim a
    // one-off copy-paste mistake in a single method would otherwise slip
    // past unnoticed.
    fn unconnected_engine() -> Engine {
        Engine::new("http://unused".to_string(), "ws://unused".to_string())
    }

    fn assert_error_json_mentions_not_connected(body: &str) {
        let parsed: Value = serde_json::from_str(body).unwrap();
        assert!(parsed["error"].as_str().unwrap().contains("not connected"), "{body}");
    }

    fn assert_offline_json_mentions_not_connected(body: &str) {
        let parsed: Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["type"], "offline", "{body}");
        assert!(parsed["message"].as_str().unwrap().contains("not connected"), "{body}");
    }

    #[tokio::test]
    async fn register_fcm_token_before_connect_returns_false() {
        assert!(!unconnected_engine().register_fcm_token("tok").await);
    }

    #[tokio::test]
    async fn workspace_diff_before_connect_is_a_typed_error() {
        assert_error_json_mentions_not_connected(&unconnected_engine().workspace_diff("dev-1", "ws-1", "", "").await);
    }

    #[tokio::test]
    async fn workspace_log_before_connect_is_a_typed_error() {
        assert_error_json_mentions_not_connected(&unconnected_engine().workspace_log("dev-1", "ws-1", "", 50).await);
    }

    #[tokio::test]
    async fn workspace_op_log_before_connect_is_a_typed_error() {
        assert_error_json_mentions_not_connected(&unconnected_engine().workspace_op_log("dev-1", "ws-1", 50).await);
    }

    #[tokio::test]
    async fn workspace_op_undo_before_connect_is_a_typed_error() {
        assert_error_json_mentions_not_connected(&unconnected_engine().workspace_op_undo("dev-1", "ws-1", "op-1").await);
    }

    #[tokio::test]
    async fn workspace_op_restore_before_connect_is_a_typed_error() {
        assert_error_json_mentions_not_connected(&unconnected_engine().workspace_op_restore("dev-1", "ws-1", "op-1").await);
    }

    #[tokio::test]
    async fn workspace_status_before_connect_is_a_typed_error() {
        assert_error_json_mentions_not_connected(&unconnected_engine().workspace_status("dev-1", "ws-1").await);
    }

    #[tokio::test]
    async fn item_list_before_connect_is_a_typed_error() {
        assert_error_json_mentions_not_connected(&unconnected_engine().item_list("dev-1", "ws-1").await);
    }

    #[tokio::test]
    async fn project_list_before_connect_is_a_typed_error() {
        assert_error_json_mentions_not_connected(&unconnected_engine().project_list("dev-1").await);
    }

    #[tokio::test]
    async fn project_set_primary_workspace_before_connect_is_a_typed_error() {
        assert_error_json_mentions_not_connected(&unconnected_engine().project_set_primary_workspace("dev-1", "proj-1", "ws-1").await);
    }

    #[tokio::test]
    async fn open_document_before_connect_is_offline_not_error() {
        assert_offline_json_mentions_not_connected(&unconnected_engine().open_document("dev-1", "ws-1", "a.txt").await);
    }

    #[tokio::test]
    async fn save_document_before_connect_is_offline_not_error() {
        assert_offline_json_mentions_not_connected(&unconnected_engine().save_document("dev-1", "ws-1", "a.txt", "rev-1", "").await);
    }

    #[tokio::test]
    async fn open_pty_tunnel_before_connect_is_a_typed_error() {
        // `PtyTunnelHandle` (the `Ok` payload) carries no `Debug` impl, so
        // `unwrap_err()` isn't available here — match instead.
        let Err(error) = unconnected_engine().open_pty_tunnel("dev-1", "item-1").await else { panic!("expected an error") };
        assert!(error.contains("not connected"), "{error}");
    }

    #[tokio::test]
    async fn open_web_tunnel_before_connect_is_a_typed_error() {
        let Err(error) = unconnected_engine().open_web_tunnel("dev-1", "item-1").await else { panic!("expected an error") };
        assert!(error.contains("not connected"), "{error}");
    }

    #[tokio::test]
    async fn read_workspace_file_range_before_connect_is_a_typed_error() {
        let error = unconnected_engine().read_workspace_file_range("dev-1", "ws-1", "a.txt", None).await.unwrap_err();
        assert!(error.contains("not connected"), "{error}");
    }

    #[tokio::test]
    async fn agent_events_resume_before_connect_is_a_typed_error() {
        assert_error_json_mentions_not_connected(&unconnected_engine().agent_events_resume("dev-1", "ws-1", 0).await);
    }

    /// [`Engine::poll_agent_events`] never errors, connected or not — an
    /// unconnected engine simply has nothing queued yet.
    #[tokio::test]
    async fn poll_agent_events_before_connect_is_an_empty_array_not_an_error() {
        let body = unconnected_engine().poll_agent_events().await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed, serde_json::json!([]), "{body}");
    }

    // --- shared fake-relayd test harness -----------------------------------
    //
    // Everything below drives a real `PhoneConnection` against a real,
    // in-process fake `relayd` over a real WebSocket — the same shape
    // `choosh-android-transport`'s own test suite uses for its
    // `call_rpc_round_trips_through_a_relayed_rpc_tunnel` test, hoisted here
    // as shared helpers (rather than each test hand-rolling its own
    // send/recv closures, which `connect_then_list_devhosts_round_trips_...`
    // used to do before this pass) since every RPC-shaped test below needs
    // the identical hello/auth/open-tunnel prefix.
    type TestWs = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    async fn bind_fake_relayd_ws() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, format!("ws://{addr}/connect"))
    }

    async fn send_control<T: Serialize>(ws: &mut TestWs, value: &T) {
        use futures_util::SinkExt;
        let mut payload = vec![choosh_protocol::relay::FRAME_CLASS_CONTROL];
        payload.extend(serde_json::to_vec(value).unwrap());
        let wire = choosh_protocol::framing::encode_frame(&payload, choosh_protocol::relay::MAX_CONTROL_FRAME_BYTES).unwrap();
        ws.send(tokio_tungstenite::tungstenite::Message::Binary(wire.into())).await.unwrap();
    }

    async fn recv_control<T: serde::de::DeserializeOwned>(ws: &mut TestWs) -> T {
        use futures_util::StreamExt;
        let Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(bytes))) = ws.next().await else {
            panic!("expected a binary frame")
        };
        let mut decoder = choosh_protocol::framing::FrameDecoder::new(
            choosh_protocol::framing::FrameLimits::new(choosh_protocol::relay::MAX_CONTROL_FRAME_BYTES, 4).unwrap(),
        );
        let frames = decoder.feed(&bytes).unwrap();
        let (_class, body) = frames[0].split_first().unwrap();
        serde_json::from_slice(body).unwrap()
    }

    async fn send_tunnel_frame(ws: &mut TestWs, tunnel_id: [u8; choosh_protocol::relay::TUNNEL_ID_BYTES], payload: &[u8]) {
        use futures_util::SinkExt;
        let wire = choosh_protocol::framing::encode_frame(
            &choosh_protocol::relay::encode_tunnel_frame(tunnel_id, payload),
            choosh_protocol::relay::MAX_TUNNEL_FRAME_BYTES,
        )
        .unwrap();
        ws.send(tokio_tungstenite::tungstenite::Message::Binary(wire.into())).await.unwrap();
    }

    async fn recv_tunnel_frame(ws: &mut TestWs) -> ([u8; choosh_protocol::relay::TUNNEL_ID_BYTES], Vec<u8>) {
        use futures_util::StreamExt;
        let Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(bytes))) = ws.next().await else {
            panic!("expected a binary frame")
        };
        let mut decoder = choosh_protocol::framing::FrameDecoder::new(
            choosh_protocol::framing::FrameLimits::new(choosh_protocol::relay::MAX_CONTROL_FRAME_BYTES, 4).unwrap(),
        );
        let frames = decoder.feed(&bytes).unwrap();
        let (id, payload) = choosh_protocol::relay::decode_tunnel_frame(&frames[0]).expect("expected a tunnel frame");
        (id, payload.to_vec())
    }

    async fn authenticate_fake_relayd(ws: &mut TestWs) {
        use choosh_protocol::relay::{AuthOk, AuthResult, ClientAuth, IdentityClass, ServerHello};
        send_control(ws, &ServerHello { nonce: "n".to_string() }).await;
        let _auth: ClientAuth = recv_control(ws).await;
        send_control(ws, &AuthResult::Ok(AuthOk { identity_class: IdentityClass::Phone, device_id: "phone-1".to_string() })).await;
    }

    #[tokio::test]
    async fn connect_then_list_devhosts_round_trips_over_a_real_websocket() {
        use choosh_protocol::relay::{ConnectionState, ControlRequest, ControlResponse, DevHostPresence};

        let (listener, ws_url) = bind_fake_relayd_ws().await;
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            let request: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::ListDevhosts { request_id } = request else { panic!("expected list-devhosts") };
            send_control(
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
        });

        let engine = Engine::new("http://unused".to_string(), ws_url);
        assert!(engine.connect("good-cred").await, "connect should succeed against a well-behaved fake relayd");
        let body = engine.list_devhosts().await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed[0]["alias"], "build-box");
    }

    /// Drives a full RPC round trip (hello/auth, open-tunnel, one tunnel
    /// frame each way, close-tunnel) exactly like
    /// `connect_then_list_devhosts_round_trips_over_a_real_websocket` does
    /// for the control-only `list_devhosts` call — the shared prefix every
    /// `workspace_*` test below needs. `respond` gets the decoded
    /// [`RpcRequest`] and returns the exact [`RpcResponse`] the fake relayd
    /// sends back over the tunnel.
    async fn connect_and_serve_one_rpc(respond: impl FnOnce(RpcRequest) -> RpcResponse + Send + 'static) -> Engine {
        use choosh_protocol::relay::{ControlRequest, ControlResponse, FRAME_CLASS_CONTROL as TUNNEL_INNER_CONTROL, encode_tunnel_id_hex};

        let (listener, ws_url) = bind_fake_relayd_ws().await;
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            let open: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::OpenTunnel { request_id, purpose, .. } = open else { panic!("expected open-tunnel") };
            assert_eq!(purpose, "rpc");
            let tunnel_id: [u8; choosh_protocol::relay::TUNNEL_ID_BYTES] = [1, 2, 3, 4, 5, 6, 7, 8];
            send_control(&mut ws, &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) }).await;

            let (id, payload) = recv_tunnel_frame(&mut ws).await;
            assert_eq!(id, tunnel_id);
            let (inner_class, inner_body) = payload.split_first().unwrap();
            assert_eq!(*inner_class, TUNNEL_INNER_CONTROL);
            let request: RpcRequest = serde_json::from_slice(inner_body).unwrap();
            let response = respond(request);

            let mut response_payload = vec![TUNNEL_INNER_CONTROL];
            response_payload.extend(serde_json::to_vec(&response).unwrap());
            send_tunnel_frame(&mut ws, tunnel_id, &response_payload).await;

            // `call_rpc` closes its fresh, one-shot tunnel proactively.
            let (closed_id, closed_payload) = recv_tunnel_frame(&mut ws).await;
            assert_eq!(closed_id, tunnel_id);
            assert!(closed_payload.is_empty());
        });

        let engine = Engine::new("http://unused".to_string(), ws_url);
        assert!(engine.connect("good-cred").await, "connect should succeed against a well-behaved fake relayd");
        engine
    }

    /// Like `connect_and_serve_one_rpc`, but drives a whole *sequence* of
    /// `expected_requests` RPC round trips, each over its own fresh tunnel
    /// (`call_rpc`'s documented fresh-tunnel-per-call discipline) —
    /// needed for [`read_whole_file`]'s multi-chunk tests, which must
    /// observe (and correctly assemble) more than one `workspace.file.read`
    /// response. `respond` is called once per request, in arrival order.
    async fn connect_and_serve_rpc_sequence(
        expected_requests: usize,
        mut respond: impl FnMut(RpcRequest) -> RpcResponse + Send + 'static,
    ) -> Engine {
        use choosh_protocol::relay::{ControlRequest, ControlResponse, FRAME_CLASS_CONTROL as TUNNEL_INNER_CONTROL, encode_tunnel_id_hex};

        let (listener, ws_url) = bind_fake_relayd_ws().await;
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            for index in 0..expected_requests {
                let open: ControlRequest = recv_control(&mut ws).await;
                let ControlRequest::OpenTunnel { request_id, purpose, .. } = open else { panic!("expected open-tunnel") };
                assert_eq!(purpose, "rpc");
                let tunnel_id: [u8; choosh_protocol::relay::TUNNEL_ID_BYTES] =
                    [1, 2, 3, 4, 5, 6, 7, u8::try_from(index + 1).unwrap()];
                send_control(&mut ws, &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) })
                    .await;

                let (id, payload) = recv_tunnel_frame(&mut ws).await;
                assert_eq!(id, tunnel_id);
                let (inner_class, inner_body) = payload.split_first().unwrap();
                assert_eq!(*inner_class, TUNNEL_INNER_CONTROL);
                let request: RpcRequest = serde_json::from_slice(inner_body).unwrap();
                let response = respond(request);

                let mut response_payload = vec![TUNNEL_INNER_CONTROL];
                response_payload.extend(serde_json::to_vec(&response).unwrap());
                send_tunnel_frame(&mut ws, tunnel_id, &response_payload).await;

                let (closed_id, closed_payload) = recv_tunnel_frame(&mut ws).await;
                assert_eq!(closed_id, tunnel_id);
                assert!(closed_payload.is_empty());
            }
        });

        let engine = Engine::new("http://unused".to_string(), ws_url);
        assert!(engine.connect("good-cred").await, "connect should succeed against a well-behaved fake relayd");
        engine
    }

    /// The load-bearing test for this pass: `open_document` must fetch a
    /// file bigger than one `workspace.file.read` response can carry
    /// (`choosh_protocol::host_rpc::MAX_FILE_READ_RANGE_BYTES`) by issuing
    /// *multiple* chunked requests and assembling them, not silently
    /// returning just the first chunk while still reporting the file's true
    /// `total_size` (the exact bug `read_whole_file` exists to close — see
    /// its doc comment). The fake relayd below is wired via
    /// `connect_and_serve_rpc_sequence(2, ...)`: if `open_document` only
    /// issued one request (the old, buggy behavior), the second `.accept`-
    /// side expectation would simply never be driven and this test would
    /// hang past its `tokio::test` default rather than silently pass — so a
    /// regression back to single-shot reads fails loudly, not quietly.
    #[tokio::test]
    async fn open_document_assembles_multiple_chunks_for_a_file_larger_than_one_rpc_response() {
        use base64::Engine as _;

        let bound = choosh_protocol::host_rpc::MAX_FILE_READ_RANGE_BYTES;
        let total_len = usize::try_from(bound).unwrap() + 1000;
        // Not all-zero: a byte pattern that would fail an equality check if
        // any chunk landed at the wrong offset or got truncated/duplicated.
        let content: Vec<u8> = (0..total_len).map(|i| u8::try_from(i % 251).unwrap()).collect();
        let expected_revision = "rev-fixed".to_string();

        let content_for_server = content.clone();
        let revision_for_server = expected_revision.clone();
        let engine = connect_and_serve_rpc_sequence(2, move |request| {
            let RpcRequest::WorkspaceFileRead { request_id, range, .. } = request else { panic!("expected workspace.file.read") };
            let range = range.expect("open_document's chunked reader must always pass an explicit range");
            let start = usize::try_from(range.offset).unwrap();
            let end = (start + usize::try_from(range.length).unwrap()).min(content_for_server.len());
            assert!(start < end, "must never issue a request for an empty/out-of-bounds range");
            RpcResponse::WorkspaceFileReadOk {
                request_id,
                content_base64: base64::engine::general_purpose::STANDARD.encode(&content_for_server[start..end]),
                total_size: content_for_server.len() as u64,
                revision: revision_for_server.clone(),
            }
        })
        .await;

        let body = tokio::time::timeout(std::time::Duration::from_secs(10), engine.open_document("dev-1", "ws-1", "big.txt"))
            .await
            .expect("open_document must not hang waiting on a request the fake relayd never receives");
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["type"], "ok", "{body}");
        assert_eq!(parsed["revision"], expected_revision, "{body}");
        assert_eq!(parsed["total_size"], content.len() as u64, "{body}");

        let returned = base64::engine::general_purpose::STANDARD.decode(parsed["content_base64"].as_str().unwrap()).unwrap();
        assert_eq!(returned.len(), content.len(), "content must be the full file, not truncated to the first chunk");
        assert_eq!(returned, content, "assembled content must be byte-identical to the source across the chunk boundary");
    }

    /// [`read_workspace_file_range`]'s `range: None` case (the Markdown
    /// gateway's `/doc` fetch) must chunk exactly like `open_document`
    /// does — same underlying [`read_whole_file`] helper, exercised through
    /// the other public entry point so a future refactor that wires one but
    /// not the other back up to a single-shot request gets caught here too.
    #[tokio::test]
    async fn read_workspace_file_range_with_no_range_assembles_multiple_chunks() {
        let bound = choosh_protocol::host_rpc::MAX_FILE_READ_RANGE_BYTES;
        let total_len = usize::try_from(bound).unwrap() + 1000;
        let content: Vec<u8> = (0..total_len).map(|i| u8::try_from((i * 7) % 251).unwrap()).collect();

        let content_for_server = content.clone();
        let engine = connect_and_serve_rpc_sequence(2, move |request| {
            use base64::Engine as _;
            let RpcRequest::WorkspaceFileRead { request_id, range, .. } = request else { panic!("expected workspace.file.read") };
            let range = range.expect("read_workspace_file_range(None) must still pass an explicit range downstream");
            let start = usize::try_from(range.offset).unwrap();
            let end = (start + usize::try_from(range.length).unwrap()).min(content_for_server.len());
            RpcResponse::WorkspaceFileReadOk {
                request_id,
                content_base64: base64::engine::general_purpose::STANDARD.encode(&content_for_server[start..end]),
                total_size: content_for_server.len() as u64,
                revision: "rev-fixed".to_string(),
            }
        })
        .await;

        let (returned, total_size) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            engine.read_workspace_file_range("dev-1", "ws-1", "big.md", None),
        )
        .await
        .expect("must not hang")
        .unwrap();
        assert_eq!(total_size, content.len() as u64);
        assert_eq!(returned, content, "assembled content must be byte-identical to the source across the chunk boundary");
    }

    /// The counterpart to the two chunking tests above: an *explicit* range
    /// (the `/asset` route's own use, per [`Self::read_workspace_file_range`]'s
    /// doc comment) must stay a single request even when its length happens
    /// to reach all the way to `total_size` — this path must never
    /// second-guess a caller-chosen range into "keep looping until whole
    /// file," which would break asset byte-range semantics (e.g. an HTTP
    /// `Range` request that's deliberately smaller than the whole file).
    /// `connect_and_serve_rpc_sequence(1, ...)` only ever drives one
    /// request/response pair — if `read_workspace_file_range` tried to
    /// issue a second one, the `timeout` below is what turns that into a
    /// clean test failure instead of a hang.
    #[tokio::test]
    async fn read_workspace_file_range_with_an_explicit_range_issues_exactly_one_request() {
        let content = vec![9_u8; 10];
        let content_for_server = content.clone();
        let engine = connect_and_serve_rpc_sequence(1, move |request| {
            use base64::Engine as _;
            let RpcRequest::WorkspaceFileRead { request_id, range, .. } = request else { panic!("expected workspace.file.read") };
            let range = range.expect("an explicit-range call must pass that range through unmodified");
            assert_eq!((range.offset, range.length), (0, 10));
            RpcResponse::WorkspaceFileReadOk {
                request_id,
                content_base64: base64::engine::general_purpose::STANDARD.encode(&content_for_server),
                total_size: content_for_server.len() as u64,
                revision: "rev-fixed".to_string(),
            }
        })
        .await;

        let (returned, total_size) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            engine.read_workspace_file_range("dev-1", "ws-1", "small.png", Some((0, 10))),
        )
        .await
        .expect("must not hang — a second request here would mean this path is wrongly chunking an explicit range")
        .unwrap();
        assert_eq!(total_size, 10);
        assert_eq!(returned, content);
    }

    /// The revision-stability guard: if the file changes on disk between
    /// this loop's chunk 1 and chunk 2 (a concurrent `workspace.file.write`
    /// from another session), `read_whole_file`'s own doc comment says
    /// assembling those chunks would silently stitch bytes from two
    /// different points in time. This proves that's caught and reported as
    /// an application-level error rather than ever reaching the caller as a
    /// falsely-successful `{"type":"ok", ...}`.
    #[tokio::test]
    async fn open_document_reports_an_error_if_the_file_changes_between_chunks_instead_of_silently_stitching_them() {
        let bound = choosh_protocol::host_rpc::MAX_FILE_READ_RANGE_BYTES;
        let total_len = bound + 10;
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let call_count_for_server = std::sync::Arc::clone(&call_count);
        let engine = connect_and_serve_rpc_sequence(2, move |request| {
            use base64::Engine as _;
            let RpcRequest::WorkspaceFileRead { request_id, .. } = request else { panic!("expected workspace.file.read") };
            let call = call_count_for_server.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let chunk_len: usize = if call == 0 { usize::try_from(bound).unwrap() } else { 10 };
            RpcResponse::WorkspaceFileReadOk {
                request_id,
                content_base64: base64::engine::general_purpose::STANDARD.encode(vec![0_u8; chunk_len]),
                total_size: total_len,
                revision: if call == 0 { "rev-1".to_string() } else { "rev-2".to_string() },
            }
        })
        .await;

        let body = tokio::time::timeout(std::time::Duration::from_secs(10), engine.open_document("dev-1", "ws-1", "big.txt"))
            .await
            .expect("must not hang");
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["type"], "error", "{body}");
        assert!(parsed["message"].as_str().unwrap().contains("changed on disk"), "{body}");
    }

    /// The "wrong response variant" error path: `hostd` (or, here, the fake
    /// relayd standing in for it) answers a `workspace.diff` request with a
    /// well-formed but *different* RPC response — a genuine possibility if
    /// the two ends' request/response pairing ever desyncs — and
    /// `workspace_diff` must report that as its own `{"error": ...}` shape
    /// (via `unexpected_or_error_json`) rather than panicking or silently
    /// misinterpreting the payload as a diff.
    #[tokio::test]
    async fn workspace_diff_reports_an_unexpected_response_variant() {
        let engine = connect_and_serve_one_rpc(|request| {
            let RpcRequest::WorkspaceDiff { request_id, .. } = request else { panic!("expected workspace.diff") };
            // Deliberately the wrong variant: an op-log response answering
            // a diff request.
            RpcResponse::WorkspaceOpLogOk { request_id, operations: vec![] }
        })
        .await;

        let body = engine.workspace_diff("dev-1", "ws-1", "", "").await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert!(parsed["error"].as_str().unwrap().contains("unexpected response to workspace.diff"), "{body}");
    }

    /// The application-level `hostd` rejection path: a well-formed
    /// [`RpcResponse::Error`] must surface as this module's shared
    /// `{"error": "<code>: <message>"}` shape, not the generic "unexpected
    /// response" fallback — matching `unexpected_or_error_json`'s two
    /// distinct arms.
    #[tokio::test]
    async fn workspace_log_reports_a_hostd_side_rpc_error() {
        let engine = connect_and_serve_one_rpc(|request| {
            let RpcRequest::WorkspaceLog { request_id, .. } = request else { panic!("expected workspace.log") };
            RpcResponse::Error { request_id, code: "not_found".to_string(), message: "no such workspace".to_string() }
        })
        .await;

        let body = engine.workspace_log("dev-1", "ws-1", "", 50).await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["error"], "not_found: no such workspace", "{body}");
    }

    /// `project.list`'s happy path, round-tripped through a real (fake)
    /// relayd/tunnel exactly like `connect_then_list_devhosts_...` does for
    /// `list_devhosts` — proves the bare-JSON-array shape and that `active`
    /// (a `hostd`-computed value this crate never touches) survives
    /// untouched.
    #[tokio::test]
    async fn project_list_round_trips_projects_with_active_flag() {
        use choosh_protocol::host_rpc::ProjectSummary;

        let engine = connect_and_serve_one_rpc(|request| {
            let RpcRequest::ProjectList { request_id } = request else { panic!("expected project.list") };
            RpcResponse::ProjectListOk {
                request_id,
                projects: vec![
                    ProjectSummary {
                        project_id: "proj-1".to_string(),
                        name: "app".to_string(),
                        primary_workspace_id: Some("ws-1".to_string()),
                        active: true,
                    },
                    ProjectSummary { project_id: "proj-2".to_string(), name: "idle".to_string(), primary_workspace_id: None, active: false },
                ],
            }
        })
        .await;

        let body = engine.project_list("dev-1").await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed[0]["project_id"], "proj-1");
        assert_eq!(parsed[0]["active"], true);
        assert_eq!(parsed[1]["project_id"], "proj-2");
        assert_eq!(parsed[1]["active"], false);
        assert!(parsed[1]["primary_workspace_id"].is_null());
    }

    /// `project.set_primary_workspace`'s happy path.
    #[tokio::test]
    async fn project_set_primary_workspace_round_trips_success() {
        let engine = connect_and_serve_one_rpc(|request| {
            let RpcRequest::ProjectSetPrimaryWorkspace { request_id, project_id, workspace_id } = request else {
                panic!("expected project.set_primary_workspace")
            };
            assert_eq!(project_id, "proj-1");
            assert_eq!(workspace_id, "ws-2");
            RpcResponse::ProjectSetPrimaryWorkspaceOk { request_id }
        })
        .await;

        let body = engine.project_set_primary_workspace("dev-1", "proj-1", "ws-2").await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["ok"], true);
    }

    /// `hostd`'s `invalid_argument` rejection (`workspace_id` not
    /// belonging to `project_id`, per `host-rpc.md`) surfaces as this
    /// module's shared `{"error": "<code>: <message>"}` shape, same as
    /// `workspace_log_reports_a_hostd_side_rpc_error`.
    #[tokio::test]
    async fn project_set_primary_workspace_reports_a_hostd_side_rejection() {
        let engine = connect_and_serve_one_rpc(|request| {
            let RpcRequest::ProjectSetPrimaryWorkspace { request_id, .. } = request else {
                panic!("expected project.set_primary_workspace")
            };
            RpcResponse::Error { request_id, code: "invalid_argument".to_string(), message: "workspace_id does not belong to project_id".to_string() }
        })
        .await;

        let body = engine.project_set_primary_workspace("dev-1", "proj-1", "ws-other-project").await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["error"], "invalid_argument: workspace_id does not belong to project_id", "{body}");
    }

    /// Transport failure mid-call: the fake relayd disappears (drops the
    /// socket) right after authenticating, before ever answering the
    /// `open-tunnel` request `open_document`'s call triggers — the
    /// `CallError::Transport` path `Engine::open_document` maps to its
    /// `{"type":"offline", ...}` shape, per that method's own doc comment.
    #[tokio::test]
    async fn open_document_reports_offline_on_a_transport_failure() {
        let (listener, ws_url) = bind_fake_relayd_ws().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;
            // No response to the open-tunnel request that follows — just
            // drop the connection, simulating relayd vanishing mid-call.
        });

        let engine = Engine::new("http://unused".to_string(), ws_url);
        assert!(engine.connect("good-cred").await);
        server.await.unwrap();

        let body = engine.open_document("dev-1", "ws-1", "a.txt").await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["type"], "offline", "{body}");
    }

    // --- agent-events.md subscription tests --------------------------------
    //
    // The three scenarios this pass's verification section calls for by
    // name: a live push surfacing through `poll_agent_events`, a
    // `Replayed` resume applying in order with no duplicates, and a
    // `SnapshotRequired` resume being reported distinctly rather than
    // silently doing nothing.

    /// Polls `poll_agent_events` until it returns a non-empty array or a
    /// generous deadline elapses — the live push arrives on a background
    /// pump task ([`pump_agent_events`]) whose exact scheduling relative to
    /// this test's own polling isn't guaranteed, so a single immediate poll
    /// would be flaky. Panics (via `expect`) rather than returning empty on
    /// timeout, so a regression that stops delivering pushes fails loudly.
    async fn poll_until_nonempty(engine: &Engine) -> Value {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let body = engine.poll_agent_events().await;
                let parsed: Value = serde_json::from_str(&body).unwrap();
                if parsed.as_array().is_some_and(|array| !array.is_empty()) {
                    return parsed;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("poll_agent_events must eventually surface the live push")
    }

    /// A fake relayd pushes a live `ServerPush::AgentEvent` with no request
    /// from this side at all — proves the bridge surfaces it through
    /// `poll_agent_events`, and that a second poll comes back empty (the
    /// queue is drained, not merely peeked).
    #[tokio::test]
    async fn live_agent_event_push_is_surfaced_through_poll_agent_events() {
        let (listener, ws_url) = bind_fake_relayd_ws().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;
            send_control(
                &mut ws,
                &choosh_protocol::relay::ServerPush::AgentEvent {
                    from_device_id: "dev-1".to_string(),
                    event: choosh_protocol::relay::WireAgentEvent::TurnCompleted {
                        workspace_id: "ws-1".to_string(),
                        item_id: "item-1".to_string(),
                    },
                    sequence: Some(9),
                },
            )
            .await;
            // Keep the socket open past the push — dropping it immediately
            // could race the pump task's read against the test's poll loop
            // on some schedulers.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        let engine = Engine::new("http://unused".to_string(), ws_url);
        assert!(engine.connect("good-cred").await);

        let events = poll_until_nonempty(&engine).await;
        let events = events.as_array().unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["from_device_id"], "dev-1");
        assert_eq!(events[0]["sequence"], 9);
        assert_eq!(events[0]["event"]["kind"], "turn_completed");
        assert_eq!(events[0]["event"]["workspace_id"], "ws-1");

        let second_poll = engine.poll_agent_events().await;
        assert_eq!(serde_json::from_str::<Value>(&second_poll).unwrap(), serde_json::json!([]), "{second_poll}");
        server.await.unwrap();
    }

    /// A fake relayd answers `AgentEventsResumeRequest` with `Replayed`
    /// carrying several events — proves they come back through
    /// `agent_events_resume` in order, with no duplicates and no gaps
    /// (mirroring exactly what the fake relayd sent).
    #[tokio::test]
    async fn agent_events_resume_applies_a_replayed_batch_in_order_with_no_duplicates() {
        use choosh_protocol::relay::{
            AgentEventsResumeRequest, AgentEventsResumeResponse, ControlRequest, ControlResponse, SequencedAgentEvent,
            WireAgentEvent, WireAgentStatus, encode_tunnel_id_hex,
        };

        let (listener, ws_url) = bind_fake_relayd_ws().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            let open: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::OpenTunnel { request_id, purpose, .. } = open else { panic!("expected open-tunnel") };
            assert_eq!(purpose, "agent-events");
            let tunnel_id: [u8; choosh_protocol::relay::TUNNEL_ID_BYTES] = [3, 3, 3, 3, 3, 3, 3, 3];
            send_control(&mut ws, &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) }).await;

            let (id, payload) = recv_tunnel_frame(&mut ws).await;
            assert_eq!(id, tunnel_id);
            let (_inner_class, inner_body) = payload.split_first().unwrap();
            let request: AgentEventsResumeRequest = serde_json::from_slice(inner_body).unwrap();
            assert_eq!(request.workspace_id, "ws-1");
            assert_eq!(request.after_sequence, Some(5));

            let response = AgentEventsResumeResponse::Replayed {
                request_id: request.request_id,
                events: vec![
                    SequencedAgentEvent {
                        sequence: 6,
                        event: WireAgentEvent::AgentStatus {
                            workspace_id: "ws-1".to_string(),
                            item_id: "item-1".to_string(),
                            status: WireAgentStatus::Waiting,
                        },
                    },
                    SequencedAgentEvent {
                        sequence: 7,
                        event: WireAgentEvent::InputRequired {
                            workspace_id: "ws-1".to_string(),
                            item_id: "item-1".to_string(),
                            reason: choosh_protocol::relay::WireInputReason::NextPrompt,
                        },
                    },
                ],
                latest_sequence: 7,
            };
            let mut response_payload = vec![choosh_protocol::relay::FRAME_CLASS_CONTROL];
            response_payload.extend(serde_json::to_vec(&response).unwrap());
            send_tunnel_frame(&mut ws, tunnel_id, &response_payload).await;
            let _ = recv_tunnel_frame(&mut ws).await; // proactive close, per resume_agent_events' fresh-tunnel-per-call contract
        });

        let engine = Engine::new("http://unused".to_string(), ws_url);
        assert!(engine.connect("good-cred").await);
        let body = engine.agent_events_resume("dev-1", "ws-1", 5).await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["outcome"], "replayed", "{body}");
        assert_eq!(parsed["latest_sequence"], 7, "{body}");
        let events = parsed["events"].as_array().unwrap();
        let sequences: Vec<u64> = events.iter().map(|event| event["sequence"].as_u64().unwrap()).collect();
        assert_eq!(sequences, vec![6, 7], "events must be applied oldest-first with no gaps or duplicates: {body}");
        assert_eq!(events[0]["event"]["kind"], "agent_status");
        assert_eq!(events[1]["event"]["kind"], "input_required");
        server.await.unwrap();
    }

    /// A fake relayd answers `AgentEventsResumeRequest` with
    /// `SnapshotRequired` — proves this comes back as its own, distinct
    /// outcome the caller (Kotlin) can branch on to trigger a full
    /// `workspace.status`/`workspace.list` refresh, per agent-events.md,
    /// rather than being swallowed as an empty/no-op replay.
    #[tokio::test]
    async fn agent_events_resume_reports_snapshot_required_distinctly() {
        use choosh_protocol::relay::{AgentEventsResumeRequest, AgentEventsResumeResponse, ControlRequest, ControlResponse, encode_tunnel_id_hex};

        let (listener, ws_url) = bind_fake_relayd_ws().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            let open: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::OpenTunnel { request_id, purpose, .. } = open else { panic!("expected open-tunnel") };
            assert_eq!(purpose, "agent-events");
            let tunnel_id: [u8; choosh_protocol::relay::TUNNEL_ID_BYTES] = [4, 4, 4, 4, 4, 4, 4, 4];
            send_control(&mut ws, &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) }).await;

            let (_id, payload) = recv_tunnel_frame(&mut ws).await;
            let (_inner_class, inner_body) = payload.split_first().unwrap();
            let request: AgentEventsResumeRequest = serde_json::from_slice(inner_body).unwrap();

            let response =
                AgentEventsResumeResponse::SnapshotRequired { request_id: request.request_id, workspace_id: request.workspace_id };
            let mut response_payload = vec![choosh_protocol::relay::FRAME_CLASS_CONTROL];
            response_payload.extend(serde_json::to_vec(&response).unwrap());
            send_tunnel_frame(&mut ws, tunnel_id, &response_payload).await;
            let _ = recv_tunnel_frame(&mut ws).await;
        });

        let engine = Engine::new("http://unused".to_string(), ws_url);
        assert!(engine.connect("good-cred").await);
        // A very old/unknown sequence — this is exactly the shape a real
        // devhost spool would answer with snapshot_required for.
        let body = engine.agent_events_resume("dev-1", "ws-1", 999_999).await;
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed, serde_json::json!({ "outcome": "snapshot_required" }), "{body}");
        server.await.unwrap();
    }

    /// `after_sequence <= 0` (Kotlin's "no prior ack" convention, per
    /// [`Engine::agent_events_resume`]'s doc comment) must reach the wire as
    /// `None`, matching `choosh-hostd::agent_event_spool`'s own "`Some(0)`
    /// behaves like `None`" treatment — proves the JNI-facing sentinel
    /// translation actually happens rather than sending a bogus `Some(0)`/
    /// `Some(-1)` a real devhost spool was never designed to receive.
    #[tokio::test]
    async fn agent_events_resume_maps_zero_and_negative_after_sequence_to_none() {
        use choosh_protocol::relay::{AgentEventsResumeRequest, AgentEventsResumeResponse, ControlRequest, ControlResponse, encode_tunnel_id_hex};

        let (listener, ws_url) = bind_fake_relayd_ws().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            for _ in 0..2 {
                let open: ControlRequest = recv_control(&mut ws).await;
                let ControlRequest::OpenTunnel { request_id, purpose, .. } = open else { panic!("expected open-tunnel") };
                assert_eq!(purpose, "agent-events");
                let tunnel_id: [u8; choosh_protocol::relay::TUNNEL_ID_BYTES] = [5, 5, 5, 5, 5, 5, 5, 5];
                send_control(&mut ws, &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) }).await;

                let (_id, payload) = recv_tunnel_frame(&mut ws).await;
                let (_inner_class, inner_body) = payload.split_first().unwrap();
                let request: AgentEventsResumeRequest = serde_json::from_slice(inner_body).unwrap();
                assert_eq!(request.after_sequence, None, "expected None on the wire, got {:?}", request.after_sequence);

                let response = AgentEventsResumeResponse::Replayed { request_id: request.request_id, events: vec![], latest_sequence: 0 };
                let mut response_payload = vec![choosh_protocol::relay::FRAME_CLASS_CONTROL];
                response_payload.extend(serde_json::to_vec(&response).unwrap());
                send_tunnel_frame(&mut ws, tunnel_id, &response_payload).await;
                let _ = recv_tunnel_frame(&mut ws).await;
            }
        });

        let engine = Engine::new("http://unused".to_string(), ws_url);
        assert!(engine.connect("good-cred").await);
        let _ = engine.agent_events_resume("dev-1", "ws-1", 0).await;
        let _ = engine.agent_events_resume("dev-1", "ws-1", -1).await;
        server.await.unwrap();
    }
}
