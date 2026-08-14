//! `choosh-hostd serve`: devhost daemon mode. Enrolls on first run (per
//! `docs/specs/auth-and-enrollment.md`'s "Devhost enrollment"), then holds
//! an authenticated connection to `choosh-relayd` open, reconnecting with
//! backoff on any drop (per `docs/specs/relay-protocol.md`'s transport
//! requirement). Since M1, an `rpc`-purpose tunnel offered on that
//! connection is accepted and its `host-rpc.md` traffic dispatched via
//! [`crate::rpc`] — see `docs/milestones/M1-workspace-and-jj.md`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use choosh_protocol::relay::{
    AuthResult, ClientAuth, ControlRequest, ControlResponse, DeviceAuth, FRAME_CLASS_CONTROL, FRAME_CLASS_TUNNEL,
    IdentityClass, ServerHello, ServerPush, TUNNEL_ID_BYTES, decode_tunnel_id_hex,
};
use ed25519_dalek::SigningKey;

use crate::backoff::compute_backoff;
use crate::credential::{self, Credential, CredentialError};
use crate::frame_channel::FrameChannel;
use crate::local_ipc;
use crate::pty::{PtySession, PtyWriteHalf};
use crate::rpc::{self, RpcContext};
use choosh_protocol::relay::WireAgentEvent;

const DEFAULT_RELAYD_URL: &str = "ws://127.0.0.1:7443/connect";

#[derive(Debug)]
pub enum ServeError {
    Credential(CredentialError),
    MissingToken,
    InvalidRelayUrl(String),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Credential(error) => write!(f, "{error}"),
            Self::MissingToken => write!(
                f,
                "no device credential found and CHOOSH_ENROLLMENT_TOKEN is not set; \
                 set it to a token minted from the Choosh app (relayd's request-enrollment-token) \
                 before starting choosh-hostd serve for the first time"
            ),
            Self::InvalidRelayUrl(url) => write!(f, "CHOOSH_RELAYD_URL is not a valid WebSocket URL: {url}"),
        }
    }
}

impl std::error::Error for ServeError {}

impl From<CredentialError> for ServeError {
    fn from(error: CredentialError) -> Self {
        Self::Credential(error)
    }
}

/// Public for integration tests (`tests/serve_enrollment.rs`), which build a
/// `ServeConfig` pointed at a fake `relayd` rather than going through
/// `from_env`'s real environment variables.
pub struct ServeConfig {
    relay_url: String,
    credential_path: PathBuf,
    alias: String,
    platform: String,
    account_label: Option<String>,
}

impl ServeConfig {
    #[must_use]
    pub fn for_test(relay_url: String, credential_path: PathBuf) -> Self {
        Self { relay_url, credential_path, alias: "test-host".to_string(), platform: "test".to_string(), account_label: None }
    }

    fn from_env() -> Result<Self, ServeError> {
        let relay_url = std::env::var("CHOOSH_RELAYD_URL").unwrap_or_else(|_| DEFAULT_RELAYD_URL.to_string());
        let credential_path = match std::env::var("CHOOSH_HOSTD_CREDENTIAL_PATH") {
            Ok(path) => PathBuf::from(path),
            Err(_) => credential::default_path()?,
        };
        let alias = std::env::var("CHOOSH_HOSTD_ALIAS").unwrap_or_else(|_| local_hostname());
        let platform = std::env::var("CHOOSH_HOSTD_PLATFORM").unwrap_or_else(|_| std::env::consts::OS.to_string());
        let account_label = std::env::var("CHOOSH_HOSTD_ACCOUNT_LABEL").ok();
        Ok(Self { relay_url, credential_path, alias, platform, account_label })
    }
}

fn local_hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown-host".to_string())
}

