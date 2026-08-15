//! `choosh-hostd serve`: devhost daemon mode. Enrolls on first run (per
//! `docs/specs/auth-and-enrollment.md`'s "Devhost enrollment"), then holds
//! an authenticated connection to `choosh-relayd` open, reconnecting with
//! backoff on any drop (per `docs/specs/relay-protocol.md`'s transport
//! requirement). Since M1, an `rpc`-purpose tunnel offered on that
//! connection is accepted and its `host-rpc.md` traffic dispatched via
//! [`crate::rpc`] — see `docs/milestones/M1-workspace-and-jj.md`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use choosh_protocol::relay::{
    AuthResult, ClientAuth, ControlRequest, ControlResponse, DeviceAuth, FRAME_CLASS_CONTROL, FRAME_CLASS_TUNNEL,
    IdentityClass, ServerHello, ServerPush, TUNNEL_ID_BYTES, decode_tunnel_id_hex,
};
use ed25519_dalek::SigningKey;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::backoff::compute_backoff;
use crate::credential::{self, Credential, CredentialError};
use crate::frame_channel::FrameChannel;
use crate::local_ipc;
use crate::pty::{PtySession, PtyWriteHalf};
use crate::rpc::{self, RpcContext};
use crate::zellij_ops;
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

    let rpc_context = build_rpc_context(&credential.device_id)?;

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
                tokio::spawn(local_ipc::serve_forever(listener, agent_event_tx.clone()));
            }
            Err(error) => tracing::error!(%error, "failed to bind local IPC socket; agent hooks will not be delivered"),
        },
        Err(error) => tracing::error!(%error, "failed to determine local IPC socket path; agent hooks will not be delivered"),
    }

    // The loopback SSH bridge (ssh-bridge-and-zed.md): started once for
    // this `serve` process's lifetime, same as the local IPC listener
    // above — every `"ssh"`-purpose tunnel offered on every relayd
    // reconnect bridges to this same long-lived port, the exact pattern
    // `zellij-web`'s on-demand port already uses for `web:`/`zellij-web`
    // tunnels (see `open_ssh_bridge_tunnel`). Its own `SessionHandler`
    // pushes `editor_attached`/`editor_detached` into this same
    // `agent_event_tx` — see `ssh_server::SshBridgeConfig`'s doc comment.
    let signing_key = credential.signing_key().map_err(ServeError::Credential)?;
    let ssh_bridge_config = crate::ssh_server::SshBridgeConfig {
        registry: rpc_context.registry.clone(),
        mise_host_tools_dir: mise_host_tools_dir()?,
        mise_bin: crate::mise_ops::mise_bin_from_env(),
        agent_event_tx,
    };
    let ssh_port = match crate::ssh_server::spawn_loopback_server(&signing_key, ssh_bridge_config).await {
        Ok(port) => {
            tracing::info!(port, "loopback SSH server listening");
            Some(port)
        }
        Err(error) => {
            tracing::error!(%error, "failed to start the loopback SSH server; \"ssh\"-purpose tunnels will not be served");
            None
        }
    };

    connect_loop(&config, &credential, &rpc_context, agent_event_rx, ssh_port).await;
    Ok(())
}

/// `choosh-hostd`'s own dedicated `mise` data/config root for host-managed
/// tools (`mise_ops`'s doc comment) — distinct from any workspace's own
/// `mise.toml` resolution, per toolchain-provisioning.md's isolation
/// requirement.
fn mise_host_tools_dir() -> Result<PathBuf, ServeError> {
    match std::env::var("CHOOSH_HOSTD_MISE_HOST_TOOLS_DIR") {
        Ok(path) => Ok(PathBuf::from(path)),
        Err(_) => Ok(directories::ProjectDirs::from("ai", "choosh", "hostd")
            .ok_or(ServeError::Credential(CredentialError::NoConfigDir))?
            .data_dir()
            .join("mise-host-tools")),
    }
}

/// Builds an [`RpcContext`] against this devhost's on-disk registry.
/// `pub(crate)` (not `fn`-private) because `choosh-hostd service run`
/// (`lib.rs`) reuses this exact same path for its one-shot, non-`serve`
/// invocation — the whole point of that CLI form is to drive the same
/// `item.create` RPC logic `serve` uses, not a parallel reimplementation,
/// so it needs to build the same kind of context `serve` does.
///
/// Takes a bare `device_id` rather than a full [`Credential`]: `serve`'s
/// own call site has a real one, but `service run` has no relayd
/// connection at all (and therefore no credential requirement) — it just
/// needs *some* stable string for `RpcContext::devhost_id`, which nothing
/// in `item.create`'s path actually validates against an enrolled
/// identity (see `rpc::handle_create`'s use of `ctx.devhost_id`: only
/// `workspace.create`, registering a *new* workspace, ever reads it, and
/// `service run` only ever targets an already-registered `--workspace`).
pub(crate) fn build_rpc_context(device_id: &str) -> Result<RpcContext, ServeError> {
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
    Ok(RpcContext {
        registry: std::sync::Arc::new(tokio::sync::Mutex::new(registry)),
        devhost_id: device_id.to_string(),
        workspaces_dir,
    })
}

