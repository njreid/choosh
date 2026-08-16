//! Control-frame wire types for the `choosh-relayd` protocol.
//!
//! Mirrors `docs/specs/relay-protocol.md` and `docs/specs/auth-and-enrollment.md`
//! exactly: frame classing, the connect-time challenge/credential handshake, and
//! the full control-frame/server-push catalog as it has grown milestone by
//! milestone — enrollment and presence (M0), `open-tunnel` (M1), `agent-event`/
//! `register-fcm-token` (M2), the laptop-proxy-only `list-devhost-ssh-endpoints`
//! read (M6), and the `relayd`-pushed self-update instruction (M8). Shared by
//! `choosh-relayd` and `choosh-hostd` so both sides serialize/deserialize
//! identical Rust types rather than independently-drifting JSON shapes.
//!
//! **`agent-events.md`'s "Delivery and replay" machinery** ([`SequencedAgentEvent`],
//! [`AgentEventsResumeRequest`], [`AgentEventsResumeResponse`], and the
//! `sequence` field on [`ControlRequest::AgentEvent`]/[`ServerPush::AgentEvent`])
//! lands here rather than as new `RpcRequest`/`RpcResponse` variants in
//! `host_rpc.rs`. Both are real options — the spec section itself frames
//! this as "phone tells the owning `choosh-hostd` 'resume workspace X from
//! sequence N', over the `rpc`-purpose tunnel it already opens to that
//! devhost, since that tunnel is already point-to-point to where the spool
//! lives and `relayd` doesn't track workspace-to-devhost ownership" — but
//! resuming *literally* over the `"rpc"` purpose would mean adding a
//! `RpcRequest` variant, which `choosh-hostd::rpc::dispatch` must then
//! exhaustively match on. This crate has no dependency on that dispatch
//! function, so nothing here *forces* that coupling — the resume
//! request/response ride their own `"agent-events"` tunnel purpose instead
//! (opened with the exact same `ControlRequest::OpenTunnel` a phone already
//! uses for `"rpc"`; `relayd`'s `open-tunnel` purpose is an opaque tag it
//! never validates, per relay-protocol.md, so a second purpose string costs
//! `relayd` nothing new) and are handled directly in `choosh-hostd::serve`'s
//! existing tunnel-frame dispatch, alongside `pty:`/`web:`/`ssh`/`offload` —
//! not through `choosh-hostd::rpc`'s `RpcRequest` surface at all. The
//! *routing* shape is identical to the RPC-tunnel design either way (a
//! phone-to-specific-devhost point-to-point tunnel, no new `relayd`-side
//! workspace-to-devhost resolution capability); only which existing dispatch
//! surface it rides differs.

use serde::{Deserialize, Serialize};

/// First byte of every frame payload, per relay-protocol.md's frame classing.
pub const FRAME_CLASS_CONTROL: u8 = 0x01;
pub const FRAME_CLASS_TUNNEL: u8 = 0x02;

/// relay-protocol.md's stated bounds.
pub const MAX_CONTROL_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_TUNNEL_FRAME_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IdentityClass {
    Phone,
    LaptopProxy,
    Devhost,
}

/// `relayd`'s first frame on every new connection: an unauthenticated
/// challenge nonce the connecting Identity must sign (devhost/laptop-proxy)
/// or present alongside a stored session credential (phone), per
/// auth-and-enrollment.md. Not itself a control frame in the request/response
/// sense — it precedes any `request_id`-bearing exchange.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServerHello {
    pub nonce: String,
}

/// A devhost or laptop-proxy's answer to `ServerHello`, proving possession
/// of the private key behind its enrolled certificate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceAuth {
    pub device_id: String,
    /// Base64-encoded certificate issued at enrollment.
    pub certificate: String,
    /// Base64-encoded Ed25519 signature over `nonce`.
    pub signature: String,
}

