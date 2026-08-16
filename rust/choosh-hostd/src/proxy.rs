//! `choosh-hostd proxy`: the laptop-side bridge, per
//! `docs/specs/ssh-bridge-and-zed.md`'s "Laptop proxy mode" section. No
//! daemon, no Zellij control, no workspace registry — three client-only
//! operations against `relayd`:
//!
//! - [`enroll`]: exchange a one-shot token for a laptop's own device
//!   credential (a distinct `IdentityClass::LaptopProxy` Identity from a
//!   devhost's own `serve` enrollment — see [`credential_path`]'s doc
//!   comment for why it lives at a different file), then runs [`sync`]
//!   once and installs a periodic trigger for it.
//! - [`connect`]: the literal `ProxyCommand` target — authenticate, open
//!   an `"ssh"`-purpose tunnel to the named devhost, and pipe stdio over
//!   it raw.
//! - [`sync`]: keeps `~/.ssh/known_hosts`/`~/.ssh/config` current against
//!   `relayd`'s fleet list, idempotently.
//!
//! **Why this duplicates a small amount of `serve.rs`'s dial/auth logic**
//! rather than sharing it: `serve.rs`'s `dial`/`run_one_connection` are
//! private to that module and built around `serve`'s own devhost-specific
//! state (an `RpcContext`, a long-lived reconnect loop, `pty:`/`web:`
//! tunnel bookkeeping) that a laptop-proxy connection has none of. Pulling
//! a shared abstraction out of `serve.rs` — an already-tested, working
//! module this task doesn't otherwise need to touch — was a worse
//! trade than the ~30 lines of connect+auth logic duplicated here in a
//! much smaller, purpose-built shape.

use std::io;
use std::path::{Path, PathBuf};

use base64::Engine;
use choosh_protocol::relay::{
    ControlRequest, ControlResponse, DevhostSshEndpoint, FRAME_CLASS_CONTROL, FRAME_CLASS_TUNNEL, IdentityClass, ServerHello,
    TUNNEL_ID_BYTES, decode_tunnel_id_hex,
};
use ed25519_dalek::SigningKey;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::credential::{self, Credential, CredentialError};
use crate::frame_channel::ChannelError;
use crate::relay_client::{self, RelayClientError, WsChannel};

#[derive(Debug)]
pub enum ProxyError {
    Credential(CredentialError),
    /// `proxy connect`/`proxy sync` ran before `proxy enroll`.
    NotEnrolled,
    Transport(String),
    AuthFailed(String),
    RelayError { code: String, message: String },
    UnexpectedResponse(String),
    /// `proxy connect <host_id>` named a host `relayd`'s fleet list
    /// doesn't currently know about.
    UnknownHost(String),
    Io(io::Error),
    SshKey(crate::ssh_keys::SshKeyError),
    InvalidHostKey(String),
}

impl std::fmt::Display for ProxyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Credential(error) => write!(f, "{error}"),
            Self::NotEnrolled => write!(f, "no laptop-proxy credential found; run `choosh-hostd proxy enroll --token <token>` first"),
            Self::Transport(reason) => write!(f, "relayd transport error: {reason}"),
            Self::AuthFailed(reason) => write!(f, "relayd rejected authentication: {reason}"),
            Self::RelayError { code, message } => write!(f, "relayd error {code}: {message}"),
            Self::UnexpectedResponse(reason) => write!(f, "unexpected response from relayd: {reason}"),
            Self::UnknownHost(host) => write!(f, "{host} is not a devhost in the current fleet (run `choosh-hostd proxy sync`?)"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::SshKey(error) => write!(f, "SSH key formatting error: {error}"),
            Self::InvalidHostKey(reason) => write!(f, "relayd returned an invalid SSH host key: {reason}"),
        }
    }
}

impl std::error::Error for ProxyError {}

impl From<CredentialError> for ProxyError {
    fn from(error: CredentialError) -> Self {
        Self::Credential(error)
    }
}

impl From<ChannelError> for ProxyError {
    fn from(error: ChannelError) -> Self {
        Self::Transport(error.to_string())
    }
}

impl From<io::Error> for ProxyError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::ssh_keys::SshKeyError> for ProxyError {
    fn from(error: crate::ssh_keys::SshKeyError) -> Self {
        Self::SshKey(error)
    }
}

impl From<RelayClientError> for ProxyError {
    fn from(error: RelayClientError) -> Self {
        match error {
            RelayClientError::Transport(reason) => Self::Transport(reason),
            RelayClientError::AuthFailed(reason) => Self::AuthFailed(reason),
            RelayClientError::Credential(error) => Self::Credential(error),
        }
    }
}

/// The laptop-proxy credential's own file, distinct from a devhost's own
/// `serve`-mode credential (`credential::default_path`) — the same
/// physical machine could plausibly run both a devhost `serve` and a
/// laptop `proxy` against the same `relayd` (e.g. a workstation that's
/// both a devhost other machines reach *and* a laptop reaching other
/// devhosts), and per auth-and-enrollment.md these are two independently
/// enrolled Identities (different `identity_class`, different
/// `device_id`) that must never be confused or overwrite one another.
///
/// # Errors
///
/// Returns [`CredentialError::NoConfigDir`] if no config directory can be
/// determined for the current user.
pub fn credential_path() -> Result<PathBuf, ProxyError> {
    if let Ok(path) = std::env::var("CHOOSH_HOSTD_PROXY_CREDENTIAL_PATH") {
        return Ok(PathBuf::from(path));
    }
    let dirs = directories::ProjectDirs::from("ai", "choosh", "hostd").ok_or(CredentialError::NoConfigDir)?;
    Ok(dirs.config_dir().join("laptop-proxy-credential.json"))
}

