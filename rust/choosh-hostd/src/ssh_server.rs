//! The loopback SSH server, per `docs/specs/ssh-bridge-and-zed.md`'s
//! "Loopback SSH server" and "Session handling" sections.
//!
//! **Trust model** (why this is not, and cannot be, a normal SSH server):
//! this server performs no client-side key-based authentication of its
//! own — every `auth_*` callback below unconditionally accepts. That's
//! deliberate, not a placeholder: a connection only ever reaches this
//! server's loopback socket via a `"ssh"`-purpose tunnel that `relayd`
//! already authenticated as a `laptop-proxy` or `phone` Identity before
//! opening it (see `serve.rs`'s `handle_control_push`, which refuses to
//! bridge a tunnel-offer with no `from_device_id`). By the time SSH bytes
//! reach `russh`, the connection has already been vouched for; asking the
//! SSH layer to re-authenticate it would just be a second, redundant
//! (and, since there's no real per-user key material here, theatrical)
//! gate.
//!
//! **What was and wasn't ported from the legacy `choosh-ssh` crate**: that
//! crate is entirely an SSH *client* (`russh::client`, host-key-pinning,
//! public-key client authentication) built for the pre-reset SSH-only
//! architecture's opposite direction — a devhost dispatcher invoked
//! *through* SSH as transport for a bespoke binary protocol
//! (`choosh-host --exec-stdio-v1`), not a real interactive SSH server for
//! unmodified `ssh`/Zed clients. Nothing in it uses `russh::server` at
//! all. None of its code — connection admission, host-key pinning,
//! public-key signing, the fixed-command dispatcher encoding — is reusable
//! here: this module is the other side of the connection, with the
//! opposite trust model (no client authentication at all vs. exact
//! client-key verification) and a different job (be a normal SSH server
//! real clients already speak to, vs. drive a private binary protocol
//! over an SSH transport). The one thing worth carrying forward was
//! purely informational: the pinned `russh = "0.62.2"` version, reused
//! here (see `Cargo.toml`) for consistency across the workspace.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use russh::server::{Auth, Handle, Handler, Msg, Server as _, Session};
use russh::{Channel, ChannelId, Pty};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc};

use choosh_protocol::relay::{WireAgentEvent, WireEditor};

use crate::mise_ops;
use crate::registry::Registry;
use crate::ssh_keys;

#[derive(Debug)]
pub enum SshServerError {
    HostKey(ssh_keys::SshKeyError),
    Bind(std::io::Error),
}

impl std::fmt::Display for SshServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HostKey(error) => write!(f, "failed to derive the SSH host key: {error}"),
            Self::Bind(error) => write!(f, "failed to bind the loopback SSH server: {error}"),
        }
    }
}

impl std::error::Error for SshServerError {}

/// Shared, cheaply-`Clone`-able state every per-connection [`SessionHandler`]
/// needs — constructed once in `serve::run` and handed to
/// [`spawn_loopback_server`].
#[derive(Clone)]
pub struct SshBridgeConfig {
    pub registry: Arc<Mutex<Registry>>,
    /// `choosh-hostd`'s own dedicated `mise` data/config root for
    /// host-managed tools (see `mise_ops`'s doc comment) — never a
    /// workspace's own `mise.toml` resolution.
    pub mise_host_tools_dir: PathBuf,
    pub mise_bin: String,
    /// Drained by `serve.rs`'s `serve_dispatch` loop and forwarded to
    /// `relayd` as `ControlRequest::AgentEvent` — the exact same channel
    /// `local_ipc`-sourced hook events already use, per
    /// `ssh-bridge-and-zed.md`'s "Editor presence" section: this is an
    /// extension of that existing routing, not a second, parallel path.
    pub agent_event_tx: mpsc::Sender<WireAgentEvent>,
}

