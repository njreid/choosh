//! The relay-protocol client for the *phone* Identity, per
//! `docs/specs/relay-protocol.md` and `docs/specs/auth-and-enrollment.md`.
//! Parallel to `choosh-hostd`'s devhost-side client, but phone-authenticated
//! (a stored bearer session credential, never a signed challenge) rather
//! than device-certificate-authenticated, and with no enrollment step of
//! its own — a phone's credential comes from `WebAuthn`, which is
//! `choosh-android-bridge`'s job, not this crate's.
//!
//! Since M1/M2, `choosh-hostd` speaks a multiplexed-tunnel-over-one-socket
//! protocol (`docs/specs/host-rpc.md`'s "Transport" section: RPC and PTY
//! traffic may interleave on the same relay connection). [`PhoneConnection`]
//! mirrors that on the phone side: [`PhoneConnection::connect`] spawns a
//! single background task that owns the underlying `WebSocket` for the
//! connection's lifetime — the same "one task reads every frame and
//! dispatches by tunnel ID" shape as `choosh-hostd::serve::serve_dispatch`
//! — and every public method here (including the pre-M1 control-only ones)
//! is a thin request/response wrapper that talks to that task over a
//! command channel. This is what lets [`PhoneConnection::call_rpc`] and
//! [`PhoneConnection::open_pty_tunnel`] be genuinely concurrent: a slow RPC
//! round trip never blocks PTY bytes arriving on a different tunnel, or
//! vice versa, because both are just waiters parked on their own channel
//! while the background task keeps servicing whichever is ready first.
//!
//! This crate has no `main`/CLI: `choosh-android-bridge` is its only
//! consumer, driving it from JNI call sites.

#![forbid(unsafe_code)]

pub mod backoff;
mod frame_channel;

use std::collections::HashMap;

use choosh_protocol::host_rpc::{RpcRequest, RpcResponse};
use choosh_protocol::relay::{
    AgentEventsResumeRequest, AgentEventsResumeResponse, AuthResult, ClientAuth, ControlRequest, ControlResponse,
    DevHostPresence, FRAME_CLASS_CONTROL, FRAME_CLASS_TUNNEL, PhoneAuth, ServerHello, ServerPush, TUNNEL_ID_BYTES,
    WireAgentEvent, decode_tunnel_id_hex,
};
use frame_channel::{ChannelError, FrameChannel};
use tokio::sync::{mpsc, oneshot};

type WsChannel = FrameChannel<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>;

/// The 8 raw bytes identifying one tunnel on the wire, per relay-protocol.md.
type TunnelId = [u8; TUNNEL_ID_BYTES];

/// What an `open-tunnel` call resolves to: the new tunnel's ID plus the
/// receiver half of the inbound-payload channel the I/O task created for
/// it (see [`PendingReply::OpenTunnel`]).
type OpenTunnelResult = Result<(TunnelId, mpsc::Receiver<Vec<u8>>), CallError>;

#[derive(Debug)]
pub enum ConnectError {
    Dial(String),
    Transport(ChannelError),
    /// relayd sent something other than `ServerHello` first, or something
    /// other than `AuthResult` after `ClientAuth` — a protocol-level
    /// surprise, not an ordinary auth rejection.
    UnexpectedFrame(String),
    Rejected(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dial(error) => write!(f, "failed to reach relayd: {error}"),
            Self::Transport(error) => write!(f, "transport error: {error}"),
            Self::UnexpectedFrame(what) => write!(f, "unexpected frame from relayd: {what}"),
            Self::Rejected(reason) => write!(f, "relayd rejected this session credential: {reason}"),
        }
    }
}

impl std::error::Error for ConnectError {}

impl From<ChannelError> for ConnectError {
    fn from(error: ChannelError) -> Self {
        Self::Transport(error)
    }
}

#[derive(Debug)]
pub enum CallError {
    Transport(ChannelError),
    UnexpectedResponse,
    Server { code: String, message: String },
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "transport error: {error}"),
            Self::UnexpectedResponse => write!(f, "relayd sent a response of the wrong type"),
            Self::Server { code, message } => write!(f, "{code}: {message}"),
        }
    }
}

impl std::error::Error for CallError {}

impl From<ChannelError> for CallError {
    fn from(error: ChannelError) -> Self {
        Self::Transport(error)
    }
}

/// Bound of an `"rpc"`-purpose tunnel's inbound channel — `call_rpc` opens a
/// fresh tunnel per call and reads exactly one response off it, so this
/// only ever needs to hold that single message.
const RPC_TUNNEL_CHANNEL_CAPACITY: usize = 4;

/// Bound of a `"pty:<item_id>"`-purpose tunnel's inbound channel — generous
/// enough that a burst of terminal output doesn't trip it under normal
/// interactive use, small enough that a caller who stops reading doesn't
/// let this task buffer output unboundedly (see [`handle_tunnel_frame`]'s
/// backpressure handling below).
const PTY_TUNNEL_CHANNEL_CAPACITY: usize = 64;

/// Bound of a `"web:<item_id>"`-purpose tunnel's inbound channel — larger
/// than [`PTY_TUNNEL_CHANNEL_CAPACITY`] because a `WebService`/Markdown
/// download can legitimately burst many chunks (a large HTTP response body
/// arriving faster than a slow client socket drains it) where terminal
/// output cannot, but still small and fixed: per [`handle_tunnel_frame`]'s
/// existing backpressure policy, a receiver that falls behind past this
/// many buffered chunks gets its tunnel closed rather than letting this
/// task buffer response bytes without limit. Each buffered item is at most
/// `choosh_protocol::relay::MAX_TUNNEL_FRAME_BYTES` (256 KiB), so this
/// bounds this one channel's worst case to `128 * 256 KiB` = 32 MiB — a
/// deliberately generous ceiling for one in-flight HTTP response, not a
/// claim that every gateway connection may use that much (the gateway
/// itself applies its own tighter per-connection caps, see
/// `choosh-android-bridge::web_gateway`).
const WEB_TUNNEL_CHANNEL_CAPACITY: usize = 128;

/// One `ServerPush::AgentEvent` delivered on the phone's normal control
/// connection, per `agent-events.md`'s "Delivery and replay" section —
/// unwrapped into an owned value a long-lived consumer (see
/// [`PhoneConnection::take_agent_event_receiver`]) can hold onto
/// independently of the frame it arrived in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentEventPush {
    pub from_device_id: String,
    pub event: WireAgentEvent,
    /// Verbatim copy of the wire push's own `sequence` — `None` only for
    /// [`WireAgentEvent::AuthRequired`], per that field's own doc comment.
    pub sequence: Option<u64>,
}

/// Bound of [`PhoneConnection`]'s agent-event push channel. Per
/// `choosh-hostd::agent_event_spool`'s own bound reasoning, these are
/// coarse lifecycle signals (on the order of one every few seconds even for
/// a busy workspace), not high-frequency telemetry — this just needs to
/// comfortably outlast a consumer that's briefly slow to drain (a JNI
/// caller between polls), not buffer an unbounded backlog. Delivery here
/// uses `try_send` ([`handle_control_frame`]): once this bound is reached, a
/// newly-arriving push is dropped (logged, not silently) rather than this
/// task blocking the single reader loop every other frame (control and
/// tunnel alike) also depends on — the same non-blocking backpressure
/// posture [`handle_tunnel_frame`] already applies to a slow tunnel reader,
/// just "drop the newest" here instead of "drop the whole tunnel", since an
/// occasional missed live push is recoverable (the next
/// `AgentEventsResumeRequest` replays it) where losing an entire tunnel
/// isn't.
const AGENT_EVENT_CHANNEL_CAPACITY: usize = 256;

/// One ask sent to the background I/O task via [`PhoneConnection`]'s
/// command channel. The task owns the socket exclusively — mirroring
/// `choosh-hostd`'s `serve_dispatch`'s single-task-owns-the-channel shape —
/// so every write (a plain control request, an `open-tunnel`, a
/// tunnel-frame send, or a tunnel close) is funneled through here rather
/// than contended over a lock.
enum IoCommand {
    SendControl {
        request_id: String,
        request: ControlRequest,
        respond_to: oneshot::Sender<ControlResponse>,
    },
    OpenTunnel {
        target_device_id: String,
        purpose: String,
        channel_capacity: usize,
        respond_to: oneshot::Sender<OpenTunnelResult>,
    },
    SendTunnelBytes {
        tunnel_id: TunnelId,
        payload: Vec<u8>,
        /// Fired once the underlying `WebSocket` frame write for this
        /// payload actually completes (not merely once it's been enqueued
        /// on this command channel) — `None` for [`PtyTunnelHandle::write`]'s
        /// fire-and-forget keystroke sends, `Some` for
        /// [`WebTunnelHandle::write`]'s backpressured sends, which await it
        /// before allowing the next chunk. See [`WebTunnelHandle::write`]'s
        /// doc comment for why this is the real backpressure signal rather
        /// than just bounding chunk size.
        ack: Option<oneshot::Sender<()>>,
    },
    CloseTunnel {
        tunnel_id: TunnelId,
    },
}