fn known_hosts_path() -> Result<PathBuf, ProxyError> {
    if let Ok(path) = std::env::var("CHOOSH_HOSTD_KNOWN_HOSTS_PATH") {
        return Ok(PathBuf::from(path));
    }
    Ok(home_dir()?.join(".ssh").join("known_hosts"))
}

fn ssh_config_path() -> Result<PathBuf, ProxyError> {
    if let Ok(path) = std::env::var("CHOOSH_HOSTD_SSH_CONFIG_PATH") {
        return Ok(PathBuf::from(path));
    }
    Ok(home_dir()?.join(".ssh").join("config"))
}

fn home_dir() -> Result<PathBuf, ProxyError> {
    std::env::var("HOME").map(PathBuf::from).map_err(|_| ProxyError::Io(io::Error::other("$HOME is not set")))
}

/// `choosh-hostd proxy enroll --token <token>`. Exchanges `token` for this
/// laptop's own `LaptopProxy` device credential, persists it, then runs
/// [`sync`] once and installs a periodic trigger for it — both required by
/// ssh-bridge-and-zed.md's "Laptop proxy mode" section.
///
/// # Errors
///
/// Returns [`ProxyError`] for any transport/auth/relayd-rejection failure.
/// A failure to install the periodic sync trigger is logged, not returned
/// — the credential and the one-shot `sync` this function already ran
/// both still succeeded, and the operator can install/run `sync`
/// themselves if the platform-specific scheduler step didn't work.
pub async fn enroll(token: &str) -> Result<(), ProxyError> {
    let path = credential_path()?;
    let signing_key = SigningKey::generate(&mut rand::rng());
    let public_key_b64 = base64::engine::general_purpose::STANDARD.encode(signing_key.verifying_key().to_bytes());

    let mut channel = relay_client::dial(&relay_client::relay_url()).await?;
    let _hello: ServerHello = channel.recv().await?;

    let request_id = relay_client::new_request_id();
    channel
        .send(
            FRAME_CLASS_CONTROL,
            &ControlRequest::Enroll {
                request_id,
                token: token.to_string(),
                identity_class: IdentityClass::LaptopProxy,
                public_key: public_key_b64,
                host_ssh_public_key: None, // laptop-proxy has no SSH server of its own.
                alias: None,
                platform: Some(std::env::consts::OS.to_string()),
                account_label: None,
            },
        )
        .await?;

    let response: ControlResponse = channel.recv().await?;
    let saved_credential = match response {
        ControlResponse::EnrollOk { device_id, certificate, .. } => {
            let credential = Credential::new(device_id, certificate, &signing_key);
            credential::save(&path, &credential)?;
            credential
        }
        ControlResponse::Error { code, message, .. } => return Err(ProxyError::RelayError { code, message }),
        other => return Err(ProxyError::UnexpectedResponse(format!("{other:?}"))),
    };
    tracing::info!(device_id = %saved_credential.device_id, "laptop-proxy enrollment complete, credential persisted");

    sync().await?;

    if let Err(error) = install_periodic_sync() {
        tracing::warn!(%error, "failed to install the periodic proxy-sync trigger; run `choosh-hostd proxy sync` manually or schedule it another way");
    }

    Ok(())
}

/// `choosh-hostd proxy connect <host_id>` — the literal `ProxyCommand`
/// target. Production entry point; delegates to [`connect_with_io`] with
/// real process stdio.
///
/// # Errors
///
/// See [`connect_with_io`]. Per ssh-bridge-and-zed.md: "MUST exit
/// non-zero, with no partial byte-piping, if authentication or tunnel
/// setup fails" — every error variant here is returned strictly before
/// any byte of `stdout` is touched (see `connect_with_io`'s structure).
pub async fn connect(host_id: &str) -> Result<(), ProxyError> {
    let credential = credential::load(&credential_path()?)?.ok_or(ProxyError::NotEnrolled)?;
    Box::pin(connect_with_io(host_id, &relay_client::relay_url(), &credential, tokio::io::stdin(), tokio::io::stdout())).await
}

