//! `choosh-hostd dev-exec --host=<id> --workspace=<name> <cmd...>`: the
//! CLI-facing client half of M7's cross-host task offload
//! (`docs/milestones/M7-fleet-and-provisioning.md`'s scope bullet:
//! `"dev-exec --host=<id> <cmd>: cross-host task offload, brokered by
//! relayd, running against a matching jj revision in an ephemeral
//! workspace on the target host"`). This is the one milestone with no
//! dedicated spec file, so the design decisions below are this pass's own,
//! reported interpretation, not a restatement of an existing spec.
//!
//! `dev-exec` dials `relayd` and authenticates using this devhost's own
//! persisted device credential — the same one `serve` uses (loaded from
//! [`credential::default_path`]) — then exercises a `devhost` Identity's
//! own `open-tunnel { purpose: "offload" }` capability
//! (`docs/specs/auth-and-enrollment.md`'s capability table; already
//! enforced relay-side, see `choosh-relayd::ws::check_open_tunnel_permitted`).
//! It is not a new Identity class or a phone-facing command. See
//! `serve.rs`'s `"offload"`-purpose `handle_control_push`/
//! `handle_offload_request_frame` for the server (target devhost) half this
//! module talks to, and `choosh_protocol::offload` for the exact wire
//! framing once the tunnel is open.
//!
//! ## Design decisions (recorded here since M7 has no spec of its own)
//!
//! 1. **"a matching jj revision" across two independently-cloned jj
//!    stores.** A `jj` change id is a local-store concept: each store
//!    assigns change ids independently the first time it imports a commit
//!    (jj's own documented Git-backend behavior), so two devhosts'
//!    independent clones of the same repo are not guaranteed to agree on a
//!    change id for the same commit. The underlying Git commit id IS
//!    portable — it's a content hash of history both devhosts already
//!    share (both are Projects registered from the same Git remote,
//!    per `workspace.create`) — so this module resolves `--workspace`'s
//!    root via this devhost's own registry, reads its `@`'s commit id via
//!    a real `jj log -r @ -T commit_id` (`jj_ops::current_commit_id`,
//!    verified against a real `jj 0.44.0` binary), and sends *that* as the
//!    offload request's `commit_id`. The target resolves an ephemeral
//!    workspace against it via `jj workspace add <dest> --name <name> -r
//!    <commit_id> -R <target-workspace-root>` — also verified against the
//!    real binary: `-r`/`--revision` accepts a bare commit hash and anchors
//!    the new workspace's working copy there. If the target's store has
//!    never fetched that commit, this fails cleanly with a typed
//!    `not_found` [`choosh_protocol::offload::OffloadError`] — there is
//!    deliberately no fetch-on-demand in this pass (a real, stated gap:
//!    both devhosts are assumed to already share the same Git remote and
//!    be reasonably up to date, the same assumption `workspace.create`'s
//!    own `clone_url` path already makes about remote reachability).
//!
//!    **A sharper limitation, found while building this module's own
//!    integration test** (see `jj_ops::current_commit_id`'s doc comment for
//!    the full explanation): `@`'s commit id is only actually portable when
//!    `@` itself was imported unchanged from the shared remote (typically
//!    true right after a clone/fetch with no local edits yet) — `jj`
//!    creates a fresh, store-local commit object (with this store's own
//!    timestamp) every time `@` gets a new snapshot, so a workspace with
//!    real *uncommitted* local edits has a `@` whose commit id was never
//!    shared with anything and cannot resolve on the target. This pass does
//!    not solve "ship my uncommitted working-copy content to another
//!    devhost" — that would mean transferring the diff itself, not just a
//!    commit id, a materially larger piece of work. `dev-exec` is correct
//!    and useful for its stated scope (offload a build/test run against a
//!    revision that has actually reached the shared remote — e.g. right
//!    after a clone, or after a `jj git push`/`jj git fetch` cycle either
//!    side of the fleet already performed) and fails cleanly, not
//!    silently, outside it.
//! 2. **"brokered by relayd" end to end.** The originating devhost opens an
//!    `"offload"`-purpose tunnel to the target devhost's raw `device_id`
//!    (`--host`), exactly like `proxy connect` (`proxy.rs`) opens an
//!    `"ssh"`-purpose tunnel to its target — same `open-tunnel` control
//!    request, a different requester Identity class and purpose. `--host`
//!    is the literal `device_id`, not an alias: per
//!    `auth-and-enrollment.md`'s capability table, a `devhost` Identity has
//!    no capability to call `list-devhosts`/`list-devhost-ssh-endpoints`
//!    (`phone`-only and `laptop-proxy`-only respectively) — it genuinely
//!    cannot resolve an alias to a `device_id` itself, so the
//!    operator/agent invoking `dev-exec` supplies the `device_id` directly
//!    (as seen in the phone's fleet view). Once the tunnel is open, this
//!    module speaks the small request/response protocol
//!    `choosh_protocol::offload` defines: one JSON `OffloadRequest` frame
//!    out, then any number of raw stdout/stderr frames back, then one
//!    exit-code frame, then the ordinary tunnel close.
//! 3. **"streams output back to the originating agent's terminal."** This
//!    process's own stdout/stderr — the same posture `proxy connect`
//!    already takes toward its own stdio, not redirection into a
//!    *different*, already-running agent's Zellij pane (which would need
//!    an additional design decision this milestone doc doesn't specify:
//!    which pane, how "the originating agent" is identified). `dev-exec`
//!    is a CLI invocation an agent runs via its own shell tool, so its own
//!    stdout/stderr already lands in that agent's terminal the same way
//!    any other subprocess's output would.