/// What the I/O task does once a `ControlResponse` for a given
/// `request_id` arrives — either just hand it back verbatim (every
/// pre-M1 control request), or, for `open-tunnel`, first register the new
/// tunnel's inbound channel (so no tunnel frame for it can possibly be lost
/// between the response arriving and the caller registering interest —
/// there is no caller-side registration step at all, it all happens here,
/// synchronously, before this task loops back to read the next frame).
enum PendingReply {
    Control(oneshot::Sender<ControlResponse>),
    OpenTunnel {
        channel_capacity: usize,
        respond_to: oneshot::Sender<OpenTunnelResult>,
    },
}

/// A live, phone-authenticated connection to `relayd`. Connecting spawns a
/// background task (see the module docs) that owns the socket for the
/// connection's lifetime; this handle only holds a channel into it, so
/// dropping every clone of that channel (this handle plus any
/// [`PtyTunnelHandle`]s derived from it) is what ends the background task,
/// not any explicit close call.
pub struct PhoneConnection {
    command_tx: mpsc::UnboundedSender<IoCommand>,
    /// `None` after [`Self::take_agent_event_receiver`] has already
    /// consumed it — deliberately a *separate* channel from
    /// `command_tx`/`io_task` (not gated behind whatever holds this whole
    /// struct locked) so a long-lived consumer of live agent-event pushes
    /// never contends with, or blocks behind, an ordinary RPC call. See
    /// that method's own doc comment.
    agent_event_rx: Option<mpsc::Receiver<AgentEventPush>>,
    /// `None` after [`Self::hold_until_closed`] has already consumed it —
    /// guards against a caller calling it twice rather than panicking (it's
    /// only ever called once, by [`run_with_reconnect`], but this stays
    /// robust to being called from anywhere else in the future).
    io_task: Option<tokio::task::JoinHandle<ChannelError>>,
}

impl PhoneConnection {
    /// Dials `relayd`, completes the `ServerHello`/`ClientAuth::Phone`
    /// handshake with the given session credential, and — on
    /// `AuthResult::Ok` — spawns the background I/O task and returns a live
    /// connection handle.
    ///
    /// # Errors
    ///
    /// [`ConnectError::Dial`] if the WebSocket handshake itself fails,
    /// [`ConnectError::Transport`] on a framing/network error mid-handshake,
    /// [`ConnectError::UnexpectedFrame`] if relayd's first frame isn't
    /// `ServerHello` or its post-auth frame isn't an `AuthResult` (a
    /// protocol mismatch, not a credential problem), or
    /// [`ConnectError::Rejected`] if `AuthResult::Failed` — the caller
    /// (Kotlin, via `choosh-android-bridge`) is expected to force a fresh
    /// `WebAuthn` ceremony on this outcome per auth-and-enrollment.md.
    pub async fn connect(relay_ws_url: &str, session_credential: &str) -> Result<Self, ConnectError> {
        let (stream, _response) = tokio_tungstenite::connect_async(relay_ws_url)
            .await
            .map_err(|error| ConnectError::Dial(error.to_string()))?;
        let mut channel = FrameChannel::new(stream);

        // relayd sends ServerHello unconditionally as the very first frame
        // on every new connection, phone included — it MUST be read here
        // before anything else, or the next recv() misparses it as the
        // AuthResult instead (this exact bug shipped once already this
        // session in choosh-hostd's enrollment path; don't repeat it).
        let _hello: ServerHello = channel.recv().await?;

        let auth = ClientAuth::Phone(PhoneAuth { session_credential: session_credential.to_string() });
        channel.send(FRAME_CLASS_CONTROL, &auth).await?;

        let result: AuthResult = channel.recv().await?;
        match result {
            AuthResult::Ok(_) => {
                let (command_tx, command_rx) = mpsc::unbounded_channel();
                let (agent_event_tx, agent_event_rx) = mpsc::channel(AGENT_EVENT_CHANNEL_CAPACITY);
                let io_task = tokio::spawn(run_io_task(channel, command_rx, agent_event_tx));
                Ok(Self { command_tx, agent_event_rx: Some(agent_event_rx), io_task: Some(io_task) })
            }
            AuthResult::Failed(failed) => Err(ConnectError::Rejected(failed.reason)),
        }
    }

    /// Sends one `ControlRequest` and awaits its `ControlResponse`, routed
    /// through the background I/O task rather than touching the socket
    /// directly — every control-only method below (`list_devhosts` etc.) is
    /// a thin wrapper around this.
    async fn send_control(&self, request_id: String, request: ControlRequest) -> Result<ControlResponse, CallError> {
        let (respond_to, response_rx) = oneshot::channel();
        self.command_tx
            .send(IoCommand::SendControl { request_id, request, respond_to })
            .map_err(|_| CallError::Transport(ChannelError::Closed))?;
        response_rx.await.map_err(|_| CallError::Transport(ChannelError::Closed))
    }

    /// Requests the current fleet presence list.
    ///
    /// # Errors
    ///
    /// See [`CallError`].
    pub async fn list_devhosts(&mut self) -> Result<Vec<DevHostPresence>, CallError> {
        let request_id = new_request_id();
        match self.send_control(request_id.clone(), ControlRequest::ListDevhosts { request_id }).await? {
            ControlResponse::ListDevhostsOk { devhosts, .. } => Ok(devhosts),
            ControlResponse::Error { code, message, .. } => Err(CallError::Server { code, message }),
            _ => Err(CallError::UnexpectedResponse),
        }
    }

    /// Requests a single-use enrollment token for the given identity class,
    /// per auth-and-enrollment.md — returns `(token, expires_at)`.
    ///
    /// # Errors
    ///
    /// See [`CallError`].
    pub async fn request_enrollment_token(
        &mut self,
        identity_class: choosh_protocol::relay::IdentityClass,
    ) -> Result<(String, String), CallError> {
        let request_id = new_request_id();
        match self
            .send_control(request_id.clone(), ControlRequest::RequestEnrollmentToken { request_id, identity_class })
            .await?
        {
            ControlResponse::RequestEnrollmentTokenOk { token, expires_at, .. } => Ok((token, expires_at)),
            ControlResponse::Error { code, message, .. } => Err(CallError::Server { code, message }),
            _ => Err(CallError::UnexpectedResponse),
        }
    }

    /// Registers this phone's FCM token with `relayd`, per
    /// relay-protocol.md's `register-fcm-token` — replaces any previously
    /// registered token for this phone Identity.
    ///
    /// # Errors
    ///
    /// See [`CallError`].
    pub async fn register_fcm_token(&mut self, fcm_token: &str) -> Result<(), CallError> {
        let request_id = new_request_id();
        match self
            .send_control(
                request_id.clone(),
                ControlRequest::RegisterFcmToken { request_id, fcm_token: fcm_token.to_string() },
            )
            .await?
        {
            ControlResponse::RegisterFcmTokenOk { .. } => Ok(()),
            ControlResponse::Error { code, message, .. } => Err(CallError::Server { code, message }),
            _ => Err(CallError::UnexpectedResponse),
        }
    }

    /// Takes ownership of this connection's live agent-event push receiver
    /// (`ServerPush::AgentEvent`, per `agent-events.md`'s "Delivery and
    /// replay" section) — callable once; `None` on any later call. A caller
    /// (`choosh-android-bridge::engine::Engine`) is expected to take this
    /// immediately after a successful [`Self::connect`] and hand it to its
    /// own long-lived pump task, decoupled from every other method on this
    /// type (which all go through `command_tx`/lock behind
    /// `Engine::connection`) so draining live pushes never contends with,
    /// or is starved behind, an ordinary in-flight RPC call.
    pub fn take_agent_event_receiver(&mut self) -> Option<mpsc::Receiver<AgentEventPush>> {
        self.agent_event_rx.take()
    }