/// Runs `choosh-hostd serve` until a fatal error or a shutdown signal.
///
/// # Errors
///
/// Returns [`ServeError::MissingToken`] on a clean first run with no
/// `CHOOSH_ENROLLMENT_TOKEN` set, or [`ServeError::Credential`] if the
/// persisted credential file exists but is corrupt. A connection failure
/// after successful enrollment is NOT an error return — it triggers the
/// reconnect-with-backoff loop instead, per relay-protocol.md.
pub async fn run() -> Result<(), ServeError> {
    let config = ServeConfig::from_env()?;

    let credential = match credential::load(&config.credential_path)? {
        Some(credential) => credential,
        None => enroll(&config).await?,
    };

    let rpc_context = build_rpc_context(&credential)?;

    // The local IPC listener and its channel live for the whole `serve`
    // process lifetime, not per-connection: an `emit` invocation while
    // `serve` is between relayd connections (a reconnect backoff window)
    // still queues its event here rather than being dropped, and gets
    // forwarded once reconnected. Bounded, not unbounded — a burst of
    // hook events during an extended outage backpressures onto `emit`
    // (which fails closed and exits non-zero) rather than growing memory
    // without limit; there is deliberately no persistence across a
    // `serve` restart itself (no on-disk spool) — a real, documented gap,
    // not the full replay/sequence machinery `agent-events.md` describes,
    // which is out of scope for this increment.
    let (agent_event_tx, agent_event_rx) = tokio::sync::mpsc::channel(256);
    match local_ipc::default_socket_path() {
        Ok(socket_path) => match local_ipc::bind(&socket_path) {
            Ok(listener) => {
                tokio::spawn(local_ipc::serve_forever(listener, agent_event_tx));
            }
            Err(error) => tracing::error!(%error, "failed to bind local IPC socket; agent hooks will not be delivered"),
        },
        Err(error) => tracing::error!(%error, "failed to determine local IPC socket path; agent hooks will not be delivered"),
    }

    connect_loop(&config, &credential, &rpc_context, agent_event_rx).await;
    Ok(())
}

fn build_rpc_context(credential: &Credential) -> Result<RpcContext, ServeError> {
    let workspaces_dir = match std::env::var("CHOOSH_HOSTD_WORKSPACES_DIR") {
        Ok(path) => PathBuf::from(path),
        Err(_) => directories::ProjectDirs::from("ai", "choosh", "hostd")
            .ok_or(ServeError::Credential(CredentialError::NoConfigDir))?
            .data_dir()
            .join("workspaces"),
    };
    std::fs::create_dir_all(&workspaces_dir).map_err(|e| ServeError::Credential(CredentialError::Io(e)))?;
    let registry_path = match std::env::var("CHOOSH_HOSTD_REGISTRY_PATH") {
        Ok(path) => PathBuf::from(path),
        Err(_) => crate::registry::Registry::default_path()
            .map_err(|e| ServeError::Credential(CredentialError::Io(io_error(e))))?,
    };
    let registry = crate::registry::Registry::load(&registry_path)
        .map_err(|e| ServeError::Credential(CredentialError::Io(io_error(e))))?;
    Ok(RpcContext { registry: tokio::sync::Mutex::new(registry), devhost_id: credential.device_id.clone(), workspaces_dir })
}

