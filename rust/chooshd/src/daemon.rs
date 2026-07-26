//! Minimal bounded daemon composition behavior over a private Unix socket.

#![cfg(unix)]

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex, mpsc};

use choosh_protocol::framing::{FrameDecoder, FrameError, FrameLimits, encode_frame};
use choosh_protocol::handshake::{
    Capability, HandshakeError, PeerIdentity, ProtocolLimits, ProtocolVersion, ServerNegotiator,
    ServerReply,
};
use choosh_protocol::wire::{
    WireEnvelope, WireError, decode_envelope, decode_hello, encode_server_reply,
};
use serde_json::{Value, json};

use crate::socket::{self, OwnedUnixListener, SocketError, SocketPlan};
use crate::state::DaemonCoordinator;
use crate::{
    git::ChangeKind,
    git_status::{GitStatusError, GitStatusOperation},
};
use choosh_core::event_spool::Subscription;

pub const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;
const READ_CHUNK_BYTES: usize = 8192;
const MAX_FRAMES_PER_READ: usize = 16;

#[derive(Clone, Debug)]
pub struct HandshakeConfig {
    pub protocol: ProtocolVersion,
    pub daemon: PeerIdentity,
    pub host: PeerIdentity,
    pub capabilities: Vec<Capability>,
    pub limits: ProtocolLimits,
}

/// Injected post-handshake request composition. It never receives a socket,
/// path, environment, or process-launch capability.
pub trait RpcHandler: Send + Sync {
    /// Encodes one terminal response for a validated request.
    ///
    /// # Errors
    ///
    /// Returns a bounded framing or handler failure.
    fn handle(
        &self,
        request: &choosh_protocol::envelope::Request<Value>,
        config: &HandshakeConfig,
        max_frame_bytes: usize,
    ) -> Result<Vec<u8>, DaemonError>;
}

/// Default daemon RPC composition containing `host.describe` and injected,
/// opaque-identity `git.status` workspace operations.
#[derive(Default)]
pub struct DaemonRpc {
    git_status: BTreeMap<choosh_protocol::envelope::EnvelopeId, Arc<dyn GitStatusOperation>>,
    events: Option<Arc<Mutex<DaemonCoordinator>>>,
}

impl DaemonRpc {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds the coordinator-owned event subscription surface. The mutex is
    /// deliberately supplied by the composition root so request workers never
    /// acquire hidden global state.
    pub fn with_events(mut self, coordinator: Arc<Mutex<DaemonCoordinator>>) -> Self {
        self.events = Some(coordinator);
        self
    }