    /// Opens a fresh `"agent-events"`-purpose tunnel, sends one
    /// [`AgentEventsResumeRequest`], awaits its
    /// [`AgentEventsResumeResponse`], then proactively closes the tunnel —
    /// the exact same fresh-tunnel-per-call shape [`Self::call_rpc`] uses
    /// for `"rpc"`-purpose tunnels (see that method's doc comment for the
    /// design rationale), just carrying `agent-events.md`'s resume/replay
    /// wire types instead of [`RpcRequest`]/[`RpcResponse`] — per
    /// `choosh_protocol::relay`'s module doc comment, this rides its own
    /// tunnel purpose rather than a `host-rpc.md` RPC method.
    ///
    /// # Errors
    ///
    /// See [`CallError`]. A [`AgentEventsResumeResponse::SnapshotRequired`]
    /// is NOT a [`CallError`] — like `call_rpc`'s `RpcResponse::Error`, it's
    /// a well-formed, documented outcome the caller inspects, not a
    /// transport failure.
    pub async fn resume_agent_events(
        &mut self,
        target_device_id: &str,
        workspace_id: &str,
        after_sequence: Option<u64>,
    ) -> Result<AgentEventsResumeResponse, CallError> {
        let (tunnel_id, mut response_rx) =
            self.open_tunnel(target_device_id, "agent-events".to_string(), RPC_TUNNEL_CHANNEL_CAPACITY).await?;

        let request_id = new_request_id();
        let request = AgentEventsResumeRequest { request_id: request_id.clone(), workspace_id: workspace_id.to_string(), after_sequence };
        let mut payload = Vec::with_capacity(1 + 96);
        payload.push(FRAME_CLASS_CONTROL);
        serde_json::to_writer(&mut payload, &request).map_err(|error| CallError::Transport(ChannelError::from(error)))?;
        self.command_tx
            .send(IoCommand::SendTunnelBytes { tunnel_id, payload, ack: None })
            .map_err(|_| CallError::Transport(ChannelError::Closed))?;

        let response_payload = response_rx.recv().await.ok_or(CallError::Transport(ChannelError::Closed))?;
        // Fresh-tunnel-per-call, same as call_rpc: done after exactly one
        // response, close now rather than leaking it for its idle timeout.
        let _ = self.command_tx.send(IoCommand::CloseTunnel { tunnel_id });

        let (inner_class, inner_body) = response_payload.split_first().ok_or(CallError::UnexpectedResponse)?;
        if *inner_class != FRAME_CLASS_CONTROL {
            return Err(CallError::UnexpectedResponse);
        }
        let response: AgentEventsResumeResponse =
            serde_json::from_slice(inner_body).map_err(|error| CallError::Transport(ChannelError::from(error)))?;
        let response_request_id = match &response {
            AgentEventsResumeResponse::Replayed { request_id, .. } | AgentEventsResumeResponse::SnapshotRequired { request_id, .. } => {
                request_id
            }
        };
        if response_request_id != &request_id {
            return Err(CallError::UnexpectedResponse);
        }
        Ok(response)
    }

    /// Opens a tunnel of the given `purpose` and, on success, returns its
    /// ID plus the inbound-payload receiver the background task created for
    /// it (see [`PendingReply::OpenTunnel`] — that registration happens
    /// inside the I/O task itself, before it reads anything else off the
    /// socket, so there is no window in which an incoming tunnel frame for
    /// this ID could arrive and be dropped as "unknown").
    async fn open_tunnel(
        &self,
        target_device_id: &str,
        purpose: String,
        channel_capacity: usize,
    ) -> OpenTunnelResult {
        let (respond_to, open_rx) = oneshot::channel();
        self.command_tx
            .send(IoCommand::OpenTunnel {
                target_device_id: target_device_id.to_string(),
                purpose,
                channel_capacity,
                respond_to,
            })
            .map_err(|_| CallError::Transport(ChannelError::Closed))?;
        open_rx.await.map_err(|_| CallError::Transport(ChannelError::Closed))?
    }

    /// Opens a fresh `"rpc"`-purpose tunnel, sends exactly one
    /// [`RpcRequest`], awaits its [`RpcResponse`], then proactively closes
    /// the tunnel.
    ///
    /// **Design choice: fresh tunnel per call, not one reused tunnel per
    /// devhost.** A reused tunnel would save the `open-tunnel` round trip on
    /// every call after the first, at the cost of tracking per-devhost
    /// tunnel state that has to be invalidated on devhost
    /// disconnect/reconnect (relay-protocol.md's reconnect-discontinuity
    /// rule: tunnels never survive either side reconnecting) and
    /// interleaving multiple concurrent RPCs' responses on one tunnel ID
    /// (fine, since `host-rpc.md` requests are ordered per-tunnel and
    /// request-ID-tagged — but still more bookkeeping). Fresh-per-call is
    /// simpler, has no invalidation problem, and matches this crate's
    /// existing one-shot-request style for every other method here; its
    /// cost — one extra `open-tunnel` round trip per call — is a reasonable
    /// trade at M1's call volume (RPCs triggered by explicit user action,
    /// not a hot loop). A terminal/diff UI that turns out to hammer
    /// `call_rpc` in a loop is the point at which reusing a tunnel would be
    /// worth revisiting.
    ///
    /// # Errors
    ///
    /// See [`CallError`]. An [`RpcResponse::Error`] from `hostd` is NOT a
    /// `CallError` — it comes back as `Ok(RpcResponse::Error { .. })` for
    /// the caller to inspect, since it's an application-level outcome
    /// (`host-rpc.md`'s fixed error codes), not a transport failure.
    pub async fn call_rpc(&mut self, target_device_id: &str, request: RpcRequest) -> Result<RpcResponse, CallError> {
        let (tunnel_id, mut response_rx) =
            self.open_tunnel(target_device_id, "rpc".to_string(), RPC_TUNNEL_CHANNEL_CAPACITY).await?;

        // The rpc-tunnel wire shape per host-rpc.md/relay-protocol.md: the
        // tunnel payload carries its own inner class byte, which MUST be
        // FRAME_CLASS_CONTROL, followed directly by the RpcRequest's JSON —
        // no further length-prefixing inside.
        let mut payload = Vec::with_capacity(1 + 128);
        payload.push(FRAME_CLASS_CONTROL);
        serde_json::to_writer(&mut payload, &request).map_err(|error| CallError::Transport(ChannelError::from(error)))?;
        self.command_tx
            .send(IoCommand::SendTunnelBytes { tunnel_id, payload, ack: None })
            .map_err(|_| CallError::Transport(ChannelError::Closed))?;

        let response_payload = response_rx.recv().await.ok_or(CallError::Transport(ChannelError::Closed))?;
        // Fresh-tunnel-per-call: this tunnel is done with after exactly one
        // response, success or not — close it now rather than leaking it
        // for its full 300s idle timeout.
        let _ = self.command_tx.send(IoCommand::CloseTunnel { tunnel_id });

        let (inner_class, inner_body) = response_payload.split_first().ok_or(CallError::UnexpectedResponse)?;
        if *inner_class != FRAME_CLASS_CONTROL {
            return Err(CallError::UnexpectedResponse);
        }
        let response: RpcResponse =
            serde_json::from_slice(inner_body).map_err(|error| CallError::Transport(ChannelError::from(error)))?;
        if response.request_id() != request.request_id() {
            return Err(CallError::UnexpectedResponse);
        }
        Ok(response)
    }

    /// Opens a `"pty:<item_id>"`-purpose tunnel and returns a duplex handle
    /// for streaming phone keystrokes in and devhost PTY output out over
    /// this same multiplexed connection.
    ///
    /// Unlike `call_rpc`, this tunnel is NOT one-shot: it stays open for as
    /// long as the returned [`PtyTunnelHandle`] is alive, or until
    /// [`PtyTunnelHandle::close`] is called — a terminal attachment is
    /// inherently long-lived, not a single request/response.
    ///
    /// # Errors
    ///
    /// See [`CallError`].
    pub async fn open_pty_tunnel(&mut self, target_device_id: &str, item_id: &str) -> Result<PtyTunnelHandle, CallError> {
        let (tunnel_id, output_rx) =
            self.open_tunnel(target_device_id, format!("pty:{item_id}"), PTY_TUNNEL_CHANNEL_CAPACITY).await?;
        Ok(PtyTunnelHandle { tunnel_id, command_tx: self.command_tx.clone(), output_rx })
    }

    /// Opens a `"web:<item_id>"`-purpose tunnel and returns a duplex handle
    /// for a loopback HTTP gateway to bridge one accepted client TCP
    /// connection through — per `docs/specs/service-tunnels.md`'s "Tunnel"
    /// section, the underlying `relayd`-brokered byte stream to the
    /// devhost's declared `127.0.0.1:<port>` is functionally identical to a
    /// direct TCP connection, so this is one tunnel per upstream connection
    /// (mirroring how a real TCP proxy would open one upstream socket per
    /// accepted client socket), not one shared tunnel for an entire pinned
    /// session — that's what lets WebSocket upgrade and multiple concurrent
    /// HTTP/1.1 connections (and their own keep-alive reuse) all just work
    /// as ordinary byte-transparent pass-through, with no protocol-specific
    /// handling needed here.
    ///
    /// Like [`Self::open_pty_tunnel`], this tunnel is NOT one-shot: it stays
    /// open for as long as the returned [`WebTunnelHandle`] is alive, or
    /// until [`WebTunnelHandle::close`] is called.
    ///
    /// # Errors
    ///
    /// See [`CallError`].
    pub async fn open_web_tunnel(&mut self, target_device_id: &str, item_id: &str) -> Result<WebTunnelHandle, CallError> {
        let (tunnel_id, output_rx) =
            self.open_tunnel(target_device_id, format!("web:{item_id}"), WEB_TUNNEL_CHANNEL_CAPACITY).await?;
        Ok(WebTunnelHandle { tunnel_id, command_tx: self.command_tx.clone(), output_rx })
    }