/// The testable core of `proxy connect`: authenticate, resolve `host_id`
/// against the restricted fleet read, open an `"ssh"`-purpose tunnel, and
/// only then start piping `stdin`/`stdout` raw over it. Every fallible
/// step before the tunnel is actually open happens before `stdin`/`stdout`
/// are touched at all, so a caller-injected recording writer in tests can
/// assert zero bytes were ever written on any failure path.
///
/// # Errors
///
/// [`ProxyError::AuthFailed`]/[`ProxyError::Transport`] for a failed
/// dial/authenticate, [`ProxyError::UnknownHost`] if `host_id` isn't in
/// the current fleet, [`ProxyError::RelayError`] if `open-tunnel` itself
/// is rejected.
pub(crate) async fn connect_with_io<R, W>(host_id: &str, relay_url: &str, credential: &Credential, stdin: R, stdout: W) -> Result<(), ProxyError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut channel = relay_client::connect_authenticated(relay_url, credential).await?;

    let endpoints = fetch_ssh_endpoints(&mut channel).await?;
    let target = endpoints.iter().find(|endpoint| endpoint.alias == host_id).ok_or_else(|| ProxyError::UnknownHost(host_id.to_string()))?;

    let request_id = relay_client::new_request_id();
    channel
        .send(FRAME_CLASS_CONTROL, &ControlRequest::OpenTunnel { request_id, target_device_id: target.device_id.clone(), purpose: "ssh".to_string() })
        .await?;
    let tunnel_id = match channel.recv().await? {
        ControlResponse::OpenTunnelOk { tunnel_id, .. } => {
            decode_tunnel_id_hex(&tunnel_id).ok_or_else(|| ProxyError::UnexpectedResponse("relayd returned a malformed tunnel_id".to_string()))?
        }
        ControlResponse::Error { code, message, .. } => return Err(ProxyError::RelayError { code, message }),
        other => return Err(ProxyError::UnexpectedResponse(format!("{other:?}"))),
    };

    // Every failure mode above returns here, before this line — stdin/
    // stdout are never touched until the tunnel is genuinely open.
    pipe_stdio_over_tunnel(channel, tunnel_id, stdin, stdout).await
}

async fn fetch_ssh_endpoints(channel: &mut WsChannel) -> Result<Vec<DevhostSshEndpoint>, ProxyError> {
    let request_id = relay_client::new_request_id();
    channel.send(FRAME_CLASS_CONTROL, &ControlRequest::ListDevhostSshEndpoints { request_id }).await?;
    match channel.recv().await? {
        ControlResponse::ListDevhostSshEndpointsOk { endpoints, .. } => Ok(endpoints),
        ControlResponse::Error { code, message, .. } => Err(ProxyError::RelayError { code, message }),
        other => Err(ProxyError::UnexpectedResponse(format!("{other:?}"))),
    }
}

/// Raw, bidirectional byte piping between `stdin`/`stdout` and the opened
/// tunnel — "no protocol translation, buffering beyond what's needed for
/// the underlying transport, or inspection of the SSH bytes it carries",
/// per ssh-bridge-and-zed.md. Returns once either side closes.
async fn pipe_stdio_over_tunnel<R, W>(mut channel: WsChannel, tunnel_id: [u8; TUNNEL_ID_BYTES], mut stdin: R, mut stdout: W) -> Result<(), ProxyError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut read_buf = [0u8; 16384];
    loop {
        tokio::select! {
            frame = channel.recv_raw() => {
                match frame {
                    Ok((FRAME_CLASS_TUNNEL, body)) if body.len() >= TUNNEL_ID_BYTES => {
                        let (id_bytes, payload) = body.split_at(TUNNEL_ID_BYTES);
                        if id_bytes != tunnel_id {
                            continue; // not this tunnel; relay-protocol.md carries only one connection per proxy, but ignore rather than crash on a stray frame.
                        }
                        if payload.is_empty() {
                            return Ok(()); // the far end closed the tunnel.
                        }
                        stdout.write_all(payload).await?;
                        stdout.flush().await?;
                    }
                    Ok(_) => {} // an unrelated frame class/shape; ignore.
                    Err(_) => return Ok(()), // relayd connection ended — the SSH client observes this as its own stdout EOF.
                }
            }
            result = stdin.read(&mut read_buf) => {
                match result {
                    Ok(0) => {
                        // stdin EOF: forward as the tunnel close signal
                        // (an empty tunnel-data frame, same convention
                        // used everywhere else in relay-protocol.md), then
                        // stop — the wrapping ssh/Zed process closed its
                        // write side, so there's nothing further to pipe.
                        let mut payload = Vec::with_capacity(TUNNEL_ID_BYTES);
                        payload.extend_from_slice(&tunnel_id);
                        let _ = channel.send_bytes(FRAME_CLASS_TUNNEL, &payload).await;
                        return Ok(());
                    }
                    Ok(n) => {
                        let mut payload = Vec::with_capacity(TUNNEL_ID_BYTES + n);
                        payload.extend_from_slice(&tunnel_id);
                        payload.extend_from_slice(&read_buf[..n]);
                        if channel.send_bytes(FRAME_CLASS_TUNNEL, &payload).await.is_err() {
                            return Ok(());
                        }
                    }
                    Err(_) => return Ok(()),
                }
            }
        }
    }
}

/// `choosh-hostd proxy sync`. Production entry point; delegates to
/// [`sync_with_paths`] with the real credential/`known_hosts`/`config`
/// paths.
///
/// # Errors
///
/// See [`sync_with_paths`].
pub async fn sync() -> Result<(), ProxyError> {
    let credential = credential::load(&credential_path()?)?.ok_or(ProxyError::NotEnrolled)?;
    sync_with_paths(&relay_client::relay_url(), &credential, &known_hosts_path()?, &ssh_config_path()?).await
}

/// The testable core of `proxy sync`: fetch the restricted fleet read,
/// then update `known_hosts`/`config` idempotently per
/// ssh-bridge-and-zed.md's numbered algorithm.
///
/// # Errors
///
/// [`ProxyError::AuthFailed`]/[`ProxyError::Transport`] for a failed
/// dial/authenticate, [`ProxyError::RelayError`] if the restricted fleet
/// read itself is rejected, [`ProxyError::Io`] for a file I/O failure.
pub(crate) async fn sync_with_paths(relay_url: &str, credential: &Credential, known_hosts_path: &Path, ssh_config_path: &Path) -> Result<(), ProxyError> {
    let mut channel = relay_client::connect_authenticated(relay_url, credential).await?;
    let endpoints = fetch_ssh_endpoints(&mut channel).await?;

    let mut sorted = endpoints;
    sorted.sort_by(|a, b| a.alias.cmp(&b.alias));

    update_known_hosts(known_hosts_path, &sorted)?;
    update_ssh_config(ssh_config_path, &sorted)?;
    Ok(())
}