/// Best-effort device id for `service run`'s standalone `RpcContext`: this
/// path has no relayd connection and therefore no requirement to actually
/// be enrolled, but a persisted device credential (from a `serve` run on
/// the same devhost) is a more meaningful `devhost_id` than a placeholder
/// when one happens to already exist.
pub(crate) fn best_effort_device_id() -> String {
    let loaded = credential::default_path().ok().and_then(|path| credential::load(&path).ok().flatten());
    loaded.map_or_else(|| "local-cli".to_string(), |credential| credential.device_id)
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

    // auth-and-enrollment.md step 6: this devhost's loopback SSH server's
    // host public key is established as trusted at this exact moment.
    // Per ssh_keys's doc comment, that key is this same enrollment signing
    // key (not a second, independently generated one) reformatted as a
    // raw Ed25519 public key — the same base64 encoding `public_key`
    // above already uses.
    let host_ssh_public_key_b64 = base64_encode(&crate::ssh_keys::raw_public_key_bytes(&signing_key));

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
                host_ssh_public_key: Some(host_ssh_public_key_b64),
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
    ssh_port: Option<u16>,
) {
    let mut attempt: u32 = 0;
    loop {
        let shutdown = tokio::signal::ctrl_c();
        tokio::select! {
            () = run_one_connection(config, credential, rpc_context, &mut agent_event_rx, ssh_port) => {
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
    ssh_port: Option<u16>,
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

    serve_dispatch(&mut channel, rpc_context, agent_event_rx, ssh_port).await;
}

type WsChannel = FrameChannel<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>;

/// One item of output — PTY bytes or raw TCP-bridge bytes, tagged with the
/// tunnel it belongs to. Produced by a per-tunnel background reader task
/// (spawned when a `pty:`/`web:`/`zellij-web`-purpose tunnel is offered),
/// consumed by [`serve_dispatch`]'s main loop, which is the only place
/// still holding the shared `channel` to actually send it. Shared by both
/// the pty and web/TCP-bridge paths — they differ only in what backs the
/// write half ([`PtyWriteHalf`] vs. [`WebWriteHalf`]), not in how output
/// flows back to the phone.
struct TunnelOutput {
    tunnel_id: [u8; TUNNEL_ID_BYTES],
    /// An empty payload is the tunnel close signal, same as everywhere
    /// else in this protocol — see [`open_tcp_bridge_tunnel`]'s doc
    /// comment for where the web/TCP-bridge path proactively sends one
    /// (unlike the pty path, which never does; see that path's own
    /// comments for why that asymmetry is deliberate here).
    bytes: Vec<u8>,
}

/// Idle-read timeout for a web/TCP-bridge tunnel's background reader task
/// (both the `web:<item_id>` registered-service path and the `zellij-web`
/// break-glass path): if the local process/server this bridges to sends
/// nothing at all for this long, the bridge tears itself down rather than
/// leaking a task and a loopback TCP connection forever for a tunnel the
/// phone may have already abandoned without ever sending the close frame
/// relay-protocol.md's tunnel lifecycle otherwise relies on. Chosen
/// generously: long enough not to trip on an ordinary quiet dev-server
/// connection or a slow-but-legitimate SSE/long-poll stream sitting idle
/// between events, short enough to actually bound the leak in practice.
/// This is deliberately just a safety backstop, not a general-purpose
/// per-connection lifetime cap — `service-tunnels.md`'s fuller set of caps
/// (backpressure, connection count, buffered-byte limits, header caps)
/// belongs to the Android gateway, not this dumb byte pipe (see this
/// module's doc comment on the `web:`/`zellij-web` handling below for the
/// full reasoning).
const WEB_TUNNEL_IDLE_TIMEOUT: Duration = Duration::from_mins(10);

/// Per-read buffer size for the web/TCP-bridge reader task, matching
/// `pty.rs`'s own PTY-reader loop's buffer size — together with the
/// bounded `tunnel_output_tx` channel below (capacity 64), this keeps the
/// amount of never-yet-sent-to-relayd data a stalled connection can
/// accumulate in memory to a small, fixed multiple of this constant rather
/// than unbounded: this dumb pipe's whole "buffered bytes" safety bound,
/// per this task's own scope note that the richer caps in
/// `service-tunnels.md`'s Tunnel section belong to the Android gateway,
/// not here.
const WEB_TUNNEL_READ_BUF_SIZE: usize = 8192;

/// Processes frames on an authenticated connection until it ends: `rpc`-,
/// `pty:<item_id>`-, `web:<item_id>`-, and `zellij-web`-purpose tunnels
/// (`docs/specs/host-rpc.md`, `docs/milestones/M2-terminal-and-agents.md`,
/// `docs/specs/service-tunnels.md`), plus forwarding any locally-emitted
/// agent events (from `choosh-hostd emit`, via `local_ipc`) as
/// `agent-event` control requests.
async fn serve_dispatch(
    channel: &mut WsChannel,
    rpc_context: &RpcContext,
    agent_event_rx: &mut tokio::sync::mpsc::Receiver<WireAgentEvent>,
    ssh_port: Option<u16>,
) {
    // `rpc`-purpose tunnels offered on this connection are tracked here;
    // per relay-protocol.md's reconnect-discontinuity rule, tunnels never
    // survive a reconnect, so this set is deliberately scoped to one
    // connection attempt, not `connect_loop`'s outer state. Same for
    // `pty_tunnels`/`web_tunnels` and the output-forwarding channel below.
    let mut rpc_tunnels: HashSet<[u8; TUNNEL_ID_BYTES]> = HashSet::new();
    let mut pty_tunnels: HashMap<[u8; TUNNEL_ID_BYTES], PtyWriteHalf> = HashMap::new();
    let mut web_tunnels: HashMap<[u8; TUNNEL_ID_BYTES], WebWriteHalf> = HashMap::new();
    let (tunnel_output_tx, mut tunnel_output_rx) = tokio::sync::mpsc::channel::<TunnelOutput>(64);

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

            Some(output) = tunnel_output_rx.recv() => {
                let is_web = web_tunnels.contains_key(&output.tunnel_id);
                if !pty_tunnels.contains_key(&output.tunnel_id) && !is_web {
                    continue; // the tunnel closed after this output was already queued; drop it.
                }
                let mut payload = Vec::with_capacity(TUNNEL_ID_BYTES + output.bytes.len());
                payload.extend_from_slice(&output.tunnel_id);
                payload.extend_from_slice(&output.bytes);
                if let Err(error) = channel.send_bytes(FRAME_CLASS_TUNNEL, &payload).await {
                    tracing::warn!(%error, "failed to send tunnel output");
                    return;
                }
                if is_web && output.bytes.is_empty() {
                    // The local TCP connection closed on its own (EOF, an
                    // idle timeout, or a read error) — this was the
                    // proactive close-frame path `open_tcp_bridge_tunnel`
                    // documents. Drop the write half now so a
                    // subsequently-arriving phone-originated frame for
                    // this tunnel_id is cleanly treated as "unknown tunnel"
                    // rather than writing into an already-dead socket.
                    web_tunnels.remove(&output.tunnel_id);
                }
            }

            frame = channel.recv_raw() => match frame {
                Ok((FRAME_CLASS_CONTROL, body)) => {
                    handle_control_push(&body, rpc_context, &tunnel_output_tx, &mut rpc_tunnels, &mut pty_tunnels, &mut web_tunnels, ssh_port).await;
                }
                Ok((FRAME_CLASS_TUNNEL, body)) => {
                    if handle_tunnel_frame(&body, channel, rpc_context, &mut rpc_tunnels, &mut pty_tunnels, &mut web_tunnels).await == FrameOutcome::Disconnect {
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

/// Handles one `FRAME_CLASS_CONTROL` frame: `tunnel-offered` pushes
/// (`rpc`, `pty:<item_id>`, `web:<item_id>`, and `zellij-web` purposes) and
/// `AgentEvent` pushes (rejected — devhost connections never legitimately
/// receive one, see the inline comment). Split out of [`serve_dispatch`]
/// to keep that function's line count reasonable; takes the
/// tunnel-tracking maps by reference rather than owning them since
/// [`serve_dispatch`]'s loop needs them across iterations.
///
/// **`web:<item_id>` and `zellij-web`** (`docs/specs/service-tunnels.md`'s
/// Tunnel and Zellij-web-client-break-glass sections): both are a plain,
/// dumb `tokio::net::TcpStream` bridge to a loopback port — no PTY, no
/// `openpty`, much simpler than the `pty:` path above. `web:<item_id>`
/// resolves `item_id` to its registered `WebService` item's declared port
/// via the registry (rejecting anything that doesn't resolve to a real,
/// currently-`running` `WebService` item — a stale or malicious `item_id`
/// must never cause an arbitrary TCP connect); `zellij-web` instead points
/// at Zellij's own web-client server (`zellij web`, confirmed to be a
/// real, working HTTP+WebSocket terminal server in this environment — see
/// `zellij_ops::ensure_web_server_running`'s doc comment), started
/// on-demand if it isn't already running. Both purposes share the exact
/// same bridge implementation ([`open_tcp_bridge_tunnel`]) and the same
/// `web_tunnels` map — per `service-tunnels.md`'s own framing, `zellij-web`
/// is "the same tunnel mechanism... with no new transport code", not a
/// second, parallel implementation.
///
/// Deliberately does **not** parse or transform the bytes flowing through
/// either purpose (no HTTP parsing, no WebSocket-upgrade awareness) — that
/// posture matches `relayd`'s own "MUST NOT parse or transform" rule
/// (DESIGN.md §2.3) and the existing `pty:` bridge above. The richer set of
/// caps `service-tunnels.md`'s Tunnel section lists (backpressure,
/// WebSocket upgrade, SSE, header caps, connection-count caps) is
/// explicitly the Android gateway's job, a separate, parallel piece of
/// work — this bridge's own responsibility is limited to the safety bound
/// documented on [`WEB_TUNNEL_IDLE_TIMEOUT`]/[`WEB_TUNNEL_READ_BUF_SIZE`].
async fn handle_control_push(
    body: &[u8],
    rpc_context: &RpcContext,
    tunnel_output_tx: &tokio::sync::mpsc::Sender<TunnelOutput>,
    rpc_tunnels: &mut HashSet<[u8; TUNNEL_ID_BYTES]>,
    pty_tunnels: &mut HashMap<[u8; TUNNEL_ID_BYTES], PtyWriteHalf>,
    web_tunnels: &mut HashMap<[u8; TUNNEL_ID_BYTES], WebWriteHalf>,
    ssh_port: Option<u16>,
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
            match open_pty_tunnel(rpc_context, item_id, id, tunnel_output_tx.clone()).await {
                Ok(write_half) => {
                    pty_tunnels.insert(id, write_half);
                }
                Err(error) => tracing::warn!(%error, item_id, "failed to attach pty for offered tunnel"),
            }
        }
        Ok(ServerPush::TunnelOffered { tunnel_id, purpose, .. }) if purpose.starts_with("web:") => {
            let item_id = &purpose["web:".len()..];
            let Some(id) = decode_tunnel_id_hex(&tunnel_id) else {
                tracing::warn!(tunnel_id, "tunnel-offered carried a malformed tunnel_id");
                return;
            };
            match open_web_tunnel(rpc_context, item_id, id, tunnel_output_tx.clone()).await {
                Ok(write_half) => {
                    web_tunnels.insert(id, write_half);
                }
                Err(error) => tracing::warn!(%error, item_id, "failed to attach web tunnel for offered tunnel"),
            }
        }
        // ssh-bridge-and-zed.md's "Loopback SSH server" section: admitted
        // solely because it arrived as a tunnel-offer on this already
        // relayd-authenticated connection — `relayd` itself already
        // restricted which Identity classes/purposes could reach this
        // point (auth-and-enrollment.md's capability table: `laptop-proxy`
        // `open-tunnel` is scoped to `purpose = "ssh"` only, and the
        // break-glass path allows a `phone`). The one thing this bridge
        // still checks locally, per that same section ("MUST NOT proceed
        // if the tunnel-open control frame is missing or malformed"), is
        // that `from_device_id` is actually present — an empty identity
        // claim is treated as malformed, not silently bridged.
        Ok(ServerPush::TunnelOffered { tunnel_id, from_device_id, purpose }) if purpose == "ssh" => {
            let Some(id) = decode_tunnel_id_hex(&tunnel_id) else {
                tracing::warn!(tunnel_id, "tunnel-offered carried a malformed tunnel_id");
                return;
            };
            if from_device_id.trim().is_empty() {
                tracing::warn!("refusing ssh-purpose tunnel-offered with no requester identity");
                return;
            }
            let Some(port) = ssh_port else {
                tracing::warn!("ssh-purpose tunnel offered but the loopback SSH server is not running");
                return;
            };
            match open_tcp_bridge_tunnel(port, id, tunnel_output_tx.clone()).await {
                Ok(write_half) => {
                    web_tunnels.insert(id, write_half);
                }
                Err(error) => tracing::warn!(%error, "failed to bridge offered ssh tunnel to the loopback SSH server"),
            }
        }
        Ok(ServerPush::TunnelOffered { tunnel_id, purpose, .. }) if purpose == "zellij-web" => {
            let Some(id) = decode_tunnel_id_hex(&tunnel_id) else {
                tracing::warn!(tunnel_id, "tunnel-offered carried a malformed tunnel_id");
                return;
            };
            match open_zellij_web_tunnel(id, tunnel_output_tx.clone()).await {
                Ok(write_half) => {
                    web_tunnels.insert(id, write_half);
                }
                Err(error) => tracing::warn!(%error, "failed to attach zellij-web break-glass tunnel"),
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

/// Handles one `FRAME_CLASS_TUNNEL` frame: routes to an active pty or web
/// tunnel's write half, or dispatches as an `rpc`-tunnel `host-rpc.md`
/// request. Returns [`FrameOutcome::Disconnect`] only for a malformed
/// frame or a send failure that per relay-protocol.md means the
/// connection itself is unrecoverable — every other outcome is
/// [`FrameOutcome::Continue`].
async fn handle_tunnel_frame(
    body: &[u8],
    channel: &mut WsChannel,
    rpc_context: &RpcContext,
    rpc_tunnels: &mut HashSet<[u8; TUNNEL_ID_BYTES]>,
    pty_tunnels: &mut HashMap<[u8; TUNNEL_ID_BYTES], PtyWriteHalf>,
    web_tunnels: &mut HashMap<[u8; TUNNEL_ID_BYTES], WebWriteHalf>,
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

    if let Some(mut write_half) = web_tunnels.remove(&tunnel_id) {
        if payload.is_empty() {
            return FrameOutcome::Continue; // zero-payload close signal — dropping write_half above shuts down the TCP connection's write side (tokio::net::tcp::OwnedWriteHalf::drop).
        }
        if let Err(error) = write_half.write_all(payload).await {
            tracing::debug!(%error, "web tunnel write failed, tunnel is presumably closing");
            return FrameOutcome::Continue; // do not re-insert; the tunnel is done.
        }
        web_tunnels.insert(tunnel_id, write_half);
        return FrameOutcome::Continue;
    }

    if !rpc_tunnels.contains(&tunnel_id) {
        tracing::debug!("tunnel frame for an unknown/non-rpc/non-pty/non-web tunnel id, ignoring");
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
    output_tx: tokio::sync::mpsc::Sender<TunnelOutput>,
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
                    if output_tx.send(TunnelOutput { tunnel_id, bytes: buf[..n].to_vec() }).await.is_err() {
                        return; // serve_dispatch's loop has ended; nothing left to forward to.
                    }
                }
            }
        }
    });
    Ok(write_half)
}

/// The write half of a `web:<item_id>`/`zellij-web` TCP bridge — a plain
/// wrapper over `tokio::net::tcp::OwnedWriteHalf` so [`handle_tunnel_frame`]
/// can call `write_all` the same shape as [`PtyWriteHalf`], keeping the two
/// tunnel kinds' handling symmetric in `serve_dispatch`/`handle_tunnel_frame`
/// above. Unlike [`PtyWriteHalf`], dropping this does not kill any child
/// process (there isn't one) — it just shuts down the TCP connection's
/// write direction, per `tokio::net::tcp::OwnedWriteHalf`'s own documented
/// drop behavior.
struct WebWriteHalf {
    inner: tokio::net::tcp::OwnedWriteHalf,
}

impl WebWriteHalf {
    async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.inner.write_all(buf).await
    }
}

/// Resolves `item_id` to its registered `WebService` item's declared port
/// via the registry — refusing (not just logging, actually returning
/// `Err` and never dialing anything) unless it resolves to a real,
/// currently-`running`-status `WebService` item, per
/// `service-tunnels.md`'s Tunnel section: a stale or malicious `item_id`
/// must never cause an arbitrary loopback TCP connect.
async fn open_web_tunnel(
    rpc_context: &RpcContext,
    item_id: &str,
    tunnel_id: [u8; TUNNEL_ID_BYTES],
    output_tx: tokio::sync::mpsc::Sender<TunnelOutput>,
) -> Result<WebWriteHalf, String> {
    let port = {
        let registry = rpc_context.registry.lock().await;
        let item = registry.find_item(item_id).ok_or_else(|| format!("item {item_id:?} is not registered"))?;
        if item.item_type != choosh_protocol::host_rpc::ItemType::WebService {
            return Err(format!("item {item_id:?} is not a WebService item"));
        }
        if item.status != choosh_protocol::host_rpc::ItemStatus::Running {
            return Err(format!("item {item_id:?} is not currently running (status: {:?})", item.status));
        }
        item.port.ok_or_else(|| format!("item {item_id:?} has no declared port"))?
    };
    open_tcp_bridge_tunnel(port, tunnel_id, output_tx).await
}

/// Ensures Zellij's own web-client server is running (starting it
/// on-demand if not — see `zellij_ops::ensure_web_server_running`'s doc
/// comment for the real, confirmed-present `zellij web` capability this
/// relies on) and bridges to it exactly like [`open_web_tunnel`] does to a
/// registered service's port — the phone-only break-glass path,
/// `docs/specs/service-tunnels.md`'s last section.
async fn open_zellij_web_tunnel(
    tunnel_id: [u8; TUNNEL_ID_BYTES],
    output_tx: tokio::sync::mpsc::Sender<TunnelOutput>,
) -> Result<WebWriteHalf, String> {
    let port = zellij_ops::ensure_web_server_running().await.map_err(|error| error.to_string())?;
    open_tcp_bridge_tunnel(port, tunnel_id, output_tx).await
}

/// The actual TCP-bridge mechanism shared by [`open_web_tunnel`] and
/// [`open_zellij_web_tunnel`]: dials `127.0.0.1:<port>` and spawns a
/// background task that forwards everything it reads into `output_tx`
/// tagged with `tunnel_id`, mirroring [`open_pty_tunnel`]'s shape closely
/// but over a plain socket instead of a pty master. Unlike the pty path,
/// this proactively sends a zero-length `TunnelOutput` (the close signal,
/// same convention as every other tunnel kind here) the moment the local
/// connection ends for any reason — EOF, a read error, or
/// [`WEB_TUNNEL_IDLE_TIMEOUT`] elapsing with nothing read — so the far end
/// finds out promptly instead of the tunnel silently going one-way dead
/// until it separately times out. (The pty path doesn't do this today; see
/// this module's other doc comments — that's a pre-existing, narrower gap
/// this change deliberately doesn't also fix, since `service-tunnels.md`
/// only calls out this requirement for the web/TCP-bridge case.)
async fn open_tcp_bridge_tunnel(
    port: u16,
    tunnel_id: [u8; TUNNEL_ID_BYTES],
    output_tx: tokio::sync::mpsc::Sender<TunnelOutput>,
) -> Result<WebWriteHalf, String> {
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.map_err(|error| error.to_string())?;
    let (mut read_half, write_half) = stream.into_split();
    tokio::spawn(async move {
        let mut buf = [0u8; WEB_TUNNEL_READ_BUF_SIZE];
        loop {
            match tokio::time::timeout(WEB_TUNNEL_IDLE_TIMEOUT, read_half.read(&mut buf)).await {
                Ok(Ok(0)) => {
                    let _ = output_tx.send(TunnelOutput { tunnel_id, bytes: Vec::new() }).await;
                    return;
                }
                Ok(Ok(n)) => {
                    if output_tx.send(TunnelOutput { tunnel_id, bytes: buf[..n].to_vec() }).await.is_err() {
                        return; // serve_dispatch's loop has ended; nothing left to forward to.
                    }
                }
                Ok(Err(error)) => {
                    tracing::debug!(%error, "web tunnel read failed, closing");
                    let _ = output_tx.send(TunnelOutput { tunnel_id, bytes: Vec::new() }).await;
                    return;
                }
                Err(_elapsed) => {
                    tracing::debug!("web tunnel idle timeout elapsed, closing");
                    let _ = output_tx.send(TunnelOutput { tunnel_id, bytes: Vec::new() }).await;
                    return;
                }
            }
        }
    });
    Ok(WebWriteHalf { inner: write_half })
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

/// Integration coverage for the `web:<item_id>` tunnel-serving path
/// (`docs/specs/service-tunnels.md`), against a hand-rolled fake `relayd`
/// (same style as `tests/serve_enrollment.rs`) and a real, minimal
/// tokio-based HTTP server standing in for a registered `WebService`'s dev
/// server — real bytes flow through the real `serve_dispatch`/
/// `handle_control_push`/`handle_tunnel_frame`/`open_web_tunnel` path,
/// nothing here is mocked at the boundary being tested. Lives inside
/// `serve.rs` itself (not `tests/`) because `serve_dispatch` and `dial`
/// are private — the same reason this crate's other "drive the private
/// dispatch loop directly" tests don't exist as external integration
/// tests either.
///
/// Uses `tokio::join!` rather than `tokio::spawn` for both the fake
/// relayd's connection-handling future and the
/// `dial`-then-`serve_dispatch` future: `spawn` requires `'static`, but
/// both futures here borrow this test function's own locals (`ctx`, the
/// `WsChannel`) — `join!` polls them concurrently within the same task
/// without that requirement, and does not need connect-then-handshake to
/// be sequenced up front either (both sides of the WebSocket handshake
/// only complete once *both* futures are being polled).
#[cfg(test)]
mod tunnel_tests {
    use super::*;
    use choosh_protocol::framing::{FrameDecoder, FrameLimits, encode_frame};
    use choosh_protocol::host_rpc::{ItemStatus, ItemType};
    use choosh_protocol::relay::{FRAME_CLASS_CONTROL as CTRL, FRAME_CLASS_TUNNEL as TUNNEL, MAX_CONTROL_FRAME_BYTES, MAX_TUNNEL_FRAME_BYTES, encode_tunnel_id_hex};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    async fn bind_fake_relayd() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, format!("ws://{addr}/connect"))
    }

    /// A real, minimal TCP server speaking real HTTP/1.1 request/response
    /// bytes — not a library, but genuine wire-format HTTP text, which is
    /// what this test needs to round-trip through the bridge. Echoes the
    /// request's first line back in the response body so the test can
    /// confirm the *exact* bytes it sent reached the server, not just that
    /// *some* response came back.
    async fn spawn_test_http_server() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    let request_line = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string();
                    let body = format!("choosh-test-http-server saw: {request_line}");
                    let response =
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        port
    }

    /// Registers a bare `WebService` item with no backing workspace or
    /// Zellij session — `open_web_tunnel` never looks either up (that's
    /// the whole point of this path being simpler than the `pty:` one), so
    /// this test doesn't need to build either just to exercise it.
    fn registry_with_web_service_item(port: u16, status: ItemStatus) -> (tempfile::TempDir, RpcContext, String) {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = crate::registry::Registry::load(&dir.path().join("registry.json")).unwrap();
        let item_id = "item-1".to_string();
        registry
            .register_item(item_id.clone(), "ws-untouched".to_string(), ItemType::WebService, "web".to_string(), "web".to_string(), None, Some(port), status)
            .unwrap();
        let ctx = RpcContext {
            registry: std::sync::Arc::new(tokio::sync::Mutex::new(registry)),
            devhost_id: "dev-1".to_string(),
            workspaces_dir: dir.path().to_path_buf(),
        };
        (dir, ctx, item_id)
    }

    /// Sends a `tunnel-offered` control push for `purpose`, then a single
    /// tunnel-data frame carrying `request_bytes` — standing in for what
    /// `relayd` forwards from a phone, per relay-protocol.md.
    /// `serve_dispatch` has no way to distinguish "genuinely relayed from a
    /// phone" from "this test sent it directly": it only ever sees frames
    /// on its `WsChannel`, which is exactly what makes this a faithful test
    /// of the real dispatch path.
    async fn send_tunnel_offer_and_data(
        ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        tunnel_id: [u8; TUNNEL_ID_BYTES],
        purpose: &str,
        request_bytes: &[u8],
    ) {
        let push = ServerPush::TunnelOffered { tunnel_id: encode_tunnel_id_hex(tunnel_id), from_device_id: "phone-1".to_string(), purpose: purpose.to_string() };
        let mut control_payload = vec![CTRL];
        control_payload.extend(serde_json::to_vec(&push).unwrap());
        let control_wire = encode_frame(&control_payload, MAX_CONTROL_FRAME_BYTES).unwrap();
        ws.send(Message::Binary(control_wire.into())).await.unwrap();

        let mut tunnel_payload = vec![TUNNEL];
        tunnel_payload.extend_from_slice(&tunnel_id);
        tunnel_payload.extend_from_slice(request_bytes);
        let tunnel_wire = encode_frame(&tunnel_payload, MAX_TUNNEL_FRAME_BYTES).unwrap();
        ws.send(Message::Binary(tunnel_wire.into())).await.unwrap();
    }

    #[tokio::test]
    async fn web_tunnel_bridges_real_http_bytes_end_to_end_through_serve_dispatch() {
        let http_port = spawn_test_http_server().await;
        let (_dir, ctx, item_id) = registry_with_web_service_item(http_port, ItemStatus::Running);
        let (listener, relay_url) = bind_fake_relayd().await;
        let (_agent_tx, mut agent_event_rx) = tokio::sync::mpsc::channel(1);

        let dispatch_and_dial = async {
            let mut channel = dial(&relay_url).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(15), serve_dispatch(&mut channel, &ctx, &mut agent_event_rx, None)).await;
        };

        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

            let tunnel_id = [9u8, 8, 7, 6, 5, 4, 3, 2];
            let request: &[u8] = b"GET /marker HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n";
            send_tunnel_offer_and_data(&mut ws, tunnel_id, &format!("web:{item_id}"), request).await;

            let mut decoder = FrameDecoder::new(FrameLimits::new(MAX_TUNNEL_FRAME_BYTES, 8).unwrap());
            let mut collected = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                assert!(tokio::time::Instant::now() < deadline, "timed out waiting for the web tunnel's HTTP response; collected so far: {:?}", String::from_utf8_lossy(&collected));
                let Ok(Some(Ok(Message::Binary(bytes)))) = tokio::time::timeout(Duration::from_secs(2), ws.next()).await else { continue };
                for frame in decoder.feed(&bytes).unwrap() {
                    let (class, body) = frame.split_first().unwrap();
                    assert_eq!(*class, TUNNEL);
                    let (id_bytes, payload) = body.split_at(TUNNEL_ID_BYTES);
                    assert_eq!(id_bytes, tunnel_id);
                    if payload.is_empty() {
                        let _ = ws.close(None).await;
                        return collected; // the close signal — the test http server closed its end (Connection: close).
                    }
                    collected.extend_from_slice(payload);
                }
            }
        };

        let (collected, ()) = tokio::join!(server_fut, dispatch_and_dial);
        let text = String::from_utf8_lossy(&collected);
        assert!(text.contains("HTTP/1.1 200 OK"), "expected a real HTTP response, got: {text:?}");
        assert!(text.contains("GET /marker HTTP/1.1"), "expected the server to echo back exactly what it received, got: {text:?}");
    }

    /// `service-tunnels.md`'s requirement that a `web:<item_id>` offer
    /// targeting anything other than a real, currently-`running`
    /// `WebService` item must be refused rather than dialed — proven here
    /// by pointing a `starting`-status item at a server that would happily
    /// accept the connection if `open_web_tunnel` incorrectly ignored
    /// status, and confirming no response ever arrives.
    #[tokio::test]
    async fn web_tunnel_is_refused_for_a_non_running_webservice_item() {
        let http_port = spawn_test_http_server().await;
        let (_dir, ctx, item_id) = registry_with_web_service_item(http_port, ItemStatus::Starting);
        let (listener, relay_url) = bind_fake_relayd().await;
        let (_agent_tx, mut agent_event_rx) = tokio::sync::mpsc::channel(1);

        let dispatch_and_dial = async {
            let mut channel = dial(&relay_url).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(10), serve_dispatch(&mut channel, &ctx, &mut agent_event_rx, None)).await;
        };

        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let tunnel_id = [1u8; TUNNEL_ID_BYTES];
            send_tunnel_offer_and_data(&mut ws, tunnel_id, &format!("web:{item_id}"), b"GET / HTTP/1.1\r\n\r\n").await;

            let result = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
            assert!(result.is_err(), "expected no tunnel response for a non-running item, got {result:?}");
            let _ = ws.close(None).await;
        };

        tokio::join!(server_fut, dispatch_and_dial);
    }

    /// Proves the `"ssh"`-purpose tunnel-offer wiring end to end: a real
    /// tunnel-offered push (as `relayd` would forward from an
    /// authenticated `laptop-proxy`/`phone` Identity opening an
    /// `open-tunnel { purpose: "ssh" }`) drives `handle_control_push`'s new
    /// `"ssh"` arm, which bridges to a *real*, running
    /// `ssh_server::spawn_loopback_server` instance via the exact same
    /// `open_tcp_bridge_tunnel` mechanism the `web:`/`zellij-web` tests
    /// above already exercise — proven here by observing the real SSH
    /// server's own protocol version banner come back through the tunnel,
    /// which only a genuine SSH server on the other end would send.
    /// `ssh_server`'s own `session_tests` module covers the deeper
    /// shell/exec session behavior directly against the bound port (see
    /// its doc comment for why that's the right split); this test's job
    /// is only the tunnel-to-port bridging this module owns.
    #[tokio::test]
    async fn ssh_purpose_tunnel_bridges_to_the_real_loopback_ssh_server() {
        let dir = tempfile::tempdir().unwrap();
        let signing_key = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let registry = crate::registry::Registry::load(&dir.path().join("registry.json")).unwrap();
        let (agent_event_tx, _agent_event_rx_for_ssh_server) = tokio::sync::mpsc::channel(16);
        let ssh_config = crate::ssh_server::SshBridgeConfig {
            registry: std::sync::Arc::new(tokio::sync::Mutex::new(registry)),
            mise_host_tools_dir: dir.path().join("mise-host-tools"),
            mise_bin: "mise-not-used-in-this-test".to_string(),
            agent_event_tx,
        };
        let ssh_port = crate::ssh_server::spawn_loopback_server(&signing_key, ssh_config).await.unwrap();

        // `serve_dispatch` needs *some* `RpcContext` to run at all, but the
        // `"ssh"`-purpose path itself never touches the registry (unlike
        // `pty:`/`web:`, which resolve an `item_id` through it) — an
        // otherwise-empty context is all this test needs.
        let ctx_registry = crate::registry::Registry::load(&dir.path().join("rpc-registry.json")).unwrap();
        let ctx = RpcContext {
            registry: std::sync::Arc::new(tokio::sync::Mutex::new(ctx_registry)),
            devhost_id: "dev-1".to_string(),
            workspaces_dir: dir.path().to_path_buf(),
        };
        let (listener, relay_url) = bind_fake_relayd().await;
        let (_agent_tx, mut agent_event_rx) = tokio::sync::mpsc::channel(1);

        let dispatch_and_dial = async {
            let mut channel = dial(&relay_url).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(10), serve_dispatch(&mut channel, &ctx, &mut agent_event_rx, Some(ssh_port))).await;
        };

        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let tunnel_id = [4u8; TUNNEL_ID_BYTES];
            // The exact bytes don't matter for this test — a real SSH
            // server unconditionally sends its own version banner the
            // instant a client connects, before reading anything, per the
            // SSH transport protocol (RFC 4253 §4.2). The payload just
            // needs to be non-empty: an empty tunnel-data frame is this
            // protocol's close signal (relay-protocol.md), which would
            // tear the bridged connection down before any banner comes
            // back.
            send_tunnel_offer_and_data(&mut ws, tunnel_id, "ssh", b"\n").await;

            let mut decoder = FrameDecoder::new(FrameLimits::new(MAX_TUNNEL_FRAME_BYTES, 8).unwrap());
            let mut collected = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                assert!(tokio::time::Instant::now() < deadline, "timed out waiting for the SSH server's version banner");
                let Ok(Some(Ok(Message::Binary(bytes)))) = tokio::time::timeout(Duration::from_secs(2), ws.next()).await else { continue };
                for frame in decoder.feed(&bytes).unwrap() {
                    let (class, body) = frame.split_first().unwrap();
                    assert_eq!(*class, TUNNEL);
                    let (id_bytes, payload) = body.split_at(TUNNEL_ID_BYTES);
                    assert_eq!(id_bytes, tunnel_id);
                    collected.extend_from_slice(payload);
                    if collected.starts_with(b"SSH-2.0-") {
                        let _ = ws.close(None).await;
                        return;
                    }
                }
            }
        };

        tokio::join!(server_fut, dispatch_and_dial);
    }
}