/// Binds the loopback-only SSH server and runs it for the lifetime of the
/// returned task. Per ssh-bridge-and-zed.md: "`choosh-hostd serve` MUST
/// bind its SSH server to a loopback address only" — this never binds
/// anything but `127.0.0.1`. Returns the bound (ephemeral) port so
/// `serve.rs` can bridge `"ssh"`-purpose relay tunnels to it with the
/// exact same loopback TCP-bridge mechanism already used for `web:`/
/// `zellij-web` tunnels — this module has no relay/tunnel awareness of its
/// own, and none is needed: by the time bytes reach here they're already
/// a plain loopback TCP connection.
///
/// # Errors
///
/// Returns [`SshServerError::HostKey`] if the devhost's enrollment signing
/// key cannot be converted to an SSH host key (not expected for a
/// well-formed key), or [`SshServerError::Bind`] if the loopback socket
/// cannot be bound.
pub async fn spawn_loopback_server(
    host_signing_key: &SigningKey,
    config: SshBridgeConfig,
) -> Result<u16, SshServerError> {
    let host_key = ssh_keys::host_private_key(host_signing_key).map_err(SshServerError::HostKey)?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(SshServerError::Bind)?;
    let port = listener.local_addr().map_err(SshServerError::Bind)?.port();

    let russh_config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        nodelay: true,
        ..Default::default()
    });

    let mut server = SshBridge { config };
    tokio::spawn(async move {
        if let Err(error) = server.run_on_socket(russh_config, &listener).await {
            tracing::error!(%error, "loopback SSH server stopped");
        }
    });

    Ok(port)
}

#[derive(Clone)]
struct SshBridge {
    config: SshBridgeConfig,
}

impl russh::server::Server for SshBridge {
    type Handler = SessionHandler;

    fn new_client(&mut self, _peer_addr: Option<std::net::SocketAddr>) -> SessionHandler {
        SessionHandler { config: self.config.clone(), channels: HashMap::new() }
    }

    fn handle_session_error(&mut self, error: <Self::Handler as Handler>::Error) {
        tracing::debug!(%error, "SSH session ended with an error");
    }
}

/// Per-channel state once a shell or exec has actually started — the
/// write half a `data()` callback forwards client input into, plus enough
/// to clean up when the channel closes.
enum RunningChannel {
    /// A PTY-backed child (interactive shell, or a plain, unrecognized
    /// exec — both behave "exactly as it would over a normal SSH
    /// connection", per ssh-bridge-and-zed.md, which for a real sshd
    /// means a real controlling terminal is available when one was
    /// requested).
    Pty { master_write: tokio::fs::File, child: tokio::process::Child },
    /// A raw-pipe child (the Zed `zed-remote-server` exec path): no PTY,
    /// since Zed's remote-server speaks a binary framed protocol over
    /// stdio that a PTY's line-discipline processing (echo, `ICANON`,
    /// CR/LF translation) would corrupt.
    Piped { stdin: tokio::process::ChildStdin, child: tokio::process::Child },
}

struct SessionHandler {
    config: SshBridgeConfig,
    channels: HashMap<ChannelId, RunningChannel>,
}

impl Handler for SessionHandler {
    type Error = russh::Error;