/// Trailing `known_hosts` comment field marking a line as choosh-managed
/// — the mechanism that lets `sync` find and remove *every* previously
/// written line on each pass (including ones for a devhost retired since
/// the last sync, which no longer appear anywhere in the current fleet
/// list to match against) before writing exactly the current fleet's
/// lines back, rather than needing to separately persist "what did I
/// write last time" across runs.
const KNOWN_HOSTS_MARKER: &str = "choosh-managed";

fn update_known_hosts(path: &Path, endpoints: &[DevhostSshEndpoint]) -> Result<(), ProxyError> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().filter(|line| !line.trim_end().ends_with(KNOWN_HOSTS_MARKER)).map(str::to_string).collect();

    for endpoint in endpoints {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(&endpoint.ssh_host_public_key)
            .map_err(|error| ProxyError::InvalidHostKey(error.to_string()))?;
        let key = crate::ssh_keys::openssh_public_key_algorithm_and_base64(&raw)?;
        lines.push(format!("{} {} {}", endpoint.alias, key, KNOWN_HOSTS_MARKER));
    }

    write_if_changed(path, &join_lines(&lines))
}

const CONFIG_BEGIN_MARKER: &str = "# BEGIN choosh";
const CONFIG_END_MARKER: &str = "# END choosh";

fn update_ssh_config(path: &Path, endpoints: &[DevhostSshEndpoint]) -> Result<(), ProxyError> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = existing.lines().collect();

    let mut region: Vec<String> = vec![CONFIG_BEGIN_MARKER.to_string()];
    for endpoint in endpoints {
        region.push(format!("Host {}", endpoint.alias));
        region.push("    ProxyCommand choosh-hostd proxy connect %h".to_string());
    }
    region.push(CONFIG_END_MARKER.to_string());

    let begin = lines.iter().position(|line| line.trim() == CONFIG_BEGIN_MARKER);
    let end = lines.iter().position(|line| line.trim() == CONFIG_END_MARKER);

    let mut new_lines: Vec<String> = Vec::new();
    match (begin, end) {
        (Some(begin_index), Some(end_index)) if begin_index <= end_index => {
            // Content outside the marker region — both before and after —
            // is copied through completely unchanged; only the region
            // itself (including its own begin/end marker lines) is
            // replaced.
            new_lines.extend(lines[..begin_index].iter().map(|s| (*s).to_string()));
            new_lines.extend(region);
            new_lines.extend(lines[end_index + 1..].iter().map(|s| (*s).to_string()));
        }
        _ => {
            // No existing, well-formed marker region: preserve every
            // hand-written line exactly as it is, then append a fresh
            // region at the end (never inserted into or interleaved with
            // the user's own content).
            new_lines.extend(lines.iter().map(|s| (*s).to_string()));
            if !new_lines.is_empty() {
                new_lines.push(String::new());
            }
            new_lines.extend(region);
        }
    }

    write_if_changed(path, &join_lines(&new_lines))
}

fn join_lines(lines: &[String]) -> Vec<u8> {
    let mut content = lines.join("\n");
    content.push('\n');
    content.into_bytes()
}

/// Writes `content` to `path` only if it differs from what's already
/// there — running `sync` twice with no fleet change must produce zero
/// diff, and skipping an identical rewrite is the simplest way to
/// guarantee that (no reliance on a byte-identical write producing a
/// byte-identical file, which it would anyway, but this also avoids
/// needlessly bumping the file's mtime on a no-op sync). Creates the
/// parent directory and writes atomically (temp file + rename), same
/// pattern as `credential::save`.
fn write_if_changed(path: &Path, content: &[u8]) -> Result<(), ProxyError> {
    if let Ok(existing) = std::fs::read(path)
        && existing == content
    {
        return Ok(());
    }
    let parent = path.parent().ok_or_else(|| ProxyError::Io(io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")))?;
    std::fs::create_dir_all(parent)?;
    crate::fs_util::atomic_write(path, content, None)?;
    Ok(())
}

/// Installs the periodic (every 15 minutes) `proxy sync` trigger, per
/// ssh-bridge-and-zed.md: a `systemd --user` timer on Linux, a `launchd`
/// `StartInterval` agent on macOS. Writes the unit file(s) and then
/// best-effort activates them (`systemctl --user enable --now` /
/// `launchctl load`) — activation failure is logged, not propagated,
/// since a real system's systemd/launchd session may not be reachable in
/// every environment `enroll` runs in (a container, a CI sandbox), and
/// the unit file having been written is still useful even if this
/// process can't activate it itself right now.
fn install_periodic_sync() -> Result<(), ProxyError> {
    let hostd_bin = std::env::current_exe().map_or_else(|_| "choosh-hostd".to_string(), |path| path.display().to_string());
    match std::env::consts::OS {
        "linux" => install_systemd_timer(&hostd_bin),
        "macos" => install_launchd_agent(&hostd_bin),
        other => {
            tracing::warn!(os = other, "no periodic proxy-sync installer for this OS; run `choosh-hostd proxy sync` manually or via cron");
            Ok(())
        }
    }
}

fn systemd_unit_dir() -> Result<PathBuf, ProxyError> {
    Ok(home_dir()?.join(".config").join("systemd").join("user"))
}

fn systemd_service_unit_content(hostd_bin: &str) -> String {
    format!(
        "[Unit]\nDescription=Choosh laptop-proxy sync\n\n[Service]\nType=oneshot\nExecStart={hostd_bin} proxy sync\n"
    )
}

const SYSTEMD_TIMER_UNIT_CONTENT: &str = "[Unit]\nDescription=Run choosh-hostd proxy sync every 15 minutes\n\n[Timer]\nOnBootSec=1min\nOnUnitActiveSec=15min\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n";

fn install_systemd_timer(hostd_bin: &str) -> Result<(), ProxyError> {
    let dir = systemd_unit_dir()?;
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("choosh-hostd-proxy-sync.service"), systemd_service_unit_content(hostd_bin))?;
    std::fs::write(dir.join("choosh-hostd-proxy-sync.timer"), SYSTEMD_TIMER_UNIT_CONTENT)?;

    match std::process::Command::new("systemctl").args(["--user", "enable", "--now", "choosh-hostd-proxy-sync.timer"]).output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => tracing::warn!(stderr = %String::from_utf8_lossy(&output.stderr), "systemctl --user enable --now failed; the timer unit is written but not active"),
        Err(error) => tracing::warn!(%error, "failed to invoke systemctl; the timer unit is written but not active"),
    }
    Ok(())
}