/// A phone's answer to `ServerHello`: its stored, `relayd`-issued session
/// credential (an opaque bearer token, not the passkey itself).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PhoneAuth {
    pub session_credential: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ClientAuth {
    Device(DeviceAuth),
    Phone(PhoneAuth),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthOk {
    pub identity_class: IdentityClass,
    pub device_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthFailed {
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum AuthResult {
    Ok(AuthOk),
    Failed(AuthFailed),
}

/// One devhost's presence record, per relay-protocol.md's `list-devhosts`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DevHostPresence {
    pub device_id: String,
    pub alias: String,
    pub platform: String,
    pub account_label: Option<String>,
    pub connection_state: ConnectionState,
    /// RFC 3339 timestamp of the last frame received from this Identity.
    pub last_seen: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Online,
    Offline,
}

/// One devhost's SSH-bridge endpoint, as returned by
/// [`ControlRequest::ListDevhostSshEndpoints`] — a narrower sibling of
/// [`DevHostPresence`] carrying only what `choosh-hostd proxy sync` needs
/// (alias + SSH host key) rather than the fuller presence record
/// `list-devhosts` exposes. See auth-and-enrollment.md's "Laptop-proxy
/// enrollment" section: a `laptop-proxy` connection is permitted this
/// restricted read even though it cannot call `list-devhosts` itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DevhostSshEndpoint {
    pub device_id: String,
    pub alias: String,
    /// Base64-encoded raw 32-byte Ed25519 public key — the same encoding
    /// [`ControlRequest::Enroll`]'s `public_key`/`host_ssh_public_key`
    /// fields use, not an OpenSSH `ssh-ed25519 AAAA...` line. Converting to
    /// the OpenSSH wire format for a `known_hosts` line is the caller's
    /// job (`choosh-hostd proxy sync`), not this wire type's.
    pub ssh_host_public_key: String,
}

/// Wire shape of `agent-events.md`'s normalized event set, as carried over
/// the relay — a serde-friendly sibling of [`crate::agent_event`]'s
/// deliberately dependency-free internal validation types, not a
/// replacement for them. `choosh-hostd` is responsible for producing one of
/// these from a validated [`crate::agent_event::NormalizedAgentEvent`]
/// before sending it; this type only needs to serialize/deserialize
/// identically on both ends of the wire.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WireAgentEvent {
    InputRequired {
        workspace_id: String,
        item_id: String,
        reason: WireInputReason,
    },
    TurnCompleted {
        workspace_id: String,
        item_id: String,
    },
    FilesChanged {
        workspace_id: String,
        item_id: String,
        /// Root-relative candidate paths per agent-events.md — never
        /// absolute, never outside the workspace root.
        paths: Vec<String>,
    },
    AgentStatus {
        workspace_id: String,
        item_id: String,
        status: WireAgentStatus,
    },
    /// A Zed session attached to this workspace through the loopback SSH
    /// bridge (`ssh-bridge-and-zed.md`'s "Editor presence"). Carries no
    /// file paths, command text, or session content — existence only.
    EditorAttached {
        workspace_id: String,
        editor: WireEditor,
    },
    /// The SSH session behind a prior [`Self::EditorAttached`] closed.
    EditorDetached {
        workspace_id: String,
        editor: WireEditor,
    },
    /// `choosh-hostd` detected a headless device-code SSO/cloud-CLI auth
    /// flow (`agent-events.md`'s `auth_required`: `aws sso login`, `gcloud
    /// auth login`, `az login`, `gh auth login`) with no local browser to
    /// hand off to. Deliberately carries no `workspace_id`/`item_id` — per
    /// the spec, this payload is exactly these three fields, nothing else;
    /// it is not attributable to a single workspace item the way every
    /// other variant above is. **No token, credential, or session
    /// identifier MUST ever appear here** — `choosh-hostd`'s detector
    /// (`choosh-hostd::auth_detect`) is responsible for extracting only
    /// these three fields from a provider's real output before this type
    /// is ever constructed, never forwarding a captured line/buffer
    /// wholesale.
    AuthRequired {
        provider: WireAuthProvider,
        user_code: String,
        verification_uri: String,
    },
}

impl WireAgentEvent {
    /// The workspace this event is attributable to, per `agent-events.md`'s
    /// "Delivery and replay" section ("MUST receive a monotonically
    /// increasing sequence number per workspace"). `None` only for
    /// [`Self::AuthRequired`] — per its own doc comment, that variant is
    /// deliberately not scoped to a single workspace, so it is not
    /// sequenced or retained in [`SequencedAgentEvent`]'s per-workspace
    /// spool (`choosh-hostd::agent_event_spool`); it is still delivered
    /// live exactly as before, just outside the replay mechanism.
    #[must_use]
    pub fn workspace_id(&self) -> Option<&str> {
        match self {
            Self::InputRequired { workspace_id, .. }
            | Self::TurnCompleted { workspace_id, .. }
            | Self::FilesChanged { workspace_id, .. }
            | Self::AgentStatus { workspace_id, .. }
            | Self::EditorAttached { workspace_id, .. }
            | Self::EditorDetached { workspace_id, .. } => Some(workspace_id),
            Self::AuthRequired { .. } => None,
        }
    }
}

/// One workspace-scoped agent event as retained in `choosh-hostd`'s
/// per-workspace spool or replayed in an [`AgentEventsResumeResponse`],
/// carrying the sequence number `agent-events.md`'s "Delivery and replay"
/// section requires. Only ever constructed for an event whose
/// [`WireAgentEvent::workspace_id`] is `Some` — the spool never assigns a
/// sequence to (or retains) a [`WireAgentEvent::AuthRequired`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SequencedAgentEvent {
    pub sequence: u64,
    pub event: WireAgentEvent,
}

/// Phone -> `choosh-hostd` "resume this workspace's agent-event stream
/// after this sequence" request, per `agent-events.md`'s "Delivery and
/// replay" section. Carried as a JSON tunnel-data frame over an
/// `"agent-events"`-purpose `open-tunnel` (relay-protocol.md's tunnel
/// mechanism, same shape `host-rpc.md`'s `"rpc"`-purpose tunnel already
/// uses for request/response JSON over tunnel frames) opened directly to
/// the devhost that owns `workspace_id` — deliberately its own tunnel
/// purpose rather than a new `host-rpc.md` `RpcRequest` variant, so this
/// module (and `choosh-hostd::serve`, which handles it) can implement and
/// test the whole mechanism without touching `choosh-hostd::rpc`'s
/// `RpcRequest` dispatch surface at all. See this crate's `relay` module
/// doc comment for the fuller design rationale (RPC-tunnel vs. a new
/// `relayd`-side workspace-to-devhost routing capability).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentEventsResumeRequest {
    pub request_id: String,
    pub workspace_id: String,
    /// Replay every retained event with a sequence strictly greater than
    /// this. `None` means "no prior acknowledged sequence for this
    /// workspace" (e.g. a first subscribe) — answered with every event
    /// currently retained for `workspace_id`, oldest first, same as
    /// `Some(0)` would be (sequence numbers are assigned starting at 1, so
    /// no real event ever has sequence 0).
    pub after_sequence: Option<u64>,
}