fn io_error(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

/// Public for integration tests; production code reaches this only through
/// [`run`].
///
/// # Errors
///
/// See [`run`].
pub async fn enroll(config: &ServeConfig) -> Result<Credential, ServeError> {
    let token = std::env::var("CHOOSH_ENROLLMENT_TOKEN").map_err(|_| ServeError::MissingToken)?;
    let signing_key = SigningKey::generate(&mut rand::rng());
    let public_key_b64 = base64_encode(signing_key.verifying_key().as_bytes());

    tracing::info!(relay_url = %config.relay_url, "enrolling with relayd");
    let mut channel = dial(&config.relay_url).await?;

    // relayd sends ServerHello{nonce} unconditionally as the first frame on
    // every new connection, enrollment included — the nonce itself is only
    // meaningful for the DeviceAuth signature path (run_one_connection
    // below), but it still MUST be read off the wire here before the enroll
    // response, or that response is misparsed as this frame instead.
    let _hello: ServerHello = channel.recv().await.map_err(|error| enroll_transport_error(&error))?;

    // The connection is unauthenticated at this point, but `enroll` is
    // explicitly the one control request relay-protocol.md's transport
    // section allows before authentication completes — it's how
    // authentication material is obtained in the first place.
    let request_id = new_request_id();
    channel
        .send(
            FRAME_CLASS_CONTROL,
            &ControlRequest::Enroll {
                request_id: request_id.clone(),
                token,
                identity_class: IdentityClass::Devhost,
                public_key: public_key_b64,
                host_ssh_public_key: None, // the loopback SSH server (ssh-bridge-and-zed.md) doesn't exist yet
                alias: Some(config.alias.clone()),
                platform: Some(config.platform.clone()),
                account_label: config.account_label.clone(),
            },
        )
        .await
        .map_err(|error| enroll_transport_error(&error))?;

    let response: ControlResponse = channel.recv().await.map_err(|error| enroll_transport_error(&error))?;
    match response {
        ControlResponse::EnrollOk { device_id, certificate, .. } => {
            let credential = Credential::new(device_id.clone(), certificate, &signing_key);
            credential::save(&config.credential_path, &credential)?;
            tracing::info!(device_id, "enrollment complete, credential persisted");
            Ok(credential)
        }
        ControlResponse::Error { code, message, .. } => {
            Err(ServeError::Credential(CredentialError::Corrupt(format!("enrollment rejected: {code}: {message}"))))
        }
        other => Err(ServeError::Credential(CredentialError::Corrupt(format!(
            "unexpected response to enroll: {other:?}"
        )))),
    }
}

fn enroll_transport_error(error: &crate::frame_channel::ChannelError) -> ServeError {
    ServeError::Credential(CredentialError::Corrupt(format!("enrollment transport failure: {error}")))
}

/// Holds an authenticated connection to `relayd` open, reconnecting with
/// exponential backoff and jitter (per relay-protocol.md) on any drop.
/// Returns only on a graceful shutdown signal (Ctrl+C/SIGTERM); every
/// connection failure is logged and retried rather than propagated.
async fn connect_loop(
    config: &ServeConfig,
    credential: &Credential,
    rpc_context: &RpcContext,
    mut agent_event_rx: tokio::sync::mpsc::Receiver<WireAgentEvent>,
) {
    let mut attempt: u32 = 0;
    loop {
        let shutdown = tokio::signal::ctrl_c();
        tokio::select! {
            () = run_one_connection(config, credential, rpc_context, &mut agent_event_rx) => {
                let delay = compute_backoff(attempt, rand_unit());
                attempt = attempt.saturating_add(1);
                tracing::warn!(?delay, attempt, "connection to relayd ended; reconnecting");
                tokio::time::sleep(delay).await;
            }
            result = shutdown => {
                if let Err(error) = result {
                    tracing::error!(%error, "failed to install shutdown signal handler");
                }
                tracing::info!("shutdown requested, closing connection to relayd");
                return;
            }
        }
    }
}

/// Runs one connect-authenticate-hold cycle to completion (i.e. until the
/// connection drops for any reason). Never returns `Err` — every failure
/// mode is logged and treated as "this attempt ended," letting the caller's
/// backoff loop decide what happens next.
async fn run_one_connection(
    config: &ServeConfig,
    credential: &Credential,
    rpc_context: &RpcContext,
    agent_event_rx: &mut tokio::sync::mpsc::Receiver<WireAgentEvent>,
) {
    let mut channel = match dial(&config.relay_url).await {
        Ok(channel) => channel,
        Err(error) => {
            tracing::warn!(%error, "failed to connect to relayd");
            return;
        }
    };

    let hello: ServerHello = match channel.recv().await {
        Ok(hello) => hello,
        Err(error) => {
            tracing::warn!(%error, "did not receive server hello");
            return;
        }
    };

    let signature = match credential.sign(hello.nonce.as_bytes()) {
        Ok(signature) => signature,
        Err(error) => {
            // A corrupt credential surfacing here (rather than at startup
            // load) would be a real bug, but treat it the same as any other
            // per-connection failure rather than crashing the whole daemon.
            tracing::error!(%error, "failed to sign challenge nonce with stored credential");
            return;
        }
    };

    let auth = ClientAuth::Device(DeviceAuth {
        device_id: credential.device_id.clone(),
        certificate: credential.certificate.clone(),
        signature: base64_encode(&signature.to_bytes()),
    });
    if let Err(error) = channel.send(FRAME_CLASS_CONTROL, &auth).await {
        tracing::warn!(%error, "failed to send device auth");
        return;
    }

    let result: AuthResult = match channel.recv().await {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(%error, "did not receive auth result");
            return;
        }
    };
    match result {
        AuthResult::Ok(ok) => {
            tracing::info!(device_id = %ok.device_id, "authenticated with relayd");
        }
        AuthResult::Failed(failed) => {
            tracing::warn!(reason = %failed.reason, "relayd rejected authentication");
            return;
        }
    }

    serve_dispatch(&mut channel, rpc_context, agent_event_rx).await;
}