use choosh_protocol::offload::{OffloadError, OffloadFrame, OffloadRequest, encode_request_frame};
use choosh_protocol::relay::{
    AuthResult, ClientAuth, ControlRequest, ControlResponse, DeviceAuth, FRAME_CLASS_CONTROL, FRAME_CLASS_TUNNEL, ServerHello,
    TUNNEL_ID_BYTES, decode_tunnel_id_hex,
};
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::credential::{self, Credential, CredentialError};
use crate::frame_channel::{ChannelError, FrameChannel};
use crate::jj_ops::JjError;

const DEFAULT_RELAYD_URL: &str = "ws://127.0.0.1:7443/connect";

#[derive(Debug)]
pub enum DevExecError {
    Credential(CredentialError),
    /// No persisted devhost credential — this devhost has never run `serve`
    /// (or `serve` has never completed enrollment) to obtain one.
    NotEnrolled,
    Transport(String),
    AuthFailed(String),
    RelayError { code: String, message: String },
    UnexpectedResponse(String),
    /// `--workspace` doesn't name a workspace registered on *this* (the
    /// originating) devhost — resolved locally, before any network activity.
    UnknownWorkspace(String),
    Jj(JjError),
    /// The target devhost refused the request — see [`OffloadError`]'s own
    /// doc comment for why `message` is already redacted.
    Remote(OffloadError),
    Io(std::io::Error),
}

impl std::fmt::Display for DevExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Credential(error) => write!(f, "{error}"),
            Self::NotEnrolled => write!(f, "no devhost credential found; run `choosh-hostd serve` at least once on this devhost first"),
            Self::Transport(reason) => write!(f, "relayd transport error: {reason}"),
            Self::AuthFailed(reason) => write!(f, "relayd rejected authentication: {reason}"),
            Self::RelayError { code, message } => write!(f, "relayd error {code}: {message}"),
            Self::UnexpectedResponse(reason) => write!(f, "unexpected response from relayd: {reason}"),
            Self::UnknownWorkspace(name) => write!(f, "workspace {name:?} is not registered on this devhost"),
            Self::Jj(error) => write!(f, "{error}"),
            Self::Remote(error) => write!(f, "target devhost refused the offload request ({}): {}", error.code, error.message),
            Self::Io(error) => write!(f, "I/O error: {error}"),
        }
    }
}

impl std::error::Error for DevExecError {}

impl From<CredentialError> for DevExecError {
    fn from(error: CredentialError) -> Self {
        Self::Credential(error)
    }
}

impl From<ChannelError> for DevExecError {
    fn from(error: ChannelError) -> Self {
        Self::Transport(error.to_string())
    }
}

fn relay_url() -> String {
    std::env::var("CHOOSH_RELAYD_URL").unwrap_or_else(|_| DEFAULT_RELAYD_URL.to_string())
}

