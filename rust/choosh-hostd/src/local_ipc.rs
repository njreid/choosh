//! Local Unix-socket IPC between short-lived `choosh-hostd emit` hook
//! invocations and the long-running `serve` daemon process, per
//! `docs/specs/agent-events.md`'s adapter contract: a hook script invokes
//! `emit` once per lifecycle event, but `emit` itself has no relay
//! connection of its own (dialing out fresh per hook invocation would be
//! slow and would need its own enrollment/auth) — it hands the already-
//! normalized event to the long-running `serve` process over this socket,
//! which forwards it over its existing authenticated relay connection.
//!
//! Deliberately not the same socket/framing as a future `ssh-bridge-and-zed.md`
//! loopback listener — this one carries exactly one message type
//! (`choosh_protocol::relay::WireAgentEvent`) and nothing else, kept as
//! small and single-purpose as the thing it does.

use std::path::PathBuf;

use choosh_protocol::framing::{FrameDecoder, FrameLimits, encode_frame};
use choosh_protocol::relay::{MAX_CONTROL_FRAME_BYTES, WireAgentEvent};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

#[derive(Debug)]
pub enum LocalIpcError {
    Io(std::io::Error),
    NoRuntimeDir,
    Serialize(serde_json::Error),
    Frame(String),
}

impl std::fmt::Display for LocalIpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "local IPC I/O error: {error}"),
            Self::NoRuntimeDir => write!(f, "no runtime/config directory available for the local IPC socket"),
            Self::Serialize(error) => write!(f, "failed to serialize event for local IPC: {error}"),
            Self::Frame(reason) => write!(f, "malformed local IPC frame: {reason}"),
        }
    }
}

impl std::error::Error for LocalIpcError {}

impl From<std::io::Error> for LocalIpcError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// `<runtime dir>/choosh-hostd/emit.sock`, falling back to the config
/// directory when no XDG runtime dir is set (common outside an active
/// login session, e.g. under some service-manager contexts) — either way,
/// a per-user, not-world-readable location.
///
/// # Errors
///
/// Returns [`LocalIpcError::NoRuntimeDir`] if neither directory can be
/// determined.
pub fn default_socket_path() -> Result<PathBuf, LocalIpcError> {
    let dirs = directories::ProjectDirs::from("ai", "choosh", "hostd").ok_or(LocalIpcError::NoRuntimeDir)?;
    let base = dirs.runtime_dir().unwrap_or_else(|| dirs.config_dir());
    Ok(base.join("emit.sock"))
}

/// Binds the local IPC socket at `path`, removing a stale socket file left
/// over from a previous run first (a Unix socket bind fails with
/// `AddrInUse` if the path already exists as a file, even an abandoned
/// one — this daemon owns the path exclusively, per `DESIGN.md`'s "single
/// `choosh-hostd` per devhost").
///
/// # Errors
///
/// Returns an I/O error if the parent directory can't be created or the
/// socket can't be bound.
pub fn bind(path: &std::path::Path) -> Result<UnixListener, LocalIpcError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    // 0600: only this user can connect, matching credential.rs's file
    // permission discipline for anything carrying this devhost's data.
    #[cfg(unix)]
    std::fs::set_permissions(path, <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600))?;
    Ok(listener)
}

/// Accepts connections on `listener` forever, decoding one
/// [`WireAgentEvent`] per connection and sending it to `sink`. Runs as its
/// own background task from `serve::run` — a malformed or oversized frame
/// from one connection is logged and that connection is dropped; it never
/// takes down the listener itself.
pub async fn serve_forever(listener: UnixListener, sink: mpsc::Sender<WireAgentEvent>) {
    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!(%error, "local IPC accept failed");
                continue;
            }
        };
        let sink = sink.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, &sink).await {
                tracing::warn!(%error, "local IPC connection failed");
            }
        });
    }
}