fn launchd_dir() -> Result<PathBuf, ProxyError> {
    Ok(home_dir()?.join("Library").join("LaunchAgents"))
}

fn launchd_plist_content(hostd_bin: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>ai.choosh.hostd-proxy-sync</string>\n\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{hostd_bin}</string>\n\t\t<string>proxy</string>\n\t\t<string>sync</string>\n\t</array>\n\t<key>StartInterval</key>\n\t<integer>900</integer>\n\t<key>RunAtLoad</key>\n\t<true/>\n</dict>\n</plist>\n"
    )
}

fn install_launchd_agent(hostd_bin: &str) -> Result<(), ProxyError> {
    let dir = launchd_dir()?;
    std::fs::create_dir_all(&dir)?;
    let plist_path = dir.join("ai.choosh.hostd-proxy-sync.plist");
    std::fs::write(&plist_path, launchd_plist_content(hostd_bin))?;

    match std::process::Command::new("launchctl").args(["load", "-w"]).arg(&plist_path).output() {
        Ok(output) if output.status.success() => {}
        Ok(output) => tracing::warn!(stderr = %String::from_utf8_lossy(&output.stderr), "launchctl load failed; the agent plist is written but not loaded"),
        Err(error) => tracing::warn!(%error, "failed to invoke launchctl; the agent plist is written but not loaded"),
    }
    Ok(())
}

/// A minimal in-memory `AsyncWrite` that records everything written to
/// it — used by this module's own tests to assert `connect_with_io` never
/// touches `stdout` on a failure path, without needing to intercept the
/// real process stdio.
#[cfg(test)]
struct RecordingWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