/// `choosh-hostd`'s answer to [`AgentEventsResumeRequest`]. `SnapshotRequired`
/// is not an error — it is the documented, correct answer per
/// `agent-events.md` whenever `after_sequence` names a sequence older than
/// what the bounded spool still retains: replaying anyway would either skip
/// silently-evicted events (a gap) or panic/underflow trying to find them,
/// neither of which this type's shape allows a caller to accidentally do.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AgentEventsResumeResponse {
    Replayed {
        request_id: String,
        /// Oldest first, strictly increasing, every `sequence` strictly
        /// greater than the request's `after_sequence` and less than or
        /// equal to `latest_sequence` — never a gap, never a duplicate.
        events: Vec<SequencedAgentEvent>,
        /// The highest sequence now retained for this workspace, even when
        /// `events` is empty because the caller was already fully caught
        /// up — lets the caller advance its acknowledged cursor without
        /// having received a new event.
        latest_sequence: u64,
    },
    SnapshotRequired {
        request_id: String,
        workspace_id: String,
    },
}

/// The four named cloud/SSO providers `agent-events.md`'s `auth_required`
/// event covers. Serializes exactly as the spec's payload shape shows
/// (`"aws"`/`"gcp"`/`"azure"`/`"github"`), not as `PascalCase`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireAuthProvider {
    Aws,
    Gcp,
    Azure,
    Github,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireInputReason {
    Approval,
    Permission,
    Question,
    Elicitation,
    NextPrompt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireAgentStatus {
    Starting,
    Busy,
    Waiting,
    Stopped,
    Failed,
}

/// Editor identity for [`WireAgentEvent::EditorAttached`]/
/// [`WireAgentEvent::EditorDetached`], per `ssh-bridge-and-zed.md`'s
/// "Editor presence" section. Only `zed` exists today (the SSH bridge's
/// only editor integration), but this is its own type rather than a bare
/// `String` so a second editor integration doesn't need a wire-format
/// migration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireEditor {
    Zed,
}

/// Client-to-relayd control-frame requests.
///
/// `open-tunnel` landed with M1 (its RPC surface rides over an `rpc`-purpose
/// tunnel, so the tunnel mechanism was needed before any RPC content could
/// exist). `agent-event` and `register-fcm-token` land with M2.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ControlRequest {
    Enroll {
        request_id: String,
        token: String,
        identity_class: IdentityClass,
        /// Base64-encoded Ed25519 public key.
        public_key: String,
        /// Devhost-only: the loopback SSH server's host public key, per
        /// auth-and-enrollment.md step 6. `None` for laptop-proxy enrollment.
        host_ssh_public_key: Option<String>,
        alias: Option<String>,
        platform: Option<String>,
        account_label: Option<String>,
    },
    RequestEnrollmentToken {
        request_id: String,
        identity_class: IdentityClass,
    },
    ListDevhosts {
        request_id: String,
    },
    /// `laptop-proxy`-only restricted fleet read for `proxy sync`
    /// (auth-and-enrollment.md's "Laptop-proxy enrollment"): alias + SSH
    /// host key per devhost, nothing else `list-devhosts` exposes (no
    /// platform, account label, or presence/connection state — `proxy
    /// sync` doesn't need them and a laptop-proxy Identity isn't scoped to
    /// read them).
    ListDevhostSshEndpoints {
        request_id: String,
    },
    /// Opens a tunnel to `target_device_id`. `purpose` is an opaque tag the
    /// two tunnel endpoints agree on out of band (`relayd` does not
    /// validate its content beyond passing it through) — see
    /// relay-protocol.md's `open-tunnel` section for the capability-scope
    /// rules governing which Identity class may use which purpose.
    OpenTunnel {
        request_id: String,
        target_device_id: String,
        purpose: String,
    },
    /// Devhost-only, per auth-and-enrollment.md's capability-scope table.
    /// `relayd` forwards this to the owning phone Identity if connected
    /// (`ServerPush::AgentEvent`), or triggers FCM dispatch if not — see
    /// notifications.md.
    AgentEvent {
        request_id: String,
        event: WireAgentEvent,
        /// The sequence `choosh-hostd` assigned this event in its
        /// per-workspace spool (see [`SequencedAgentEvent`]), or `None` for
        /// an event with no [`WireAgentEvent::workspace_id`] (only
        /// `AuthRequired`). Added additively alongside the pre-existing
        /// `event` field, per `agent-events.md`'s "Delivery and replay"
        /// section — `relayd` forwards it unmodified in the matching
        /// `ServerPush::AgentEvent`.
        sequence: Option<u64>,
    },
    /// Phone-only. Replaces any previously registered token for this phone
    /// Identity — `relayd` holds at most one FCM token per phone Identity.
    RegisterFcmToken {
        request_id: String,
        fcm_token: String,
    },
    /// Phone-only. Sets `revoked = true` for `device_id` (a previously
    /// enrolled devhost or laptop-proxy Identity), per
    /// auth-and-enrollment.md's "Revocation" section — `relayd` is the sole
    /// source of truth for whether a `device_id` is still valid, not
    /// certificate expiry alone. A revoked device's *next* connection
    /// attempt already fails closed at the challenge/signature step
    /// (`authenticate_device`); this frame additionally disconnects that
    /// device's *current* live connection immediately, if it has one,
    /// rather than leaving it to keep running under a since-revoked
    /// credential until it happens to reconnect.
    RevokeDevice {
        request_id: String,
        device_id: String,
    },
    /// Phone-only. Invalidates every phone-session credential recorded
    /// against `device_id` (auth-and-enrollment.md: "may hold multiple
    /// passkey credentials, one per enrolled phone/browser") and
    /// disconnects that Identity's live connection immediately if it's
    /// currently online — the phone-session analogue of `RevokeDevice`,
    /// e.g. "log out this device's passkey session" from another
    /// already-authenticated phone session. `device_id` here identifies the
    /// enrolled phone/browser being logged out (`PhoneSession::device_id`),
    /// not the opaque session-credential bearer token itself, which a
    /// revoking session has no legitimate way to know for a session other
    /// than its own.
    RevokePhoneSession {
        request_id: String,
        device_id: String,
    },
}