async fn handle_connection(mut stream: UnixStream, sink: &mpsc::Sender<WireAgentEvent>) -> Result<(), LocalIpcError> {
    let mut decoder = FrameDecoder::new(
        FrameLimits::new(MAX_CONTROL_FRAME_BYTES, 1).map_err(|error| LocalIpcError::Frame(format!("{error:?}")))?,
    );
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(()); // client closed without sending a full frame — not an error, `emit` may have failed upstream and exited.
        }
        let frames = decoder.feed(&buf[..n]).map_err(|error| LocalIpcError::Frame(format!("{error:?}")))?;
        if let Some(frame) = frames.into_iter().next() {
            let event: WireAgentEvent = serde_json::from_slice(&frame).map_err(LocalIpcError::Serialize)?;
            let _ = sink.send(event).await;
            return Ok(()); // exactly one event per connection, per this module's contract.
        }
    }
}

/// The client half: connects to `path`, sends one `event`, and returns.
/// Used by `choosh-hostd emit`, a short-lived process per hook invocation.
///
/// # Errors
///
/// Returns an I/O error if the socket can't be reached (e.g. `serve` isn't
/// running), or [`LocalIpcError::Serialize`] if `event` fails to encode.
pub async fn send_event(path: &std::path::Path, event: &WireAgentEvent) -> Result<(), LocalIpcError> {
    let mut stream = UnixStream::connect(path).await?;
    let payload = serde_json::to_vec(event).map_err(LocalIpcError::Serialize)?;
    let framed = encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).map_err(|error| LocalIpcError::Frame(format!("{error:?}")))?;
    stream.write_all(&framed).await?;
    stream.shutdown().await?;
    Ok(())
}

/// A minimal liveness probe against the local IPC socket: succeeds iff
/// something is actively accepting connections at `path` within `timeout`.
/// Used by [`crate::update`]'s post-restart rollback monitor as the
/// concrete "local RPC socket health-check" `docs/specs/host-deployment.md`'s
/// Rollback subsection calls for.
///
/// Deliberately just a connect-and-drop, not a real `WireAgentEvent`
/// round trip: this module's protocol carries exactly one message type
/// end to end (an `emit` invocation's normalized event), and a probe
/// message would either be misdelivered to whatever's really waiting on
/// `agent_event_rx` or rejected by [`handle_connection`]'s single-frame
/// contract — neither of which a liveness probe needs. A successful
/// connect already proves `serve` reached the point (very early in
/// [`crate::serve::run`], before the SSH bridge, host-tool currency
/// checks, or the relayd connect loop) where it bound this socket, which
/// is exactly the signal "did the restarted binary come up at all" needs.
pub async fn probe(path: &std::path::Path, timeout: std::time::Duration) -> bool {
    tokio::time::timeout(timeout, UnixStream::connect(path)).await.is_ok_and(|result| result.is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_protocol::relay::WireInputReason;

    fn sample_event() -> WireAgentEvent {
        WireAgentEvent::InputRequired {
            workspace_id: "ws-1".to_string(),
            item_id: "item-1".to_string(),
            reason: WireInputReason::Permission,
        }
    }

    #[tokio::test]
    async fn a_sent_event_is_received_on_the_sink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sock");
        let listener = bind(&path).unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let server = tokio::spawn(serve_forever(listener, tx));

        send_event(&path, &sample_event()).await.unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert_eq!(received, sample_event());
        server.abort();
    }

    #[tokio::test]
    async fn bind_removes_a_stale_socket_file_left_by_a_previous_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sock");
        std::fs::write(&path, b"not a socket").unwrap();

        let listener = bind(&path).unwrap();
        drop(listener);
    }

    #[tokio::test]
    async fn probe_succeeds_against_a_live_listener_and_fails_against_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sock");

        assert!(!probe(&path, std::time::Duration::from_millis(200)).await, "no listener bound yet");

        let listener = bind(&path).unwrap();
        let (tx, _rx) = mpsc::channel(1);
        let server = tokio::spawn(serve_forever(listener, tx));

        assert!(probe(&path, std::time::Duration::from_secs(2)).await);

        server.abort();
    }

    #[tokio::test]
    async fn socket_file_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sock");
        let _listener = bind(&path).unwrap();

        let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&path).unwrap().permissions()) & 0o777;
        assert_eq!(mode, 0o600);
    }
}