    // No client-side key-based authentication challenge of its own — see
    // this module's doc comment. Every method below unconditionally
    // admits the session; only the transport that got a connection here
    // at all (a relayd-brokered tunnel) is the real gate.
    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn auth_publickey_offered(&mut self, _user: &str, _key: &russh::keys::ssh_key::PublicKey) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn auth_publickey(&mut self, _user: &str, _key: &russh::keys::ssh_key::PublicKey) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // A pty is allocated lazily when the shell/exec actually starts
        // (see `spawn_pty_backed`) — this callback only needs to
        // acknowledge the request itself; resizing an already-running
        // session's pty on a later `window_change_request` is not
        // implemented (a scoped simplification, not required by
        // ssh-bridge-and-zed.md's session-handling requirements).
        let _ = session.channel_success(channel);
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        let handle = session.handle();
        match spawn_pty_backed(&shell_command(), &[]) {
            Ok((master_write, child, output_task_input)) => {
                self.channels.insert(channel, RunningChannel::Pty { master_write, child });
                spawn_pty_output_forwarder(handle, channel, output_task_input);
                let _ = session.channel_success(channel);
            }
            Err(error) => {
                tracing::warn!(%error, "failed to spawn interactive shell");
                let _ = session.channel_failure(channel);
            }
        }
        Ok(())
    }

    async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        let Ok(command) = std::str::from_utf8(data) else {
            tracing::warn!("exec command was not valid UTF-8, rejecting");
            let _ = session.channel_failure(channel);
            return Ok(());
        };

        if let Some(invocation) = zed_pattern::detect(command) {
            self.exec_zed_remote_server(channel, invocation, session).await;
            return Ok(());
        }

        // Not a recognized Zed invocation: an ordinary exec, handled
        // exactly like a normal SSH server would — the user's shell
        // interprets the command text (this is real sshd behavior for
        // any `exec` request, not something this bridge adds; see
        // ssh-bridge-and-zed.md's "no shell interpolation" rule, which
        // governs *this module's own* argv construction for the
        // recognized zellij/zed paths, not a normal client's own exec
        // text). Covers both a plain command (`zellij attach <workspace>`)
        // and anything else a real SSH client might run.
        let handle = session.handle();
        match spawn_pty_backed(&shell_command(), &["-c".to_string(), command.to_string()]) {
            Ok((master_write, child, output_task_input)) => {
                self.channels.insert(channel, RunningChannel::Pty { master_write, child });
                spawn_pty_output_forwarder(handle, channel, output_task_input);
                let _ = session.channel_success(channel);
            }
            Err(error) => {
                tracing::warn!(%error, %command, "failed to spawn exec command");
                let _ = session.channel_failure(channel);
            }
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // See `pty_request`'s doc comment: resize is not wired to the
        // underlying pty here, but the request is still acknowledged so a
        // client doesn't perceive it as a protocol error.
        let _ = session.channel_success(channel);
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], _session: &mut Session) -> Result<(), Self::Error> {
        match self.channels.get_mut(&channel) {
            Some(RunningChannel::Pty { master_write, .. }) => {
                let _ = master_write.write_all(data).await;
            }
            Some(RunningChannel::Piped { stdin, .. }) => {
                let _ = stdin.write_all(data).await;
            }
            None => {}
        }
        Ok(())
    }

    async fn channel_eof(&mut self, channel: ChannelId, _session: &mut Session) -> Result<(), Self::Error> {
        // Client half-closed its input: drop our write half (if any) so
        // the child observes EOF on its stdin, without tearing down the
        // channel outright — output may still be in flight.
        if let Some(RunningChannel::Piped { stdin, .. }) = self.channels.get_mut(&channel) {
            let _ = stdin.shutdown().await;
        }
        Ok(())
    }

    async fn channel_close(&mut self, channel: ChannelId, _session: &mut Session) -> Result<(), Self::Error> {
        if let Some(RunningChannel::Pty { mut child, .. } | RunningChannel::Piped { mut child, .. }) = self.channels.remove(&channel) {
            let _ = child.start_kill();
        }
        Ok(())
    }
}

impl SessionHandler {
    /// The Zed-remote-server exec path: version-check/update against the
    /// `mise`-managed install (toolchain-provisioning.md), then exec the
    /// resolved binary with fixed argv over raw stdio (no PTY, no shell) —
    /// and, per ssh-bridge-and-zed.md's "Editor presence" section, emit
    /// `editor_attached`/`editor_detached` around the process's lifetime.
    async fn exec_zed_remote_server(&mut self, channel: ChannelId, invocation: zed_pattern::ZedInvocation, session: &mut Session) {
        let resolved = mise_ops::ensure_zed_remote_server(&self.config.mise_bin, &invocation.version, &self.config.mise_host_tools_dir).await;
        let resolved_path = match resolved {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(%error, version = %invocation.version, "failed to provision zed-remote-server");
                let handle = session.handle();
                let message = format!("choosh-hostd: failed to provision zed-remote-server {}: {error}\n", invocation.version);
                let _ = handle.extended_data(channel, 1, message.into_bytes()).await;
                let _ = session.channel_failure(channel);
                let _ = handle.exit_status_request(channel, 1).await;
                let _ = handle.close(channel).await;
                return;
            }
        };

        // Fixed argv: the resolved, mise-managed binary path plus exactly
        // the trailing arguments the client's own exec text carried after
        // the recognized zed-remote-server token — never a shell, never
        // string-concatenated, per ssh-bridge-and-zed.md's "Command
        // construction" rule.
        let mut command = Command::new(&resolved_path);
        command.args(&invocation.trailing_args);
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(%error, path = %resolved_path.display(), "failed to spawn zed-remote-server");
                let _ = session.channel_failure(channel);
                return;
            }
        };
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let workspace_id = resolve_sole_workspace_id(&self.config.registry).await;
        if let Some(workspace_id) = &workspace_id {
            let _ = self
                .config
                .agent_event_tx
                .send(WireAgentEvent::EditorAttached { workspace_id: workspace_id.clone(), editor: WireEditor::Zed })
                .await;
        }

        self.channels.insert(channel, RunningChannel::Piped { stdin, child });

        let handle = session.handle();
        let agent_event_tx = self.config.agent_event_tx.clone();
        tokio::spawn(async move {
            Box::pin(forward_piped_output(handle.clone(), channel, stdout, stderr)).await;
            if let Some(workspace_id) = workspace_id {
                let _ = agent_event_tx.send(WireAgentEvent::EditorDetached { workspace_id, editor: WireEditor::Zed }).await;
            }
        });

        let _ = session.channel_success(channel);
    }
}