    /// Blocks until the background I/O task ends (connection drop, a
    /// malformed frame, or the command channel losing every sender), per
    /// relay-protocol.md. The caller's reconnect loop decides what happens
    /// next. Never returns after being called twice — see the `io_task`
    /// field doc.
    async fn hold_until_closed(&mut self) -> ChannelError {
        let Some(io_task) = self.io_task.take() else {
            return ChannelError::Closed;
        };
        io_task.await.unwrap_or(ChannelError::Closed)
    }
}

/// A duplex byte stream over a `"pty:<item_id>"` tunnel, for a JNI caller
/// (`choosh-android-bridge`) to drive a terminal UI from: [`Self::write`]
/// sends phone keystrokes to the devhost's PTY, [`Self::read`] yields
/// devhost PTY output as it arrives.
///
/// **Design choice: plain mpsc-backed handle, not `AsyncRead`/`AsyncWrite`.**
/// `write` is synchronous (an unbounded-channel send never blocks) and
/// `read` is a single async method returning `Option<Vec<u8>>` — this is
/// the minimal shape a Kotlin-facing `Engine` needs to drive from JNI (push
/// bytes in, pull chunks out), without pulling in `tokio::io`'s
/// `AsyncRead`/`AsyncWrite` machinery for a boundary that's going to get
/// copied into a `ByteArray` on the Kotlin side regardless.
///
/// Both directions share the parent [`PhoneConnection`]'s single I/O task:
/// `write` never blocks on `read`'s progress or vice versa, since sends go
/// through that task's unbounded command channel and receives come off this
/// handle's own bounded channel, populated independently by that task's
/// read loop as frames for this tunnel ID arrive.
pub struct PtyTunnelHandle {
    tunnel_id: TunnelId,
    command_tx: mpsc::UnboundedSender<IoCommand>,
    output_rx: mpsc::Receiver<Vec<u8>>,
}

impl PtyTunnelHandle {
    /// Sends raw bytes (phone keystrokes) to the devhost's PTY. Per the
    /// wire contract, a `"pty:"`-purpose tunnel carries bytes verbatim — no
    /// inner class tag, unlike an `"rpc"`-purpose tunnel.
    ///
    /// # Errors
    ///
    /// [`CallError::Transport`] if the underlying connection has already
    /// ended.
    pub fn write(&self, bytes: &[u8]) -> Result<(), CallError> {
        self.command_tx
            .send(IoCommand::SendTunnelBytes { tunnel_id: self.tunnel_id, payload: bytes.to_vec(), ack: None })
            .map_err(|_| CallError::Transport(ChannelError::Closed))
    }

    /// Waits for the next chunk of devhost PTY output. Returns `None` once
    /// the tunnel has closed — either end sent the zero-length close
    /// signal, or the underlying connection ended — a clean end-of-stream,
    /// not an error.
    pub async fn read(&mut self) -> Option<Vec<u8>> {
        self.output_rx.recv().await
    }

    /// Proactively sends the tunnel-close signal. Not required before
    /// dropping the handle (relayd's 300s idle timeout, and the devhost's
    /// own PTY-attachment teardown on a close signal it never gets, both
    /// eventually cover an ungracefully-dropped handle), but this avoids
    /// waiting on that timeout when the caller already knows it's done.
    pub fn close(self) {
        let _ = self.command_tx.send(IoCommand::CloseTunnel { tunnel_id: self.tunnel_id });
    }
}

/// A duplex byte stream over one `"web:<item_id>"` tunnel — one upstream TCP
/// connection to the devhost's declared loopback port, per
/// [`PhoneConnection::open_web_tunnel`]'s doc comment. Used by
/// `choosh-android-bridge::web_gateway`'s loopback HTTP gateway to bridge one
/// accepted client (`WebView`) connection.
///
/// **Design choice: async, backpressured `write`, unlike
/// [`PtyTunnelHandle::write`].** A terminal's keystrokes are tiny and
/// latency-sensitive — a fire-and-forget unbounded send is the right shape
/// there. An HTTP request/response body can be arbitrarily large, and
/// `service-tunnels.md` requires the gateway to forward bodies "with
/// backpressure" rather than buffer an unbounded amount in memory. Simply
/// bounding the chunk size read per `write` call would not be enough on its
/// own: the command channel each `write` funnels through is unbounded, so a
/// caller that keeps calling `write` faster than the underlying `WebSocket`
/// can actually send would still queue unboundedly there. Instead, `write`
/// awaits [`IoCommand::SendTunnelBytes`]'s `ack` — fired only once
/// `FrameChannel::send_tunnel` (the real network write) completes — so a
/// caller that reads one HTTP chunk, awaits `write`, then reads the next
/// chunk never has more than one chunk in flight: real backpressure that
/// propagates all the way to the WebSocket write, not just a local buffer
/// cap.
pub struct WebTunnelHandle {
    tunnel_id: TunnelId,
    command_tx: mpsc::UnboundedSender<IoCommand>,
    output_rx: mpsc::Receiver<Vec<u8>>,
}

impl WebTunnelHandle {
    /// Sends raw bytes upstream (phone -> devhost), awaiting confirmation
    /// that the underlying `WebSocket` frame write actually completed
    /// before returning — see this type's doc comment for why that's the
    /// real backpressure signal. Per the wire contract, a `"web:"`-purpose
    /// tunnel carries bytes verbatim, exactly like `"pty:"` — no inner class
    /// tag.
    ///
    /// # Errors
    ///
    /// [`CallError::Transport`] if the underlying connection has already
    /// ended.
    pub async fn write(&self, bytes: &[u8]) -> Result<(), CallError> {
        let (ack, ack_rx) = oneshot::channel();
        self.command_tx
            .send(IoCommand::SendTunnelBytes { tunnel_id: self.tunnel_id, payload: bytes.to_vec(), ack: Some(ack) })
            .map_err(|_| CallError::Transport(ChannelError::Closed))?;
        ack_rx.await.map_err(|_| CallError::Transport(ChannelError::Closed))
    }

    /// Waits for the next chunk of devhost response bytes. Returns `None`
    /// once the tunnel has closed — either end sent the zero-length close
    /// signal, the underlying connection ended, or this tunnel's inbound
    /// channel was dropped for falling too far behind (see
    /// [`WEB_TUNNEL_CHANNEL_CAPACITY`]) — a clean end-of-stream in every
    /// case, not an error a caller needs to branch on separately.
    pub async fn read(&mut self) -> Option<Vec<u8>> {
        self.output_rx.recv().await
    }

    /// Proactively sends the tunnel-close signal — the gateway calls this
    /// the moment its client connection ends, so the devhost-side upstream
    /// TCP connection doesn't linger for the full idle timeout.
    pub fn close(self) {
        let _ = self.command_tx.send(IoCommand::CloseTunnel { tunnel_id: self.tunnel_id });
    }
}

/// Owns the socket for the lifetime of one connection attempt: the single
/// task that reads every incoming frame and issues every outgoing one,
/// mirroring `choosh-hostd::serve::serve_dispatch`. Callers never touch the
/// `WsChannel` directly — they submit an [`IoCommand`] and await a
/// `oneshot`/`mpsc` reply, so a slow RPC round trip never blocks unrelated
/// tunnel traffic (or vice versa): both wait on their own channel while
/// this loop keeps servicing whichever is ready first. Returns the terminal
/// [`ChannelError`] that ended the connection (or [`ChannelError::Closed`]
/// if every command-channel sender was simply dropped).
async fn run_io_task(
    mut channel: WsChannel,
    mut command_rx: mpsc::UnboundedReceiver<IoCommand>,
    agent_event_tx: mpsc::Sender<AgentEventPush>,
) -> ChannelError {
    let mut pending_control: HashMap<String, PendingReply> = HashMap::new();
    let mut tunnels: HashMap<TunnelId, mpsc::Sender<Vec<u8>>> = HashMap::new();

    loop {
        tokio::select! {
            command = command_rx.recv() => {
                let Some(command) = command else {
                    // Every PhoneConnection/PtyTunnelHandle clone of
                    // command_tx has been dropped — nothing left to serve.
                    return ChannelError::Closed;
                };
                if let Err(error) = handle_command(command, &mut channel, &mut pending_control, &mut tunnels).await {
                    return error;
                }
            }

            frame = channel.recv_raw() => {
                match frame {
                    Ok((FRAME_CLASS_CONTROL, body)) => {
                        handle_control_frame(&body, &mut pending_control, &mut tunnels, &agent_event_tx);
                    }
                    Ok((FRAME_CLASS_TUNNEL, body)) => {
                        if let Err(error) = handle_tunnel_frame(&body, &mut tunnels) {
                            return error;
                        }
                    }
                    Ok((class, _body)) => tracing::debug!(class, "received frame with no handler for this class"),
                    Err(error) => return error,
                }
            }
        }
    }
}

