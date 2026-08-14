//! In-memory presence and identity registry. No disk persistence beyond the
//! enrollment CA key itself ([`crate::ca`]) — a `relayd` restart currently
//! forgets enrolled devices, issued tokens, and phone sessions. That's a
//! deliberate M0 gap (see the crate-level report), not an oversight: it
//! keeps this increment's surface area to the protocol behavior the M0
//! exit criteria actually test, and every field needed to add persistence
//! later is already named and typed here.

use choosh_protocol::relay::{IdentityClass, TUNNEL_ID_BYTES};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{RwLock, mpsc};

#[derive(Clone, Debug)]
pub struct EnrolledDevice {
    pub public_key: Vec<u8>,
    pub identity_class: IdentityClass,
    pub alias: String,
    pub platform: Option<String>,
    pub account_label: Option<String>,
    pub revoked: bool,
}

#[derive(Clone, Debug)]
pub struct EnrollmentToken {
    pub identity_class: IdentityClass,
    pub expires_at_unix: u64,
    pub consumed: bool,
}

#[derive(Clone, Debug)]
pub struct PhoneSession {
    /// One per registered passkey (auth-and-enrollment.md: "may hold
    /// multiple passkey credentials, one per enrolled phone/browser"), so
    /// each session's `device_id` identifies which enrolled phone/browser
    /// this is, distinct from the single underlying user.
    pub device_id: String,
    pub expires_at_unix: u64,
}

/// A live connection's outbound byte channel: anything sent here is written
/// to that Identity's WebSocket as one `Message::Binary`, interleaved with
/// whatever that connection's own read loop is already sending (control
/// responses, its own outgoing tunnel frames). Bounded per
/// relay-protocol.md's backpressure requirement — see [`OUTBOUND_CHANNEL_CAPACITY`].
pub type OutboundSender = mpsc::Sender<Vec<u8>>;

/// One in-flight tunnel, keyed by its 8-byte ID. Both ends are named
/// explicitly (not "the two connections", since a device can be the
/// requester of one tunnel and the target of another simultaneously, and
/// this struct alone must be enough to know which connection to forward an
/// incoming frame *to* given which one it arrived *from*).
#[derive(Clone, Debug)]
pub struct Tunnel {
    pub requester_device_id: String,
    pub target_device_id: String,
    pub purpose: String,
    pub last_activity_unix: u64,
}

impl Tunnel {
    /// The device ID an incoming frame should be forwarded to, given which
    /// device it arrived from — `None` if `from_device_id` is neither party
    /// (a bug in the caller, not a protocol condition).
    #[must_use]
    pub fn other_party(&self, from_device_id: &str) -> Option<&str> {
        if from_device_id == self.requester_device_id {
            Some(&self.target_device_id)
        } else if from_device_id == self.target_device_id {
            Some(&self.requester_device_id)
        } else {
            None
        }
    }
}

/// Bound on a connection's outbound-frame backlog before `relayd` closes
/// tunnels routing traffic to it rather than buffering unboundedly, per
/// relay-protocol.md's backpressure requirement. Chosen generously relative
/// to `MAX_TUNNEL_FRAME_BYTES` (256 KiB): a fully backed-up connection would
/// need to be roughly this many frames behind before this triggers, which
/// in practice only happens when the reader has stopped entirely (a stuck
/// or dead client), not from ordinary jitter.
pub const OUTBOUND_CHANNEL_CAPACITY: usize = 256;

/// A tunnel with no data frames in either direction for this long is closed
/// by `relayd`, per relay-protocol.md's tunnel lifecycle.
pub const TUNNEL_IDLE_TIMEOUT_SECONDS: u64 = 300;

#[derive(Default)]
pub struct Registry {
    pub devices: RwLock<HashMap<String, EnrolledDevice>>,
    pub tokens: RwLock<HashMap<String, EnrollmentToken>>,
    pub phone_sessions: RwLock<HashMap<String, PhoneSession>>,
    /// `device_id -> last_seen_unix`, for online devices only. Absence
    /// means offline; there is no partial/stale-but-present state, per
    /// relay-protocol.md's presence semantics.
    pub online_devices: RwLock<HashMap<String, u64>>,
    /// `device_id -> outbound sender` for every currently-authenticated
    /// connection, however it authenticated (phone session or device
    /// certificate) — this is what makes routing a tunnel frame or a
    /// `ServerPush` to an arbitrary other Identity possible from within a
    /// different connection's task.
    pub connections: RwLock<HashMap<String, OutboundSender>>,
    pub tunnels: RwLock<HashMap<[u8; TUNNEL_ID_BYTES], Tunnel>>,
    /// `phone device_id -> FCM registration token`, per relay-protocol.md's
    /// `register-fcm-token` ("at most one FCM token per phone Identity").
    /// No disk persistence yet — same gap already noted for `devices`/
    /// `tokens`/`phone_sessions`, a `relayd` restart forgets these too.
    pub fcm_tokens: RwLock<HashMap<String, String>>,
}

impl Registry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Session credentials are valid 90 days, silently renewed on use, per
/// auth-and-enrollment.md.
pub const SESSION_CREDENTIAL_VALIDITY_SECONDS: u64 = 90 * 24 * 60 * 60;
/// Enrollment tokens are single-use and 15 minutes, per
/// auth-and-enrollment.md.
pub const ENROLLMENT_TOKEN_VALIDITY_SECONDS: u64 = 15 * 60;

#[must_use]
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs()
}