/// If this devhost has exactly one registered workspace, attribute a Zed
/// SSH session to it. **Known, documented limitation, not an oversight**:
/// Zed's real SSH exec invocation (verified against
/// `crates/remote/src/transport/ssh.rs` in zed-industries/zed) carries no
/// project-path or workspace-identifying information at all — the
/// `--identifier` value it sends is Zed's own opaque local-database id,
/// and the actual project path is chosen entirely inside the app-level
/// RPC protocol this bridge deliberately never parses (DESIGN.md §9:
/// "`hostd` doesn't need to know anything about that RPC protocol").
/// A devhost hosting more than one workspace therefore has no reliable
/// transport-layer signal for which one a given Zed session is editing;
/// rather than fabricate an attribution, this degrades to "no presence
/// event" outside the single-workspace case, which is still correct (if
/// less informative) for `EditorPresence`'s read-only indicator.
async fn resolve_sole_workspace_id(registry: &Arc<Mutex<Registry>>) -> Option<String> {
    let registry = registry.lock().await;
    match registry.list_workspaces() {
        [only] => Some(only.workspace_id.clone()),
        _ => None,
    }
}

fn shell_command() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Allocates a real pty and spawns `program args` wired to it, via
/// `pty.rs`'s [`crate::pty::allocate_and_spawn`] — the same allocation
/// mechanics `pty.rs`'s `PtySession` uses for the Zellij-attach path
/// (`zellij attach` needs a genuine controlling terminal, and so does any
/// ordinary interactive shell), shared rather than duplicated. Returns the
/// master's write half (for `data()` to forward client input into,
/// `try_clone`d immediately since it needs to live inside a synchronous
/// callback while the read half below moves into a spawned task right
/// away — see `allocate_and_spawn`'s own doc comment for why this differs
/// from `PtySession`'s single-handle-then-`.split()` approach), the child
/// (for `channel_close` to kill), and a reader over the master for the
/// caller to forward as channel output.
fn spawn_pty_backed(program: &str, args: &[String]) -> std::io::Result<(tokio::fs::File, tokio::process::Child, tokio::fs::File)> {
    let mut command = Command::new(program);
    command.args(args);
    let (master_std, child) = crate::pty::allocate_and_spawn(command)?;
    let master_write = tokio::fs::File::from_std(master_std.try_clone()?);
    let master_read = tokio::fs::File::from_std(master_std);
    Ok((master_write, child, master_read))
}

/// Forwards everything read from `master_read` as SSH channel `data`
/// pushes, until the pty closes (the child exited) — then reports an exit
/// status (best-effort; a pty doesn't cleanly separate stdout/stderr, so
/// there is no `ExtendedData` here, matching a real interactive shell
/// session) and closes the channel.
fn spawn_pty_output_forwarder(handle: Handle, channel: ChannelId, mut master_read: tokio::fs::File) {
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            // The master fd is deliberately non-blocking (see
            // `spawn_pty_backed`'s doc comment); `read_retrying_would_block`
            // (shared with `pty.rs`'s own reader, which has the same race
            // against its own, differently-obtained pty master) retries a
            // `WouldBlock` rather than treating it as end-of-stream. Any
            // other error, or a clean `Ok(0)` (the pty's slave side fully
            // closed), really is the end.
            match crate::pty::read_retrying_would_block(&mut master_read, &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if handle.data(channel, buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = handle.eof(channel).await;
        let _ = handle.exit_status_request(channel, 0).await;
        let _ = handle.close(channel).await;
    });
}

/// Forwards a raw-piped child's stdout/stderr as SSH channel `data`/
/// `extended_data` (code 1) pushes, then its real exit status, then closes
/// the channel — the Zed exec path's counterpart to
/// [`spawn_pty_output_forwarder`], reading two separate streams instead of
/// one pty master since there is no pty here to merge them.
async fn forward_piped_output(handle: Handle, channel: ChannelId, mut stdout: impl AsyncRead + Unpin, mut stderr: impl AsyncRead + Unpin) {
    let mut stdout_buf = [0u8; 8192];
    let mut stderr_buf = [0u8; 8192];
    loop {
        tokio::select! {
            result = stdout.read(&mut stdout_buf) => match result {
                Ok(0) | Err(_) => break,
                Ok(n) => { if handle.data(channel, stdout_buf[..n].to_vec()).await.is_err() { break; } }
            },
            result = stderr.read(&mut stderr_buf) => match result {
                Ok(0) | Err(_) => {}
                Ok(n) => { if handle.extended_data(channel, 1, stderr_buf[..n].to_vec()).await.is_err() { break; } }
            },
        }
    }
    let _ = handle.eof(channel).await;
    let _ = handle.exit_status_request(channel, 0).await;
    let _ = handle.close(channel).await;
}