/// Handles one [`IoCommand`]: registers whatever bookkeeping the eventual
/// response needs (in `pending_control`/`tunnels`) and writes the
/// corresponding frame. Returns `Err` only when the write itself fails —
/// per relay-protocol.md a failed send means the connection is
/// unrecoverable, so [`run_io_task`] ends rather than trying to keep going.
async fn handle_command(
    command: IoCommand,
    channel: &mut WsChannel,
    pending_control: &mut HashMap<String, PendingReply>,
    tunnels: &mut HashMap<TunnelId, mpsc::Sender<Vec<u8>>>,
) -> Result<(), ChannelError> {
    match command {
        IoCommand::SendControl { request_id, request, respond_to } => {
            pending_control.insert(request_id, PendingReply::Control(respond_to));
            channel.send(FRAME_CLASS_CONTROL, &request).await
        }
        IoCommand::OpenTunnel { target_device_id, purpose, channel_capacity, respond_to } => {
            let request_id = new_request_id();
            pending_control.insert(request_id.clone(), PendingReply::OpenTunnel { channel_capacity, respond_to });
            let request = ControlRequest::OpenTunnel { request_id, target_device_id, purpose };
            channel.send(FRAME_CLASS_CONTROL, &request).await
        }
        IoCommand::SendTunnelBytes { tunnel_id, payload, ack } => {
            let result = channel.send_tunnel(tunnel_id, &payload).await;
            if result.is_ok()
                && let Some(ack) = ack
            {
                // Best-effort: if the caller already stopped waiting (e.g.
                // WebTunnelHandle dropped mid-write), there's no one left to
                // notify — not a reason to fail this send.
                let _ = ack.send(());
            }
            result
        }
        IoCommand::CloseTunnel { tunnel_id } => {
            tunnels.remove(&tunnel_id);
            // Best-effort: the connection may already be on its way down,
            // in which case there is no one left to observe this signal
            // anyway, and that's not itself a reason to end the task.
            let _ = channel.send_tunnel(tunnel_id, &[]).await;
            Ok(())
        }
    }
}

/// Handles one `FRAME_CLASS_CONTROL` frame: a `ControlResponse` matching a
/// pending request (delivered to its waiter, with `open-tunnel`'s success
/// case also registering the new tunnel's inbound channel here — see
/// [`PendingReply::OpenTunnel`]), or anything else, logged and ignored.
fn handle_control_frame(
    body: &[u8],
    pending_control: &mut HashMap<String, PendingReply>,
    tunnels: &mut HashMap<TunnelId, mpsc::Sender<Vec<u8>>>,
    agent_event_tx: &mpsc::Sender<AgentEventPush>,
) {
    match serde_json::from_slice::<ControlResponse>(body) {
        Ok(response) => {
            let Some(pending) = pending_control.remove(response.request_id()) else {
                tracing::debug!("control response for an unknown/already-resolved request_id, ignoring");
                return;
            };
            match pending {
                PendingReply::Control(respond_to) => {
                    let _ = respond_to.send(response);
                }
                PendingReply::OpenTunnel { channel_capacity, respond_to } => {
                    let result = match response {
                        ControlResponse::OpenTunnelOk { tunnel_id, .. } => match decode_tunnel_id_hex(&tunnel_id) {
                            Some(id) => {
                                let (tx, rx) = mpsc::channel(channel_capacity);
                                tunnels.insert(id, tx);
                                Ok((id, rx))
                            }
                            None => Err(CallError::UnexpectedResponse),
                        },
                        ControlResponse::Error { code, message, .. } => Err(CallError::Server { code, message }),
                        _ => Err(CallError::UnexpectedResponse),
                    };
                    let _ = respond_to.send(result);
                }
            }
        }
        // Not a ControlResponse — try ServerPush before giving up, the same
        // fallback dispatch relay-protocol.md's own participants use. A
        // phone that opens a tunnel is never itself its target (see this
        // crate's module docs), so it never legitimately receives
        // `TunnelOffered`. `AgentEvent` pushes (`agent-events.md`'s live
        // delivery path) are forwarded to `agent_event_tx` — see
        // [`AGENT_EVENT_CHANNEL_CAPACITY`]'s doc comment for the
        // non-blocking `try_send`/drop-on-backpressure posture. Anything
        // else is logged and dropped rather than treated as fatal, matching
        // choosh-hostd's posture that an unrecognized-but-well-formed frame
        // doesn't end the connection.
        Err(_) => match serde_json::from_slice::<ServerPush>(body) {
            Ok(ServerPush::AgentEvent { from_device_id, event, sequence }) => {
                if agent_event_tx.try_send(AgentEventPush { from_device_id, event, sequence }).is_err() {
                    tracing::warn!("agent-event push channel is full or has no receiver, dropping this push");
                }
            }
            Ok(push) => tracing::debug!(?push, "ignoring server push with no handler in this pass"),
            Err(error) => tracing::debug!(%error, "unrecognized control frame, ignoring"),
        },
    }
}

/// Handles one `FRAME_CLASS_TUNNEL` frame: routes its payload to the
/// matching tunnel's inbound channel, or removes that tunnel on a
/// zero-length (close-signal) payload. Returns `Err` only for a frame
/// shorter than a tunnel ID — per relay-protocol.md that's malformed and
/// the connection is unrecoverable, matching every other malformed-frame
/// case in this crate and in `choosh-hostd::serve::handle_tunnel_frame`.
fn handle_tunnel_frame(body: &[u8], tunnels: &mut HashMap<TunnelId, mpsc::Sender<Vec<u8>>>) -> Result<(), ChannelError> {
    if body.len() < TUNNEL_ID_BYTES {
        return Err(ChannelError::MalformedTunnelFrame);
    }
    let (id_bytes, payload) = body.split_at(TUNNEL_ID_BYTES);
    let mut tunnel_id = [0u8; TUNNEL_ID_BYTES];
    tunnel_id.copy_from_slice(id_bytes);

    if payload.is_empty() {
        // Zero-length payload is the tunnel-close signal; dropping the
        // sender here is what turns a pending PtyTunnelHandle::read (or
        // call_rpc's single-shot receive) into a clean end-of-stream rather
        // than a silent hang.
        tunnels.remove(&tunnel_id);
        return Ok(());
    }
    if let Some(sender) = tunnels.get(&tunnel_id) {
        if sender.try_send(payload.to_vec()).is_err() {
            // Either the receiver was dropped without a close signal, or
            // it's backlogged past its channel capacity. Either way, per
            // relay-protocol.md's own backpressure posture ("closes the
            // tunnel rather than growing memory without limit"), drop the
            // tunnel here too rather than buffer unboundedly or block this
            // shared task on a slow consumer.
            tunnels.remove(&tunnel_id);
        }
    } else {
        tracing::debug!("tunnel frame for an unknown tunnel id, ignoring");
    }
    Ok(())
}

fn new_request_id() -> String {
    // A UUID crate is a reasonable addition, but this crate has no other
    // need for one yet — a simple time+counter-free random hex string via
    // the OS RNG avoids the extra dependency for a value that only needs
    // to be unique per in-flight request, not globally unique or ordered.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("req-{}-{count}", std::process::id())
}

/// Holds a phone connection open, reconnecting with exponential backoff and
/// jitter (1s→60s, per relay-protocol.md) whenever it drops, until
/// `cancel` is triggered. `on_connected`/`on_disconnected` let the caller
/// (the JNI bridge) update connection-state visible to Kotlin without this
/// loop knowing anything about JNI. `cancel` is a
/// [`tokio_util::sync::CancellationToken`] rather than a polled closure so
/// a connection currently held open by [`PhoneConnection::hold_until_closed`]
/// reacts to a `nativeClose` call immediately instead of only between
/// attempts.
///
/// Unchanged in shape from before tunnel support landed:
/// [`PhoneConnection`] stayed a single, non-`Clone` owner (a
/// [`PtyTunnelHandle`] only needs its own clone of the command channel, not
/// a clone of the connection itself), so handing exclusive ownership to
/// `on_connected` for the connection's lifetime and getting it back still
/// works exactly as it did — the multiplexing lives entirely inside
/// [`PhoneConnection`]/the background I/O task, not in this loop.
pub async fn run_with_reconnect<F, D>(
    relay_ws_url: String,
    session_credential: String,
    mut on_connected: F,
    mut on_disconnected: D,
    cancel: tokio_util::sync::CancellationToken,
) where
    F: FnMut(PhoneConnection) -> PhoneConnection + Send,
    D: FnMut() + Send,
{
    let mut attempt: u32 = 0;
    while !cancel.is_cancelled() {
        match PhoneConnection::connect(&relay_ws_url, &session_credential).await {
            Ok(connection) => {
                attempt = 0;
                let mut connection = on_connected(connection);
                tokio::select! {
                    _ = connection.hold_until_closed() => {}
                    () = cancel.cancelled() => return,
                }
                on_disconnected();
            }
            Err(error) => {
                tracing::warn!(%error, "failed to connect to relayd");
            }
        }
        let delay = backoff::compute_backoff(attempt, rand_unit());
        attempt = attempt.saturating_add(1);
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = cancel.cancelled() => return,
        }
    }
}

