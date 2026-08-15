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
use tokio::sync::{RwLock, mpsc, oneshot};

#[derive(Clone, Debug)]
pub struct EnrolledDevice {
    pub public_key: Vec<u8>,
    pub identity_class: IdentityClass,
    pub alias: String,
    pub platform: Option<String>,
    pub account_label: Option<String>,
    pub revoked: bool,
    /// Devhost-only: the loopback SSH server's host public key (raw
    /// 32-byte Ed25519, same encoding as `public_key`), captured once at
    /// enrollment per auth-and-enrollment.md step 6. `None` for
    /// laptop-proxy/phone Identities, or for a devhost enrolled before
    /// this field existed. `choosh-hostd proxy sync`'s
    /// `list-devhost-ssh-endpoints` read (see `ws.rs::dispatch`) skips any
    /// devhost with no key on record rather than emitting a `known_hosts`
    /// line with nothing to verify against.
    pub host_ssh_public_key: Option<Vec<u8>>,
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

/// An authenticated connection that completes no frame at all — control or
/// tunnel — for this long is closed by `relayd`, mirroring
/// [`TUNNEL_IDLE_TIMEOUT_SECONDS`]'s pattern but scoped to the whole
/// connection rather than one tunnel. Covers relayd-threat-model.md Case 5's
/// "stalled mid-frame" gap: previously a connection that authenticated and
/// then sent nothing further (not even a truncated frame) was never reaped.
/// Set well above `TUNNEL_IDLE_TIMEOUT_SECONDS` rather than reusing it
/// outright: an authenticated devhost/laptop-proxy with no open tunnels and
/// no agent activity right now is a normal, legitimate state (this protocol
/// has no periodic application-level heartbeat), so this bound only needs to
/// reclaim connections that are *never* going to do anything again, not
/// penalize ordinary quiet periods.
pub const CONNECTION_IDLE_TIMEOUT_SECONDS: u64 = 30 * 60;

/// Per-Identity control-frame rate limit (a token bucket), per
/// relay-protocol.md's "Errors and backpressure": control-frame requests
/// arriving faster than `relayd` can process "MUST be rate limited
/// per-Identity; exceeding the limit MUST close the connection". Applied to
/// every dispatched control frame, including `open-tunnel` — the most
/// expensive request in the catalog (allocates a `Tunnel` registry entry and
/// pushes a `tunnel-offered` frame to the target).
///
/// Sized generously above any legitimate control-plane workload this
/// codebase exercises today — `list-devhosts` polling, an occasional burst
/// of `open-tunnel` calls, or a stream of `agent-event`s from a busy agent
/// session are all well under one request per 50ms sustained — while still
/// bounding a runaway client loop or flood to a small, fixed multiple of
/// ordinary traffic before `relayd` disconnects it. A rejected request is
/// cheap for `relayd` to reject (checked before any registry/tunnel work
/// runs), so the cost of an occasional false-positive disconnect
/// (reconnect + re-authenticate, per relay-protocol.md's backoff) is far
/// lower than the cost of leaving this unbounded.
pub const CONTROL_FRAME_RATE_LIMIT_BURST: u32 = 40;
pub const CONTROL_FRAME_RATE_LIMIT_PER_SECOND: u32 = 20;

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
    /// `device_id -> kill switch` for every currently-authenticated
    /// connection, alongside its entry in [`Self::connections`]. Firing this
    /// (send, or just drop to close the channel) tells that connection's own
    /// `serve_authenticated_loop` to stop serving and close the socket —
    /// this is what makes an *already-connected* device's revocation take
    /// effect immediately, rather than only on its next reconnect attempt.
    /// A one-shot rather than reusing `connections`' `Vec<u8>` channel: a
    /// kill is a control signal to the connection's own task, not a wire
    /// message to relay to the client.
    pub kill_switches: RwLock<HashMap<String, oneshot::Sender<()>>>,
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