    /// Adds the operation for an identity registered by the outer composition root.
    /// Duplicate identities are rejected instead of silently replacing authority.
    ///
    /// # Errors
    ///
    /// Returns [`RpcRegistrationError::DuplicateWorkspace`] when the identity is
    /// already registered.
    pub fn register_git_status(
        &mut self,
        workspace_id: choosh_protocol::envelope::EnvelopeId,
        operation: Arc<dyn GitStatusOperation>,
    ) -> Result<(), RpcRegistrationError> {
        if self.git_status.insert(workspace_id, operation).is_some() {
            return Err(RpcRegistrationError::DuplicateWorkspace);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RpcRegistrationError {
    DuplicateWorkspace,
}

impl RpcHandler for DaemonRpc {
    fn handle(
        &self,
        request: &choosh_protocol::envelope::Request<Value>,
        config: &HandshakeConfig,
        max_frame_bytes: usize,
    ) -> Result<Vec<u8>, DaemonError> {
        if request.method.as_str() == "host.describe" {
            return describe_response(request, config, max_frame_bytes);
        }
        if request.method.as_str() == "events.subscribe-v1" {
            return self.subscribe_response(request, max_frame_bytes);
        }
        if request.method.as_str() == "events.ack-v1" {
            return self.ack_response(request, max_frame_bytes);
        }
        if request.method.as_str() != "git.status" {
            return error_response(
                request,
                "invalid_request",
                "unsupported request",
                max_frame_bytes,
            );
        }
        let Some(workspace_id) = git_status_workspace_id(&request.params) else {
            return error_response(
                request,
                "invalid_request",
                "invalid git.status request",
                max_frame_bytes,
            );
        };
        let Some(operation) = self.git_status.get(&workspace_id) else {
            return error_response(request, "not_found", "workspace not found", max_frame_bytes);
        };
        match operation.status_snapshot() {
            Ok(snapshot) => {
                git_status_response(request, workspace_id.as_str(), &snapshot, max_frame_bytes)
            }
            Err(error) => {
                let (code, message) = match error {
                    GitStatusError::Execution(
                        crate::git_status::GitStatusExecutionError::OutputTooLarge,
                    )
                    | GitStatusError::Parse(
                        crate::git::StatusParseError::OutputTooLarge
                        | crate::git::StatusParseError::TooManyEntries
                        | crate::git::StatusParseError::PathTooLong,
                    ) => ("limit_exceeded", "git status limit exceeded"),
                    _ => ("internal", "git status unavailable"),
                };
                error_response(request, code, message, max_frame_bytes)
            }
        }
    }
}

impl DaemonRpc {
    fn subscribe_response(
        &self,
        request: &choosh_protocol::envelope::Request<Value>,
        max: usize,
    ) -> Result<Vec<u8>, DaemonError> {
        let Some(coordinator) = &self.events else {
            return error_response(request, "unsupported", "events unavailable", max);
        };
        let Some((workspace, after)) = event_params(&request.params) else {
            return error_response(
                request,
                "invalid_request",
                "invalid events.subscribe-v1 request",
                max,
            );
        };
        let result = coordinator
            .lock()
            .map_err(|_| DaemonError::WorkerFailed)?
            .subscribe(&workspace, after);
        match result {
            Ok(Subscription::Replay {
                retained_low,
                committed_high,
                events,
            }) => {
                let events: Vec<Value> = events.into_iter().map(|e| json!({"sequence":e.sequence,"received_at":e.received_at,"payload_b64":base64_url_unpadded(&e.payload)})).collect();
                json_response(
                    request,
                    json!({"retained_low":retained_low,"committed_high":committed_high,"events":events}),
                    max,
                )
            }
            Ok(Subscription::SnapshotRequired {
                retained_low,
                committed_high,
            }) => json_response(
                request,
                json!({"retained_low":retained_low,"committed_high":committed_high,"snapshot_required":true}),
                max,
            ),
            Err(_) => error_response(request, "not_found", "workspace not found", max),
        }
    }

    fn ack_response(
        &self,
        request: &choosh_protocol::envelope::Request<Value>,
        max: usize,
    ) -> Result<Vec<u8>, DaemonError> {
        let Some(coordinator) = &self.events else {
            return error_response(request, "unsupported", "events unavailable", max);
        };
        let Some((workspace, client, sequence)) = ack_params(&request.params) else {
            return error_response(
                request,
                "invalid_request",
                "invalid events.ack-v1 request",
                max,
            );
        };
        let result = coordinator
            .lock()
            .map_err(|_| DaemonError::WorkerFailed)?
            .acknowledge_event(&workspace, &client, sequence);
        match result {
            Ok(()) => json_response(request, json!({}), max),
            Err(_) => error_response(
                request,
                "invalid_request",
                "event acknowledgement rejected",
                max,
            ),
        }
    }
}

fn event_params(value: &Value) -> Option<(choosh_core::workspace::WorkspaceId, u64)> {
    let o = value.as_object()?;
    if o.len() != 2 {
        return None;
    }
    let id =
        choosh_core::workspace::WorkspaceId::parse(o.get("workspace_id")?.as_str()?, 256).ok()?;
    Some((id, o.get("after_sequence")?.as_u64()?))
}
fn ack_params(value: &Value) -> Option<(choosh_core::workspace::WorkspaceId, String, u64)> {
    let o = value.as_object()?;
    if o.len() != 3 {
        return None;
    }
    let id =
        choosh_core::workspace::WorkspaceId::parse(o.get("workspace_id")?.as_str()?, 256).ok()?;
    let client = o.get("client_id")?.as_str()?.to_owned();
    if client.is_empty() {
        return None;
    }
    Some((id, client, o.get("sequence")?.as_u64()?))
}
fn json_response(
    request: &choosh_protocol::envelope::Request<Value>,
    result: Value,
    max: usize,
) -> Result<Vec<u8>, DaemonError> {
    let bytes =
        serde_json::to_vec(&json!({"kind":"response","id":request.id.as_str(),"result":result}))
            .map_err(|_| WireError::MalformedJson)?;
    if bytes.len() > max {
        return Err(DaemonError::Wire(WireError::PayloadTooLarge));
    }
    Ok(bytes)
}

impl HandshakeConfig {
    fn negotiator(&self) -> Result<ServerNegotiator, HandshakeError> {
        ServerNegotiator::new(
            self.protocol,
            self.daemon.clone(),
            self.host.clone(),
            self.capabilities.iter().copied(),
            self.limits,
        )
    }
}

#[derive(Debug)]
pub enum DaemonError {
    Socket(SocketError),
    Io(io::Error),
    Frame(FrameError),
    Wire(WireError),
    Handshake(HandshakeError),
    ExpectedHello,
    ExpectedRequest,
    WorkerFailed,
}

impl fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Socket(_) => formatter.write_str("daemon_socket_error"),
            Self::Io(_) => formatter.write_str("daemon_io_error"),
            Self::Frame(error) => write!(formatter, "daemon_frame_{}", error.code()),
            Self::Wire(error) => write!(formatter, "daemon_wire_{}", error.code()),
            Self::Handshake(error) => write!(formatter, "daemon_handshake_{}", error.code()),
            Self::ExpectedHello => formatter.write_str("daemon_expected_hello"),
            Self::ExpectedRequest => formatter.write_str("daemon_expected_request"),
            Self::WorkerFailed => formatter.write_str("daemon_worker_failed"),
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Socket(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Frame(_)
            | Self::Wire(_)
            | Self::Handshake(_)
            | Self::ExpectedHello
            | Self::ExpectedRequest
            | Self::WorkerFailed => None,
        }
    }
}

impl From<SocketError> for DaemonError {
    fn from(error: SocketError) -> Self {
        Self::Socket(error)
    }
}

impl From<io::Error> for DaemonError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<FrameError> for DaemonError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<WireError> for DaemonError {
    fn from(error: WireError) -> Self {
        Self::Wire(error)
    }
}

impl From<HandshakeError> for DaemonError {
    fn from(error: HandshakeError) -> Self {
        Self::Handshake(error)
    }
}

/// Binds the injected private socket plan for the outer binary composition root.
///
/// # Errors
///
/// Returns the existing socket boundary's fail-closed path, permission, and I/O
/// errors.
pub fn bind(plan: &SocketPlan) -> Result<OwnedUnixListener, DaemonError> {
    socket::bind(plan).map_err(Into::into)
}

/// Serves accepted connections until the listener fails.
///
/// A malformed connection is closed without terminating the listener. Accept
/// failure is returned to the composition root.
///
/// # Errors
///
/// Returns invalid framing limits or listener accept failure.
pub fn serve(
    listener: &UnixListener,
    config: &HandshakeConfig,
    max_frame_bytes: usize,
) -> Result<(), DaemonError> {
    serve_with_handler(listener, config, max_frame_bytes, &DaemonRpc::new())
}

/// Serves accepted private-socket connections with an explicitly injected RPC graph.
///
/// The outer composition root supplies only operations that already own their
/// registered roots and launch capabilities.
///
/// # Errors
///
/// Returns invalid framing limits or listener accept failure.
pub fn serve_with_handler<H>(
    listener: &UnixListener,
    config: &HandshakeConfig,
    max_frame_bytes: usize,
    handler: &H,
) -> Result<(), DaemonError>
where
    H: RpcHandler,
{
    let limits = limits(max_frame_bytes)?;
    loop {
        let (mut stream, _) = listener.accept()?;
        socket::verify_same_effective_user(&stream)?;
        let _ = serve_stream_with_rpc_handler(&mut stream, config, limits, handler);
    }
}

/// Accepts and serves exactly one connection for a deterministic black-box seam.
///
/// # Errors
///
/// Returns invalid framing limits, accept failure, malformed/truncated framing,
/// or stream I/O errors.
pub fn serve_once(
    listener: &UnixListener,
    config: &HandshakeConfig,
    max_frame_bytes: usize,
) -> Result<(), DaemonError> {
    serve_once_with_handler(listener, config, max_frame_bytes, &DaemonRpc::new())
}

/// Accepts exactly one private connection with an explicitly injected RPC graph.
///
/// This is the deterministic acceptance-test seam for the same peer admission
/// and framed request path used by [`serve_with_handler`].
///
/// # Errors
///
/// Returns invalid framing limits, accept failure, peer-admission failure,
/// malformed/truncated framing, or stream I/O errors.
pub fn serve_once_with_handler<H>(
    listener: &UnixListener,
    config: &HandshakeConfig,
    max_frame_bytes: usize,
    handler: &H,
) -> Result<(), DaemonError>
where
    H: RpcHandler,
{
    let limits = limits(max_frame_bytes)?;
    let (mut stream, _) = listener.accept()?;
    socket::verify_same_effective_user(&stream)?;
    serve_stream_with_rpc_handler(&mut stream, config, limits, handler)
}

fn limits(max_frame_bytes: usize) -> Result<FrameLimits, DaemonError> {
    FrameLimits::new(max_frame_bytes, MAX_FRAMES_PER_READ).map_err(Into::into)
}

#[cfg(test)]
fn serve_stream_with_limits(
    stream: &mut UnixStream,
    config: &HandshakeConfig,
    limits: FrameLimits,
) -> Result<(), DaemonError> {
    serve_stream_with_rpc_handler(stream, config, limits, &DaemonRpc::new())
}

fn serve_stream_with_rpc_handler<H>(
    stream: &mut UnixStream,
    config: &HandshakeConfig,
    limits: FrameLimits,
    handler: &H,
) -> Result<(), DaemonError>
where
    H: RpcHandler,
{
    serve_stream_with_handler(stream, config, limits, &|request| {
        handler.handle(request, config, limits.max_frame_bytes)
    })
}

fn serve_stream_with_handler<H>(
    stream: &mut UnixStream,
    config: &HandshakeConfig,
    limits: FrameLimits,
    handler: &H,
) -> Result<(), DaemonError>
where
    H: Fn(&choosh_protocol::envelope::Request<Value>) -> Result<Vec<u8>, DaemonError> + Sync,
{
    let mut decoder = FrameDecoder::new(limits);
    let mut buffer = [0_u8; READ_CHUNK_BYTES];
    let mut negotiated = false;
    let mut writer = stream.try_clone()?;
    std::thread::scope(|scope| {
        let (responses, completed) = mpsc::channel::<Vec<u8>>();
        let writer = scope.spawn(move || -> Result<(), DaemonError> {
            for response in completed {
                write_payload(&mut writer, &response, limits.max_frame_bytes)?;
            }
            Ok(())
        });
        let mut workers = VecDeque::new();
        let outcome = loop {
            let read = stream.read(&mut buffer)?;
            if read == 0 {
                decoder.finish()?;
                break if negotiated {
                    Ok(())
                } else {
                    Err(DaemonError::ExpectedHello)
                };
            }
            for payload in decoder.feed(&buffer[..read])? {
                if !negotiated {
                    let hello = decode_hello(&payload, limits.max_frame_bytes)?;
                    let reply = config.negotiator()?.receive_hello(&hello)?;
                    write_payload(
                        stream,
                        &encode_server_reply(&reply, limits.max_frame_bytes)?,
                        limits.max_frame_bytes,
                    )?;
                    if matches!(reply, ServerReply::Incompatible(_)) {
                        return Ok(());
                    }
                    negotiated = true;
                    continue;
                }

                let envelope = decode_envelope(&payload, limits.max_frame_bytes)?;
                let WireEnvelope::Request(request) = envelope else {
                    return Err(DaemonError::ExpectedRequest);
                };
                if workers.len() == usize::from(config.limits.max_in_flight_requests) {
                    join_worker(workers.pop_front().expect("bounded workers is nonempty"))?;
                }
                let responses = responses.clone();
                workers.push_back(scope.spawn(move || {
                    let response = handler(&request)?;
                    responses
                        .send(response)
                        .map_err(|_| DaemonError::WorkerFailed)
                }));
            }
        };
        while let Some(worker) = workers.pop_front() {
            join_worker(worker)?;
        }
        drop(responses);
        writer.join().map_err(|_| DaemonError::WorkerFailed)??;
        outcome
    })
}

fn join_worker(
    worker: std::thread::ScopedJoinHandle<'_, Result<(), DaemonError>>,
) -> Result<(), DaemonError> {
    worker.join().map_err(|_| DaemonError::WorkerFailed)?
}

fn write_payload(
    stream: &mut UnixStream,
    payload: &[u8],
    max_frame_bytes: usize,
) -> Result<(), DaemonError> {
    stream.write_all(&encode_frame(payload, max_frame_bytes)?)?;
    Ok(())
}

fn describe_response(
    request: &choosh_protocol::envelope::Request<Value>,
    config: &HandshakeConfig,
    max_frame_bytes: usize,
) -> Result<Vec<u8>, DaemonError> {
    let valid = request.method.as_str() == "host.describe"
        && request
            .params
            .as_object()
            .is_some_and(serde_json::Map::is_empty);
    let value = if valid {
        let capabilities: Vec<_> = config
            .capabilities
            .iter()
            .map(|capability| match capability {
                Capability::Events => "events",
                Capability::GitBlobs => "git-blobs",
                Capability::Services => "services",
            })
            .collect();
        json!({
            "kind": "response",
            "id": request.id.as_str(),
            "result": {
                "protocol": {"major": config.protocol.major, "minor": config.protocol.minor},
                "daemon": {"name": config.daemon.name, "version": config.daemon.version},
                "host": {"name": config.host.name, "version": config.host.version},
                "capabilities": capabilities,
                "limits": {
                    "max_control_frame_bytes": config.limits.max_control_frame_bytes,
                    "max_in_flight_requests": config.limits.max_in_flight_requests,
                },
            },
        })
    } else {
        json!({
            "kind": "response",
            "id": request.id.as_str(),
            "error": {"code": "invalid_request", "message": "invalid host.describe request"},
        })
    };
    let encoded = serde_json::to_vec(&value).map_err(|_| WireError::MalformedJson)?;
    if encoded.len() > max_frame_bytes {
        return Err(DaemonError::Wire(WireError::PayloadTooLarge));
    }
    Ok(encoded)
}

fn git_status_workspace_id(params: &Value) -> Option<choosh_protocol::envelope::EnvelopeId> {
    let object = params.as_object()?;
    if object.len() != 1 {
        return None;
    }
    let workspace_id = object.get("workspace_id")?.as_str()?;
    choosh_protocol::envelope::EnvelopeId::new(workspace_id).ok()
}

fn git_status_response(
    request: &choosh_protocol::envelope::Request<Value>,
    workspace_id: &str,
    snapshot: &crate::git::StatusSnapshot,
    max_frame_bytes: usize,
) -> Result<Vec<u8>, DaemonError> {
    let entries: Vec<Value> = snapshot
        .entries()
        .iter()
        .map(|entry| {
            let mut value = serde_json::Map::new();
            value.insert(
                "staged".to_owned(),
                Value::String(change_kind_name(entry.staged()).to_owned()),
            );
            value.insert(
                "unstaged".to_owned(),
                Value::String(change_kind_name(entry.unstaged()).to_owned()),
            );
            value.insert(
                "new_path_b64".to_owned(),
                Value::String(base64_url_unpadded(entry.new_path())),
            );
            if let Some(old_path) = entry.old_path() {
                value.insert(
                    "old_path_b64".to_owned(),
                    Value::String(base64_url_unpadded(old_path)),
                );
            }
            Value::Object(value)
        })
        .collect();
    let value = json!({
        "kind": "response",
        "id": request.id.as_str(),
        "result": {"workspace_id": workspace_id, "entries": entries},
    });
    response_bytes(&value, max_frame_bytes)
}

fn change_kind_name(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Unmodified => "unmodified",
        ChangeKind::Modified => "modified",
        ChangeKind::Added => "added",
        ChangeKind::Deleted => "deleted",
        ChangeKind::Renamed => "renamed",
        ChangeKind::Copied => "copied",
        ChangeKind::UpdatedButUnmerged => "updated_but_unmerged",
        ChangeKind::Untracked => "untracked",
        ChangeKind::Ignored => "ignored",
    }
}