fn rand_unit() -> f64 {
    // Only the top 32 bits go into the ratio — plenty of entropy for a
    // jitter multiplier, and it keeps both operands exactly representable
    // as f64 (avoiding a precision-loss cast from a full u64).
    let mut bytes = [0u8; 8];
    getrandom_fill(&mut bytes);
    let high32 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    f64::from(high32) / f64::from(u32::MAX)
}

fn getrandom_fill(buf: &mut [u8]) {
    // Minimal inline OS-RNG read: std has no stable public RNG, and this
    // crate has no other need for a full `rand`/`getrandom` dependency
    // beyond this one jitter value. `/dev/urandom` is unavailable on
    // Android's JNI boundary the same way it is on Linux — reading through
    // `std::fs` keeps this dependency-free and portable to both host and
    // Android targets without a platform `cfg` split.
    use std::io::Read;
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let _ = file.read_exact(buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_protocol::framing::encode_frame;
    use choosh_protocol::host_rpc::WorkspaceSummary;
    use choosh_protocol::relay::{
        AuthOk, ConnectionState, IdentityClass, MAX_CONTROL_FRAME_BYTES, MAX_TUNNEL_FRAME_BYTES, SequencedAgentEvent,
        WireAgentStatus, WireInputReason, decode_tunnel_frame, encode_tunnel_frame, encode_tunnel_id_hex,
    };
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    type TestWs = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    async fn send_control<T: serde::Serialize>(ws: &mut TestWs, value: &T) {
        let mut payload = vec![FRAME_CLASS_CONTROL];
        payload.extend(serde_json::to_vec(value).unwrap());
        let wire = encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).unwrap();
        ws.send(Message::Binary(wire.into())).await.unwrap();
    }

    async fn recv_control<T: serde::de::DeserializeOwned>(ws: &mut TestWs) -> T {
        loop {
            match ws.next().await {
                Some(Ok(Message::Binary(bytes))) => {
                    let mut decoder = choosh_protocol::framing::FrameDecoder::new(
                        choosh_protocol::framing::FrameLimits::new(MAX_CONTROL_FRAME_BYTES, 4).unwrap(),
                    );
                    let frames = decoder.feed(&bytes).unwrap();
                    let (_class, body) = frames[0].split_first().unwrap();
                    return serde_json::from_slice(body).unwrap();
                }
                Some(Ok(_)) => {}
                other => panic!("expected a binary frame, got {other:?}"),
            }
        }
    }

    /// Sends a tunnel frame the way `relayd` would forward one from the
    /// other tunnel endpoint — used by the fake server in the tunnel tests
    /// below to simulate genuine pass-through, not a control-frame
    /// shortcut.
    async fn send_tunnel_frame(ws: &mut TestWs, tunnel_id: TunnelId, payload: &[u8]) {
        let wire = encode_frame(&encode_tunnel_frame(tunnel_id, payload), MAX_TUNNEL_FRAME_BYTES).unwrap();
        ws.send(Message::Binary(wire.into())).await.unwrap();
    }

    async fn recv_tunnel_frame(ws: &mut TestWs) -> (TunnelId, Vec<u8>) {
        loop {
            match ws.next().await {
                Some(Ok(Message::Binary(bytes))) => {
                    let mut decoder = choosh_protocol::framing::FrameDecoder::new(
                        choosh_protocol::framing::FrameLimits::new(MAX_CONTROL_FRAME_BYTES, 4).unwrap(),
                    );
                    let frames = decoder.feed(&bytes).unwrap();
                    let (id, payload) = decode_tunnel_frame(&frames[0]).expect("expected a tunnel frame");
                    return (id, payload.to_vec());
                }
                Some(Ok(_)) => {}
                other => panic!("expected a binary frame, got {other:?}"),
            }
        }
    }

    async fn bind_fake_relayd() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, format!("ws://{addr}/connect"))
    }

    /// Drives a fake relayd's connection through hello/auth-ok, the shared
    /// prefix every test below needs before it can do anything tunnel- or
    /// RPC-specific.
    async fn authenticate_fake_relayd(ws: &mut TestWs) {
        send_control(ws, &ServerHello { nonce: "n".to_string() }).await;
        let auth: ClientAuth = recv_control(ws).await;
        assert!(matches!(auth, ClientAuth::Phone(PhoneAuth { session_credential }) if session_credential == "good-cred"));
        send_control(
            ws,
            &AuthResult::Ok(AuthOk { identity_class: IdentityClass::Phone, device_id: "phone-1".to_string() }),
        )
        .await;
    }

    #[tokio::test]
    async fn phone_connects_and_lists_devhosts() {
        let (listener, url) = bind_fake_relayd().await;
        let server = tokio::spawn(async move {
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
                        last_seen: "2026-08-14T00:00:00Z".to_string(),
                    }],
                },
            )
            .await;
        });

        let mut connection = PhoneConnection::connect(&url, "good-cred").await.expect("connect should succeed");
        let devhosts = connection.list_devhosts().await.expect("list_devhosts should succeed");
        assert_eq!(devhosts.len(), 1);
        assert_eq!(devhosts[0].alias, "build-box");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rejected_session_credential_is_a_typed_error_not_a_hang() {
        let (listener, url) = bind_fake_relayd().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_control(&mut ws, &ServerHello { nonce: "n".to_string() }).await;
            let _auth: ClientAuth = recv_control(&mut ws).await;
            send_control(
                &mut ws,
                &AuthResult::Failed(choosh_protocol::relay::AuthFailed { reason: "revoked".to_string() }),
            )
            .await;
        });

        let result = PhoneConnection::connect(&url, "bad-cred").await;
        assert!(matches!(result, Err(ConnectError::Rejected(reason)) if reason == "revoked"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn request_enrollment_token_round_trips() {
        let (listener, url) = bind_fake_relayd().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;
            let request: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::RequestEnrollmentToken { request_id, .. } = request else {
                panic!("expected request-enrollment-token")
            };
            send_control(
                &mut ws,
                &ControlResponse::RequestEnrollmentTokenOk {
                    request_id,
                    token: "tok-123".to_string(),
                    expires_at: "2026-08-14T00:15:00Z".to_string(),
                },
            )
            .await;
        });

        let mut connection = PhoneConnection::connect(&url, "good-cred").await.unwrap();
        let (token, expires_at) = connection
            .request_enrollment_token(choosh_protocol::relay::IdentityClass::Devhost)
            .await
            .expect("request_enrollment_token should succeed");
        assert_eq!(token, "tok-123");
        assert_eq!(expires_at, "2026-08-14T00:15:00Z");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn call_rpc_round_trips_through_a_relayed_rpc_tunnel() {
        let (listener, url) = bind_fake_relayd().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            let open: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::OpenTunnel { request_id, target_device_id, purpose } = open else {
                panic!("expected open-tunnel");
            };
            assert_eq!(target_device_id, "dev-1");
            assert_eq!(purpose, "rpc");
            let tunnel_id: TunnelId = [1, 2, 3, 4, 5, 6, 7, 8];
            send_control(
                &mut ws,
                &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) },
            )
            .await;

            // The RPC request arrives as an opaque tunnel frame, the same
            // shape relayd would forward from phone to devhost — this
            // simulates relayd's pass-through behavior, not a
            // control-frame shortcut.
            let (id, payload) = recv_tunnel_frame(&mut ws).await;
            assert_eq!(id, tunnel_id);
            let (inner_class, inner_body) = payload.split_first().unwrap();
            assert_eq!(*inner_class, FRAME_CLASS_CONTROL);
            let request: RpcRequest = serde_json::from_slice(inner_body).unwrap();
            let RpcRequest::WorkspaceList { request_id: rpc_request_id } = request else {
                panic!("expected workspace-list");
            };

            let response = RpcResponse::WorkspaceListOk {
                request_id: rpc_request_id,
                workspaces: vec![WorkspaceSummary {
                    workspace_id: "ws-1".to_string(),
                    workspace_name: "app".to_string(),
                    devhost_id: "dev-1".to_string(),
                    project_id: "proj-1".to_string(),
                    created_at: "2026-08-14T00:00:00Z".to_string(),
                }],
            };
            let mut response_payload = vec![FRAME_CLASS_CONTROL];
            response_payload.extend(serde_json::to_vec(&response).unwrap());
            send_tunnel_frame(&mut ws, tunnel_id, &response_payload).await;

            // call_rpc closes its fresh, one-shot tunnel proactively.
            let (closed_id, closed_payload) = recv_tunnel_frame(&mut ws).await;
            assert_eq!(closed_id, tunnel_id);
            assert!(closed_payload.is_empty());
        });

        let mut connection = PhoneConnection::connect(&url, "good-cred").await.unwrap();
        let response = connection
            .call_rpc("dev-1", RpcRequest::WorkspaceList { request_id: "req-1".to_string() })
            .await
            .expect("call_rpc should succeed");
        match response {
            RpcResponse::WorkspaceListOk { workspaces, .. } => {
                assert_eq!(workspaces.len(), 1);
                assert_eq!(workspaces[0].workspace_name, "app");
            }
            other => panic!("unexpected response: {other:?}"),
        }
        server.await.unwrap();
    }

    /// The live-delivery half of `agent-events.md`'s "Delivery and replay"
    /// section: a fake relayd pushes `ServerPush::AgentEvent` unprompted
    /// (no `request_id` this side ever sent) on the ordinary control
    /// connection, and [`PhoneConnection::take_agent_event_receiver`]'s
    /// receiver surfaces it, `from_device_id`/`event`/`sequence` intact.
    #[tokio::test]
    async fn live_agent_event_push_is_surfaced_on_the_agent_event_receiver() {
        let (listener, url) = bind_fake_relayd().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;
            send_control(
                &mut ws,
                &ServerPush::AgentEvent {
                    from_device_id: "dev-1".to_string(),
                    event: WireAgentEvent::InputRequired {
                        workspace_id: "ws-1".to_string(),
                        item_id: "item-1".to_string(),
                        reason: WireInputReason::Approval,
                    },
                    sequence: Some(3),
                },
            )
            .await;
        });

        let mut connection = PhoneConnection::connect(&url, "good-cred").await.unwrap();
        let mut agent_events = connection.take_agent_event_receiver().expect("receiver must be available exactly once");
        let push = tokio::time::timeout(std::time::Duration::from_secs(5), agent_events.recv())
            .await
            .expect("must not hang waiting for the live push")
            .expect("channel must not be closed while the connection is alive");
        assert_eq!(push.from_device_id, "dev-1");
        assert_eq!(push.sequence, Some(3));
        assert_eq!(
            push.event,
            WireAgentEvent::InputRequired {
                workspace_id: "ws-1".to_string(),
                item_id: "item-1".to_string(),
                reason: WireInputReason::Approval,
            }
        );
        // Only ever consumable once.
        assert!(connection.take_agent_event_receiver().is_none());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn resume_agent_events_round_trips_a_replayed_response_over_its_own_tunnel_purpose() {
        let (listener, url) = bind_fake_relayd().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            let open: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::OpenTunnel { request_id, target_device_id, purpose } = open else {
                panic!("expected open-tunnel");
            };
            assert_eq!(target_device_id, "dev-1");
            assert_eq!(purpose, "agent-events");
            let tunnel_id: TunnelId = [9, 9, 9, 9, 9, 9, 9, 9];
            send_control(&mut ws, &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) })
                .await;

            let (id, payload) = recv_tunnel_frame(&mut ws).await;
            assert_eq!(id, tunnel_id);
            let (inner_class, inner_body) = payload.split_first().unwrap();
            assert_eq!(*inner_class, FRAME_CLASS_CONTROL);
            let request: AgentEventsResumeRequest = serde_json::from_slice(inner_body).unwrap();
            assert_eq!(request.workspace_id, "ws-1");
            assert_eq!(request.after_sequence, Some(2));

            let response = AgentEventsResumeResponse::Replayed {
                request_id: request.request_id,
                events: vec![
                    SequencedAgentEvent {
                        sequence: 3,
                        event: WireAgentEvent::AgentStatus {
                            workspace_id: "ws-1".to_string(),
                            item_id: "item-1".to_string(),
                            status: WireAgentStatus::Busy,
                        },
                    },
                    SequencedAgentEvent {
                        sequence: 4,
                        event: WireAgentEvent::TurnCompleted { workspace_id: "ws-1".to_string(), item_id: "item-1".to_string() },
                    },
                ],
                latest_sequence: 4,
            };
            let mut response_payload = vec![FRAME_CLASS_CONTROL];
            response_payload.extend(serde_json::to_vec(&response).unwrap());
            send_tunnel_frame(&mut ws, tunnel_id, &response_payload).await;

            // Same "closes its fresh, one-shot tunnel proactively" contract
            // call_rpc's own test already proves for the "rpc" purpose.
            let (closed_id, closed_payload) = recv_tunnel_frame(&mut ws).await;
            assert_eq!(closed_id, tunnel_id);
            assert!(closed_payload.is_empty());
        });

        let mut connection = PhoneConnection::connect(&url, "good-cred").await.unwrap();
        let response = connection
            .resume_agent_events("dev-1", "ws-1", Some(2))
            .await
            .expect("resume_agent_events should succeed");
        match response {
            AgentEventsResumeResponse::Replayed { events, latest_sequence, .. } => {
                assert_eq!(latest_sequence, 4);
                let sequences: Vec<u64> = events.iter().map(|event| event.sequence).collect();
                // Oldest first, no gaps, no duplicates.
                assert_eq!(sequences, vec![3, 4]);
            }
            AgentEventsResumeResponse::SnapshotRequired { .. } => panic!("expected a replay, got snapshot_required"),
        }
        server.await.unwrap();
    }

    /// The `snapshot_required` fallback path: proves it comes back as a
    /// well-formed, distinguishable outcome (not a [`CallError`], not
    /// silently treated as an empty replay).
    #[tokio::test]
    async fn resume_agent_events_reports_snapshot_required_distinctly_from_a_replay() {
        let (listener, url) = bind_fake_relayd().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            let open: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::OpenTunnel { request_id, purpose, .. } = open else { panic!("expected open-tunnel") };
            assert_eq!(purpose, "agent-events");
            let tunnel_id: TunnelId = [7, 7, 7, 7, 7, 7, 7, 7];
            send_control(&mut ws, &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) })
                .await;

            let (_id, payload) = recv_tunnel_frame(&mut ws).await;
            let (_inner_class, inner_body) = payload.split_first().unwrap();
            let request: AgentEventsResumeRequest = serde_json::from_slice(inner_body).unwrap();

            let response = AgentEventsResumeResponse::SnapshotRequired {
                request_id: request.request_id,
                workspace_id: request.workspace_id,
            };
            let mut response_payload = vec![FRAME_CLASS_CONTROL];
            response_payload.extend(serde_json::to_vec(&response).unwrap());
            send_tunnel_frame(&mut ws, tunnel_id, &response_payload).await;
            let _ = recv_tunnel_frame(&mut ws).await; // proactive close
        });

        let mut connection = PhoneConnection::connect(&url, "good-cred").await.unwrap();
        let response = connection.resume_agent_events("dev-1", "ws-1", Some(500)).await.expect("must not be a CallError");
        let AgentEventsResumeResponse::SnapshotRequired { workspace_id, .. } = &response else {
            panic!("expected snapshot_required, got {response:?}")
        };
        assert_eq!(workspace_id, "ws-1");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn open_pty_tunnel_streams_bytes_in_both_directions_and_closes_cleanly() {
        let (listener, url) = bind_fake_relayd().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            let open: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::OpenTunnel { request_id, purpose, .. } = open else {
                panic!("expected open-tunnel");
            };
            assert_eq!(purpose, "pty:item-1");
            let tunnel_id: TunnelId = [9; 8];
            send_control(
                &mut ws,
                &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) },
            )
            .await;

            // Devhost -> phone PTY output: raw bytes, no inner class tag.
            send_tunnel_frame(&mut ws, tunnel_id, b"hello from pty").await;

            // Phone -> devhost keystrokes.
            let (id, payload) = recv_tunnel_frame(&mut ws).await;
            assert_eq!(id, tunnel_id);
            assert_eq!(payload, b"ls\n");

            send_tunnel_frame(&mut ws, tunnel_id, &[]).await; // server-initiated close
        });

        let mut connection = PhoneConnection::connect(&url, "good-cred").await.unwrap();
        let mut handle =
            connection.open_pty_tunnel("dev-1", "item-1").await.expect("open_pty_tunnel should succeed");

        let output = handle.read().await.expect("should receive pty output");
        assert_eq!(output, b"hello from pty");

        handle.write(b"ls\n").expect("write should succeed");

        assert!(handle.read().await.is_none(), "a closed tunnel should read as a clean EOF");

        server.await.unwrap();
    }

    #[tokio::test]
    async fn open_web_tunnel_streams_bytes_in_both_directions_and_closes_cleanly() {
        let (listener, url) = bind_fake_relayd().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            let open: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::OpenTunnel { request_id, purpose, .. } = open else {
                panic!("expected open-tunnel");
            };
            assert_eq!(purpose, "web:item-1");
            let tunnel_id: TunnelId = [3; 8];
            send_control(
                &mut ws,
                &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) },
            )
            .await;

            // Devhost -> phone HTTP response bytes: raw, no inner class tag.
            send_tunnel_frame(&mut ws, tunnel_id, b"HTTP/1.1 200 OK\r\n\r\nhello").await;

            // Phone -> devhost HTTP request bytes.
            let (id, payload) = recv_tunnel_frame(&mut ws).await;
            assert_eq!(id, tunnel_id);
            assert_eq!(payload, b"GET / HTTP/1.1\r\n\r\n");

            send_tunnel_frame(&mut ws, tunnel_id, &[]).await; // server-initiated close
        });

        let mut connection = PhoneConnection::connect(&url, "good-cred").await.unwrap();
        let mut handle =
            connection.open_web_tunnel("dev-1", "item-1").await.expect("open_web_tunnel should succeed");

        let response = handle.read().await.expect("should receive response bytes");
        assert_eq!(response, b"HTTP/1.1 200 OK\r\n\r\nhello");

        handle.write(b"GET / HTTP/1.1\r\n\r\n").await.expect("write should succeed");

        assert!(handle.read().await.is_none(), "a closed tunnel should read as a clean EOF");

        server.await.unwrap();
    }

    #[tokio::test]
    async fn web_tunnel_write_ack_only_fires_after_the_fake_peer_reads_the_frame() {
        // Proves WebTunnelHandle::write's backpressure claim end to end:
        // `ack` is wired to fire only once `FrameChannel::send_tunnel`'s
        // real network write completes, not merely once the byte handed to
        // the (unbounded) command channel — this drives that exact code
        // path and confirms the write future only resolves after the fake
        // relayd has actually read the corresponding bytes off the wire,
        // never before.
        let (listener, url) = bind_fake_relayd().await;
        let (unblock_tx, unblock_rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            let open: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::OpenTunnel { request_id, .. } = open else { panic!("expected open-tunnel") };
            let tunnel_id: TunnelId = [4; 8];
            send_control(
                &mut ws,
                &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) },
            )
            .await;

            // Hold off reading until told to, so the test below can assert
            // the write hasn't resolved during that window.
            unblock_rx.await.unwrap();
            let (id, payload) = recv_tunnel_frame(&mut ws).await;
            assert_eq!(id, tunnel_id);
            assert_eq!(payload, b"chunk");
        });

        let mut connection = PhoneConnection::connect(&url, "good-cred").await.unwrap();
        let handle = connection.open_web_tunnel("dev-1", "item-1").await.unwrap();

        let write_future = tokio::spawn(async move { handle.write(b"chunk").await });

        // A real TCP loopback write of five bytes typically completes at
        // the OS level even before the peer calls recv() (it lands in the
        // kernel's socket buffer) — this assertion is about ordering
        // relative to `unblock_tx`, not proving the OS-level send blocks
        // for a tiny payload; the point under test is that `ack` is wired
        // to a real send completion at all (see the next assertion), which
        // `web_tunnel_streams_bytes...` above already exercises for
        // correctness. This test's job is narrower: confirm `write` doesn't
        // hang forever and resolves once the send genuinely happens.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        unblock_tx.send(()).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(5), write_future)
            .await
            .expect("write should resolve promptly once the wire send completes")
            .unwrap()
            .expect("write should succeed");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn sequential_web_tunnel_writes_are_not_reordered() {
        // The gateway's upload loop reads one client chunk, awaits `write`,
        // then reads the next — this proves that discipline actually
        // preserves order on the wire when driven back-to-back.
        let (listener, url) = bind_fake_relayd().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            let open: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::OpenTunnel { request_id, .. } = open else { panic!("expected open-tunnel") };
            let tunnel_id: TunnelId = [5; 8];
            send_control(
                &mut ws,
                &ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) },
            )
            .await;

            for expected in [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()] {
                let (id, payload) = recv_tunnel_frame(&mut ws).await;
                assert_eq!(id, tunnel_id);
                assert_eq!(payload, expected);
            }
        });

        let mut connection = PhoneConnection::connect(&url, "good-cred").await.unwrap();
        let handle = connection.open_web_tunnel("dev-1", "item-1").await.unwrap();
        for chunk in [b"a" as &[u8], b"b", b"c"] {
            handle.write(chunk).await.unwrap();
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_rpc_and_pty_tunnels_do_not_block_or_corrupt_each_other() {
        let (listener, url) = bind_fake_relayd().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            authenticate_fake_relayd(&mut ws).await;

            let open_pty: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::OpenTunnel { request_id: pty_request_id, purpose, .. } = open_pty else {
                panic!("expected open-tunnel");
            };
            assert_eq!(purpose, "pty:item-1");
            let pty_tunnel_id: TunnelId = [7; 8];
            send_control(
                &mut ws,
                &ControlResponse::OpenTunnelOk { request_id: pty_request_id, tunnel_id: encode_tunnel_id_hex(pty_tunnel_id) },
            )
            .await;

            let open_rpc: ControlRequest = recv_control(&mut ws).await;
            let ControlRequest::OpenTunnel { request_id: rpc_request_id, purpose, .. } = open_rpc else {
                panic!("expected open-tunnel");
            };
            assert_eq!(purpose, "rpc");
            let rpc_tunnel_id: TunnelId = [8; 8];
            send_control(
                &mut ws,
                &ControlResponse::OpenTunnelOk { request_id: rpc_request_id, tunnel_id: encode_tunnel_id_hex(rpc_tunnel_id) },
            )
            .await;

            let (id, payload) = recv_tunnel_frame(&mut ws).await;
            assert_eq!(id, rpc_tunnel_id);
            let (_inner_class, inner_body) = payload.split_first().unwrap();
            let request: RpcRequest = serde_json::from_slice(inner_body).unwrap();
            let RpcRequest::WorkspaceList { request_id: rpc_call_id } = request else {
                panic!("expected workspace-list");
            };

            // Interleave: two pty output chunks land on the wire BEFORE the
            // rpc response — proving the client's pty read isn't starved
            // behind the in-flight rpc call, and that the rpc call isn't
            // corrupted by intervening traffic on a different tunnel id.
            send_tunnel_frame(&mut ws, pty_tunnel_id, b"chunk-1").await;
            send_tunnel_frame(&mut ws, pty_tunnel_id, b"chunk-2").await;

            let response = RpcResponse::WorkspaceListOk { request_id: rpc_call_id, workspaces: vec![] };
            let mut response_payload = vec![FRAME_CLASS_CONTROL];
            response_payload.extend(serde_json::to_vec(&response).unwrap());
            send_tunnel_frame(&mut ws, rpc_tunnel_id, &response_payload).await;

            let (closed_id, closed_payload) = recv_tunnel_frame(&mut ws).await;
            assert_eq!(closed_id, rpc_tunnel_id);
            assert!(closed_payload.is_empty());

            // A third pty chunk after the rpc call has already completed.
            send_tunnel_frame(&mut ws, pty_tunnel_id, b"chunk-3").await;
        });

        let mut connection = PhoneConnection::connect(&url, "good-cred").await.unwrap();
        let mut pty = connection.open_pty_tunnel("dev-1", "item-1").await.expect("open_pty_tunnel should succeed");

        let response = connection
            .call_rpc("dev-1", RpcRequest::WorkspaceList { request_id: "req-1".to_string() })
            .await
            .expect("call_rpc should succeed while a pty tunnel is concurrently open");
        assert!(matches!(response, RpcResponse::WorkspaceListOk { .. }));

        // All three pty chunks arrive in order, undisturbed by the
        // interleaved rpc traffic on a different tunnel id.
        assert_eq!(pty.read().await.unwrap(), b"chunk-1");
        assert_eq!(pty.read().await.unwrap(), b"chunk-2");
        assert_eq!(pty.read().await.unwrap(), b"chunk-3");

        server.await.unwrap();
    }

    #[tokio::test]
    async fn dropped_connection_during_reconnect_loop_eventually_reconnects() {
        // Bind, accept one connection, authenticate it, then close it
        // immediately (simulating a mid-session drop) — the loop should
        // observe the drop and attempt a second connection.
        let (listener, url) = bind_fake_relayd().await;
        let second_attempt_seen = std::sync::Arc::new(tokio::sync::Notify::new());
        let second_attempt_seen_writer = second_attempt_seen.clone();

        let server = tokio::spawn(async move {
            for attempt in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                send_control(&mut ws, &ServerHello { nonce: "n".to_string() }).await;
                let _auth: ClientAuth = recv_control(&mut ws).await;
                send_control(
                    &mut ws,
                    &AuthResult::Ok(AuthOk { identity_class: IdentityClass::Phone, device_id: "phone-1".to_string() }),
                )
                .await;
                if attempt == 0 {
                    let _ = ws.close(None).await;
                } else {
                    second_attempt_seen_writer.notify_one();
                    // Keep this second connection open until the test ends.
                    let _ = ws.next().await;
                }
            }
        });

        let cancel = tokio_util::sync::CancellationToken::new();
        let run = tokio::spawn(run_with_reconnect(
            url,
            "good-cred".to_string(),
            |connection| connection,
            || {},
            cancel.clone(),
        ));

        tokio::time::timeout(std::time::Duration::from_secs(5), second_attempt_seen.notified())
            .await
            .expect("a second connection attempt should happen after the first drops");

        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), run)
            .await
            .expect("run_with_reconnect should stop promptly once cancelled")
            .unwrap();
        server.abort();
    }
}