#[cfg(test)]
impl AsyncWrite for RecordingWriter {
    fn poll_write(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>, buf: &[u8]) -> std::task::Poll<io::Result<usize>> {
        self.0.lock().unwrap().extend_from_slice(buf);
        std::task::Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_protocol::framing::{FrameDecoder, FrameLimits, encode_frame};
    use choosh_protocol::relay::{AuthOk, AuthResult, MAX_CONTROL_FRAME_BYTES, MAX_TUNNEL_FRAME_BYTES};
    use futures_util::{SinkExt, StreamExt};
    use std::sync::{Arc, Mutex};
    use tokio_tungstenite::tungstenite::Message;

    async fn bind_fake_relayd() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, format!("ws://{addr}/connect"))
    }

    fn fake_credential() -> Credential {
        let signing_key = SigningKey::generate(&mut rand::rng());
        Credential::new("laptop-1".to_string(), "fake-cert".to_string(), &signing_key)
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

    /// Drives a fake `relayd`'s handshake far enough to authenticate a
    /// `laptop-proxy` connection: `ServerHello`, then accept whatever
    /// `ClientAuth` arrives and answer `AuthResult::Ok` unconditionally
    /// (these tests aren't exercising `relayd`'s own signature
    /// verification, which `choosh-relayd`'s own integration tests
    /// already cover).
    async fn fake_relayd_authenticate(ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>) {
        let hello = ServerHello { nonce: "test-nonce".to_string() };
        send_control(ws, &hello).await;
        let _client_auth: serde_json::Value = recv_control(ws).await;
        send_control(ws, &AuthResult::Ok(AuthOk { identity_class: IdentityClass::LaptopProxy, device_id: "laptop-1".to_string() })).await;
    }

    #[tokio::test]
    async fn connect_fails_cleanly_with_no_stdout_bytes_when_the_host_is_unknown() {
        let (listener, relay_url) = bind_fake_relayd().await;
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let stdout = RecordingWriter(recorded.clone());

        let client_fut = async {
            let credential = fake_credential();
            Box::pin(connect_with_io("no-such-host", &relay_url, &credential, tokio::io::empty(), stdout)).await
        };
        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            fake_relayd_authenticate(&mut ws).await;
            let _list_request: serde_json::Value = recv_control(&mut ws).await;
            send_control(&mut ws, &ControlResponse::ListDevhostSshEndpointsOk { request_id: "r".to_string(), endpoints: vec![] }).await;
        };

        let (result, ()) = tokio::join!(client_fut, server_fut);
        assert!(matches!(result, Err(ProxyError::UnknownHost(ref host)) if host == "no-such-host"), "expected UnknownHost, got {result:?}");
        assert!(recorded.lock().unwrap().is_empty(), "no bytes must ever reach stdout on a failed connect");
    }

    #[tokio::test]
    async fn connect_fails_cleanly_with_no_stdout_bytes_when_open_tunnel_is_rejected() {
        let (listener, relay_url) = bind_fake_relayd().await;
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let stdout = RecordingWriter(recorded.clone());

        let client_fut = async {
            let credential = fake_credential();
            Box::pin(connect_with_io("build-box", &relay_url, &credential, tokio::io::empty(), stdout)).await
        };
        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            fake_relayd_authenticate(&mut ws).await;
            let _list_request: serde_json::Value = recv_control(&mut ws).await;
            send_control(
                &mut ws,
                &ControlResponse::ListDevhostSshEndpointsOk {
                    request_id: "r".to_string(),
                    endpoints: vec![DevhostSshEndpoint { device_id: "dev-1".to_string(), alias: "build-box".to_string(), ssh_host_public_key: "cHVi".to_string() }],
                },
            )
            .await;
            let _open_tunnel_request: serde_json::Value = recv_control(&mut ws).await;
            send_control(&mut ws, &ControlResponse::Error { request_id: "r2".to_string(), code: "target_offline".to_string(), message: "offline".to_string() }).await;
        };

        let (result, ()) = tokio::join!(client_fut, server_fut);
        assert!(matches!(result, Err(ProxyError::RelayError { ref code, .. }) if code == "target_offline"), "expected RelayError, got {result:?}");
        assert!(recorded.lock().unwrap().is_empty(), "no bytes must ever reach stdout on a failed connect");
    }

    #[tokio::test]
    async fn connect_fails_cleanly_with_no_stdout_bytes_on_auth_failure() {
        let (listener, relay_url) = bind_fake_relayd().await;
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let stdout = RecordingWriter(recorded.clone());

        let client_fut = async {
            let credential = fake_credential();
            Box::pin(connect_with_io("build-box", &relay_url, &credential, tokio::io::empty(), stdout)).await
        };
        let server_fut = async {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let hello = ServerHello { nonce: "test-nonce".to_string() };
            send_control(&mut ws, &hello).await;
            let _client_auth: serde_json::Value = recv_control(&mut ws).await;
            send_control(&mut ws, &AuthResult::Failed(choosh_protocol::relay::AuthFailed { reason: "device revoked".to_string() })).await;
        };

        let (result, ()) = tokio::join!(client_fut, server_fut);
        assert!(matches!(result, Err(ProxyError::AuthFailed(ref reason)) if reason == "device revoked"), "expected AuthFailed, got {result:?}");
        assert!(recorded.lock().unwrap().is_empty(), "no bytes must ever reach stdout on a failed connect");
    }

    #[tokio::test]
    async fn connect_pipes_bytes_raw_in_both_directions_once_the_tunnel_is_open() {
        let (listener, relay_url) = bind_fake_relayd().await;
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let stdout = RecordingWriter(recorded.clone());
        let (stdin_write, stdin_read) = tokio::io::duplex(4096);

        let client_fut = async {
            let credential = fake_credential();
            Box::pin(connect_with_io("build-box", &relay_url, &credential, stdin_read, stdout)).await
        };
        // `stdin_write` is owned by (and so kept alive for the whole
        // duration of) `server_fut` below, not a separately spawned task:
        // spawning it separately would let the task finish (and so drop
        // the duplex write half) the instant it wrote its one message,
        // which delivers a spurious EOF to the client's "stdin" — racing
        // against the client having finished forwarding the server's
        // *earlier* push to "stdout" first, since `connect_with_io`'s
        // select loop treats stdin EOF as its own, independent reason to
        // return. Keeping the write half alive until `server_fut` itself
        // is done (only after it has already received and validated
        // `hello-from-laptop`) removes that race: the client can now only
        // ever exit via the tunnel-closed path, which happens strictly
        // after both directions have been exercised.
        let server_fut = async move {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            fake_relayd_authenticate(&mut ws).await;
            let _list_request: serde_json::Value = recv_control(&mut ws).await;
            send_control(
                &mut ws,
                &ControlResponse::ListDevhostSshEndpointsOk {
                    request_id: "r".to_string(),
                    endpoints: vec![DevhostSshEndpoint { device_id: "dev-1".to_string(), alias: "build-box".to_string(), ssh_host_public_key: "cHVi".to_string() }],
                },
            )
            .await;
            let _open_tunnel_request: serde_json::Value = recv_control(&mut ws).await;
            let tunnel_id = [3u8; TUNNEL_ID_BYTES];
            send_control(&mut ws, &ControlResponse::OpenTunnelOk { request_id: "r2".to_string(), tunnel_id: choosh_protocol::relay::encode_tunnel_id_hex(tunnel_id) }).await;

            // Server-to-client (relayd-to-proxy) direction: proves bytes
            // arriving over the tunnel reach the proxy's own "stdout".
            let mut server_to_client = vec![FRAME_CLASS_TUNNEL];
            server_to_client.extend_from_slice(&tunnel_id);
            server_to_client.extend_from_slice(b"hello-from-devhost");
            ws.send(Message::Binary(encode_frame(&server_to_client, MAX_TUNNEL_FRAME_BYTES).unwrap().into())).await.unwrap();

            // Client-to-server direction: proves bytes written to the
            // proxy's own "stdin" reach the tunnel as tunnel-data frames.
            let mut stdin_write = stdin_write;
            stdin_write.write_all(b"hello-from-laptop").await.unwrap();

            let mut decoder = FrameDecoder::new(FrameLimits::new(MAX_TUNNEL_FRAME_BYTES, 4).unwrap());
            loop {
                let Some(Ok(Message::Binary(bytes))) = ws.next().await else { panic!("expected a binary tunnel message") };
                for frame in decoder.feed(&bytes).unwrap() {
                    let (class, body) = frame.split_first().unwrap();
                    assert_eq!(*class, FRAME_CLASS_TUNNEL);
                    let (id_bytes, payload) = body.split_at(TUNNEL_ID_BYTES);
                    assert_eq!(id_bytes, tunnel_id);
                    if payload == b"hello-from-laptop" {
                        let _ = ws.close(None).await;
                        return;
                    }
                }
            }
        };

        let (_result, ()) = tokio::join!(client_fut, server_fut);
        let stdout_bytes = recorded.lock().unwrap().clone();
        assert_eq!(stdout_bytes, b"hello-from-devhost".to_vec());
    }

    #[test]
    fn update_known_hosts_is_idempotent_and_removes_retired_devhosts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        std::fs::write(&path, "user-added-line ssh-ed25519 AAAAunrelated\n").unwrap();

        let key_a = base64::engine::general_purpose::STANDARD.encode([1u8; 32]);
        let key_b = base64::engine::general_purpose::STANDARD.encode([2u8; 32]);
        let endpoints = vec![
            DevhostSshEndpoint { device_id: "dev-a".to_string(), alias: "alpha".to_string(), ssh_host_public_key: key_a.clone() },
            DevhostSshEndpoint { device_id: "dev-b".to_string(), alias: "beta".to_string(), ssh_host_public_key: key_b.clone() },
        ];

        update_known_hosts(&path, &endpoints).unwrap();
        let first_pass = std::fs::read_to_string(&path).unwrap();
        assert!(first_pass.contains("user-added-line"), "content outside choosh's own lines must be preserved");
        assert!(first_pass.contains("alpha") && first_pass.contains("beta"));

        // Idempotency: running again with the same fleet produces byte-identical content.
        update_known_hosts(&path, &endpoints).unwrap();
        let second_pass = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first_pass, second_pass, "running sync twice with no fleet change must produce no diff");

        // A changed fleet (beta retired, gamma added, alpha's key rotated) updates in place / removes.
        let key_a_rotated = base64::engine::general_purpose::STANDARD.encode([9u8; 32]);
        let key_c = base64::engine::general_purpose::STANDARD.encode([3u8; 32]);
        let changed = vec![
            DevhostSshEndpoint { device_id: "dev-a".to_string(), alias: "alpha".to_string(), ssh_host_public_key: key_a_rotated },
            DevhostSshEndpoint { device_id: "dev-c".to_string(), alias: "gamma".to_string(), ssh_host_public_key: key_c },
        ];
        update_known_hosts(&path, &changed).unwrap();
        let third_pass = std::fs::read_to_string(&path).unwrap();
        assert!(third_pass.contains("user-added-line"), "unrelated content must still be preserved");
        assert!(!third_pass.contains("beta"), "a retired devhost's line must be removed");
        assert!(third_pass.contains("gamma"), "a newly added devhost's line must appear");
        assert!(!third_pass.contains(&key_a), "alpha's old key must not remain after a rotation");
    }

    #[test]
    fn update_ssh_config_writes_a_marker_region_and_leaves_hand_written_content_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        std::fs::write(&path, "Host jump-box\n    HostName 10.0.0.5\n    User admin\n").unwrap();

        let endpoints = vec![DevhostSshEndpoint { device_id: "dev-a".to_string(), alias: "build-box".to_string(), ssh_host_public_key: "unused".to_string() }];
        update_ssh_config(&path, &endpoints).unwrap();
        let first_pass = std::fs::read_to_string(&path).unwrap();
        assert!(first_pass.contains("Host jump-box"));
        assert!(first_pass.contains("HostName 10.0.0.5"));
        assert!(first_pass.contains(CONFIG_BEGIN_MARKER) && first_pass.contains(CONFIG_END_MARKER));
        assert!(first_pass.contains("Host build-box"));
        assert!(first_pass.contains("ProxyCommand choosh-hostd proxy connect %h"));

        // Idempotency.
        update_ssh_config(&path, &endpoints).unwrap();
        let second_pass = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first_pass, second_pass, "running sync twice with no fleet change must produce no diff");

        // A retired devhost's Host block is removed on the next pass, and
        // the hand-written block outside the markers is still untouched.
        update_ssh_config(&path, &[]).unwrap();
        let third_pass = std::fs::read_to_string(&path).unwrap();
        assert!(third_pass.contains("Host jump-box"), "hand-written config outside the markers must survive a fleet change");
        assert!(!third_pass.contains("build-box"), "a retired devhost's Host block must be removed");
    }

    #[test]
    fn update_ssh_config_appends_a_fresh_region_when_no_marker_exists_yet() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        // No file at all yet — the common case on first `proxy enroll`.
        let endpoints = vec![DevhostSshEndpoint { device_id: "dev-a".to_string(), alias: "build-box".to_string(), ssh_host_public_key: "unused".to_string() }];
        update_ssh_config(&path, &endpoints).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Host build-box"));
    }

    /// End-to-end coverage of `enroll()` itself (not just its
    /// `sync`/`connect` building blocks tested above): a fake `relayd`
    /// accepts the unauthenticated `enroll` exchange on one connection,
    /// then a second, authenticated connection for the `sync` `enroll`
    /// MUST run once at the end (ssh-bridge-and-zed.md) — proving the
    /// credential is persisted AND `known_hosts`/`config` get written
    /// from that same call, with no separate manual `proxy sync` needed.
    #[tokio::test]
    async fn enroll_persists_a_credential_and_runs_sync_once_at_the_end() {
        let (listener, relay_url) = bind_fake_relayd().await;
        let dir = tempfile::tempdir().unwrap();
        let credential_path = dir.path().join("laptop-proxy-credential.json");
        let known_hosts_path = dir.path().join("known_hosts");
        let ssh_config_path = dir.path().join("config");
        let fake_home = dir.path().join("home");
        std::fs::create_dir_all(&fake_home).unwrap();

        let server_fut = async {
            // Connection 1: the unauthenticated `enroll` exchange.
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let hello = ServerHello { nonce: "test-nonce".to_string() };
            send_control(&mut ws, &hello).await;
            let enroll_request: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::Enroll { request_id, identity_class, host_ssh_public_key, .. } = enroll_request else {
                panic!("expected an Enroll request, got {enroll_request:?}");
            };
            assert_eq!(identity_class, IdentityClass::LaptopProxy);
            assert!(host_ssh_public_key.is_none(), "a laptop-proxy has no SSH server of its own");
            send_control(&mut ws, &ControlResponse::EnrollOk { request_id, device_id: "laptop-1".to_string(), certificate: "fake-cert".to_string() }).await;
            let _ = ws.close(None).await;

            // Connection 2: `sync`'s own authenticated connection.
            let (stream2, _addr2) = listener.accept().await.unwrap();
            let mut ws2 = tokio_tungstenite::accept_async(stream2).await.unwrap();
            fake_relayd_authenticate(&mut ws2).await;
            let _list_request: serde_json::Value = recv_control(&mut ws2).await;
            send_control(
                &mut ws2,
                &ControlResponse::ListDevhostSshEndpointsOk {
                    request_id: "r".to_string(),
                    endpoints: vec![DevhostSshEndpoint {
                        device_id: "dev-1".to_string(),
                        alias: "build-box".to_string(),
                        ssh_host_public_key: base64::engine::general_purpose::STANDARD.encode([1u8; 32]),
                    }],
                },
            )
            .await;
        };

        let client_fut = temp_env::async_with_vars(
            [
                ("CHOOSH_RELAYD_URL", Some(relay_url.as_str())),
                ("CHOOSH_HOSTD_PROXY_CREDENTIAL_PATH", Some(credential_path.to_str().unwrap())),
                ("CHOOSH_HOSTD_KNOWN_HOSTS_PATH", Some(known_hosts_path.to_str().unwrap())),
                ("CHOOSH_HOSTD_SSH_CONFIG_PATH", Some(ssh_config_path.to_str().unwrap())),
                ("HOME", Some(fake_home.to_str().unwrap())),
            ],
            enroll("one-shot-token"),
        );

        let (result, ()) = tokio::join!(client_fut, server_fut);
        result.expect("enroll should succeed");

        let saved = credential::load(&credential_path).unwrap().expect("credential file should exist");
        assert_eq!(saved.device_id, "laptop-1");

        let known_hosts = std::fs::read_to_string(&known_hosts_path).unwrap();
        assert!(known_hosts.contains("build-box"), "sync must have run as part of enroll: {known_hosts}");
        let ssh_config = std::fs::read_to_string(&ssh_config_path).unwrap();
        assert!(ssh_config.contains("Host build-box"));

        // Best-effort periodic-sync installer also ran (Linux in this
        // sandbox): the unit files land under the overridden $HOME, even
        // though activating them via a real systemd --user session isn't
        // expected to succeed in a test sandbox.
        if std::env::consts::OS == "linux" {
            let service_path = fake_home.join(".config/systemd/user/choosh-hostd-proxy-sync.service");
            let timer_path = fake_home.join(".config/systemd/user/choosh-hostd-proxy-sync.timer");
            assert!(service_path.exists(), "expected the systemd service unit to be written");
            assert!(timer_path.exists(), "expected the systemd timer unit to be written");
        }
    }

    #[test]
    fn systemd_and_launchd_unit_content_reference_proxy_sync() {
        let service = systemd_service_unit_content("/usr/local/bin/choosh-hostd");
        assert!(service.contains("/usr/local/bin/choosh-hostd proxy sync"));
        assert!(SYSTEMD_TIMER_UNIT_CONTENT.contains("OnUnitActiveSec=15min"));

        let plist = launchd_plist_content("/usr/local/bin/choosh-hostd");
        assert!(plist.contains("<string>proxy</string>"));
        assert!(plist.contains("<string>sync</string>"));
        assert!(plist.contains("<integer>900</integer>"), "15 minutes = 900 seconds");
    }
}
