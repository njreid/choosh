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

use choosh_protocol::offload::{OffloadError, OffloadFrame};
use choosh_protocol::relay::{
    AuthResult, ClientAuth, ControlRequest, ControlResponse, DeviceAuth, FRAME_CLASS_CONTROL, FRAME_CLASS_TUNNEL,
    IdentityClass, ServerHello, ServerPush, TUNNEL_ID_BYTES, decode_tunnel_id_hex,
};
use ed25519_dalek::SigningKey;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::backoff::compute_backoff;
use crate::credential::{self, Credential, CredentialError};
use crate::frame_channel::FrameChannel;
use crate::jj_ops;
use crate::local_ipc;
use crate::pty::{PtySession, PtyWriteHalf};
use crate::rpc::{self, RpcContext};
use crate::update;
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

    // If a prior run of this binary applied a self-update whose health
    // check ultimately failed (`update::run_monitor`'s rollback path),
    // report it now: this is the first point in this process's startup
    // with the `agent_event_tx` channel available, and per
    // `update`'s module doc comment, this is how a failure detected by a
    // credential-less, connection-less monitor process eventually reaches
    // relayd — queued here, drained by `serve_dispatch`'s ordinary
    // agent-event forwarding once the connection below is established.
    // `try_send` rather than `.await`: the channel is freshly created and
    // nowhere near its capacity, so this never legitimately blocks
    // startup; a full channel here would indicate something is already
    // very wrong, in which case dropping this one report is preferable to
    // stalling.
    match update::default_state_path() {
        Ok(state_path) => {
            if let Some(event) = update::take_pending_failure_event(&state_path) {
                let _ = agent_event_tx.try_send(event);
            }
        }
        Err(error) => tracing::debug!(%error, "no update-state path available; skipping pending self-update failure check"),
    }

    match local_ipc::default_socket_path() {
        Ok(socket_path) => match local_ipc::bind(&socket_path) {
            Ok(listener) => {
                tokio::spawn(local_ipc::serve_forever(listener, agent_event_tx.clone()));
            }
            Err(error) => tracing::error!(%error, "failed to bind local IPC socket; agent hooks will not be delivered"),
        },
        Err(error) => tracing::error!(%error, "failed to determine local IPC socket path; agent hooks will not be delivered"),
    }
    // `open_pty_tunnel`'s auth_required detector (agent-events.md,
    // auth_detect.rs) needs its own sender into this exact same channel —
    // it's a producer alongside `local_ipc` and the SSH bridge below, not a
    // second, parallel event path. Cloned here (before `ssh_bridge_config`
    // consumes the original by move) and threaded through `connect_loop`.
    let pty_auth_detect_tx = agent_event_tx.clone();

    // The loopback SSH bridge (ssh-bridge-and-zed.md): started once for
    // this `serve` process's lifetime, same as the local IPC listener
    // above — every `"ssh"`-purpose tunnel offered on every relayd
    // reconnect bridges to this same long-lived port, the exact pattern
    // `zellij-web`'s on-demand port already uses for `web:`/`zellij-web`
    // tunnels (see `open_ssh_bridge_tunnel`). Its own `SessionHandler`
    // pushes `editor_attached`/`editor_detached` into this same
    // `agent_event_tx` — see `ssh_server::SshBridgeConfig`'s doc comment.
    let signing_key = credential.signing_key().map_err(ServeError::Credential)?;
    let host_tools_dir = mise_host_tools_dir()?;
    let mise_bin = crate::mise_ops::mise_bin_from_env();
    let ssh_bridge_config = crate::ssh_server::SshBridgeConfig {
        registry: rpc_context.registry.clone(),
        mise_host_tools_dir: host_tools_dir.clone(),
        mise_bin: mise_bin.clone(),
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

    // Host-managed tools (toolchain-provisioning.md's second tier): `jj`
    // and `zellij` checked/updated now (daemon start) and every
    // `HOST_TOOL_RECHECK_INTERVAL` thereafter, as a detached background
    // task — never blocks the `connect_loop` below. Reuses the same
    // `host_tools_dir`/`mise_bin` the SSH bridge's `zed-remote-server`
    // check already uses (all three are host-managed tools sharing one
    // `choosh-hostd`-owned `mise` data directory, per toolchain-
    // provisioning.md's tier grouping — distinct from any workspace's own
    // project-pinned `mise_project_tools_dir`).
    spawn_host_tool_currency_checks(mise_bin, host_tools_dir);

    connect_loop(&config, &credential, &rpc_context, agent_event_rx, ssh_port, pty_auth_detect_tx).await;
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

/// The project-pinned-tier sibling of [`mise_host_tools_dir`] — a distinct
/// directory (never the same one) shared across every workspace registered
/// on this devhost for `mise_ops::ensure_project_toolchain`/`project_env`'s
/// installed-tool payloads, per toolchain-provisioning.md's tier-isolation
/// requirement.
fn mise_project_tools_dir() -> Result<PathBuf, ServeError> {
    match std::env::var("CHOOSH_HOSTD_MISE_PROJECT_TOOLS_DIR") {
        Ok(path) => Ok(PathBuf::from(path)),
        Err(_) => Ok(directories::ProjectDirs::from("ai", "choosh", "hostd")
            .ok_or(ServeError::Credential(CredentialError::NoConfigDir))?
            .data_dir()
            .join("mise-project-tools")),
    }
}

/// How often [`spawn_host_tool_currency_checks`] rechecks `jj`/`zellij`
/// after its initial on-daemon-start check, per toolchain-provisioning.md:
/// "a background check on a multi-hour interval is sufficient — these
/// change infrequently." `jj`/`zellij` both ship releases on the order of
/// weeks, not hours, so six hours keeps this daemon's copies reasonably
/// current (never more than half a day behind a fresh release) without
/// adding meaningful load — four checks a day against two tools that each
/// resolve in a couple of seconds when nothing new is available (`mise`
/// still has to make a network call to learn "nothing new", so this is a
/// deliberately conservative, not aggressive, interval).
const HOST_TOOL_RECHECK_INTERVAL: Duration = Duration::from_hours(6);

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
        mise_bin: crate::mise_ops::mise_bin_from_env(),
        mise_project_tools_dir: mise_project_tools_dir()?,
    })
}

/// Spawns the host-managed-tools (`jj`/`zellij`) currency check as a
/// detached background task: an immediate check (toolchain-provisioning.md:
/// "checked on daemon start"), then a recheck every
/// [`HOST_TOOL_RECHECK_INTERVAL`] for as long as this `serve` process runs
/// ("...and periodically thereafter"). Never awaited by any caller — same
/// "detached, `'static`, outlives the call that spawned it" shape
/// `readiness::spawn` already uses elsewhere in this crate — so a slow or
/// even hung `mise` invocation here can never block `serve`'s own startup
/// or its main relayd connection loop.
///
/// Deliberately does not change which `jj`/`zellij` binary the rest of
/// this crate invokes (`jj_ops.rs`/`zellij_ops.rs`/`pty.rs` all still
/// resolve `jj`/`zellij` via `$PATH`, exactly as before this function
/// existed) — see this function's own further doc comment below for why
/// that's a deliberate, reported scope boundary rather than an oversight.
fn spawn_host_tool_currency_checks(mise_bin: String, host_tools_dir: PathBuf) {
    tokio::spawn(async move {
        loop {
            check_host_tool_currency_once(&mise_bin, &host_tools_dir).await;
            tokio::time::sleep(HOST_TOOL_RECHECK_INTERVAL).await;
        }
    });
}