/// Detects Zed's remote-server SSH exec invocation and extracts the
/// version it declares — see [`SessionHandler::exec_zed_remote_server`]'s
/// doc comment for the real-world shape this is matched against (verified
/// via zed-industries/zed's `crates/remote/src/transport/ssh.rs`:
/// `zed-remote-server-<channel>-<version>[.exe]`, invoked as `<path>
/// version` to probe and `<path> proxy --identifier <id> [--reconnect]`
/// to run — no shell wrapping, no working-directory change, no
/// project-path argument).
mod zed_pattern {
    #[derive(Debug, Clone, Eq, PartialEq)]
    pub struct ZedInvocation {
        pub version: String,
        pub trailing_args: Vec<String>,
    }

    const BINARY_PREFIX: &str = "zed-remote-server";

    /// # Panics
    ///
    /// Never — `detect` only calls this on a non-empty, whitespace-only
    /// tokenization it already validated.
    pub fn detect(command: &str) -> Option<ZedInvocation> {
        let tokens = tokenize(command)?;
        let (index, token) = tokens.iter().enumerate().find(|(_, token)| basename(token).starts_with(BINARY_PREFIX))?;

        let basename = basename(token);
        let trailing = tokens[index + 1..].to_vec();

        // The common, real shape: version embedded in the binary's own
        // filename (`zed-remote-server-stable-0.190.0`,
        // `zed-remote-server-dev-build`). Anything after the fixed
        // `zed-remote-server-` prefix is the version string Zed declares.
        if let Some(version) = basename.strip_prefix(&format!("{BINARY_PREFIX}-"))
            && !version.is_empty()
        {
            return Some(ZedInvocation { version: version.to_string(), trailing_args: trailing });
        }

        // A bare `zed-remote-server` token (no version suffix): only a
        // recognized invocation if a version is declared explicitly via a
        // `--version`/`--version=<v>` flag — with no version anywhere,
        // there is nothing to compare against the mise-managed install
        // per toolchain-provisioning.md, so this isn't treated as a
        // matching Zed invocation at all (falls through to the generic
        // exec path instead of erroring).
        if basename == BINARY_PREFIX {
            let version = extract_version_flag(&trailing)?;
            return Some(ZedInvocation { version, trailing_args: trailing });
        }

        None
    }

    fn basename(token: &str) -> &str {
        token.rsplit('/').next().unwrap_or(token)
    }

    fn extract_version_flag(args: &[String]) -> Option<String> {
        for (index, arg) in args.iter().enumerate() {
            if let Some(value) = arg.strip_prefix("--version=") {
                return Some(value.to_string());
            }
            if arg == "--version" {
                return args.get(index + 1).cloned();
            }
        }
        None
    }