type WsChannel = FrameChannel<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>;

async fn dial(relay_url: &str) -> Result<WsChannel, DevExecError> {
    let (stream, _response) = tokio_tungstenite::connect_async(relay_url).await.map_err(|error| DevExecError::Transport(error.to_string()))?;
    Ok(FrameChannel::new(stream))
}

/// Dials `relay_url` and authenticates with this devhost's own persisted
/// device credential — mirrors `proxy.rs`'s `connect_authenticated`
/// exactly, duplicated rather than shared for the same reason documented
/// there: `serve.rs`'s own dial/auth logic is private and built around
/// `serve`'s devhost-specific state, and this is a small, independently
/// testable amount of logic.
async fn connect_authenticated(relay_url: &str, credential: &Credential) -> Result<WsChannel, DevExecError> {
    let mut channel = dial(relay_url).await?;
    let hello: ServerHello = channel.recv().await?;
    let signature = credential.sign(hello.nonce.as_bytes()).map_err(DevExecError::Credential)?;
    let auth = ClientAuth::Device(DeviceAuth {
        device_id: credential.device_id.clone(),
        certificate: credential.certificate.clone(),
        signature: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, signature.to_bytes()),
    });
    channel.send(FRAME_CLASS_CONTROL, &auth).await?;
    match channel.recv().await? {
        AuthResult::Ok(_) => Ok(channel),
        AuthResult::Failed(failed) => Err(DevExecError::AuthFailed(failed.reason)),
    }
}

fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// `choosh-hostd dev-exec --host=<host_id> --workspace=<workspace_name>
/// <argv...>`. Production entry point: resolves `workspace_name` against
/// this devhost's own registry (the same one `serve` uses), resolves its
/// `@`'s commit id via a real `jj log`, then delegates to [`run_with_io`]
/// with real process stdout/stderr. Returns the *remote* command's exit
/// code for `lib.rs` to `std::process::exit` with — a non-zero exit from
/// the offloaded command is not itself a `dev-exec` failure, it's the
/// faithfully-propagated result `lib.rs` must reflect in this process's own
/// exit status.
///
/// # Errors
///
/// See [`DevExecError`]'s variants.
pub async fn run(host_id: &str, workspace_name: &str, argv: Vec<String>) -> Result<i32, DevExecError> {
    let credential = credential::load(&credential::default_path()?)?.ok_or(DevExecError::NotEnrolled)?;

    let ctx = crate::serve::build_rpc_context(&credential.device_id)
        .map_err(|error| DevExecError::Io(std::io::Error::other(error.to_string())))?;
    let root_path = {
        let registry = ctx.registry.lock().await;
        registry
            .find_workspace_by_name(workspace_name)
            .map(|workspace| workspace.root_path.clone())
            .ok_or_else(|| DevExecError::UnknownWorkspace(workspace_name.to_string()))?
    };
    let commit_id = crate::jj_ops::current_commit_id(&root_path).await.map_err(DevExecError::Jj)?;

    let request = OffloadRequest { workspace_name: workspace_name.to_string(), commit_id, argv };
    Box::pin(run_with_io(host_id, &relay_url(), &credential, request, tokio::io::stdout(), tokio::io::stderr())).await
}

