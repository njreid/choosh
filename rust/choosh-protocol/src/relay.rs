//! Control-frame wire types for the `choosh-relayd` protocol.
//!
//! Mirrors `docs/specs/relay-protocol.md` and `docs/specs/auth-and-enrollment.md`
//! exactly: frame classing, the connect-time challenge/credential handshake, and
//! the M0 control-frame catalog (enrollment, presence). Shared by `choosh-relayd`
//! and `choosh-hostd` so both sides serialize/deserialize identical Rust types
//! rather than independently-drifting JSON shapes.

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
    },
    /// Phone-only. Replaces any previously registered token for this phone
    /// Identity — `relayd` holds at most one FCM token per phone Identity.
    RegisterFcmToken {
        request_id: String,
        fcm_token: String,
    },
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
    Error {
        request_id: String,
        code: String,
        message: String,
    },
}

impl ControlResponse {
    #[must_use]
    pub fn request_id(&self) -> &str {
        match self {
            Self::EnrollOk { request_id, .. }
            | Self::RequestEnrollmentTokenOk { request_id, .. }
            | Self::ListDevhostsOk { request_id, .. }
            | Self::ListDevhostSshEndpointsOk { request_id, .. }
            | Self::OpenTunnelOk { request_id, .. }
            | Self::AgentEventOk { request_id, .. }
            | Self::RegisterFcmTokenOk { request_id, .. }
            | Self::Error { request_id, .. } => request_id,
        }
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
        };
        let json = serde_json::to_string(&request).expect("serialize");
        assert!(json.contains("\"type\":\"agent-event\""));
        assert!(json.contains("\"kind\":\"input_required\""));
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
        };
        let json = serde_json::to_string(&push).expect("serialize");
        assert!(serde_json::from_str::<ControlResponse>(&json).is_err());
        let decoded: ServerPush = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, push);
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