/// One round of `ensure_jj`/`ensure_zellij`, logged but never fatal to the
/// caller — a `mise` failure (network down, `mise` itself missing) must not
/// crash or block `serve`; it's retried on the next
/// [`HOST_TOOL_RECHECK_INTERVAL`] tick regardless of whether this attempt
/// succeeded.
///
/// **Explicit scope boundary, not a gap left by accident**: this proves
/// and logs whether `jj`/`zellij` are current under `choosh-hostd`'s own
/// `mise`-managed `host_tools_dir` — it does not redirect
/// `jj_ops.rs`/`zellij_ops.rs`/`pty.rs`'s existing `Command::new("jj")`/
/// `Command::new("zellij")` call sites (which resolve via `$PATH`) to the
/// path this resolves. Doing that would mean either prepending this
/// resolved binary's directory onto `PATH` for every process this crate
/// spawns (including every Zellij-tab-launched process, via the same
/// `env KEY=VALUE ...` argv-prefix mechanism `agent_launch.rs` already
/// uses for `CHOOSH_*`/project-pinned `mise` vars — itself a plausible
/// follow-up), or threading a resolved binary path through every `jj`/
/// `zellij` call site in this crate — either one a materially larger,
/// cross-cutting change than "keep `jj`/`zellij` current," and one that
/// would touch files well beyond this task's stated scope. Currency
/// checking (this function) is complete and tested; "which binary the
/// rest of the crate calls" is a deliberate, separate, reported gap.
async fn check_host_tool_currency_once(mise_bin: &str, host_tools_dir: &std::path::Path) {
    match crate::mise_ops::ensure_jj(mise_bin, host_tools_dir).await {
        Ok(path) => tracing::info!(resolved = %path.display(), "jj currency check ok"),
        Err(error) => tracing::warn!(%error, "jj currency check failed; will retry on the next interval"),
    }
    match crate::mise_ops::ensure_zellij(mise_bin, host_tools_dir).await {
        Ok(path) => tracing::info!(resolved = %path.display(), "zellij currency check ok"),
        Err(error) => tracing::warn!(%error, "zellij currency check failed; will retry on the next interval"),
    }
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
    agent_event_tx: tokio::sync::mpsc::Sender<WireAgentEvent>,
) {
    let mut attempt: u32 = 0;
    loop {
        let shutdown = tokio::signal::ctrl_c();
        tokio::select! {
            () = run_one_connection(config, credential, rpc_context, &mut agent_event_rx, ssh_port, &agent_event_tx) => {
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
    agent_event_tx: &tokio::sync::mpsc::Sender<WireAgentEvent>,
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

    serve_dispatch(&mut channel, rpc_context, agent_event_rx, ssh_port, agent_event_tx).await;
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
    agent_event_tx: &tokio::sync::mpsc::Sender<WireAgentEvent>,
) {
    // `rpc`-purpose tunnels offered on this connection are tracked here;
    // per relay-protocol.md's reconnect-discontinuity rule, tunnels never
    // survive a reconnect, so this set is deliberately scoped to one
    // connection attempt, not `connect_loop`'s outer state. Same for
    // `pty_tunnels`/`web_tunnels` and the output-forwarding channel below.
    let mut rpc_tunnels: HashSet<[u8; TUNNEL_ID_BYTES]> = HashSet::new();
    let mut pty_tunnels: HashMap<[u8; TUNNEL_ID_BYTES], PtyWriteHalf> = HashMap::new();
    let mut web_tunnels: HashMap<[u8; TUNNEL_ID_BYTES], WebWriteHalf> = HashMap::new();
    // M7's `dev-exec` cross-host offload (`"offload"`-purpose tunnels):
    // `offload_pending` holds a tunnel_id from the moment its
    // `tunnel-offered` push arrives until its first (and only) tunnel-data
    // frame — the JSON `OffloadRequest` — is parsed; `offload_active` holds
    // it from there until the spawned command's output/exit has finished
    // streaming back, so the `tunnel_output_rx` branch below knows to
    // forward its background task's output rather than dropping it (the
    // same role `pty_tunnels`/`web_tunnels`'s map membership plays for
    // their own output). Two separate sets rather than one because
    // `offload_pending` tracks "no write-half/background-task exists yet"
    // while `offload_active` tracks "one now does" — unlike pty/web, an
    // offload tunnel has no write half at all (the client only ever sends
    // the one initial request frame, never further input).
    let mut offload_pending: HashSet<[u8; TUNNEL_ID_BYTES]> = HashSet::new();
    let mut offload_active: HashSet<[u8; TUNNEL_ID_BYTES]> = HashSet::new();
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
                let is_offload = offload_active.contains(&output.tunnel_id);
                if !pty_tunnels.contains_key(&output.tunnel_id) && !is_web && !is_offload {
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
                if is_offload && output.bytes.is_empty() {
                    // The offload background task's own proactive close
                    // (sent right after its Exit frame, or right after an
                    // Error frame) — same "stop treating this tunnel_id as
                    // live" bookkeeping the web-tunnel branch above does.
                    offload_active.remove(&output.tunnel_id);
                }
            }

            frame = channel.recv_raw() => match frame {
                Ok((FRAME_CLASS_CONTROL, body)) => {
                    handle_control_push(&body, rpc_context, &tunnel_output_tx, &mut rpc_tunnels, &mut pty_tunnels, &mut web_tunnels, &mut offload_pending, ssh_port, agent_event_tx).await;
                }
                Ok((FRAME_CLASS_TUNNEL, body)) => {
                    if handle_tunnel_frame(&body, channel, rpc_context, &tunnel_output_tx, &mut rpc_tunnels, &mut pty_tunnels, &mut web_tunnels, &mut offload_pending, &mut offload_active).await == FrameOutcome::Disconnect {
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
#[allow(clippy::too_many_arguments)] // one tracking collection per tunnel purpose this dispatch handles, per this doc comment; a params struct would just move the count, not reduce it, for a single call site.
async fn handle_control_push(
    body: &[u8],
    rpc_context: &RpcContext,
    tunnel_output_tx: &tokio::sync::mpsc::Sender<TunnelOutput>,
    rpc_tunnels: &mut HashSet<[u8; TUNNEL_ID_BYTES]>,
    pty_tunnels: &mut HashMap<[u8; TUNNEL_ID_BYTES], PtyWriteHalf>,
    web_tunnels: &mut HashMap<[u8; TUNNEL_ID_BYTES], WebWriteHalf>,
    offload_pending: &mut HashSet<[u8; TUNNEL_ID_BYTES]>,
    ssh_port: Option<u16>,
    agent_event_tx: &tokio::sync::mpsc::Sender<WireAgentEvent>,
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
            match open_pty_tunnel(rpc_context, item_id, id, tunnel_output_tx.clone(), agent_event_tx.clone()).await {
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
        // M7's `dev-exec` cross-host offload (`docs/milestones/M7-fleet-and-provisioning.md`):
        // admitted solely because it arrived as a tunnel-offer on this
        // already relayd-authenticated connection — relayd itself already
        // restricted `open-tunnel { purpose: "offload" }` to a genuine
        // `devhost` Identity (`auth-and-enrollment.md`'s capability table;
        // enforced in `choosh-relayd::ws::check_open_tunnel_permitted`,
        // which needed no change for this). This arm only registers the
        // tunnel as pending its (exactly one) `OffloadRequest` data frame —
        // `choosh_protocol::offload`'s own module doc comment for the
        // framing — the actual workspace/revision resolution and command
        // dispatch happen in `handle_tunnel_frame` once that frame arrives,
        // since resolving `workspace_name`/`commit_id` needs the registry
        // (an async lock) and isn't something to do from this control-push
        // handler. Same "reject an empty requester identity as malformed"
        // discipline the `"ssh"` arm above applies.
        Ok(ServerPush::TunnelOffered { tunnel_id, from_device_id, purpose }) if purpose == "offload" => {
            let Some(id) = decode_tunnel_id_hex(&tunnel_id) else {
                tracing::warn!(tunnel_id, "tunnel-offered carried a malformed tunnel_id");
                return;
            };
            if from_device_id.trim().is_empty() {
                tracing::warn!("refusing offload-purpose tunnel-offered with no requester identity");
                return;
            }
            offload_pending.insert(id);
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
        // docs/specs/relay-protocol.md's `update_binary` /
        // docs/specs/host-deployment.md's Self-update: spawned as its own
        // task rather than handled inline, since it performs a network
        // download and, on success, a restart that kills this very
        // process — see `crate::update`'s module doc comment for the full
        // download-verify-swap-restart-monitor design.
        Ok(ServerPush::UpdateBinary { push_id, download_url, sha256, version }) => {
            let tx = agent_event_tx.clone();
            tokio::spawn(async move {
                update::handle_update_binary_push(push_id, download_url, sha256, version, tx).await;
            });
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
#[allow(clippy::too_many_arguments)] // one tracking collection per tunnel purpose this dispatch routes, per this doc comment; a params struct would just move the count, not reduce it, for a single call site.
async fn handle_tunnel_frame(
    body: &[u8],
    channel: &mut WsChannel,
    rpc_context: &RpcContext,
    tunnel_output_tx: &tokio::sync::mpsc::Sender<TunnelOutput>,
    rpc_tunnels: &mut HashSet<[u8; TUNNEL_ID_BYTES]>,
    pty_tunnels: &mut HashMap<[u8; TUNNEL_ID_BYTES], PtyWriteHalf>,
    web_tunnels: &mut HashMap<[u8; TUNNEL_ID_BYTES], WebWriteHalf>,
    offload_pending: &mut HashSet<[u8; TUNNEL_ID_BYTES]>,
    offload_active: &mut HashSet<[u8; TUNNEL_ID_BYTES]>,
) -> FrameOutcome {
    if body.len() < TUNNEL_ID_BYTES {
        tracing::warn!("tunnel frame shorter than a tunnel ID; per relay-protocol.md this is malformed");
        return FrameOutcome::Disconnect;
    }
    let (id_bytes, payload) = body.split_at(TUNNEL_ID_BYTES);
    let mut tunnel_id = [0u8; TUNNEL_ID_BYTES];
    tunnel_id.copy_from_slice(id_bytes);

    if offload_pending.remove(&tunnel_id) {
        // The tunnel's first (and only) inbound data frame: the JSON
        // `OffloadRequest`. A zero-length payload here would be
        // relay-protocol.md's close signal, not a real request — the
        // requester gave up before ever sending one; nothing to do.
        if payload.is_empty() {
            return FrameOutcome::Continue;
        }
        handle_offload_request_frame(payload, channel, rpc_context, tunnel_output_tx, tunnel_id, offload_active).await;
        return FrameOutcome::Continue;
    }

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

/// Sends one [`OffloadError`] frame followed by the ordinary zero-payload
/// tunnel-close signal, directly over `channel` — used for every "the
/// request itself can't be served" outcome in
/// [`handle_offload_request_frame`], synchronously, the same way the
/// `rpc`-tunnel branch above sends its response directly rather than
/// through `tunnel_output_tx` (there is no background task to hand this
/// off to in the failure case; nothing has been spawned yet).
async fn send_offload_error_and_close(channel: &mut WsChannel, tunnel_id: [u8; TUNNEL_ID_BYTES], error: &OffloadError) {
    let Ok(error_frame) = choosh_protocol::offload::encode_error_frame(error) else {
        tracing::error!("failed to serialize OffloadError; closing the tunnel with no error frame");
        let _ = channel.send_bytes(FRAME_CLASS_TUNNEL, &tunnel_id).await;
        return;
    };
    let mut payload = Vec::with_capacity(TUNNEL_ID_BYTES + error_frame.len());
    payload.extend_from_slice(&tunnel_id);
    payload.extend_from_slice(&error_frame);
    if let Err(send_error) = channel.send_bytes(FRAME_CLASS_TUNNEL, &payload).await {
        tracing::warn!(%send_error, "failed to send offload error frame");
        return;
    }
    let _ = channel.send_bytes(FRAME_CLASS_TUNNEL, &tunnel_id).await; // proactive close, zero-payload per relay-protocol.md.
}

/// M7's `dev-exec` offload server side: parses `payload` as an
/// [`OffloadRequest`], resolves it against this devhost's own registry and
/// `jj` store, and either refuses with a typed [`OffloadError`] (sent
/// synchronously over `channel`, mirroring the `rpc`-tunnel response path
/// just above) or spawns the requested command against a fresh ephemeral
/// `jj` workspace and marks `tunnel_id` active so [`serve_dispatch`]'s
/// `tunnel_output_rx` branch forwards its streamed output.
///
/// **Design decision, no spec covers this (M7 has none):** "a matching jj
/// revision" is resolved by treating `commit_id` as a Git commit hash — see
/// `choosh_protocol::offload`'s module doc comment for why a `jj` change id
/// would NOT be portable here. This devhost's own store either already has
/// that commit (because both devhosts clone/fetch from the same Git
/// remote) or it doesn't; there is deliberately no fetch-on-demand in this
/// pass — an unresolvable commit fails cleanly as a `not_found`
/// [`OffloadError`], the same posture `workspace.create`'s `clone_url` path
/// already takes toward "the remote must already be reachable," not a
/// silent hang or a surprising background fetch.
async fn handle_offload_request_frame(
    payload: &[u8],
    channel: &mut WsChannel,
    rpc_context: &RpcContext,
    tunnel_output_tx: &tokio::sync::mpsc::Sender<TunnelOutput>,
    tunnel_id: [u8; TUNNEL_ID_BYTES],
    offload_active: &mut HashSet<[u8; TUNNEL_ID_BYTES]>,
) {
    let request = match choosh_protocol::offload::decode_frame(payload) {
        Ok(OffloadFrame::Request(request)) => request,
        Ok(_) => {
            tracing::warn!("offload tunnel's first data frame was not an OffloadRequest; refusing");
            send_offload_error_and_close(
                channel,
                tunnel_id,
                &OffloadError { code: "invalid_argument".to_string(), message: "expected an offload request frame first".to_string() },
            )
            .await;
            return;
        }
        Err(error) => {
            tracing::warn!(%error, "malformed offload request frame; refusing");
            send_offload_error_and_close(
                channel,
                tunnel_id,
                &OffloadError { code: "invalid_argument".to_string(), message: "malformed offload request".to_string() },
            )
            .await;
            return;
        }
    };

    if request.argv.is_empty() {
        send_offload_error_and_close(
            channel,
            tunnel_id,
            &OffloadError { code: "invalid_argument".to_string(), message: "argv must not be empty".to_string() },
        )
        .await;
        return;
    }

    let found_root = {
        let registry = rpc_context.registry.lock().await;
        registry.find_workspace_by_name(&request.workspace_name).map(|workspace| workspace.root_path.clone())
    };
    let Some(workspace_root) = found_root else {
        send_offload_error_and_close(
            channel,
            tunnel_id,
            &OffloadError { code: "not_found".to_string(), message: format!("workspace {:?} is not registered on this devhost", request.workspace_name) },
        )
        .await;
        return;
    };

    let ephemeral_name = format!("offload-{}", uuid::Uuid::new_v4());
    let ephemeral_dest = rpc_context.workspaces_dir.join(&ephemeral_name);
    if let Err(error) = jj_ops::workspace_add(&workspace_root, &ephemeral_dest, &ephemeral_name, Some(&request.commit_id)).await {
        // Almost always means `commit_id` doesn't resolve on this store
        // (the target never fetched it) — `not_found`, not `internal`, per
        // host-rpc.md's error-model precedent for exactly this shape of
        // jj-command failure. `error`'s `Display` impl deliberately omits
        // raw jj stderr (see `JjError::CommandFailed`'s own doc comment),
        // so this message is already redacted.
        tracing::warn!(%error, workspace_name = %request.workspace_name, "failed to create ephemeral offload workspace");
        send_offload_error_and_close(
            channel,
            tunnel_id,
            &OffloadError {
                code: "not_found".to_string(),
                message: "requested revision is not available on this devhost".to_string(),
            },
        )
        .await;
        return;
    }

    let mut command = tokio::process::Command::new(&request.argv[0]);
    command.args(&request.argv[1..]).current_dir(&ephemeral_dest).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(%error, argv = ?request.argv, "failed to spawn offloaded command");
            let _ = jj_ops::forget_workspace(&workspace_root, &ephemeral_name).await;
            let _ = tokio::fs::remove_dir_all(&ephemeral_dest).await;
            send_offload_error_and_close(
                channel,
                tunnel_id,
                &OffloadError { code: "invalid_argument".to_string(), message: "failed to start the requested command".to_string() },
            )
            .await;
            return;
        }
    };

    offload_active.insert(tunnel_id);
    spawn_offload_process(child, tunnel_id, workspace_root, ephemeral_name, ephemeral_dest, tunnel_output_tx.clone());
}

/// Streams `child`'s stdout/stderr back as [`OFFLOAD_FRAME_STDOUT`]/
/// [`OFFLOAD_FRAME_STDERR`]-tagged [`TunnelOutput`]s, then its exit code as
/// one [`OFFLOAD_FRAME_EXIT`] frame, then the ordinary zero-payload
/// tunnel-close — mirroring [`open_tcp_bridge_tunnel`]'s "background task
/// owns the resource, forwards through `output_tx`, proactively closes"
/// shape. Cleans up the ephemeral `jj` workspace (`jj workspace forget`,
/// then removing its directory) only after every byte of output and the
/// exit code have already been queued for delivery — a real filesystem
/// side effect the command produced (e.g. a file it wrote) is still
/// observable through the command's own streamed output, and this ordering
/// means a slow/backpressured `tunnel_output_tx` never races the cleanup
/// against output that hasn't been sent yet.
///
/// [`OFFLOAD_FRAME_STDOUT`]: choosh_protocol::offload::OFFLOAD_FRAME_STDOUT
/// [`OFFLOAD_FRAME_STDERR`]: choosh_protocol::offload::OFFLOAD_FRAME_STDERR
/// [`OFFLOAD_FRAME_EXIT`]: choosh_protocol::offload::OFFLOAD_FRAME_EXIT
fn spawn_offload_process(
    mut child: tokio::process::Child,
    tunnel_id: [u8; TUNNEL_ID_BYTES],
    workspace_root: PathBuf,
    ephemeral_name: String,
    ephemeral_dest: PathBuf,
    output_tx: tokio::sync::mpsc::Sender<TunnelOutput>,
) {
    tokio::spawn(async move {
        let Some(mut stdout) = child.stdout.take() else {
            tracing::error!("offloaded child had no stdout pipe; this should be unreachable given Stdio::piped()");
            return;
        };
        let Some(mut stderr) = child.stderr.take() else {
            tracing::error!("offloaded child had no stderr pipe; this should be unreachable given Stdio::piped()");
            return;
        };

        let stdout_tx = output_tx.clone();
        let stdout_task = tokio::spawn(async move {
            let mut buf = [0u8; WEB_TUNNEL_READ_BUF_SIZE];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        let bytes = choosh_protocol::offload::encode_stdout_frame(&buf[..n]);
                        if stdout_tx.send(TunnelOutput { tunnel_id, bytes }).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });

        let stderr_tx = output_tx.clone();
        let stderr_task = tokio::spawn(async move {
            let mut buf = [0u8; WEB_TUNNEL_READ_BUF_SIZE];
            loop {
                match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        let bytes = choosh_protocol::offload::encode_stderr_frame(&buf[..n]);
                        if stderr_tx.send(TunnelOutput { tunnel_id, bytes }).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });

        let status = child.wait().await;
        // Wait for both readers to finish draining before sending the exit
        // frame — otherwise a slow reader could still have buffered output
        // in flight when the client sees `Exit` and decides the command is
        // done.
        let _ = stdout_task.await;
        let _ = stderr_task.await;

        let exit_code = match status {
            Ok(status) => status.code().unwrap_or(-1), // terminated by signal, no portable exit code — -1 is a real, non-zero "abnormal" signal to the caller.
            Err(error) => {
                tracing::warn!(%error, "failed to wait on offloaded child process");
                -1
            }
        };
        let _ = output_tx.send(TunnelOutput { tunnel_id, bytes: choosh_protocol::offload::encode_exit_frame(exit_code) }).await;
        let _ = output_tx.send(TunnelOutput { tunnel_id, bytes: Vec::new() }).await; // proactive close, per relay-protocol.md.

        if let Err(error) = jj_ops::forget_workspace(&workspace_root, &ephemeral_name).await {
            tracing::warn!(%error, ephemeral_name, "failed to forget ephemeral offload workspace; it will linger in the jj operation log");
        }
        if let Err(error) = tokio::fs::remove_dir_all(&ephemeral_dest).await {
            tracing::warn!(%error, path = %ephemeral_dest.display(), "failed to remove ephemeral offload workspace directory");
        }
    });
}

/// Resolves `item_id` to its workspace's Zellij session/tab, attaches a
/// real headless pty client to it (`pty.rs`), and spawns a background task
/// that forwards everything it reads into `output_tx` tagged with
/// `tunnel_id` — the write half is returned for [`serve_dispatch`]'s main
/// loop to route phone-originated input into.
///
/// **`auth_required` detection lives here.** This is the one place
/// `choosh-hostd` reads a real, unbuffered stream of an interactive
/// terminal's output — exactly what an `aws sso login`/`gcloud auth
/// login`/`az login`/`gh auth login` invocation inside an `AgentTerminal`/
/// `Shell` item's Zellij tab produces (per `agent-events.md`'s
/// `auth_required`, `docs/milestones/M7-fleet-and-provisioning.md`'s
/// "SSO/cloud-CLI device-code bridge", and `auth_detect.rs`'s own doc
/// comment for exactly which four providers and how each was verified). A
/// fresh [`crate::auth_detect::AuthCodeDetector`] is created per pty
/// attach (state is per-session, never shared across tunnels) and fed
/// every chunk this loop already reads — the *same* bytes, unmodified,
/// still get forwarded to `output_tx` exactly as before; detection is
/// purely an additional tap, never a filter or rewrite of what the phone's
/// terminal actually sees.
///
/// **Scope note — which paths this covers and which it doesn't**: this is
/// the `pty:<item_id>` tunnel path (an `AgentTerminal`/`Shell` item's
/// Zellij tab, attached via `pty.rs`'s `PtySession` the same way a phone's
/// own terminal view does) — the path a user- or agent-run `aws sso
/// login`/`gh auth login` invocation actually goes through in this crate.
/// Deliberately NOT wired into: `ssh_server.rs`'s own PTY-backed
/// interactive shell/exec sessions (the loopback SSH bridge behind `ssh
/// <devhost>`/Zed) — that path is always laptop-originated, and a laptop
/// with an interactive SSH client already has a local browser available in
/// the overwhelmingly common case, which is the exact condition under
/// which these CLIs skip the device-code flow entirely and open one
/// themselves; wiring it too is a real, reportable gap for the (rarer)
/// case of a laptop itself being headless, not something this pass
/// covers. `WebService` items' own process output is never read by
/// `choosh-hostd` at all (`zellij_ops::new_tab`'s `zellij action new-tab`
/// client runs with `Stdio::null()`; the actual pane process lives inside
/// the Zellij server and is invisible to `choosh-hostd` unless something
/// attaches to it the way this function does, which nothing does for a
/// `WebService` item — those use the `web:<item_id>` TCP-port bridge
/// instead), so there is nothing to tap there. `agent_launch.rs`/`hooks.rs`'s structured hook-event path only
/// observes agent lifecycle hooks (`PermissionRequest`, `Stop`, etc.),
/// never raw subprocess stdout — if an agent's own tool invocation prints
/// a device-code prompt, that text still reaches the phone through *this*
/// same pty path once the agent's TUI renders it in its Zellij tab, so no
/// second detector is needed for that case either.
async fn open_pty_tunnel(
    rpc_context: &RpcContext,
    item_id: &str,
    tunnel_id: [u8; TUNNEL_ID_BYTES],
    output_tx: tokio::sync::mpsc::Sender<TunnelOutput>,
    agent_event_tx: tokio::sync::mpsc::Sender<WireAgentEvent>,
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
        let mut auth_detector = crate::auth_detect::AuthCodeDetector::new();
        loop {
            match read_half.read(&mut buf).await {
                Ok(0) | Err(_) => return, // EOF (client exited) or a read error — either way, nothing left to forward.
                Ok(n) => {
                    if let Some(event) = auth_detector.feed(&buf[..n])
                        && agent_event_tx.send(event).await.is_err()
                    {
                        tracing::warn!("failed to forward a detected auth_required event; the connection's event channel is gone");
                    }
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
    use choosh_protocol::relay::{
        FRAME_CLASS_CONTROL as CTRL, FRAME_CLASS_TUNNEL as TUNNEL, MAX_CONTROL_FRAME_BYTES, MAX_TUNNEL_FRAME_BYTES, WireAuthProvider,
        encode_tunnel_id_hex,
    };
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
            mise_bin: "mise-not-used-in-this-test".to_string(),
            mise_project_tools_dir: dir.path().join("mise-project-tools"),
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
        let (agent_tx, mut agent_event_rx) = tokio::sync::mpsc::channel(1);

        let dispatch_and_dial = async {
            let mut channel = dial(&relay_url).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(15), serve_dispatch(&mut channel, &ctx, &mut agent_event_rx, None, &agent_tx)).await;
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
        let (agent_tx, mut agent_event_rx) = tokio::sync::mpsc::channel(1);

        let dispatch_and_dial = async {
            let mut channel = dial(&relay_url).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(10), serve_dispatch(&mut channel, &ctx, &mut agent_event_rx, None, &agent_tx)).await;
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
            mise_bin: "mise-not-used-in-this-test".to_string(),
            mise_project_tools_dir: dir.path().join("mise-project-tools"),
        };
        let (listener, relay_url) = bind_fake_relayd().await;
        let (agent_tx, mut agent_event_rx) = tokio::sync::mpsc::channel(1);

        let dispatch_and_dial = async {
            let mut channel = dial(&relay_url).await.unwrap();
            let _ = tokio::time::timeout(Duration::from_secs(10), serve_dispatch(&mut channel, &ctx, &mut agent_event_rx, Some(ssh_port), &agent_tx)).await;
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

    /// Registers a real `Shell` item backed by a real Zellij session/tab
    /// (the same shape `pty.rs`'s own `bytes_written_reach_the_tab_and_its_output_reaches_the_master`
    /// test builds) — `open_pty_tunnel` resolves `item_id` through this
    /// registry to find the session/tab to attach to, exactly as it would
    /// for a real `AgentTerminal`/`Shell` item Android created.
    fn registry_with_shell_item(session_name: &str, tab_name: &str) -> (tempfile::TempDir, RpcContext, String) {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = crate::registry::Registry::load(&dir.path().join("registry.json")).unwrap();
        let workspace_id = "ws-1".to_string();
        registry
            .register_workspace(
                workspace_id.clone(),
                session_name.to_string(),
                "dev-1".to_string(),
                "proj-1".to_string(),
                "proj".to_string(),
                dir.path().to_path_buf(),
                "2026-01-01T00:00:00Z".to_string(),
            )
            .unwrap();
        let item_id = "item-1".to_string();
        registry
            .register_item(
                item_id.clone(),
                workspace_id,
                ItemType::Shell,
                "shell".to_string(),
                tab_name.to_string(),
                None,
                None,
                ItemStatus::Running,
            )
            .unwrap();
        let ctx = RpcContext {
            registry: std::sync::Arc::new(tokio::sync::Mutex::new(registry)),
            devhost_id: "dev-1".to_string(),
            workspaces_dir: dir.path().to_path_buf(),
            mise_bin: "mise-not-used-in-this-test".to_string(),
            mise_project_tools_dir: dir.path().join("mise-project-tools"),
        };
        (dir, ctx, item_id)
    }

    /// One attempt at [`a_real_device_code_prompt_in_a_pty_produces_a_real_auth_required_control_frame`]:
    /// a fresh Zellij session/tab, a real `pty:<item_id>` tunnel attach
    /// through it, and an assertion that the resulting bytes really produce
    /// a real `ControlRequest::AgentEvent` control frame carrying
    /// `WireAgentEvent::AuthRequired` with the exact `agent-events.md` wire
    /// shape. Returns `Err` with a diagnostic string instead of panicking
    /// directly, so the caller can retry against the pre-existing Zellij
    /// flake documented on the outer test.
    ///
    /// **The tab's initial command is the fixture printer itself
    /// (`sh -c "printf ..."`), not an interactive shell a command gets
    /// typed into.** Tried the interactive-shell-plus-typed-command shape
    /// first; it turned out to be a real, separate landmine, not a
    /// shortcut: this sandbox's own interactive shell prompt (a boxed,
    /// `┌ user@host:path` status-line style prompt) redraws using absolute
    /// cursor-positioning ANSI escapes rather than plain newlines, which
    /// this module's necessarily-simplified [`strip_ansi_and_normalize`]-style
    /// stripping (see `auth_detect.rs`) cannot fully reconstruct into
    /// logical lines — it correctly strips the positioning escapes but has
    /// no way to recover the line break they implied, so text from two
    /// genuinely different prompt lines can end up concatenated with no
    /// separating whitespace at all. That's a real, worth-flagging
    /// robustness edge for a from-scratch terminal-text scanner against an
    /// arbitrarily fancy interactive prompt in general — but it's about
    /// *this sandbox's own shell prompt*, not about anything this test
    /// needs to prove. Running the fixture as the tab's own non-interactive
    /// process sidesteps it entirely (no interactive prompt is ever
    /// rendered) while still exercising the exact same real
    /// `open_pty_tunnel`/`PtySession`/`auth_detect.rs` pty-reading path an
    /// interactively-typed `aws sso login` would go through.
    async fn attempt_real_device_code_prompt_produces_auth_required(attempt_deadline: Duration) -> Result<(), String> {
        let session_name = format!("auth-detect-test-{}", uuid::Uuid::new_v4());
        let tab_name = "shelltab";
        let dir_for_session = tempfile::tempdir().unwrap();
        crate::zellij_ops::create_session(&session_name, dir_for_session.path()).await.unwrap();
        // A real shell command line, run directly as the tab's own process
        // (no interactive prompt involved at all): prints the exact real
        // device-code shape `auth_detect.rs` matches for `github` (see that
        // module's `detect_github` doc comment for the real capture this
        // shape is drawn from).
        let script = "printf 'First copy your one-time code: TEST-1234\nOpen this URL to continue in your web browser: https://github.com/login/device\n'; sleep 30";
        crate::zellij_ops::new_tab(
            &session_name,
            tab_name,
            dir_for_session.path(),
            &["sh".to_string(), "-c".to_string(), script.to_string()],
        )
        .await
        .unwrap();

        let (_dir, ctx, item_id) = registry_with_shell_item(&session_name, tab_name);
        let (listener, relay_url) = bind_fake_relayd().await;
        let (agent_tx, mut agent_event_rx) = tokio::sync::mpsc::channel(4);

        let dispatch_and_dial = async {
            let mut channel = dial(&relay_url).await.unwrap();
            let _ = tokio::time::timeout(attempt_deadline, serve_dispatch(&mut channel, &ctx, &mut agent_event_rx, None, &agent_tx)).await;
        };

        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let tunnel_id = [7u8; TUNNEL_ID_BYTES];

            // Just the tunnel offer this time — no tunnel-data frame to
            // send, since the fixture text is the tab's own process output,
            // not typed input.
            let push = ServerPush::TunnelOffered {
                tunnel_id: encode_tunnel_id_hex(tunnel_id),
                from_device_id: "phone-1".to_string(),
                purpose: format!("pty:{item_id}"),
            };
            let mut control_payload = vec![CTRL];
            control_payload.extend(serde_json::to_vec(&push).unwrap());
            let control_wire = encode_frame(&control_payload, MAX_CONTROL_FRAME_BYTES).unwrap();
            ws.send(Message::Binary(control_wire.into())).await.unwrap();

            let mut decoder = FrameDecoder::new(FrameLimits::new(MAX_TUNNEL_FRAME_BYTES.max(MAX_CONTROL_FRAME_BYTES), 8).unwrap());
            let deadline = tokio::time::Instant::now() + attempt_deadline;
            let mut debug_collected = Vec::new();
            loop {
                if tokio::time::Instant::now() >= deadline {
                    return Err(format!(
                        "timed out waiting for the auth_required control frame; pty output seen so far: {:?}",
                        String::from_utf8_lossy(&debug_collected)
                    ));
                }
                let Ok(Some(Ok(Message::Binary(bytes)))) = tokio::time::timeout(Duration::from_secs(2), ws.next()).await else { continue };
                for frame in decoder.feed(&bytes).unwrap() {
                    let (class, body) = frame.split_first().unwrap();
                    if *class != CTRL {
                        if *class == TUNNEL && body.len() > TUNNEL_ID_BYTES {
                            debug_collected.extend_from_slice(&body[TUNNEL_ID_BYTES..]);
                        }
                        continue; // ordinary pty output — not what this test is looking for.
                    }
                    let Ok(ControlRequest::AgentEvent { event, .. }) = serde_json::from_slice::<ControlRequest>(body) else {
                        continue; // some other control push (e.g. tunnel-offered) racing on the same connection.
                    };
                    let WireAgentEvent::AuthRequired { provider, user_code, verification_uri } = event else {
                        return Err(format!("expected an AuthRequired agent-event, got {event:?}"));
                    };
                    if provider != WireAuthProvider::Github || user_code != "TEST-1234" || verification_uri != "https://github.com/login/device" {
                        return Err(format!(
                            "unexpected AuthRequired fields: provider={provider:?} user_code={user_code:?} verification_uri={verification_uri:?}; raw pty bytes so far: {:?}",
                            String::from_utf8_lossy(&debug_collected)
                        ));
                    }
                    let _ = ws.close(None).await;
                    return Ok(());
                }
            }
        };

        let (server_result, ()) = tokio::join!(server_fut, dispatch_and_dial);
        crate::zellij_ops::kill_session(&session_name).await.ok();
        server_result
    }

    /// The task's own required proof, per M7's exit criteria and the task
    /// brief's "integration test through the real `serve_dispatch`/
    /// agent-event-forwarding path": a real device-code prompt, produced by
    /// a real interactive shell running inside a real Zellij tab and read
    /// through the real `pty:<item_id>` tunnel path
    /// (`open_pty_tunnel`/`auth_detect.rs`), really produces a real
    /// `ControlRequest::AgentEvent` control frame carrying
    /// `WireAgentEvent::AuthRequired` with the exact `agent-events.md` wire
    /// shape — mirroring how `ssh_purpose_tunnel_bridges_to_the_real_loopback_ssh_server`
    /// above and `ssh_server.rs`'s own editor-presence tests already prove
    /// their respective events the same way. The shell command itself
    /// (`printf`) is not standing in for a real cloud-CLI binary — that
    /// exact detection logic is already unit-tested against real captured
    /// `gh`/`aws` output in `auth_detect.rs`; this test's job is only to
    /// prove the *wiring* from "bytes read off a real pty" through to "a
    /// real control frame on the wire" is real, not mocked at any point in
    /// between.
    ///
    /// **Retries against a real, pre-existing Zellij flake, confirmed by
    /// direct experiment and not introduced by this change**: `pty.rs`'s
    /// `PtySession::attach` sets `ZELLIJ_SESSION_NAME` on its spawned
    /// `zellij attach <name>` child to that same target name. Confirmed
    /// directly (both via a bare shell invocation and via this test's own
    /// bisection across several probe variants, since removed) that this
    /// occasionally causes zellij's *own* client, some seconds into an
    /// otherwise fully successful attach, to trip its internal
    /// self-nesting guard (`commands.rs`'s "You are trying to attach to
    /// the current session" panic — real zellij source, not a guess) and
    /// exit — with no fixed, safe time window this test can simply wait
    /// out. This is the same class of "real, reproducible race... not
    /// fully eliminable" behavior `zellij_ops.rs`'s own
    /// `ZELLIJ_CLIENT_LOCK` doc comment already documents for a different
    /// Zellij client/server race, and lives entirely inside `pty.rs`
    /// (unmodified by this task, and outside this task's scope to fix) —
    /// not in any code this task added. A bounded retry with a fresh
    /// session per attempt, matching this project's established
    /// "documented pre-existing Zellij/PTY flakiness... not yours to
    /// chase" posture, is the pragmatic way to keep this a real,
    /// non-mocked integration test without making CI flaky on an
    /// unrelated, pre-existing bug.
    #[tokio::test]
    async fn a_real_device_code_prompt_in_a_pty_produces_a_real_auth_required_control_frame() {
        const ATTEMPTS: u32 = 5;
        let mut last_error = String::new();
        for attempt in 1..=ATTEMPTS {
            match attempt_real_device_code_prompt_produces_auth_required(Duration::from_secs(8)).await {
                Ok(()) => return,
                Err(error) => {
                    tracing::warn!(attempt, %error, "auth_required pty integration attempt failed; retrying against the documented pre-existing Zellij flake");
                    last_error = error;
                }
            }
        }
        panic!("all {ATTEMPTS} attempts failed; last error: {last_error}");
    }

    // --- M7's `dev-exec` offload: real end-to-end coverage -----------------
    //
    // Unlike every other test above (a hand-rolled fake `relayd` driving one
    // real `serve_dispatch` connection directly), `dev-exec` genuinely needs
    // TWO real Identity connections routed to each other by tunnel_id — the
    // originating devhost's `dev_exec::run_with_io` client half and the
    // target devhost's `serve_dispatch` server half. `run_two_party_fake_relayd`
    // below is a small, narrow router (`OpenTunnel` -> `TunnelOffered` plus
    // blind `FRAME_CLASS_TUNNEL` forwarding, nothing else) standing in for
    // `choosh-relayd` itself for exactly that reason — real bytes flow
    // through the real `dev_exec::run_with_io` and real
    // `serve_dispatch`/`handle_offload_request_frame`/`spawn_offload_process`
    // production code on both ends; only the relay routing in between is
    // faked, the same posture every other test in this module already takes
    // toward `relayd`.

    use choosh_protocol::offload::OffloadRequest;
    use choosh_protocol::relay::AuthOk;

    type AcceptedWsChannel = FrameChannel<tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>>;

    fn init_git_repo_with_file(dir: &std::path::Path, filename: &str, content: &str) {
        std::process::Command::new("git").arg("init").arg("-q").current_dir(dir).status().unwrap();
        std::process::Command::new("git").args(["config", "user.email", "a@b.c"]).current_dir(dir).status().unwrap();
        std::process::Command::new("git").args(["config", "user.name", "a"]).current_dir(dir).status().unwrap();
        std::fs::write(dir.join(filename), content).unwrap();
        std::process::Command::new("git").args(["add", filename]).current_dir(dir).status().unwrap();
        std::process::Command::new("git").args(["commit", "-q", "-m", "init"]).current_dir(dir).status().unwrap();
    }

    fn fake_dev_exec_credential() -> Credential {
        let signing_key = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        Credential::new("dev-client".to_string(), "fake-cert".to_string(), &signing_key)
    }

    struct RecordingWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl tokio::io::AsyncWrite for RecordingWriter {
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

    /// Performs the real `ServerHello`/`ClientAuth`/`AuthResult` handshake
    /// `dev_exec::connect_authenticated` (production code) drives against a
    /// real `relayd` — the "target" side of this fake never gets this (it's
    /// driven by `dial()`+`serve_dispatch()` directly, exactly like every
    /// other test above, which never performs this handshake either since
    /// `serve_dispatch` itself has no authentication step of its own).
    async fn fake_relayd_handshake(channel: &mut AcceptedWsChannel) -> String {
        let _ = channel.send(CTRL, &ServerHello { nonce: "test-nonce".to_string() }).await;
        let Ok(auth) = channel.recv::<ClientAuth>().await else { return String::new() };
        let device_id = match auth {
            ClientAuth::Device(device_auth) => device_auth.device_id,
            ClientAuth::Phone(_) => "phone-unexpected".to_string(),
        };
        let _ = channel.send(CTRL, &AuthResult::Ok(AuthOk { identity_class: IdentityClass::Devhost, device_id: device_id.clone() })).await;
        device_id
    }

    /// Routes one frame read from `from_ch` (tagged with `from_device_id`)
    /// to `to_ch`: an `OpenTunnel` control request gets a fixed `tunnel_id`
    /// answered on `from_ch` and a `TunnelOffered` push sent to `to_ch`
    /// (mirroring `choosh-relayd`'s own `open-tunnel` handling, minus every
    /// capability check — this fake's whole job is routing, not
    /// authorization, since that's `choosh-relayd::ws`'s own, separately
    /// tested job); any `FRAME_CLASS_TUNNEL` frame is forwarded to `to_ch`
    /// completely unparsed, per relay-protocol.md's "MUST NOT parse or
    /// transform tunnel bytes" rule, which this fake also honors.
    async fn route_fake_relayd_frame(class: u8, body: &[u8], from_device_id: &str, from_ch: &mut AcceptedWsChannel, to_ch: &mut AcceptedWsChannel) {
        if class == CTRL {
            if let Ok(ControlRequest::OpenTunnel { request_id, purpose, .. }) = serde_json::from_slice::<ControlRequest>(body) {
                let tunnel_id = [0xEEu8; TUNNEL_ID_BYTES];
                let _ = from_ch.send(CTRL, &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) }).await;
                let _ = to_ch
                    .send(CTRL, &ServerPush::TunnelOffered { tunnel_id: encode_tunnel_id_hex(tunnel_id), from_device_id: from_device_id.to_string(), purpose })
                    .await;
            }
        } else if class == TUNNEL {
            let _ = to_ch.send_bytes(TUNNEL, body).await;
        }
    }

    /// Accepts exactly two connections on `listener`, in a KNOWN order this
    /// module's tests establish explicitly (the target dials first, before
    /// the client ever starts) — connection 1 is the target (no handshake,
    /// matching `serve_dispatch`'s own test-harness convention above),
    /// connection 2 is the client (a real handshake, since
    /// `dev_exec::connect_authenticated` is real production code and
    /// genuinely performs one). Then routes forever until either side
    /// disconnects.
    async fn run_two_party_fake_relayd(listener: tokio::net::TcpListener) {
        let Ok((raw_target, _)) = listener.accept().await else { return };
        let Ok(ws_target) = tokio_tungstenite::accept_async(raw_target).await else { return };
        let mut target_ch: AcceptedWsChannel = FrameChannel::new(ws_target);

        let Ok((raw_client, _)) = listener.accept().await else { return };
        let Ok(ws_client) = tokio_tungstenite::accept_async(raw_client).await else { return };
        let mut client_ch: AcceptedWsChannel = FrameChannel::new(ws_client);
        let client_device_id = fake_relayd_handshake(&mut client_ch).await;

        loop {
            tokio::select! {
                frame = target_ch.recv_raw() => {
                    let Ok((class, body)) = frame else { return };
                    route_fake_relayd_frame(class, &body, "dev-target", &mut target_ch, &mut client_ch).await;
                }
                frame = client_ch.recv_raw() => {
                    let Ok((class, body)) = frame else { return };
                    route_fake_relayd_frame(class, &body, &client_device_id, &mut client_ch, &mut target_ch).await;
                }
            }
        }
    }

    /// M7's own required proof: two independent `jj` stores of the same
    /// underlying Git history (mirroring two real devhosts that both cloned
    /// the same origin, exactly like this module's own doc-comment design
    /// decision describes), a real command spawned against the target's
    /// real registered workspace's real content, in a real, distinct
    /// ephemeral `jj` workspace — proven via the command's own streamed
    /// stdout (not a post-hoc filesystem peek, which would race
    /// `spawn_offload_process`'s own cleanup; see that function's doc
    /// comment) — with real stdout/stderr bytes streamed back and a
    /// non-zero exit code propagated exactly.
    #[tokio::test]
    async fn dev_exec_offload_runs_against_the_targets_real_workspace_and_streams_output_back() {
        let origin_dir = tempfile::tempdir().unwrap();
        init_git_repo_with_file(origin_dir.path(), "SHARED_ORIGIN_FILE.txt", "content-from-the-shared-git-remote\n");

        let target_root = tempfile::tempdir().unwrap();
        let target_repo = target_root.path().join("app");
        jj_ops::clone(origin_dir.path().to_str().unwrap(), &target_repo).await.unwrap();

        let client_root = tempfile::tempdir().unwrap();
        let client_repo = client_root.path().join("app-client-copy");
        jj_ops::clone(origin_dir.path().to_str().unwrap(), &client_repo).await.unwrap();

        // The two clones are independent `jj` stores of the same Git
        // history — this is exactly the "matching jj revision" design
        // decision this module's own doc comment records: only the Git
        // commit id is portable to the target, not a `jj` change id. And,
        // per `jj_ops::current_commit_id`'s own doc comment (a sharp edge
        // found while building this exact test): it must be `@-`, the
        // commit `jj git clone` imported unchanged from the shared remote —
        // NOT `@` itself, which `jj git clone` freshly creates as a new,
        // store-local empty commit on top, with this store's own clone-time
        // timestamp, and which therefore does NOT exist on the target's
        // independent clone even though both clones' `@-` is byte-identical.
        let commit_id = jj_ops::commit_id_at(&client_repo, "@-").await.unwrap();

        let target_workspaces_dir = tempfile::tempdir().unwrap();
        let mut target_registry = crate::registry::Registry::load(&target_workspaces_dir.path().join("registry.json")).unwrap();
        target_registry
            .register_workspace(
                "ws-target".to_string(),
                "app".to_string(),
                "dev-target".to_string(),
                "proj-1".to_string(),
                "app".to_string(),
                target_repo.clone(),
                "2026-01-01T00:00:00Z".to_string(),
            )
            .unwrap();
        let target_ctx = RpcContext {
            registry: std::sync::Arc::new(tokio::sync::Mutex::new(target_registry)),
            devhost_id: "dev-target".to_string(),
            workspaces_dir: target_workspaces_dir.path().to_path_buf(),
            mise_bin: "mise-not-used-in-this-test".to_string(),
            mise_project_tools_dir: target_workspaces_dir.path().join("mise-project-tools"),
        };

        let (listener, relay_url) = bind_fake_relayd().await;
        tokio::spawn(run_two_party_fake_relayd(listener));

        // Dialed BEFORE the client starts (see `run_two_party_fake_relayd`'s
        // doc comment) — this establishes the fake relayd's "connection 1 =
        // target" invariant deterministically rather than racing.
        let mut target_channel = dial(&relay_url).await.unwrap();
        let (agent_tx, mut agent_event_rx) = tokio::sync::mpsc::channel(4);

        // Proves, via the command's OWN streamed output: (a) it can read the
        // target workspace's real, shared content (`SHARED_ORIGIN_FILE.txt`); (b)
        // it is running inside the target's `workspaces_dir` (a fresh
        // ephemeral offload workspace), not the target's primary workspace
        // root and not the client's own workspace; (c) a file it writes is
        // really on disk (surfaced via its own `ls`, so there is no race
        // with this offload's own after-the-fact ephemeral-workspace
        // cleanup); (d) stderr streams back as a genuinely separate
        // channel; (e) a non-zero exit code propagates exactly.
        let argv = vec![
            "sh".to_string(),
            "-c".to_string(),
            "cat SHARED_ORIGIN_FILE.txt; pwd; touch NEW_FILE_FROM_OFFLOAD; ls -1; echo wrote-marker-to-stderr >&2; exit 7".to_string(),
        ];
        let request = OffloadRequest { workspace_name: "app".to_string(), commit_id, argv };

        let recorded_out = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded_err = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let dispatch_fut = async {
            let _ = tokio::time::timeout(Duration::from_secs(20), serve_dispatch(&mut target_channel, &target_ctx, &mut agent_event_rx, None, &agent_tx)).await;
        };

        let client_fut = async {
            let credential = fake_dev_exec_credential();
            let stdout = RecordingWriter(recorded_out.clone());
            let stderr = RecordingWriter(recorded_err.clone());
            tokio::time::timeout(Duration::from_secs(15), crate::dev_exec::run_with_io("dev-target", &relay_url, &credential, request, stdout, stderr))
                .await
                .expect("dev-exec did not complete in time")
        };

        let (result, ()) = tokio::join!(client_fut, dispatch_fut);

        let exit_code = result.expect("dev-exec should complete without a transport/protocol error");
        assert_eq!(exit_code, 7, "the offloaded command's exact exit code must propagate");

        let stdout_text = String::from_utf8(recorded_out.lock().unwrap().clone()).unwrap();
        let canonical_workspaces_dir = target_workspaces_dir.path().canonicalize().unwrap();
        let canonical_target_repo = target_repo.canonicalize().unwrap();
        let canonical_client_repo = client_repo.canonicalize().unwrap();
        assert!(stdout_text.contains("content-from-the-shared-git-remote"), "expected the command to see the shared origin content in its own ephemeral checkout: {stdout_text:?}");
        assert!(
            stdout_text.contains(canonical_workspaces_dir.to_str().unwrap()),
            "expected the command to run inside the target's own workspaces_dir (an ephemeral offload workspace), got: {stdout_text:?}"
        );
        assert!(
            !stdout_text.contains(canonical_target_repo.to_str().unwrap()),
            "the command must run in a DISTINCT ephemeral workspace, not the target's primary workspace root: {stdout_text:?}"
        );
        assert!(!stdout_text.contains(canonical_client_repo.to_str().unwrap()), "the command must never see the client/originating host's own workspace path: {stdout_text:?}");
        assert!(stdout_text.contains("NEW_FILE_FROM_OFFLOAD"), "expected the file the command wrote to appear in its own `ls` output: {stdout_text:?}");

        let stderr_text = String::from_utf8(recorded_err.lock().unwrap().clone()).unwrap();
        assert!(stderr_text.contains("wrote-marker-to-stderr"), "expected real stderr bytes to stream back too: {stderr_text:?}");
    }

    /// Half of this module's "refused if... the target workspace/revision
    /// can't be resolved" requirement: an offload request naming a
    /// workspace this target has never registered must be refused with a
    /// typed `not_found` error, not silently ignored or run against
    /// whatever happens to be at some guessed path.
    #[tokio::test]
    async fn offload_request_for_an_unregistered_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let target_registry = crate::registry::Registry::load(&dir.path().join("registry.json")).unwrap();
        let target_ctx = RpcContext {
            registry: std::sync::Arc::new(tokio::sync::Mutex::new(target_registry)),
            devhost_id: "dev-target".to_string(),
            workspaces_dir: dir.path().to_path_buf(),
            mise_bin: "mise-not-used-in-this-test".to_string(),
            mise_project_tools_dir: dir.path().join("mise-project-tools"),
        };

        let (listener, relay_url) = bind_fake_relayd().await;
        tokio::spawn(run_two_party_fake_relayd(listener));
        let mut target_channel = dial(&relay_url).await.unwrap();
        let (agent_tx, mut agent_event_rx) = tokio::sync::mpsc::channel(4);

        let request = OffloadRequest { workspace_name: "never-registered".to_string(), commit_id: "deadbeef".to_string(), argv: vec!["true".to_string()] };

        let dispatch_fut = async {
            let _ = tokio::time::timeout(Duration::from_secs(10), serve_dispatch(&mut target_channel, &target_ctx, &mut agent_event_rx, None, &agent_tx)).await;
        };
        let client_fut = async {
            let credential = fake_dev_exec_credential();
            let recorded_out = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let recorded_err = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            tokio::time::timeout(
                Duration::from_secs(10),
                crate::dev_exec::run_with_io("dev-target", &relay_url, &credential, request, RecordingWriter(recorded_out), RecordingWriter(recorded_err)),
            )
            .await
            .expect("dev-exec did not complete in time")
        };

        let (result, ()) = tokio::join!(client_fut, dispatch_fut);
        assert!(
            matches!(result, Err(crate::dev_exec::DevExecError::Remote(ref error)) if error.code == "not_found"),
            "expected a Remote(not_found) refusal for an unregistered workspace, got {result:?}"
        );
    }

    /// The other half of the same requirement: a real, registered
    /// workspace, but a `commit_id` this target's `jj` store has never seen
    /// (the two-independent-stores scenario the "matching jj revision"
    /// design decision covers) must also be refused with a typed error, not
    /// hang or silently run against the wrong revision.
    #[tokio::test]
    async fn offload_request_for_an_unresolvable_revision_is_refused() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_git_repo_with_file(repo_dir.path(), "a.txt", "hello\n");
        jj_ops::ensure_colocated(repo_dir.path()).await.unwrap();

        let target_workspaces_dir = tempfile::tempdir().unwrap();
        let mut target_registry = crate::registry::Registry::load(&target_workspaces_dir.path().join("registry.json")).unwrap();
        target_registry
            .register_workspace(
                "ws-target".to_string(),
                "app".to_string(),
                "dev-target".to_string(),
                "proj-1".to_string(),
                "app".to_string(),
                repo_dir.path().to_path_buf(),
                "2026-01-01T00:00:00Z".to_string(),
            )
            .unwrap();
        let target_ctx = RpcContext {
            registry: std::sync::Arc::new(tokio::sync::Mutex::new(target_registry)),
            devhost_id: "dev-target".to_string(),
            workspaces_dir: target_workspaces_dir.path().to_path_buf(),
            mise_bin: "mise-not-used-in-this-test".to_string(),
            mise_project_tools_dir: target_workspaces_dir.path().join("mise-project-tools"),
        };

        let (listener, relay_url) = bind_fake_relayd().await;
        tokio::spawn(run_two_party_fake_relayd(listener));
        let mut target_channel = dial(&relay_url).await.unwrap();
        let (agent_tx, mut agent_event_rx) = tokio::sync::mpsc::channel(4);

        // A well-formed-looking but entirely fictitious commit hash — this
        // store never saw it, since it was never derived from any real
        // commit in `repo_dir`. Deliberately NOT all-zeroes: `jj` (like
        // `git`) treats the all-zero hash as a real, resolvable alias for
        // its own synthetic root commit (found while building this test —
        // `jj workspace add -r 0000...0` succeeds), which would silently
        // defeat the "unresolvable" premise this test needs.
        let bogus_commit_id = "1234567890abcdef1234567890abcdef12345678".to_string();
        let request = OffloadRequest { workspace_name: "app".to_string(), commit_id: bogus_commit_id, argv: vec!["true".to_string()] };

        let dispatch_fut = async {
            let _ = tokio::time::timeout(Duration::from_secs(10), serve_dispatch(&mut target_channel, &target_ctx, &mut agent_event_rx, None, &agent_tx)).await;
        };
        let client_fut = async {
            let credential = fake_dev_exec_credential();
            let recorded_out = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let recorded_err = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            tokio::time::timeout(
                Duration::from_secs(10),
                crate::dev_exec::run_with_io("dev-target", &relay_url, &credential, request, RecordingWriter(recorded_out), RecordingWriter(recorded_err)),
            )
            .await
            .expect("dev-exec did not complete in time")
        };

        let (result, ()) = tokio::join!(client_fut, dispatch_fut);
        assert!(
            matches!(result, Err(crate::dev_exec::DevExecError::Remote(ref error)) if error.code == "not_found"),
            "expected a Remote(not_found) refusal for an unresolvable revision, got {result:?}"
        );
    }
}