type WsChannel = FrameChannel<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>;

/// One item of PTY output, tagged with the tunnel it belongs to — produced
/// by a per-tunnel background reader task (spawned when a `pty:`-purpose
/// tunnel is offered), consumed by [`serve_dispatch`]'s main loop, which is
/// the only place still holding the shared `channel` to actually send it.
struct PtyOutput {
    tunnel_id: [u8; TUNNEL_ID_BYTES],
    bytes: Vec<u8>,
}

/// Processes frames on an authenticated connection until it ends: `rpc`-
/// and `pty:<item_id>`-purpose tunnels (`docs/specs/host-rpc.md`,
/// `docs/milestones/M2-terminal-and-agents.md`), plus forwarding any
/// locally-emitted agent events (from `choosh-hostd emit`, via
/// `local_ipc`) as `agent-event` control requests.
async fn serve_dispatch(channel: &mut WsChannel, rpc_context: &RpcContext, agent_event_rx: &mut tokio::sync::mpsc::Receiver<WireAgentEvent>) {
    // `rpc`-purpose tunnels offered on this connection are tracked here;
    // per relay-protocol.md's reconnect-discontinuity rule, tunnels never
    // survive a reconnect, so this set is deliberately scoped to one
    // connection attempt, not `connect_loop`'s outer state. Same for
    // `pty_tunnels` and the output-forwarding channel below.
    let mut rpc_tunnels: HashSet<[u8; TUNNEL_ID_BYTES]> = HashSet::new();
    let mut pty_tunnels: HashMap<[u8; TUNNEL_ID_BYTES], PtyWriteHalf> = HashMap::new();
    let (pty_output_tx, mut pty_output_rx) = tokio::sync::mpsc::channel::<PtyOutput>(64);

    loop {
        tokio::select! {
            biased;

            // Locally-emitted agent events take priority over ordinary
            // tunnel traffic when both are ready — they're comparatively
            // rare and latency-sensitive (an `input_required` notification
            // waiting behind a burst of terminal output would be a real
            // regression), and `agent-event`'s payload is always tiny.
            Some(event) = agent_event_rx.recv() => {
                let request_id = new_request_id();
                if let Err(error) = channel.send(FRAME_CLASS_CONTROL, &ControlRequest::AgentEvent { request_id, event }).await {
                    tracing::warn!(%error, "failed to send agent-event; dropping it rather than blocking the connection");
                }
            }

            Some(output) = pty_output_rx.recv() => {
                if !pty_tunnels.contains_key(&output.tunnel_id) {
                    continue; // the tunnel closed after this output was already queued; drop it.
                }
                let mut payload = Vec::with_capacity(TUNNEL_ID_BYTES + output.bytes.len());
                payload.extend_from_slice(&output.tunnel_id);
                payload.extend_from_slice(&output.bytes);
                if let Err(error) = channel.send_bytes(FRAME_CLASS_TUNNEL, &payload).await {
                    tracing::warn!(%error, "failed to send pty output over tunnel");
                    return;
                }
            }

            frame = channel.recv_raw() => match frame {
                Ok((FRAME_CLASS_CONTROL, body)) => {
                    handle_control_push(&body, rpc_context, &pty_output_tx, &mut rpc_tunnels, &mut pty_tunnels).await;
                }
                Ok((FRAME_CLASS_TUNNEL, body)) => {
                    if handle_tunnel_frame(&body, channel, rpc_context, &mut rpc_tunnels, &mut pty_tunnels).await == FrameOutcome::Disconnect {
                        return;
                    }
                }
                Ok((class, _body)) => tracing::debug!(class, "received frame with no handler for this class"),
                Err(error) => {
                    tracing::info!(%error, "connection to relayd ended");
                    return;
                }
            }
        }
    }
}