/// The testable core of `dev-exec`: authenticate, open an `"offload"`-purpose
/// tunnel to `host_id`, send `request`, then stream the target's
/// stdout/stderr frames into `stdout`/`stderr` until an exit-code frame (or
/// an error) arrives. `pub(crate)` so `serve.rs`'s own test module (which
/// already has the private `dial`/`serve_dispatch` this needs to drive the
/// *target* side of a real two-party test) can call it directly — see that
/// module's own offload integration test.
///
/// # Errors
///
/// [`DevExecError::AuthFailed`]/[`DevExecError::Transport`] for a failed
/// dial/authenticate, [`DevExecError::RelayError`] if `open-tunnel` itself
/// is rejected (e.g. the target is offline, or this identity/purpose
/// combination isn't permitted), [`DevExecError::Remote`] if the target
/// devhost accepted the tunnel but refused the request itself (unknown
/// workspace, unresolvable revision, a command that failed to start).
pub(crate) async fn run_with_io<WOut, WErr>(
    host_id: &str,
    relay_url: &str,
    credential: &Credential,
    request: OffloadRequest,
    mut stdout: WOut,
    mut stderr: WErr,
) -> Result<i32, DevExecError>
where
    WOut: AsyncWrite + Unpin,
    WErr: AsyncWrite + Unpin,
{
    let mut channel = connect_authenticated(relay_url, credential).await?;

    let request_id = new_request_id();
    channel
        .send(FRAME_CLASS_CONTROL, &ControlRequest::OpenTunnel { request_id, target_device_id: host_id.to_string(), purpose: "offload".to_string() })
        .await?;
    let tunnel_id = match channel.recv().await? {
        ControlResponse::OpenTunnelOk { tunnel_id, .. } => {
            decode_tunnel_id_hex(&tunnel_id).ok_or_else(|| DevExecError::UnexpectedResponse("relayd returned a malformed tunnel_id".to_string()))?
        }
        ControlResponse::Error { code, message, .. } => return Err(DevExecError::RelayError { code, message }),
        other => return Err(DevExecError::UnexpectedResponse(format!("{other:?}"))),
    };

    let mut payload = Vec::with_capacity(TUNNEL_ID_BYTES);
    payload.extend_from_slice(&tunnel_id);
    payload.extend(encode_request_frame(&request).map_err(|error| DevExecError::UnexpectedResponse(format!("failed to encode offload request: {error}")))?);
    channel.send_bytes(FRAME_CLASS_TUNNEL, &payload).await?;

    loop {
        let (class, body) = channel.recv_raw().await?;
        if class != FRAME_CLASS_TUNNEL {
            continue; // interleaved traffic for a different purpose; ignore.
        }
        if body.len() < TUNNEL_ID_BYTES {
            continue; // malformed for this tunnel's own protocol; relay-protocol.md framing already validated the outer frame.
        }
        let (id_bytes, inner) = body.split_at(TUNNEL_ID_BYTES);
        if id_bytes != tunnel_id {
            continue; // a stray frame for an unrelated tunnel; this connection only ever opens one.
        }
        if inner.is_empty() {
            // The tunnel closed with no exit frame ever having arrived —
            // this is a transport-level failure, not a "the command ran and
            // we just don't know its exit code" outcome.
            return Err(DevExecError::UnexpectedResponse("offload tunnel closed before an exit code arrived".to_string()));
        }
        match choosh_protocol::offload::decode_frame(inner) {
            Ok(OffloadFrame::Stdout(bytes)) => {
                stdout.write_all(&bytes).await.map_err(DevExecError::Io)?;
                stdout.flush().await.map_err(DevExecError::Io)?;
            }
            Ok(OffloadFrame::Stderr(bytes)) => {
                stderr.write_all(&bytes).await.map_err(DevExecError::Io)?;
                stderr.flush().await.map_err(DevExecError::Io)?;
            }
            Ok(OffloadFrame::Exit(code)) => return Ok(code),
            Ok(OffloadFrame::Error(error)) => return Err(DevExecError::Remote(error)),
            Ok(OffloadFrame::Request(_)) => {} // not expected in this direction; ignore rather than treat as fatal.
            Err(error) => {
                tracing::debug!(%error, "malformed offload frame from target devhost; ignoring");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_protocol::framing::{FrameDecoder, FrameLimits, encode_frame};
    use choosh_protocol::relay::{AuthOk, IdentityClass, MAX_CONTROL_FRAME_BYTES, MAX_TUNNEL_FRAME_BYTES};
    use futures_util::{SinkExt, StreamExt};
    use std::sync::{Arc, Mutex};
    use tokio_tungstenite::tungstenite::Message;

    async fn bind_fake_relayd() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, format!("ws://{addr}/connect"))
    }

    fn fake_credential() -> Credential {
        let signing_key = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        Credential::new("dev-client".to_string(), "fake-cert".to_string(), &signing_key)
    }

    async fn send_control<T: serde::Serialize>(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>, value: &T) {
        let mut payload = vec![FRAME_CLASS_CONTROL];
        payload.extend(serde_json::to_vec(value).unwrap());
        let framed = encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).unwrap();
        ws.send(Message::Binary(framed.into())).await.unwrap();
    }

    async fn recv_control<T: serde::de::DeserializeOwned>(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) -> T {
        let mut decoder = FrameDecoder::new(FrameLimits::new(MAX_CONTROL_FRAME_BYTES, 4).unwrap());
        loop {
            let Some(Ok(Message::Binary(bytes))) = ws.next().await else { panic!("expected a binary control message") };
            let frames = decoder.feed(&bytes).unwrap();
            if let Some(frame) = frames.into_iter().next() {
                let (_class, body) = frame.split_first().unwrap();
                return serde_json::from_slice(body).unwrap();
            }
        }
    }

    async fn fake_relayd_authenticate(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) {
        send_control(ws, &ServerHello { nonce: "test-nonce".to_string() }).await;
        let _client_auth: serde_json::Value = recv_control(ws).await;
        send_control(ws, &AuthResult::Ok(AuthOk { identity_class: IdentityClass::Devhost, device_id: "dev-client".to_string() })).await;
    }

    struct RecordingWriter(Arc<Mutex<Vec<u8>>>);

    impl AsyncWrite for RecordingWriter {
        fn poll_write(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>, buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
            self.0.lock().unwrap().extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    fn sample_request() -> OffloadRequest {
        OffloadRequest { workspace_name: "app".to_string(), commit_id: "deadbeef".to_string(), argv: vec!["true".to_string()] }
    }

    /// Sends one already-offload-framed `bytes` payload as a
    /// `FRAME_CLASS_TUNNEL` frame for `tunnel_id` — the fake-relayd-server
    /// side's equivalent of what `serve.rs`'s real `TunnelOutput` forwarding
    /// does, used by tests below that stand in for the target devhost.
    async fn send_offload_tunnel_bytes(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>, tunnel_id: [u8; TUNNEL_ID_BYTES], bytes: Vec<u8>) {
        let mut payload = vec![FRAME_CLASS_TUNNEL];
        payload.extend_from_slice(&tunnel_id);
        payload.extend_from_slice(&bytes);
        let framed = encode_frame(&payload, MAX_TUNNEL_FRAME_BYTES).unwrap();
        ws.send(Message::Binary(framed.into())).await.unwrap();
    }

    #[tokio::test]
    async fn run_with_io_streams_stdout_stderr_and_propagates_a_nonzero_exit_code() {
        let (listener, relay_url) = bind_fake_relayd().await;
        let recorded_out = Arc::new(Mutex::new(Vec::new()));
        let recorded_err = Arc::new(Mutex::new(Vec::new()));
        let stdout = RecordingWriter(recorded_out.clone());
        let stderr = RecordingWriter(recorded_err.clone());

        let client_fut = async {
            let credential = fake_credential();
            Box::pin(run_with_io("dev-target", &relay_url, &credential, sample_request(), stdout, stderr)).await
        };

        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            fake_relayd_authenticate(&mut ws).await;

            let _open_tunnel: serde_json::Value = recv_control(&mut ws).await;
            let tunnel_id = [5u8; TUNNEL_ID_BYTES];
            send_control(&mut ws, &ControlResponse::OpenTunnelOk { request_id: "r".to_string(), tunnel_id: choosh_protocol::relay::encode_tunnel_id_hex(tunnel_id) }).await;

            // The client's own offload request frame — not asserted on in
            // detail here (covered by `choosh-protocol`'s own round-trip
            // tests); just drained so the test can move on to the response
            // side.
            let mut decoder = FrameDecoder::new(FrameLimits::new(MAX_TUNNEL_FRAME_BYTES, 4).unwrap());
            loop {
                let Some(Ok(Message::Binary(bytes))) = ws.next().await else { panic!("expected the offload request frame") };
                if !decoder.feed(&bytes).unwrap().is_empty() {
                    break;
                }
            }

            send_offload_tunnel_bytes(&mut ws, tunnel_id, choosh_protocol::offload::encode_stdout_frame(b"hello stdout\n")).await;
            send_offload_tunnel_bytes(&mut ws, tunnel_id, choosh_protocol::offload::encode_stderr_frame(b"hello stderr\n")).await;
            send_offload_tunnel_bytes(&mut ws, tunnel_id, choosh_protocol::offload::encode_exit_frame(7)).await;
            let mut close_payload = vec![FRAME_CLASS_TUNNEL];
            close_payload.extend_from_slice(&tunnel_id);
            let close_framed = encode_frame(&close_payload, MAX_TUNNEL_FRAME_BYTES).unwrap();
            let _ = ws.send(Message::Binary(close_framed.into())).await;
        };

        let (result, ()) = tokio::join!(client_fut, server_fut);
        assert_eq!(result.unwrap(), 7, "the remote command's exit code must propagate exactly");
        assert_eq!(recorded_out.lock().unwrap().as_slice(), b"hello stdout\n");
        assert_eq!(recorded_err.lock().unwrap().as_slice(), b"hello stderr\n");
    }

    #[tokio::test]
    async fn run_with_io_surfaces_a_remote_offload_error() {
        let (listener, relay_url) = bind_fake_relayd().await;
        let recorded_out = Arc::new(Mutex::new(Vec::new()));
        let recorded_err = Arc::new(Mutex::new(Vec::new()));
        let stdout = RecordingWriter(recorded_out);
        let stderr = RecordingWriter(recorded_err);

        let client_fut = async {
            let credential = fake_credential();
            Box::pin(run_with_io("dev-target", &relay_url, &credential, sample_request(), stdout, stderr)).await
        };

        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            fake_relayd_authenticate(&mut ws).await;

            let _open_tunnel: serde_json::Value = recv_control(&mut ws).await;
            let tunnel_id = [6u8; TUNNEL_ID_BYTES];
            send_control(&mut ws, &ControlResponse::OpenTunnelOk { request_id: "r".to_string(), tunnel_id: choosh_protocol::relay::encode_tunnel_id_hex(tunnel_id) }).await;

            let mut decoder = FrameDecoder::new(FrameLimits::new(MAX_TUNNEL_FRAME_BYTES, 4).unwrap());
            loop {
                let Some(Ok(Message::Binary(bytes))) = ws.next().await else { panic!("expected the offload request frame") };
                if !decoder.feed(&bytes).unwrap().is_empty() {
                    break;
                }
            }

            let error = OffloadError { code: "not_found".to_string(), message: "workspace not registered".to_string() };
            let mut payload = vec![FRAME_CLASS_TUNNEL];
            payload.extend_from_slice(&tunnel_id);
            payload.extend(choosh_protocol::offload::encode_error_frame(&error).unwrap());
            let framed = encode_frame(&payload, MAX_TUNNEL_FRAME_BYTES).unwrap();
            ws.send(Message::Binary(framed.into())).await.unwrap();
        };

        let (result, ()) = tokio::join!(client_fut, server_fut);
        assert!(matches!(result, Err(DevExecError::Remote(ref error)) if error.code == "not_found"), "expected a Remote(not_found) error, got {result:?}");
    }

    #[tokio::test]
    async fn run_with_io_fails_cleanly_when_the_relay_rejects_the_open_tunnel() {
        let (listener, relay_url) = bind_fake_relayd().await;
        let recorded_out = Arc::new(Mutex::new(Vec::new()));
        let recorded_err = Arc::new(Mutex::new(Vec::new()));
        let stdout = RecordingWriter(recorded_out.clone());
        let stderr = RecordingWriter(recorded_err.clone());

        let client_fut = async {
            let credential = fake_credential();
            Box::pin(run_with_io("dev-target", &relay_url, &credential, sample_request(), stdout, stderr)).await
        };

        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            fake_relayd_authenticate(&mut ws).await;
            let _open_tunnel: serde_json::Value = recv_control(&mut ws).await;
            send_control(&mut ws, &ControlResponse::Error { request_id: "r".to_string(), code: "target_offline".to_string(), message: "offline".to_string() }).await;
        };

        let (result, ()) = tokio::join!(client_fut, server_fut);
        assert!(matches!(result, Err(DevExecError::RelayError { ref code, .. }) if code == "target_offline"));
        assert!(recorded_out.lock().unwrap().is_empty());
        assert!(recorded_err.lock().unwrap().is_empty());
    }
}