    /// A conservative, shell-independent tokenizer: whitespace-separated
    /// words, with single/double-quoted groups treated as one token
    /// (backslash-escaping honored inside double quotes and unquoted
    /// text). This never invokes an actual shell — per
    /// ssh-bridge-and-zed.md's "no shell interpolation" rule — it only
    /// classifies the client's already-received exec text well enough to
    /// recognize the Zed pattern.
    fn tokenize(command: &str) -> Option<Vec<String>> {
        let mut tokens = Vec::new();
        let mut current = String::new();
        let mut in_token = false;
        let mut chars = command.chars().peekable();

        while let Some(ch) = chars.next() {
            match ch {
                ' ' | '\t' | '\n' | '\r' if !in_token => {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                }
                '\'' => {
                    in_token = true;
                    for inner in chars.by_ref() {
                        if inner == '\'' {
                            break;
                        }
                        current.push(inner);
                    }
                }
                '"' => {
                    in_token = true;
                    while let Some(inner) = chars.next() {
                        if inner == '"' {
                            break;
                        }
                        if inner == '\\' && let Some(&next) = chars.peek() {
                            current.push(next);
                            chars.next();
                        } else {
                            current.push(inner);
                        }
                    }
                }
                '\\' => {
                    in_token = true;
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                other if other.is_whitespace() => {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
                other => {
                    in_token = true;
                    current.push(other);
                }
            }
        }
        if !current.is_empty() {
            tokens.push(current);
        }
        if tokens.is_empty() { None } else { Some(tokens) }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn detects_version_embedded_in_the_binary_filename() {
            let invocation = detect("/home/user/.zed_server/zed-remote-server-stable-0.190.0 version").unwrap();
            assert_eq!(invocation.version, "stable-0.190.0");
            assert_eq!(invocation.trailing_args, vec!["version".to_string()]);
        }

        #[test]
        fn detects_the_proxy_run_invocation_and_forwards_trailing_args_unchanged() {
            let invocation =
                detect("/home/user/.zed_server/zed-remote-server-stable-0.190.0 proxy --identifier workspace-42 --reconnect").unwrap();
            assert_eq!(invocation.version, "stable-0.190.0");
            assert_eq!(
                invocation.trailing_args,
                vec!["proxy".to_string(), "--identifier".to_string(), "workspace-42".to_string(), "--reconnect".to_string()]
            );
        }

        #[test]
        fn detects_a_bare_binary_name_with_an_explicit_version_flag() {
            let invocation = detect("zed-remote-server --version=0.190.0 run").unwrap();
            assert_eq!(invocation.version, "0.190.0");

            let invocation2 = detect("zed-remote-server --version 0.190.0 run").unwrap();
            assert_eq!(invocation2.version, "0.190.0");
        }

        #[test]
        fn a_bare_binary_name_with_no_version_anywhere_is_not_a_recognized_invocation() {
            assert_eq!(detect("zed-remote-server run"), None);
        }

        #[test]
        fn unrelated_commands_are_not_recognized() {
            assert_eq!(detect("zellij attach app"), None);
            assert_eq!(detect("/bin/bash -l"), None);
            assert_eq!(detect(""), None);
        }

        #[test]
        fn quoted_paths_with_spaces_still_tokenize_as_one_argument() {
            let invocation = detect("\"/home/user/some dir/zed-remote-server-stable-0.190.0\" version").unwrap();
            assert_eq!(invocation.version, "stable-0.190.0");
            assert_eq!(invocation.trailing_args, vec!["version".to_string()]);
        }
    }
}

/// Session-handling coverage against the real loopback SSH server, driven
/// by a real `russh` client connecting straight to its bound port.
///
/// **Why straight to the port, not through a relay tunnel**: `serve.rs`'s
/// own `tunnel_tests` module (`ssh_purpose_tunnel_bridges_to_the_real_ssh_server`)
/// already proves that a `"ssh"`-purpose tunnel-offer, driven through the
/// real `serve_dispatch`/`handle_control_push` path against a fake relayd,
/// correctly bridges raw bytes to this exact port — the same
/// `open_tcp_bridge_tunnel` mechanism the `web:`/`zellij-web` purposes
/// already use and already have their own such coverage for. This module
/// has no relay/tunnel awareness of its own (see its doc comment), so the
/// most direct test of *its* actual job — real session/exec handling — is
/// a real SSH client against the real bound socket, not re-plumbing a full
/// SSH handshake through the WS/tunnel-frame test harness a second time.
#[cfg(test)]
mod session_tests {
    use super::*;
    use russh::ChannelMsg;
    use russh::keys::ssh_key::PublicKey as ClientVisiblePublicKey;

    struct TrustingClient;

    impl russh::client::Handler for TrustingClient {
        type Error = russh::Error;

        async fn check_server_key(&mut self, _server_public_key: &ClientVisiblePublicKey) -> Result<bool, Self::Error> {
            // No client-side host-key pinning here either — this test
            // stands in for a laptop's `ssh`/Zed client, which per
            // ssh-bridge-and-zed.md trusts the tunnel it arrived over, not
            // a TOFU/pinned fingerprint check at this layer.
            Ok(true)
        }
    }

    fn bridge_config(registry: Registry, mise_bin: &str, mise_host_tools_dir: &std::path::Path) -> (SshBridgeConfig, mpsc::Receiver<WireAgentEvent>) {
        let (agent_event_tx, agent_event_rx) = mpsc::channel(16);
        let config = SshBridgeConfig {
            registry: Arc::new(Mutex::new(registry)),
            mise_host_tools_dir: mise_host_tools_dir.to_path_buf(),
            mise_bin: mise_bin.to_string(),
            agent_event_tx,
        };
        (config, agent_event_rx)
    }

    async fn connected_client(port: u16) -> russh::client::Handle<TrustingClient> {
        let config = Arc::new(russh::client::Config::default());
        let mut handle = russh::client::connect(config, ("127.0.0.1", port), TrustingClient).await.expect("client connects");
        let auth = handle.authenticate_none("laptop-proxy-test").await.expect("auth_none round trip");
        assert!(matches!(auth, russh::client::AuthResult::Success), "server must accept auth_none unconditionally: {auth:?}");
        handle
    }

    /// Drains an SSH channel's messages, collecting stdout/stderr bytes
    /// until the channel closes, and returns them plus the reported exit
    /// status (if any).
    async fn drain_channel(mut channel: russh::Channel<russh::client::Msg>) -> (Vec<u8>, Vec<u8>, Option<u32>) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit_status = None;
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, .. } => stderr.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status: status } => exit_status = Some(status),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        (stdout, stderr, exit_status)
    }

    #[tokio::test]
    async fn real_shell_exec_round_trips_real_output() {
        let dir = tempfile::tempdir().unwrap();
        let signing_key = SigningKey::generate(&mut rand::rng());
        let registry = Registry::load(&dir.path().join("registry.json")).unwrap();
        let (config, _agent_event_rx) = bridge_config(registry, "mise-not-used-in-this-test", &dir.path().join("mise"));
        let port = spawn_loopback_server(&signing_key, config).await.unwrap();

        let handle = connected_client(port).await;
        let channel = handle.channel_open_session().await.unwrap();
        let marker = format!("choosh-ssh-server-test-{}", uuid::Uuid::new_v4());
        channel.exec(true, format!("echo {marker}").into_bytes()).await.unwrap();

        let (stdout, _stderr, exit_status) = tokio::time::timeout(std::time::Duration::from_secs(10), drain_channel(channel)).await.unwrap();
        assert!(String::from_utf8_lossy(&stdout).contains(&marker), "expected the real echoed marker in stdout, got: {:?}", String::from_utf8_lossy(&stdout));
        assert_eq!(exit_status, Some(0));
    }

    #[tokio::test]
    async fn a_plain_interactive_shell_request_spawns_a_real_pty_backed_shell() {
        let dir = tempfile::tempdir().unwrap();
        let signing_key = SigningKey::generate(&mut rand::rng());
        let registry = Registry::load(&dir.path().join("registry.json")).unwrap();
        let (config, _agent_event_rx) = bridge_config(registry, "mise-not-used-in-this-test", &dir.path().join("mise"));
        let port = spawn_loopback_server(&signing_key, config).await.unwrap();

        let handle = connected_client(port).await;
        let mut channel = handle.channel_open_session().await.unwrap();
        channel.request_shell(true).await.unwrap();

        let marker = format!("choosh-shell-test-{}", uuid::Uuid::new_v4());
        channel.data(std::io::Cursor::new(format!("echo {marker}\nexit\n").into_bytes())).await.unwrap();

        let mut collected = Vec::new();
        let found = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while let Some(message) = channel.wait().await {
                if let ChannelMsg::Data { data } = message {
                    collected.extend_from_slice(&data);
                    if String::from_utf8_lossy(&collected).contains(&marker) {
                        return;
                    }
                }
            }
        })
        .await;
        assert!(found.is_ok(), "did not observe the echoed marker from a real interactive shell; collected: {:?}", String::from_utf8_lossy(&collected));
    }

    /// Writes a fake `mise` that fakes `install`/`where`, and a fake
    /// `zed-remote-server` executable at the path it reports — the same
    /// shape `mise_ops`'s own tests use, but driven end-to-end through a
    /// real exec request over a real SSH session, matching the "Zed
    /// exec-detection triggers the version-check/update path" verification
    /// requirement without needing the real Zed binary.
    fn write_fake_mise_and_zed_remote_server(dir: &std::path::Path) -> (PathBuf, PathBuf) {
        let install_dir = dir.join("installs").join("zed-remote-server-0.190.0");
        std::fs::create_dir_all(&install_dir).unwrap();
        let fake_binary = install_dir.join("zed-remote-server");
        std::fs::write(&fake_binary, "#!/bin/sh\nprintf 'fake-zed-remote-server saw: %s\\n' \"$*\"\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        let calls_log = dir.join("mise-calls.log");
        let mise_script = dir.join("mise");
        let body = format!(
            "#!/bin/sh\necho \"$@\" >> \"{calls}\"\ncase \"$1\" in\n  install) exit 0 ;;\n  where) echo '{install_dir}' ;;\n  *) exit 1 ;;\nesac\n",
            calls = calls_log.display(),
            install_dir = install_dir.display(),
        );
        std::fs::write(&mise_script, body).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mise_script, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        (mise_script, calls_log)
    }

    #[tokio::test]
    async fn zed_remote_server_exec_triggers_mise_provisioning_and_execs_the_resolved_binary() {
        let dir = tempfile::tempdir().unwrap();
        let (mise_script, calls_log) = write_fake_mise_and_zed_remote_server(dir.path());

        let signing_key = SigningKey::generate(&mut rand::rng());
        let mut registry = Registry::load(&dir.path().join("registry.json")).unwrap();
        // Exactly one workspace registered, so this exercises the
        // single-workspace editor-presence attribution path too (see
        // `resolve_sole_workspace_id`'s doc comment for why that's the
        // scoped heuristic this crate uses).
        registry
            .register_workspace(
                "ws-1".to_string(),
                "solo".to_string(),
                "proj-1".to_string(),
                "solo-project".to_string(),
                dir.path().join("solo"),
                "2026-01-01T00:00:00Z".to_string(),
            )
            .unwrap();
        let (config, mut agent_event_rx) = bridge_config(registry, mise_script.to_str().unwrap(), &dir.path().join("mise-host-tools"));
        let port = spawn_loopback_server(&signing_key, config).await.unwrap();

        let handle = connected_client(port).await;
        let channel = handle.channel_open_session().await.unwrap();
        channel
            .exec(true, b"/home/user/.zed_server/zed-remote-server-0.190.0 proxy --identifier workspace-42".to_vec())
            .await
            .unwrap();

        let (stdout, _stderr, _exit_status) = tokio::time::timeout(std::time::Duration::from_secs(10), drain_channel(channel)).await.unwrap();
        let stdout_text = String::from_utf8_lossy(&stdout);
        assert!(
            stdout_text.contains("fake-zed-remote-server saw: proxy --identifier workspace-42"),
            "expected the resolved mise-managed binary to run with the trailing args forwarded unchanged, got: {stdout_text:?}"
        );

        let calls = std::fs::read_to_string(&calls_log).unwrap();
        assert!(calls.contains("install --yes ubi:zed-industries/zed[exe=zed-remote-server]@0.190.0"), "expected mise install to run with the declared version: {calls}");
        assert!(calls.contains("where ubi:zed-industries/zed[exe=zed-remote-server]@0.190.0"));

        let attached = tokio::time::timeout(std::time::Duration::from_secs(5), agent_event_rx.recv()).await.unwrap().unwrap();
        assert_eq!(attached, WireAgentEvent::EditorAttached { workspace_id: "ws-1".to_string(), editor: WireEditor::Zed });
        let detached = tokio::time::timeout(std::time::Duration::from_secs(5), agent_event_rx.recv()).await.unwrap().unwrap();
        assert_eq!(detached, WireAgentEvent::EditorDetached { workspace_id: "ws-1".to_string(), editor: WireEditor::Zed });
    }

    #[tokio::test]
    async fn zed_remote_server_exec_fails_cleanly_when_mise_provisioning_fails() {
        let dir = tempfile::tempdir().unwrap();
        let mise_script = dir.path().join("mise");
        std::fs::write(&mise_script, "#!/bin/sh\necho 'no network in this fake' >&2\nexit 1\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mise_script, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        let signing_key = SigningKey::generate(&mut rand::rng());
        let registry = Registry::load(&dir.path().join("registry.json")).unwrap();
        let (config, mut agent_event_rx) = bridge_config(registry, mise_script.to_str().unwrap(), &dir.path().join("mise-host-tools"));
        let port = spawn_loopback_server(&signing_key, config).await.unwrap();

        let handle = connected_client(port).await;
        let channel = handle.channel_open_session().await.unwrap();
        channel.exec(true, b"zed-remote-server-0.190.0 version".to_vec()).await.unwrap();

        let (_stdout, stderr, exit_status) = tokio::time::timeout(std::time::Duration::from_secs(10), drain_channel(channel)).await.unwrap();
        assert!(!String::from_utf8_lossy(&stderr).is_empty(), "expected a clear failure reason on stderr");
        assert_eq!(exit_status, Some(1), "a failed provisioning attempt must fail the connection attempt, not silently succeed");

        // No editor presence for a session that never actually started a
        // Zed process.
        assert!(agent_event_rx.try_recv().is_err());
    }
}