#[derive(PartialEq, Eq)]
enum FrameOutcome {
    Continue,
    Disconnect,
}

/// Handles one `FRAME_CLASS_CONTROL` frame: `tunnel-offered` pushes (both
/// `rpc` and `pty:<item_id>` purposes) and `AgentEvent` pushes (rejected —
/// devhost connections never legitimately receive one, see the inline
/// comment). Split out of [`serve_dispatch`] to keep that function's line
/// count reasonable; takes the tunnel-tracking maps by reference rather
/// than owning them since [`serve_dispatch`]'s loop needs them across
/// iterations.
async fn handle_control_push(
    body: &[u8],
    rpc_context: &RpcContext,
    pty_output_tx: &tokio::sync::mpsc::Sender<PtyOutput>,
    rpc_tunnels: &mut HashSet<[u8; TUNNEL_ID_BYTES]>,
    pty_tunnels: &mut HashMap<[u8; TUNNEL_ID_BYTES], PtyWriteHalf>,
) {
    match serde_json::from_slice::<ServerPush>(body) {
        Ok(ServerPush::TunnelOffered { tunnel_id, purpose, .. }) if purpose == "rpc" => {
            if let Some(id) = decode_tunnel_id_hex(&tunnel_id) {
                rpc_tunnels.insert(id);
            } else {
                tracing::warn!(tunnel_id, "tunnel-offered carried a malformed tunnel_id");
            }
        }
        Ok(ServerPush::TunnelOffered { tunnel_id, purpose, .. }) if purpose.starts_with("pty:") => {
            let item_id = &purpose["pty:".len()..];
            let Some(id) = decode_tunnel_id_hex(&tunnel_id) else {
                tracing::warn!(tunnel_id, "tunnel-offered carried a malformed tunnel_id");
                return;
            };
            match open_pty_tunnel(rpc_context, item_id, id, pty_output_tx.clone()).await {
                Ok(write_half) => {
                    pty_tunnels.insert(id, write_half);
                }
                Err(error) => tracing::warn!(%error, item_id, "failed to attach pty for offered tunnel"),
            }
        }
        Ok(ServerPush::TunnelOffered { purpose, .. }) => {
            tracing::debug!(purpose, "ignoring tunnel-offered for an unhandled purpose");
        }
        Ok(ServerPush::AgentEvent { .. }) => {
            // Devhost-to-phone only, per relay-protocol.md — a devhost's
            // own connection never receives this push, so seeing one here
            // would indicate a relayd routing bug, not something this
            // connection needs to act on.
            tracing::warn!("unexpected AgentEvent push on a devhost connection, ignoring");
        }
        Err(error) => tracing::debug!(%error, "unrecognized control frame, ignoring"),
    }
}