/// Generates a `request_id(&self) -> &str` accessor for a wire-protocol enum
/// whose variants are all struct variants carrying a `request_id` field.
/// Expands to a real, non-wildcard `match` over exactly the variants named
/// in the invocation — so this keeps the same compile-time safety net a
/// hand-written exhaustive match gives: a variant added to `$Enum` without
/// also being added to the matching macro invocation is a compile error (an
/// uncovered `match` arm), not a silently-skipped one. Shared with
/// [`crate::host_rpc`]'s `RpcRequest`/`RpcResponse`, which have the
/// identical shape.
macro_rules! impl_request_id {
    (
        $(#[$meta:meta])*
        impl $Enum:ident { $($Variant:ident),+ $(,)? }
    ) => {
        impl $Enum {
            $(#[$meta])*
            #[must_use]
            pub fn request_id(&self) -> &str {
                match self {
                    $(Self::$Variant { request_id, .. } => request_id,)+
                }
            }
        }
    };
}
pub(crate) use impl_request_id;

impl_request_id! {
    /// Every variant carries a client-generated `request_id`, echoed back in
    /// the matching [`ControlResponse`] (see [`ControlResponse::request_id`])
    /// per relay-protocol.md's "Control frames" section. Used by `relayd`
    /// to attach a caller's own `request_id` to an error it needs to send
    /// before a request has been fully dispatched (e.g. a per-Identity
    /// rate-limit rejection).
    impl ControlRequest {
        Enroll, RequestEnrollmentToken, ListDevhosts, ListDevhostSshEndpoints,
        OpenTunnel, AgentEvent, RegisterFcmToken, RevokeDevice, RevokePhoneSession,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ControlResponse {
    EnrollOk {
        request_id: String,
        device_id: String,
        /// Base64-encoded certificate binding `public_key` to `device_id`.
        certificate: String,
    },
    RequestEnrollmentTokenOk {
        request_id: String,
        token: String,
        /// RFC 3339 expiry, 15 minutes from issuance per auth-and-enrollment.md.
        expires_at: String,
    },
    ListDevhostsOk {
        request_id: String,
        devhosts: Vec<DevHostPresence>,
    },
    ListDevhostSshEndpointsOk {
        request_id: String,
        endpoints: Vec<DevhostSshEndpoint>,
    },
    OpenTunnelOk {
        request_id: String,
        /// Lowercase hex encoding of the 8 raw tunnel-ID bytes carried in
        /// every `0x02` frame for this tunnel — see [`encode_tunnel_id_hex`]
        /// / [`decode_tunnel_id_hex`].
        tunnel_id: String,
    },
    /// Acknowledges receipt — never confirms delivery to the phone (that's
    /// either an immediate `ServerPush::AgentEvent`, or, if the phone is
    /// unreachable right now, an async FCM dispatch attempt with no
    /// synchronous confirmation channel back to the devhost).
    AgentEventOk {
        request_id: String,
    },
    RegisterFcmTokenOk {
        request_id: String,
    },
    /// `device_id` echoes the revoked target, matching `RevokeDevice`'s
    /// request field.
    RevokeDeviceOk {
        request_id: String,
        device_id: String,
    },
    /// `device_id` echoes the logged-out target, matching
    /// `RevokePhoneSession`'s request field.
    RevokePhoneSessionOk {
        request_id: String,
        device_id: String,
    },
    Error {
        request_id: String,
        code: String,
        message: String,
    },
}

impl_request_id! {
    impl ControlResponse {
        EnrollOk, RequestEnrollmentTokenOk, ListDevhostsOk, ListDevhostSshEndpointsOk,
        OpenTunnelOk, AgentEventOk, RegisterFcmTokenOk, RevokeDeviceOk, RevokePhoneSessionOk,
        Error,
    }
}

/// `relayd` -> Identity unsolicited pushes: frames that don't answer a
/// `request_id` the receiver sent. Distinguished from [`ControlResponse`]
/// by using a disjoint set of `type` tag values — a receiver tries
/// `ControlResponse` first (it's waiting on a specific `request_id`) and
/// falls back to `ServerPush` for anything else, the same dispatch shape
/// `choosh-relayd` already uses to distinguish a reconnecting Identity's
/// `ClientAuth` from a fresh device's unauthenticated `enroll`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerPush {
    /// Sent to the target of a just-opened tunnel so it knows to start
    /// accepting `0x02` frames for `tunnel_id`. There is no accept/reject
    /// handshake at the relay layer — a target that doesn't want the
    /// tunnel simply never sends data and lets it idle-timeout.
    TunnelOffered {
        tunnel_id: String,
        from_device_id: String,
        purpose: String,
    },
    /// Forwarded to the owning phone Identity when a devhost sends
    /// `agent-event` and that phone is currently connected. `from_device_id`
    /// lets a phone reachable from multiple devhosts attribute the event.
    AgentEvent {
        from_device_id: String,
        event: WireAgentEvent,
        /// Verbatim copy of the originating [`ControlRequest::AgentEvent`]'s
        /// `sequence` — `relayd` never assigns or reinterprets it, it only
        /// routes it through, same as `event` itself.
        sequence: Option<u64>,
    },
    /// `relayd`-pushed self-update instruction, per relay-protocol.md's
    /// `update_binary` section and host-deployment.md's "Self-update".
    /// Triggered by an operator action (the Android app or `just
    /// deploy`-adjacent tooling), not by anything in this Identity
    /// protocol itself. The devhost does not reply over this channel —
    /// see host-deployment.md for the download-verify-swap-restart
    /// procedure and its Rollback subsection. `push_id` is what makes a
    /// redelivery after reconnect idempotently handleable (relay-protocol.md's
    /// "Control frames" section: every server-pushed frame carries one,
    /// in place of the `request_id` an ordinary client-initiated frame
    /// carries).
    ///
    /// Named `update_binary` on the wire (`snake_case`) rather than
    /// `update-binary` — the one exception to this enum's otherwise
    /// uniform kebab-case tag convention, because both relay-protocol.md
    /// and host-deployment.md spell it with an underscore throughout;
    /// `#[serde(rename = ...)]` overrides the enum-level `rename_all` for
    /// this variant alone.
    #[serde(rename = "update_binary")]
    UpdateBinary {
        push_id: String,
        download_url: String,
        /// Lowercase hex-encoded SHA-256 of the binary at `download_url`.
        sha256: String,
        version: String,
    },
}

/// Byte length of a tunnel ID as carried in a `0x02` frame, immediately
/// after the class byte.
pub const TUNNEL_ID_BYTES: usize = 8;

/// Builds a complete `0x02` tunnel-frame *payload*: class byte, tunnel ID,
/// and opaque bytes. Still needs the outer 4-byte length prefix from
/// [`choosh_protocol::framing::encode_frame`](crate::framing::encode_frame)
/// before it's wire-ready.
///
/// A zero-length `payload` is a valid, meaningful close signal per
/// relay-protocol.md's tunnel lifecycle — this function does not
/// special-case it.
#[must_use]
pub fn encode_tunnel_frame(tunnel_id: [u8; TUNNEL_ID_BYTES], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + TUNNEL_ID_BYTES + payload.len());
    out.push(FRAME_CLASS_TUNNEL);
    out.extend_from_slice(&tunnel_id);
    out.extend_from_slice(payload);
    out
}

/// Splits an already-length-unwrapped frame payload (as yielded by
/// [`choosh_protocol::framing::FrameDecoder::feed`](crate::framing::FrameDecoder::feed))
/// into its tunnel ID and opaque payload, iff its class byte is
/// [`FRAME_CLASS_TUNNEL`]. Returns `None` for a wrong class byte, an empty
/// frame, or a frame too short to contain a full tunnel ID — every case the
/// caller MUST treat as a malformed frame per relay-protocol.md (terminate
/// the connection, no partial recovery), the same way [`crate::relay`]'s
/// control-frame decoding already does for its own malformed cases.
#[must_use]
pub fn decode_tunnel_frame(frame: &[u8]) -> Option<([u8; TUNNEL_ID_BYTES], &[u8])> {
    let (class, rest) = frame.split_first()?;
    if *class != FRAME_CLASS_TUNNEL || rest.len() < TUNNEL_ID_BYTES {
        return None;
    }
    let (id_bytes, payload) = rest.split_at(TUNNEL_ID_BYTES);
    let mut id = [0u8; TUNNEL_ID_BYTES];
    id.copy_from_slice(id_bytes);
    Some((id, payload))
}

/// Lowercase hex encoding of a tunnel ID for JSON fields (`OpenTunnelOk`,
/// `TunnelOffered`) — the wire-frame representation stays raw bytes; this
/// is only for the control-plane JSON that names a tunnel.
#[must_use]
pub fn encode_tunnel_id_hex(tunnel_id: [u8; TUNNEL_ID_BYTES]) -> String {
    use std::fmt::Write;
    tunnel_id.iter().fold(String::with_capacity(TUNNEL_ID_BYTES * 2), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Inverse of [`encode_tunnel_id_hex`]. Returns `None` for anything that
/// isn't exactly `TUNNEL_ID_BYTES * 2` lowercase hex characters — never
/// panics on attacker-controlled input.
#[must_use]
pub fn decode_tunnel_id_hex(hex: &str) -> Option<[u8; TUNNEL_ID_BYTES]> {
    if hex.len() != TUNNEL_ID_BYTES * 2 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut id = [0u8; TUNNEL_ID_BYTES];
    for (index, out_byte) in id.iter_mut().enumerate() {
        *out_byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enroll_request_round_trips_through_json() {
        let request = ControlRequest::Enroll {
            request_id: "11111111-1111-1111-1111-111111111111".to_string(),
            token: "tok".to_string(),
            identity_class: IdentityClass::Devhost,
            public_key: "cHVi".to_string(),
            host_ssh_public_key: Some("c3No".to_string()),
            alias: Some("build-box".to_string()),
            platform: Some("linux".to_string()),
            account_label: None,
        };
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.contains("\"type\":\"enroll\""));
        let decoded: ControlRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, request);
    }

    #[test]
    fn control_request_variants_carry_request_id() {
        // Every `ControlRequest` variant, not just a representative subset —
        // `impl_request_id!`'s match is compile-checked exhaustive, but this
        // pins down the observable accessor behavior for each of the 9
        // variants individually, the same discipline as a hand-written match.
        assert_eq!(
            ControlRequest::Enroll {
                request_id: "id".to_string(),
                token: "tok".to_string(),
                identity_class: IdentityClass::Devhost,
                public_key: "cHVi".to_string(),
                host_ssh_public_key: None,
                alias: None,
                platform: None,
                account_label: None,
            }
            .request_id(),
            "id"
        );
        assert_eq!(
            ControlRequest::RequestEnrollmentToken {
                request_id: "id".to_string(),
                identity_class: IdentityClass::Phone,
            }
            .request_id(),
            "id"
        );
        assert_eq!(
            ControlRequest::ListDevhosts { request_id: "id".to_string() }.request_id(),
            "id"
        );
        assert_eq!(
            ControlRequest::ListDevhostSshEndpoints { request_id: "id".to_string() }.request_id(),
            "id"
        );
        assert_eq!(
            ControlRequest::OpenTunnel {
                request_id: "id".to_string(),
                target_device_id: "dev-1".to_string(),
                purpose: "rpc".to_string(),
            }
            .request_id(),
            "id"
        );
        assert_eq!(
            ControlRequest::AgentEvent {
                request_id: "id".to_string(),
                event: WireAgentEvent::TurnCompleted {
                    workspace_id: "ws-1".to_string(),
                    item_id: "item-1".to_string(),
                },
                sequence: Some(1),
            }
            .request_id(),
            "id"
        );
        assert_eq!(
            ControlRequest::RegisterFcmToken { request_id: "id".to_string(), fcm_token: "tok".to_string() }
                .request_id(),
            "id"
        );
        assert_eq!(
            ControlRequest::RevokeDevice { request_id: "id".to_string(), device_id: "dev-1".to_string() }
                .request_id(),
            "id"
        );
        assert_eq!(
            ControlRequest::RevokePhoneSession {
                request_id: "id".to_string(),
                device_id: "phone-1".to_string(),
            }
            .request_id(),
            "id"
        );
    }

    #[test]
    fn enroll_ok_request_enrollment_token_ok_and_list_devhosts_ok_carry_request_id() {
        // The remaining `ControlResponse` variants no other test's
        // `.request_id()` assertion happens to cover.
        assert_eq!(
            ControlResponse::EnrollOk {
                request_id: "id".to_string(),
                device_id: "dev-1".to_string(),
                certificate: "Y2Vy".to_string(),
            }
            .request_id(),
            "id"
        );
        assert_eq!(
            ControlResponse::RequestEnrollmentTokenOk {
                request_id: "id".to_string(),
                token: "tok".to_string(),
                expires_at: "2026-08-14T00:15:00Z".to_string(),
            }
            .request_id(),
            "id"
        );
        assert_eq!(
            ControlResponse::ListDevhostsOk { request_id: "id".to_string(), devhosts: vec![] }.request_id(),
            "id"
        );
    }

    #[test]
    fn error_response_carries_request_id() {
        let response = ControlResponse::Error {
            request_id: "id".to_string(),
            code: "token_invalid".to_string(),
            message: "token expired".to_string(),
        };
        assert_eq!(response.request_id(), "id");
    }

    #[test]
    fn open_tunnel_request_round_trips_through_json() {
        let request = ControlRequest::OpenTunnel {
            request_id: "id".to_string(),
            target_device_id: "dev-1".to_string(),
            purpose: "rpc".to_string(),
        };
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.contains("\"type\":\"open-tunnel\""));
        let decoded: ControlRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, request);
    }

    #[test]
    fn open_tunnel_ok_carries_request_id() {
        let response = ControlResponse::OpenTunnelOk {
            request_id: "id".to_string(),
            tunnel_id: "0011223344556677".to_string(),
        };
        assert_eq!(response.request_id(), "id");
    }

    #[test]
    fn server_push_and_control_response_use_disjoint_type_tags() {
        let push = ServerPush::TunnelOffered {
            tunnel_id: "0011223344556677".to_string(),
            from_device_id: "dev-1".to_string(),
            purpose: "rpc".to_string(),
        };
        let json = serde_json::to_string(&push).expect("serialize");
        // A receiver trying ControlResponse first on this JSON must fail to
        // decode (unknown `type` tag), which is what makes the "try
        // ControlResponse, fall back to ServerPush" dispatch unambiguous.
        assert!(serde_json::from_str::<ControlResponse>(&json).is_err());
        let decoded: ServerPush = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, push);
    }

    #[test]
    fn tunnel_frame_round_trips_including_a_zero_length_close_signal() {
        let id = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];
        for payload in [b"hello".as_slice(), b""] {
            let frame = encode_tunnel_frame(id, payload);
            let (decoded_id, decoded_payload) = decode_tunnel_frame(&frame).expect("decode");
            assert_eq!(decoded_id, id);
            assert_eq!(decoded_payload, payload);
        }
    }

    #[test]
    fn tunnel_frame_decode_rejects_wrong_class_and_short_frames() {
        assert!(decode_tunnel_frame(&[FRAME_CLASS_CONTROL, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
        assert!(decode_tunnel_frame(&[FRAME_CLASS_TUNNEL, 0, 0, 0]).is_none());
        assert!(decode_tunnel_frame(&[]).is_none());
    }

    #[test]
    fn tunnel_id_hex_round_trips_and_rejects_malformed_input() {
        let id = [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];
        let hex = encode_tunnel_id_hex(id);
        assert_eq!(hex, "0011223344556677");
        assert_eq!(decode_tunnel_id_hex(&hex), Some(id));
        assert_eq!(decode_tunnel_id_hex("too-short"), None);
        assert_eq!(decode_tunnel_id_hex("zz11223344556677"), None);
    }

    #[test]
    fn agent_event_request_round_trips_through_json() {
        let request = ControlRequest::AgentEvent {
            request_id: "id".to_string(),
            event: WireAgentEvent::InputRequired {
                workspace_id: "ws-1".to_string(),
                item_id: "item-1".to_string(),
                reason: WireInputReason::Approval,
            },
            sequence: Some(7),
        };
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.contains("\"type\":\"agent-event\""));
        assert!(json.contains("\"kind\":\"input_required\""));
        assert!(json.contains("\"sequence\":7"));
        let decoded: ControlRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, request);
    }

    #[test]
    fn agent_event_request_with_no_sequence_round_trips_through_json() {
        // The `AuthRequired` case: no `workspace_id`, so no sequence.
        let request = ControlRequest::AgentEvent {
            request_id: "id".to_string(),
            event: WireAgentEvent::AuthRequired {
                provider: WireAuthProvider::Github,
                user_code: "WDJB-MJHT".to_string(),
                verification_uri: "https://example.com/device".to_string(),
            },
            sequence: None,
        };
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.contains("\"sequence\":null"));
        let decoded: ControlRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, request);
    }

    #[test]
    fn register_fcm_token_request_round_trips_through_json() {
        let request = ControlRequest::RegisterFcmToken { request_id: "id".to_string(), fcm_token: "tok".to_string() };
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.contains("\"type\":\"register-fcm-token\""));
        let decoded: ControlRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, request);
    }

    #[test]
    fn agent_event_ok_and_register_fcm_token_ok_carry_request_id() {
        assert_eq!(ControlResponse::AgentEventOk { request_id: "id".to_string() }.request_id(), "id");
        assert_eq!(ControlResponse::RegisterFcmTokenOk { request_id: "id".to_string() }.request_id(), "id");
    }

    #[test]
    fn agent_event_push_is_disjoint_from_control_response() {
        let push = ServerPush::AgentEvent {
            from_device_id: "dev-1".to_string(),
            event: WireAgentEvent::TurnCompleted { workspace_id: "ws-1".to_string(), item_id: "item-1".to_string() },
            sequence: Some(3),
        };
        let json = serde_json::to_string(&push).expect("serialize");
        assert!(serde_json::from_str::<ControlResponse>(&json).is_err());
        let decoded: ServerPush = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, push);
    }

    #[test]
    fn wire_agent_event_workspace_id_is_none_only_for_auth_required() {
        assert_eq!(
            WireAgentEvent::TurnCompleted { workspace_id: "ws-1".to_string(), item_id: "item-1".to_string() }.workspace_id(),
            Some("ws-1")
        );
        assert_eq!(
            WireAgentEvent::AuthRequired {
                provider: WireAuthProvider::Aws,
                user_code: "WDJB-MJHT".to_string(),
                verification_uri: "https://example.com/device".to_string(),
            }
            .workspace_id(),
            None
        );
    }

    #[test]
    fn sequenced_agent_event_round_trips_through_json() {
        let event = SequencedAgentEvent {
            sequence: 42,
            event: WireAgentEvent::TurnCompleted { workspace_id: "ws-1".to_string(), item_id: "item-1".to_string() },
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let decoded: SequencedAgentEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, event);
    }

    #[test]
    fn agent_events_resume_request_round_trips_through_json() {
        let request =
            AgentEventsResumeRequest { request_id: "id".to_string(), workspace_id: "ws-1".to_string(), after_sequence: Some(5) };
        let json = serde_json::to_string(&request).expect("serialize");
        let decoded: AgentEventsResumeRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, request);

        let no_prior_ack =
            AgentEventsResumeRequest { request_id: "id".to_string(), workspace_id: "ws-1".to_string(), after_sequence: None };
        let json = serde_json::to_string(&no_prior_ack).expect("serialize");
        assert!(json.contains("\"after_sequence\":null"));
        let decoded: AgentEventsResumeRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, no_prior_ack);
    }

    #[test]
    fn agent_events_resume_response_variants_round_trip_through_json_and_use_disjoint_shapes() {
        let replayed = AgentEventsResumeResponse::Replayed {
            request_id: "id".to_string(),
            events: vec![SequencedAgentEvent {
                sequence: 1,
                event: WireAgentEvent::TurnCompleted { workspace_id: "ws-1".to_string(), item_id: "item-1".to_string() },
            }],
            latest_sequence: 1,
        };
        let json = serde_json::to_string(&replayed).expect("serialize");
        assert!(json.contains("\"outcome\":\"replayed\""));
        let decoded: AgentEventsResumeResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, replayed);

        let snapshot_required =
            AgentEventsResumeResponse::SnapshotRequired { request_id: "id".to_string(), workspace_id: "ws-1".to_string() };
        let json = serde_json::to_string(&snapshot_required).expect("serialize");
        assert!(json.contains("\"outcome\":\"snapshot_required\""));
        let decoded: AgentEventsResumeResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, snapshot_required);
    }

    #[test]
    fn files_changed_and_agent_status_events_round_trip() {
        for event in [
            WireAgentEvent::FilesChanged {
                workspace_id: "ws-1".to_string(),
                item_id: "item-1".to_string(),
                paths: vec!["src/main.rs".to_string()],
            },
            WireAgentEvent::AgentStatus {
                workspace_id: "ws-1".to_string(),
                item_id: "item-1".to_string(),
                status: WireAgentStatus::Busy,
            },
        ] {
            let json = serde_json::to_string(&event).expect("serialize");
            let decoded: WireAgentEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, event);
        }
    }

    #[test]
    fn editor_attached_and_detached_events_round_trip_and_use_expected_wire_shape() {
        for event in [
            WireAgentEvent::EditorAttached { workspace_id: "ws-1".to_string(), editor: WireEditor::Zed },
            WireAgentEvent::EditorDetached { workspace_id: "ws-1".to_string(), editor: WireEditor::Zed },
        ] {
            let json = serde_json::to_string(&event).expect("serialize");
            assert!(json.contains("\"editor\":\"zed\""));
            let decoded: WireAgentEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, event);
        }
        let attached_json = serde_json::to_string(&WireAgentEvent::EditorAttached {
            workspace_id: "ws-1".to_string(),
            editor: WireEditor::Zed,
        })
        .unwrap();
        assert!(attached_json.contains("\"kind\":\"editor_attached\""));
    }

    #[test]
    fn auth_required_event_round_trips_and_uses_the_spec_exact_payload_shape() {
        for provider in [WireAuthProvider::Aws, WireAuthProvider::Gcp, WireAuthProvider::Azure, WireAuthProvider::Github] {
            let event = WireAgentEvent::AuthRequired {
                provider,
                user_code: "WDJB-MJHT".to_string(),
                verification_uri: "https://example.com/device".to_string(),
            };
            let json = serde_json::to_string(&event).expect("serialize");
            let decoded: WireAgentEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, event);
        }
        let json = serde_json::to_string(&WireAgentEvent::AuthRequired {
            provider: WireAuthProvider::Aws,
            user_code: "WDJB-MJHT".to_string(),
            verification_uri: "https://example.com/device".to_string(),
        })
        .unwrap();
        assert!(json.contains("\"kind\":\"auth_required\""));
        assert!(json.contains("\"provider\":\"aws\""));
        assert!(json.contains("\"user_code\":\"WDJB-MJHT\""));
        assert!(json.contains("\"verification_uri\":\"https://example.com/device\""));
        // Per agent-events.md: "No token, credential, or session identifier
        // MUST ever appear in this event" — this is the type-level half of
        // that guarantee: the struct has exactly three fields, so there is
        // no field a future edit could accidentally start populating with
        // captured output. Parsed back as a generic Value to assert the
        // object literally has no other keys, not just that the ones we
        // expect are present.
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let object = value.as_object().unwrap();
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["kind", "provider", "user_code", "verification_uri"]);
    }

    #[test]
    fn list_devhost_ssh_endpoints_request_round_trips_through_json() {
        let request = ControlRequest::ListDevhostSshEndpoints { request_id: "id".to_string() };
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.contains("\"type\":\"list-devhost-ssh-endpoints\""));
        let decoded: ControlRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, request);
    }

    #[test]
    fn list_devhost_ssh_endpoints_ok_round_trips_and_carries_request_id() {
        let response = ControlResponse::ListDevhostSshEndpointsOk {
            request_id: "id".to_string(),
            endpoints: vec![DevhostSshEndpoint {
                device_id: "dev-1".to_string(),
                alias: "build-box".to_string(),
                ssh_host_public_key: "cHVi".to_string(),
            }],
        };
        assert_eq!(response.request_id(), "id");
        let json = serde_json::to_string(&response).expect("serialize");
        let decoded: ControlResponse = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, response);
    }

    #[test]
    fn update_binary_push_round_trips_and_uses_the_spec_exact_snake_case_wire_tag() {
        let push = ServerPush::UpdateBinary {
            push_id: "push-1".to_string(),
            download_url: "https://relay.example/releases/choosh-hostd-linux-x86_64".to_string(),
            sha256: "0011223344556677889900112233445566778899001122334455667788990a".to_string(),
            version: "0.5.0".to_string(),
        };
        let json = serde_json::to_string(&push).expect("serialize");
        // Per relay-protocol.md, this is the one push type spelled with an
        // underscore rather than this enum's usual kebab-case tag.
        assert!(json.contains("\"type\":\"update_binary\""));
        assert!(!json.contains("update-binary"));
        // Disjoint from ControlResponse, same as every other ServerPush variant.
        assert!(serde_json::from_str::<ControlResponse>(&json).is_err());
        let decoded: ServerPush = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, push);
    }

    #[test]
    fn revoke_device_request_round_trips_through_json() {
        let request = ControlRequest::RevokeDevice { request_id: "id".to_string(), device_id: "dev-1".to_string() };
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.contains("\"type\":\"revoke-device\""));
        let decoded: ControlRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, request);
    }

    #[test]
    fn revoke_phone_session_request_round_trips_through_json() {
        let request = ControlRequest::RevokePhoneSession { request_id: "id".to_string(), device_id: "phone-1".to_string() };
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.contains("\"type\":\"revoke-phone-session\""));
        let decoded: ControlRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, request);
    }

    #[test]
    fn revoke_ok_responses_carry_request_id() {
        assert_eq!(
            ControlResponse::RevokeDeviceOk { request_id: "id".to_string(), device_id: "dev-1".to_string() }.request_id(),
            "id"
        );
        assert_eq!(
            ControlResponse::RevokePhoneSessionOk { request_id: "id".to_string(), device_id: "phone-1".to_string() }.request_id(),
            "id"
        );
    }

    #[test]
    fn devhost_presence_round_trips() {
        let presence = DevHostPresence {
            device_id: "dev-1".to_string(),
            alias: "build-box".to_string(),
            platform: "linux".to_string(),
            account_label: Some("aws:123456789012".to_string()),
            connection_state: ConnectionState::Online,
            last_seen: "2026-08-14T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&presence).expect("serialize");
        let decoded: DevHostPresence = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, presence);
    }
}