fn base64_url_unpadded(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        output.push(TABLE[usize::from(chunk[0] >> 2)] as char);
        output.push(
            TABLE[usize::from(
                (chunk[0] & 0b0000_0011) << 4 | chunk.get(1).copied().unwrap_or(0) >> 4,
            )] as char,
        );
        if chunk.len() > 1 {
            output.push(
                TABLE[usize::from(
                    (chunk[1] & 0b0000_1111) << 2 | chunk.get(2).copied().unwrap_or(0) >> 6,
                )] as char,
            );
        }
        if chunk.len() > 2 {
            output.push(TABLE[usize::from(chunk[2] & 0b0011_1111)] as char);
        }
    }
    output
}

fn error_response(
    request: &choosh_protocol::envelope::Request<Value>,
    code: &'static str,
    message: &'static str,
    max_frame_bytes: usize,
) -> Result<Vec<u8>, DaemonError> {
    let value = json!({"kind": "response", "id": request.id.as_str(), "error": {"code": code, "message": message}});
    response_bytes(&value, max_frame_bytes)
}

fn response_bytes(value: &Value, max_frame_bytes: usize) -> Result<Vec<u8>, DaemonError> {
    let encoded = serde_json::to_vec(&value).map_err(|_| WireError::MalformedJson)?;
    if encoded.len() > max_frame_bytes {
        return Err(DaemonError::Wire(WireError::PayloadTooLarge));
    }
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{StatusLimits, parse_status};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Condvar, Mutex};

    fn read_frame(stream: &mut UnixStream, max_frame_bytes: usize) -> Vec<u8> {
        let mut header = [0_u8; 4];
        stream.read_exact(&mut header).unwrap();
        let length = u32::from_be_bytes(header) as usize;
        assert!(length <= max_frame_bytes);
        let mut payload = vec![0; length];
        stream.read_exact(&mut payload).unwrap();
        payload
    }

    fn config(max_frame_bytes: u32) -> HandshakeConfig {
        HandshakeConfig {
            protocol: ProtocolVersion::new(1, 2),
            daemon: PeerIdentity::new("chooshd", "test").unwrap(),
            host: PeerIdentity::new("local-host", "test").unwrap(),
            capabilities: vec![Capability::Events],
            limits: ProtocolLimits::new(max_frame_bytes, 8).unwrap(),
        }
    }

    struct StaticGitStatus(Result<crate::git::StatusSnapshot, GitStatusError>);

    impl GitStatusOperation for StaticGitStatus {
        fn status_snapshot(&self) -> Result<crate::git::StatusSnapshot, GitStatusError> {
            self.0.clone()
        }
    }

    fn request(payload: &[u8]) -> choosh_protocol::envelope::Request<Value> {
        let WireEnvelope::Request(request) = decode_envelope(payload, 512).unwrap() else {
            panic!("test input must be a request");
        };
        request
    }

    #[test]
    fn injected_git_status_uses_only_registered_uuid_and_preserves_opaque_path_bytes() {
        let workspace_id =
            choosh_protocol::envelope::EnvelopeId::new("00000000-0000-0000-0000-000000000011")
                .unwrap();
        let snapshot = parse_status(
            b" M src/opaque-\xff.rs\0",
            StatusLimits {
                max_bytes: 128,
                max_entries: 4,
                max_path_bytes: 64,
            },
        )
        .unwrap();
        let mut rpc = DaemonRpc::new();
        rpc.register_git_status(
            workspace_id.clone(),
            Arc::new(StaticGitStatus(Ok(snapshot))),
        )
        .unwrap();
        let request = request(br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000012","method":"git.status","params":{"workspace_id":"00000000-0000-0000-0000-000000000011"}}"#);
        let response = String::from_utf8(rpc.handle(&request, &config(512), 512).unwrap()).unwrap();
        assert_eq!(
            response,
            r#"{"id":"00000000-0000-0000-0000-000000000012","kind":"response","result":{"entries":[{"new_path_b64":"c3JjL29wYXF1ZS3_LnJz","staged":"unmodified","unstaged":"modified"}],"workspace_id":"00000000-0000-0000-0000-000000000011"}}"#
        );
        assert_eq!(
            rpc.register_git_status(
                workspace_id,
                Arc::new(StaticGitStatus(Err(GitStatusError::Parse(
                    crate::git::StatusParseError::MalformedRecord
                ))))
            ),
            Err(RpcRegistrationError::DuplicateWorkspace)
        );
    }

    #[test]
    fn git_status_rejects_paths_and_unknown_or_unregistered_workspace_ids_without_echoing_them() {
        let rpc = DaemonRpc::new();
        for (payload, code) in [
            (br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000013","method":"git.status","params":{"workspace_id":"00000000-0000-0000-0000-000000000099"}}"#.as_slice(), "not_found"),
            (br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000014","method":"git.status","params":{"workspace_id":"00000000-0000-0000-0000-000000000099","path":"/secret"}}"#.as_slice(), "invalid_request"),
        ] {
            let response = String::from_utf8(rpc.handle(&request(payload), &config(512), 512).unwrap()).unwrap();
            assert!(response.contains(&format!(r#""code":"{code}""#)));
            assert!(!response.contains("/secret"));
        }
    }

    #[test]
    fn git_status_maps_bounded_domain_failure_to_limit_exceeded() {
        let workspace_id =
            choosh_protocol::envelope::EnvelopeId::new("00000000-0000-0000-0000-000000000015")
                .unwrap();
        let mut rpc = DaemonRpc::new();
        rpc.register_git_status(
            workspace_id,
            Arc::new(StaticGitStatus(Err(GitStatusError::Parse(
                crate::git::StatusParseError::TooManyEntries,
            )))),
        )
        .unwrap();
        let response = String::from_utf8(
            rpc.handle(
                &request(br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000016","method":"git.status","params":{"workspace_id":"00000000-0000-0000-0000-000000000015"}}"#),
                &config(512),
                512,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(response.contains(r#""code":"limit_exceeded""#));
    }

    #[test]
    fn hello_then_host_describe_receive_canonical_typed_responses() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            serve_stream_with_limits(&mut server, &config(512), limits(512).unwrap()).unwrap();
        });
        let hello = br#"{"kind":"hello","protocol":{"major":1,"minor":1},"client":{"name":"test-client","version":"1"},"capabilities":["events"]}"#;
        client
            .write_all(&encode_frame(hello, 512).unwrap())
            .unwrap();
        let welcome = read_frame(&mut client, 512);
        assert_eq!(
            welcome,
            br#"{"capabilities":["events"],"daemon":{"name":"chooshd","version":"test"},"host":{"name":"local-host","version":"test"},"kind":"welcome","limits":{"max_control_frame_bytes":512,"max_in_flight_requests":8},"protocol":{"major":1,"minor":1}}"#
        );

        let request = br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000001","method":"host.describe","params":{}}"#;
        client
            .write_all(&encode_frame(request, 512).unwrap())
            .unwrap();
        let response = read_frame(&mut client, 512);
        assert_eq!(
            response,
            br#"{"id":"00000000-0000-0000-0000-000000000001","kind":"response","result":{"capabilities":["events"],"daemon":{"name":"chooshd","version":"test"},"host":{"name":"local-host","version":"test"},"limits":{"max_control_frame_bytes":512,"max_in_flight_requests":8},"protocol":{"major":1,"minor":2}}}"#
        );
        client.shutdown(std::net::Shutdown::Write).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn host_describe_requires_empty_params_and_returns_stable_error() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            serve_stream_with_limits(&mut server, &config(512), limits(512).unwrap()).unwrap();
        });
        let hello = br#"{"kind":"hello","protocol":{"major":1,"minor":1},"client":{"name":"test-client","version":"1"},"capabilities":[]}"#;
        client
            .write_all(&encode_frame(hello, 512).unwrap())
            .unwrap();
        let _welcome = read_frame(&mut client, 512);
        let request = br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000002","method":"host.describe","params":{"secret":"do-not-echo"}}"#;
        client
            .write_all(&encode_frame(request, 512).unwrap())
            .unwrap();
        let response = read_frame(&mut client, 512);
        assert_eq!(
            response,
            br#"{"error":{"code":"invalid_request","message":"invalid host.describe request"},"id":"00000000-0000-0000-0000-000000000002","kind":"response"}"#
        );
        assert!(!String::from_utf8(response).unwrap().contains("do-not-echo"));
        client.shutdown(std::net::Shutdown::Write).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn multiple_coalesced_requests_each_receive_their_matching_response() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            serve_stream_with_limits(&mut server, &config(512), limits(512).unwrap()).unwrap();
        });
        let hello = br#"{"kind":"hello","protocol":{"major":1,"minor":1},"client":{"name":"test-client","version":"1"},"capabilities":[]}"#;
        let first = br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000003","method":"host.describe","params":{}}"#;
        let second = br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000004","method":"host.describe","params":{}}"#;
        let mut input = encode_frame(hello, 512).unwrap();
        input.extend(encode_frame(first, 512).unwrap());
        input.extend(encode_frame(second, 512).unwrap());
        client.write_all(&input).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let welcome = read_frame(&mut client, 512);
        assert!(welcome.starts_with(br#"{"capabilities"#));
        let first_response = read_frame(&mut client, 512);
        let second_response = read_frame(&mut client, 512);
        let mut responses = [
            String::from_utf8(first_response).unwrap(),
            String::from_utf8(second_response).unwrap(),
        ];
        responses.sort();
        assert!(responses[0].contains("00000000-0000-0000-0000-000000000003"));
        assert!(responses[1].contains("00000000-0000-0000-0000-000000000004"));
        worker.join().unwrap();
    }

    #[test]
    fn injected_handler_proves_out_of_order_completion_preserves_ids() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let config = config(512);
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_release = Arc::clone(&release);
        let worker = std::thread::spawn(move || {
            let handler = |request: &choosh_protocol::envelope::Request<Value>| {
                let (lock, ready) = &*worker_release;
                if request.id.as_str().ends_with("0006") {
                    let released = lock.lock().map_err(|_| DaemonError::WorkerFailed)?;
                    drop(
                        ready
                            .wait_while(released, |released| !*released)
                            .map_err(|_| DaemonError::WorkerFailed)?,
                    );
                }
                describe_response(request, &config, 512)
            };
            serve_stream_with_handler(&mut server, &config, limits(512).unwrap(), &handler)
                .unwrap();
        });
        let hello = br#"{"kind":"hello","protocol":{"major":1,"minor":1},"client":{"name":"test-client","version":"1"},"capabilities":[]}"#;
        client
            .write_all(&encode_frame(hello, 512).unwrap())
            .unwrap();
        let _welcome = read_frame(&mut client, 512);
        let first = br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000006","method":"host.describe","params":{}}"#;
        let second = br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000007","method":"host.describe","params":{}}"#;
        let mut input = encode_frame(first, 512).unwrap();
        input.extend(encode_frame(second, 512).unwrap());
        client.write_all(&input).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let completed_first = String::from_utf8(read_frame(&mut client, 512)).unwrap();
        {
            let (lock, ready) = &*release;
            *lock.lock().unwrap() = true;
            ready.notify_one();
        }
        let completed_second = String::from_utf8(read_frame(&mut client, 512)).unwrap();
        assert!(completed_first.contains("00000000-0000-0000-0000-000000000007"));
        assert!(completed_second.contains("00000000-0000-0000-0000-000000000006"));
        worker.join().unwrap();
    }

    #[test]
    fn malformed_request_after_success_fails_closed_without_an_extra_response() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            assert!(matches!(
                serve_stream_with_limits(&mut server, &config(512), limits(512).unwrap()),
                Err(DaemonError::Wire(WireError::MalformedJson))
            ));
        });
        let hello = br#"{"kind":"hello","protocol":{"major":1,"minor":1},"client":{"name":"test-client","version":"1"},"capabilities":[]}"#;
        let request = br#"{"kind":"request","id":"00000000-0000-0000-0000-000000000005","method":"host.describe","params":{}}"#;
        for payload in [hello.as_slice(), request.as_slice(), b"{".as_slice()] {
            client
                .write_all(&encode_frame(payload, 512).unwrap())
                .unwrap();
        }
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let _welcome = read_frame(&mut client, 512);
        let response = read_frame(&mut client, 512);
        assert!(
            String::from_utf8(response)
                .unwrap()
                .contains("00000000-0000-0000-0000-000000000005")
        );
        let mut extra = Vec::new();
        client.read_to_end(&mut extra).unwrap();
        assert!(extra.is_empty());
        worker.join().unwrap();
    }

    #[test]
    fn raw_health_is_rejected_without_response() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            assert!(matches!(
                serve_stream_with_limits(&mut server, &config(32), limits(32).unwrap()),
                Err(DaemonError::Wire(WireError::MalformedJson))
            ));
        });
        client
            .write_all(&encode_frame(b"health", 32).unwrap())
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert!(response.is_empty());
        worker.join().unwrap();
    }

    #[test]
    fn incompatible_major_receives_canonical_terminal_reply() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            serve_stream_with_limits(&mut server, &config(256), limits(256).unwrap()).unwrap();
        });
        let hello = br#"{"kind":"hello","protocol":{"major":2,"minor":0},"client":{"name":"test-client","version":"1"},"capabilities":[]}"#;
        client
            .write_all(&encode_frame(hello, 256).unwrap())
            .unwrap();
        let mut output = Vec::new();
        client.read_to_end(&mut output).unwrap();
        let mut decoder = FrameDecoder::new(FrameLimits::new(256, 1).unwrap());
        assert_eq!(
            decoder.feed(&output).unwrap(),
            [br#"{"client":{"major":2,"minor":0},"daemon":{"major":1,"minor":2},"kind":"incompatible"}"#.to_vec()]
        );
        decoder.finish().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn oversized_frame_is_rejected_without_response() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            assert!(matches!(
                serve_stream_with_limits(&mut server, &config(4), limits(4).unwrap()),
                Err(DaemonError::Frame(FrameError::FrameTooLarge))
            ));
        });
        client.write_all(&5_u32.to_be_bytes()).unwrap();
        client.write_all(b"12345").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert!(response.is_empty());
        worker.join().unwrap();
    }
}