/// Handles one `FRAME_CLASS_TUNNEL` frame: routes to an active pty tunnel's
/// write half, or dispatches as an `rpc`-tunnel `host-rpc.md` request.
/// Returns [`FrameOutcome::Disconnect`] only for a malformed frame or a
/// send failure that per relay-protocol.md means the connection itself is
/// unrecoverable — every other outcome is [`FrameOutcome::Continue`].
async fn handle_tunnel_frame(
    body: &[u8],
    channel: &mut WsChannel,
    rpc_context: &RpcContext,
    rpc_tunnels: &mut HashSet<[u8; TUNNEL_ID_BYTES]>,
    pty_tunnels: &mut HashMap<[u8; TUNNEL_ID_BYTES], PtyWriteHalf>,
) -> FrameOutcome {
    if body.len() < TUNNEL_ID_BYTES {
        tracing::warn!("tunnel frame shorter than a tunnel ID; per relay-protocol.md this is malformed");
        return FrameOutcome::Disconnect;
    }
    let (id_bytes, payload) = body.split_at(TUNNEL_ID_BYTES);
    let mut tunnel_id = [0u8; TUNNEL_ID_BYTES];
    tunnel_id.copy_from_slice(id_bytes);

    if let Some(mut write_half) = pty_tunnels.remove(&tunnel_id) {
        if payload.is_empty() {
            return FrameOutcome::Continue; // zero-payload close signal — dropping write_half above already kills the attached client (PtyWriteHalf::drop).
        }
        if let Err(error) = write_half.write_all(payload).await {
            tracing::debug!(%error, "pty write failed, tunnel is presumably closing");
            return FrameOutcome::Continue; // do not re-insert; the tunnel is done.
        }
        pty_tunnels.insert(tunnel_id, write_half);
        return FrameOutcome::Continue;
    }

    if !rpc_tunnels.contains(&tunnel_id) {
        tracing::debug!("tunnel frame for an unknown/non-rpc/non-pty tunnel id, ignoring");
        return FrameOutcome::Continue;
    }
    if payload.is_empty() {
        // Zero-length payload is the tunnel close signal.
        rpc_tunnels.remove(&tunnel_id);
        return FrameOutcome::Continue;
    }
    let Some((inner_class, inner_body)) = payload.split_first() else {
        return FrameOutcome::Continue;
    };
    if *inner_class != FRAME_CLASS_CONTROL {
        tracing::warn!("rpc tunnel payload used an unexpected inner frame class");
        return FrameOutcome::Continue;
    }
    let request: choosh_protocol::host_rpc::RpcRequest = match serde_json::from_slice(inner_body) {
        Ok(request) => request,
        Err(error) => {
            tracing::warn!(%error, "malformed host-rpc request on rpc tunnel");
            return FrameOutcome::Continue;
        }
    };
    let response = rpc::dispatch(rpc_context, request).await;
    let mut response_payload = Vec::with_capacity(TUNNEL_ID_BYTES + 1 + 128);
    response_payload.extend_from_slice(&tunnel_id);
    response_payload.push(FRAME_CLASS_CONTROL);
    if let Err(error) = serde_json::to_writer(&mut response_payload, &response) {
        tracing::error!(%error, "failed to serialize rpc response");
        return FrameOutcome::Continue;
    }
    if let Err(error) = channel.send_bytes(FRAME_CLASS_TUNNEL, &response_payload).await {
        tracing::warn!(%error, "failed to send rpc response over tunnel");
        return FrameOutcome::Disconnect;
    }
    FrameOutcome::Continue
}

/// Resolves `item_id` to its workspace's Zellij session/tab, attaches a
/// real headless pty client to it (`pty.rs`), and spawns a background task
/// that forwards everything it reads into `output_tx` tagged with
/// `tunnel_id` — the write half is returned for [`serve_dispatch`]'s main
/// loop to route phone-originated input into.
async fn open_pty_tunnel(
    rpc_context: &RpcContext,
    item_id: &str,
    tunnel_id: [u8; TUNNEL_ID_BYTES],
    output_tx: tokio::sync::mpsc::Sender<PtyOutput>,
) -> Result<PtyWriteHalf, String> {
    let (session_name, tab_name) = {
        let registry = rpc_context.registry.lock().await;
        let item = registry.find_item(item_id).ok_or_else(|| format!("item {item_id:?} is not registered"))?;
        let workspace = registry
            .find_workspace(&item.workspace_id)
            .ok_or_else(|| "item references a workspace that no longer exists".to_string())?;
        (workspace.workspace_name.clone(), item.tab_target.clone())
    };

    let session = PtySession::attach(&session_name, &tab_name).await.map_err(|error| error.to_string())?;
    let (mut read_half, write_half) = session.split();
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match read_half.read(&mut buf).await {
                Ok(0) | Err(_) => return, // EOF (client exited) or a read error — either way, nothing left to forward.
                Ok(n) => {
                    if output_tx.send(PtyOutput { tunnel_id, bytes: buf[..n].to_vec() }).await.is_err() {
                        return; // serve_dispatch's loop has ended; nothing left to forward to.
                    }
                }
            }
        }
    });
    Ok(write_half)
}

async fn dial(relay_url: &str) -> Result<FrameChannel<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>, ServeError> {
    let (stream, _response) = tokio_tungstenite::connect_async(relay_url)
        .await
        .map_err(|error| ServeError::InvalidRelayUrl(format!("{relay_url}: {error}")))?;
    Ok(FrameChannel::new(stream))
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn rand_unit() -> f64 {
    use rand::RngExt;
    rand::rng().random_range(0.0..1.0)
}
