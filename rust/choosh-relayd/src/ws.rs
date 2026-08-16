//! The `/connect` `WebSocket` endpoint: the `ServerHello`/`ClientAuth`
//! handshake, unauthenticated `enroll`, and the full authenticated
//! control-frame catalog `dispatch` below serves — `request-enrollment-token`/
//! `list-devhosts` (M0), `open-tunnel` and its `0x02` tunnel-frame routing
//! (M1), `agent-event`/`register-fcm-token` (M2), `list-devhost-ssh-endpoints`
//! (M6), the `offload`-purpose tunnel capability (M7), and — closing three
//! gaps named in `docs/security/relayd-threat-model.md`'s M8 review —
//! phone-only `revoke-device`/`revoke-phone-session` control frames (each
//! closing any live connection immediately, not just blocking its next
//! reconnect), a per-Identity control-frame rate limit ([`RateLimiter`]),
//! and a connection-wide idle timeout for a connection stalled mid-frame
//! (`AppState::connection_idle_timeout`, alongside [`reap_idle_tunnels`]'s
//! existing per-tunnel one). See `docs/specs/relay-protocol.md` and
//! `docs/specs/auth-and-enrollment.md`.

use crate::AppState;
use crate::ca;
use crate::state::{
    CONTROL_FRAME_RATE_LIMIT_BURST, CONTROL_FRAME_RATE_LIMIT_PER_SECOND,
    ENROLLMENT_TOKEN_VALIDITY_SECONDS, EnrolledDevice, EnrollmentToken, OUTBOUND_CHANNEL_CAPACITY,
    Registry, SESSION_CREDENTIAL_VALIDITY_SECONDS, TUNNEL_IDLE_TIMEOUT_SECONDS, Tunnel, now_unix,
};
use crate::wire::{decode_control, encode_control, new_decoder};
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as base64_engine;
use choosh_protocol::relay::{
    AuthFailed, AuthOk, AuthResult, ClientAuth, ConnectionState, ControlRequest, ControlResponse,
    DevHostPresence, FRAME_CLASS_CONTROL, FRAME_CLASS_TUNNEL, IdentityClass, ServerHello,
    ServerPush, TUNNEL_ID_BYTES, WireAgentEvent, decode_tunnel_frame, encode_tunnel_frame,
    encode_tunnel_id_hex,
};
use ed25519_dalek::{Signature, VerifyingKey, ed25519::signature::Verifier};
use futures_util::SinkExt as _;
use getrandom::rand_core::Rng as _;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub async fn connect(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_connection(socket, state))
}

/// Authenticated Identity established for one live connection.
struct Authenticated {
    device_id: String,
    identity_class: IdentityClass,
}

async fn handle_connection(mut socket: WebSocket, state: Arc<AppState>) {
    let mut decoder = new_decoder();

    let Some(nonce) = send_hello(&mut socket).await else {
        return;
    };

    let Some(first_frame) = next_control_frame(&mut socket, &mut decoder).await else {
        return;
    };

    // The first frame is either a reconnecting Identity's `ClientAuth`, or
    // an unauthenticated `enroll` from a device that has no credential yet.
    // These are distinguished by trying `ClientAuth` first (tag `"kind"`)
    // and falling back to `ControlRequest::Enroll` (tag `"type": "enroll"`)
    // — the two shapes never collide because they use different tag
    // fields, so this is an unambiguous dispatch, not a heuristic guess.
    if let Ok(client_auth) = decode_control::<ClientAuth>(&first_frame) {
        let Some(authenticated) = authenticate(&state, &nonce, client_auth, &mut socket).await
        else {
            return;
        };
        mark_online(&state.registry, &authenticated.device_id).await;
        serve_authenticated(&mut socket, &mut decoder, &state, &authenticated).await;
        mark_offline(&state.registry, &authenticated.device_id).await;
        return;
    }

    match decode_control::<ControlRequest>(&first_frame) {
        Ok(request @ ControlRequest::Enroll { .. }) => {
            handle_enroll(&state, request, &mut socket).await;
            // Enrollment is a one-shot exchange: the newly enrolled device
            // reconnects using its new certificate through the normal
            // ServerHello/ClientAuth path rather than continuing this
            // connection as authenticated. This keeps "authenticated" a
            // single, unambiguous state entered only one way.
        }
        _ => {
            // Neither a recognized ClientAuth nor an enroll request: per
            // relay-protocol.md, an unauthenticated connection accepts
            // nothing else.
            let _ = socket.close().await;
        }
    }
}

async fn send_hello(socket: &mut WebSocket) -> Option<String> {
    let mut nonce_bytes = [0u8; 32];
    crate::rng::os_rng().fill_bytes(&mut nonce_bytes);
    let nonce = base64_engine.encode(nonce_bytes);
    let hello = ServerHello { nonce: nonce.clone() };
    let frame = match encode_control(&hello) {
        Ok(frame) => frame,
        Err(err) => {
            tracing::warn!(?err, "failed to encode ServerHello");
            return None;
        }
    };
    if socket.send(Message::Binary(frame.into())).await.is_err() {
        return None;
    }
    Some(nonce)
}

/// Reads WS messages until exactly one complete control frame has been
/// decoded, or the connection closes/violates framing — matching
/// relay-protocol.md's "malformed frame terminates the connection, no
/// partial recovery" rule.
async fn next_control_frame(
    socket: &mut WebSocket,
    decoder: &mut choosh_protocol::framing::FrameDecoder,
) -> Option<Vec<u8>> {
    loop {
        let message = socket.recv().await?.ok()?;
        let bytes = match message {
            Message::Binary(bytes) => bytes,
            Message::Close(_) => return None,
            // Non-binary application data is not part of this protocol.
            _ => continue,
        };
        let frames = decoder.feed(&bytes).ok()?;
        if let Some(frame) = frames.into_iter().next() {
            return Some(frame);
        }
    }
}

async fn authenticate(
    state: &AppState,
    nonce: &str,
    client_auth: ClientAuth,
    socket: &mut WebSocket,
) -> Option<Authenticated> {
    let result = match client_auth {
        ClientAuth::Device(device_auth) => authenticate_device(state, nonce, &device_auth).await,
        ClientAuth::Phone(phone_auth) => authenticate_phone(state, &phone_auth).await,
    };
    let (outcome, authenticated) = match result {
        Ok(authenticated) => (
            AuthResult::Ok(AuthOk {
                identity_class: authenticated.identity_class,
                device_id: authenticated.device_id.clone(),
            }),
            Some(authenticated),
        ),
        Err(reason) => (AuthResult::Failed(AuthFailed { reason }), None),
    };
    match encode_control(&outcome) {
        Ok(frame) => {
            let _ = socket.send(Message::Binary(frame.into())).await;
        }
        Err(err) => tracing::warn!(?err, "failed to encode AuthResult"),
    }
    if authenticated.is_none() {
        let _ = socket.close().await;
    }
    authenticated
}

async fn authenticate_device(
    state: &AppState,
    nonce: &str,
    device_auth: &choosh_protocol::relay::DeviceAuth,
) -> Result<Authenticated, String> {
    let (cert_device_id, cert_public_key) =
        ca::verify(&state.ca_key.verifying_key(), &device_auth.certificate, now_unix())
            .map_err(|_| "certificate invalid or expired".to_string())?;
    if cert_device_id != device_auth.device_id {
        return Err("certificate does not match presented device_id".to_string());
    }

    let devices = state.registry.devices.read().await;
    let entry = devices
        .get(&device_auth.device_id)
        .ok_or_else(|| "unknown device".to_string())?;
    if entry.revoked {
        return Err("device revoked".to_string());
    }
    if entry.public_key != cert_public_key {
        return Err("certificate public key does not match enrollment record".to_string());
    }

    let verifying_key = VerifyingKey::from_bytes(
        cert_public_key
            .as_slice()
            .try_into()
            .map_err(|_| "malformed public key".to_string())?,
    )
    .map_err(|_| "malformed public key".to_string())?;
    let signature_bytes: [u8; 64] = base64_engine
        .decode(&device_auth.signature)
        .map_err(|_| "malformed signature".to_string())?
        .try_into()
        .map_err(|_| "malformed signature".to_string())?;
    let signature = Signature::from_bytes(&signature_bytes);
    verifying_key
        .verify(nonce.as_bytes(), &signature)
        .map_err(|_| "signature does not verify".to_string())?;

    Ok(Authenticated {
        // Sourced from `cert_device_id` (the CA-verified certificate body),
        // not `device_auth.device_id` (the caller's presented claim) —
        // they're provably equal by the check above, but binding the
        // authenticated Identity to the verified value directly means a
        // future edit that weakens or removes that check can't silently
        // reintroduce a claim-not-derived-from-the-certificate identity bug.
        device_id: cert_device_id,
        identity_class: entry.identity_class,
    })
}

async fn authenticate_phone(
    state: &AppState,
    phone_auth: &choosh_protocol::relay::PhoneAuth,
) -> Result<Authenticated, String> {
    let mut sessions = state.registry.phone_sessions.write().await;
    let session = sessions
        .get_mut(&phone_auth.session_credential)
        .ok_or_else(|| "unknown or revoked session credential".to_string())?;
    if now_unix() > session.expires_at_unix {
        sessions.remove(&phone_auth.session_credential);
        return Err("session credential expired".to_string());
    }
    // Silently renewed on use, per auth-and-enrollment.md.
    session.expires_at_unix = now_unix() + SESSION_CREDENTIAL_VALIDITY_SECONDS;
    Ok(Authenticated {
        device_id: session.device_id.clone(),
        identity_class: IdentityClass::Phone,
    })
}

async fn mark_online(registry: &Registry, device_id: &str) {
    registry
        .online_devices
        .write()
        .await
        .insert(device_id.to_string(), now_unix());
}

async fn mark_offline(registry: &Registry, device_id: &str) {
    registry.online_devices.write().await.remove(device_id);
}

/// Registers this connection's outbound channel (so other connections'
/// tasks can route tunnel frames and pushes to it) and kill switch (so a
/// revoke elsewhere can force this connection closed — see
/// `Registry::kill_switches`), runs the read/route loop to completion, then
/// unconditionally deregisters both and closes every tunnel this device was
/// party to — the "finally" this fans out to on every exit path of
/// [`serve_authenticated_loop`], including a socket error, a malformed
/// frame, a revoke, an idle timeout, or the caller's own eventual
/// disconnect.
async fn serve_authenticated(
    socket: &mut WebSocket,
    decoder: &mut choosh_protocol::framing::FrameDecoder,
    state: &AppState,
    authenticated: &Authenticated,
) {
    let (outbound_tx, mut outbound_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_CHANNEL_CAPACITY);
    let (kill_tx, kill_rx) = oneshot::channel::<()>();
    state.registry.connections.write().await.insert(authenticated.device_id.clone(), outbound_tx);
    state.registry.kill_switches.write().await.insert(authenticated.device_id.clone(), kill_tx);

    serve_authenticated_loop(socket, decoder, state, authenticated, &mut outbound_rx, kill_rx).await;

    state.registry.connections.write().await.remove(&authenticated.device_id);
    state.registry.kill_switches.write().await.remove(&authenticated.device_id);
    close_tunnels_for_device(state, &authenticated.device_id).await;
}

async fn serve_authenticated_loop(
    socket: &mut WebSocket,
    decoder: &mut choosh_protocol::framing::FrameDecoder,
    state: &AppState,
    authenticated: &Authenticated,
    outbound_rx: &mut mpsc::Receiver<Vec<u8>>,
    mut kill_rx: oneshot::Receiver<()>,
) {
    let mut rate_limiter =
        RateLimiter::new(CONTROL_FRAME_RATE_LIMIT_BURST, CONTROL_FRAME_RATE_LIMIT_PER_SECOND);
    // Tracks the most recent *complete* frame (control or tunnel), not raw
    // bytes received — a connection trickling partial bytes forever without
    // ever completing one is exactly relayd-threat-model.md Case 5's
    // "stalled mid-frame" gap this timeout closes, so raw byte receipt must
    // not reset it.
    let mut last_frame_at = std::time::Instant::now();
    loop {
        let idle_remaining = state.connection_idle_timeout.saturating_sub(last_frame_at.elapsed());
        tokio::select! {
            () = tokio::time::sleep(idle_remaining) => {
                tracing::debug!(device_id = %authenticated.device_id, "closing connection idle since its last complete frame");
                let _ = socket.close().await;
                return;
            }
            _ = &mut kill_rx => {
                tracing::debug!(device_id = %authenticated.device_id, "closing connection: killed by a revoke");
                let _ = socket.close().await;
                return;
            }
            message = socket.recv() => {
                let Some(message) = message else { return };
                let Ok(message) = message else { return };
                let bytes = match message {
                    Message::Binary(bytes) => bytes,
                    Message::Close(_) => return,
                    _ => continue,
                };
                let Ok(frames) = decoder.feed(&bytes) else {
                    let _ = socket.close().await;
                    return;
                };
                if !frames.is_empty() {
                    last_frame_at = std::time::Instant::now();
                }
                for frame in frames {
                    registry_touch(&state.registry, &authenticated.device_id).await;
                    match frame.first().copied() {
                        Some(FRAME_CLASS_CONTROL) => {
                            if !handle_control_frame(socket, state, authenticated, &frame, &mut rate_limiter).await {
                                return;
                            }
                        }
                        Some(FRAME_CLASS_TUNNEL) => {
                            // The shared ingress decoder is sized to the
                            // larger control-frame cap (`new_decoder`'s
                            // `control_frame_limits`), since one decoder
                            // serves both frame classes for the connection's
                            // whole lifetime. That means a tunnel frame
                            // between `MAX_TUNNEL_FRAME_BYTES` and
                            // `MAX_CONTROL_FRAME_BYTES` would otherwise pass
                            // decoding uncaught. relay-protocol.md requires
                            // `relayd` to terminate the connection on "a
                            // frame exceeding its class's size cap" — this
                            // check enforces that cap explicitly for the
                            // tunnel class, which the shared decoder alone
                            // cannot.
                            if frame.len()
                                > 1 + TUNNEL_ID_BYTES + choosh_protocol::relay::MAX_TUNNEL_FRAME_BYTES
                            {
                                let _ = socket.close().await;
                                return;
                            }
                            handle_tunnel_frame(state, &authenticated.device_id, &frame).await;
                        }
                        // Any other class byte (or an empty frame, which
                        // `split_first` in the decoders below would also
                        // reject) — per relay-protocol.md, an unrecognized
                        // frame class terminates the connection, no partial
                        // recovery.
                        _ => {
                            let _ = socket.close().await;
                            return;
                        }
                    }
                }
            }
            Some(bytes) = outbound_rx.recv() => {
                if socket.send(Message::Binary(bytes.into())).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Per-connection control-frame rate limiter — see
/// `state::CONTROL_FRAME_RATE_LIMIT_BURST`/`CONTROL_FRAME_RATE_LIMIT_PER_SECOND`
/// for the bound and its reasoning. A token bucket: starts full, drains one
/// token per accepted control frame, refills continuously over real elapsed
/// time. Kept as connection-local state (constructed once per
/// `serve_authenticated_loop` call) rather than threaded through
/// `Registry`: relay-protocol.md's "exactly one persistent connection per
/// Identity" means per-connection state already *is* per-Identity state, so
/// no cross-task sharing or locking is needed to enforce this per-Identity.
struct RateLimiter {
    tokens: f64,
    capacity: f64,
    refill_per_second: f64,
    last_refill: std::time::Instant,
}

impl RateLimiter {
    fn new(capacity: u32, refill_per_second: u32) -> Self {
        Self {
            tokens: f64::from(capacity),
            capacity: f64::from(capacity),
            refill_per_second: f64::from(refill_per_second),
            last_refill: std::time::Instant::now(),
        }
    }

    /// Refills against the real clock, then attempts to take one token.
    fn try_acquire(&mut self) -> bool {
        let now = std::time::Instant::now();
        let elapsed = now.saturating_duration_since(self.last_refill);
        self.last_refill = now;
        self.try_acquire_after(elapsed)
    }

    /// The pure boundary decision [`Self::try_acquire`] delegates to, split
    /// out so it's unit-testable without waiting on the real clock — the
    /// same discipline [`expired_tunnel_ids`] uses for [`reap_idle_tunnels`].
    fn try_acquire_after(&mut self, elapsed: Duration) -> bool {
        self.tokens = (self.tokens + elapsed.as_secs_f64() * self.refill_per_second).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Decodes and dispatches one class-`0x01` frame, sending its response
/// directly to `socket`. Returns `false` if the connection was closed
/// (already done inside) and the caller must stop serving it.
async fn handle_control_frame(
    socket: &mut WebSocket,
    state: &AppState,
    authenticated: &Authenticated,
    frame: &[u8],
    rate_limiter: &mut RateLimiter,
) -> bool {
    let response = match decode_control::<ControlRequest>(frame) {
        Ok(request) if !rate_limiter.try_acquire() => {
            // relay-protocol.md's "Errors and backpressure": exceeding the
            // per-Identity control-frame rate "MUST close the connection
            // rather than silently dropping requests, so a client can
            // detect and back off". The error frame is sent best-effort
            // before closing — a client that's still reading gets an
            // explicit, typed reason rather than an unexplained close.
            tracing::debug!(device_id = %authenticated.device_id, "closing connection: control-frame rate limit exceeded");
            let error = ControlResponse::Error {
                request_id: request.request_id().to_string(),
                code: "rate_limited".to_string(),
                message: "control-frame rate limit exceeded".to_string(),
            };
            if let Ok(wire) = encode_control(&error) {
                let _ = socket.send(Message::Binary(wire.into())).await;
            }
            let _ = socket.close().await;
            return false;
        }
        Ok(request) => dispatch(state, authenticated, request).await,
        Err(err) => {
            // Covers unrecognized `type` values (and anything else that
            // fails to decode as `ControlRequest`) — correctly treated as a
            // malformed frame per relay-protocol.md: terminate the
            // connection, no partial recovery.
            tracing::debug!(?err, device_id = %authenticated.device_id, "closing connection on malformed control frame");
            let _ = socket.close().await;
            return false;
        }
    };
    let wire = match encode_control(&response) {
        Ok(wire) => wire,
        Err(err) => {
            tracing::warn!(?err, "failed to encode ControlResponse");
            let _ = socket.close().await;
            return false;
        }
    };
    if socket.send(Message::Binary(wire.into())).await.is_err() {
        return false;
    }
    true
}

/// Routes one class-`0x02` frame per relay-protocol.md's tunnel lifecycle.
/// An unknown/already-closed tunnel ID, or a frame from a device that isn't
/// actually a party to the named tunnel, is dropped silently — the spec
/// defines no error path back to the sender for either case, and inventing
/// one risks disagreeing with what a client implementation assumes.
async fn handle_tunnel_frame(state: &AppState, from_device_id: &str, frame: &[u8]) {
    let Some((tunnel_id, payload)) = decode_tunnel_frame(frame) else { return };

    let tunnel = state.registry.tunnels.read().await.get(&tunnel_id).cloned();
    let Some(tunnel) = tunnel else { return };
    let Some(other_party) = tunnel.other_party(from_device_id).map(str::to_string) else {
        return;
    };

    if payload.is_empty() {
        // A zero-payload frame is a close signal: tear down and relay the
        // same signal to the other side so it observes the closure too.
        state.registry.tunnels.write().await.remove(&tunnel_id);
        forward_close(state, &other_party, tunnel_id).await;
        return;
    }

    if let Some(entry) = state.registry.tunnels.write().await.get_mut(&tunnel_id) {
        entry.last_activity_unix = now_unix();
    }

    if !forward_data(state, &other_party, tunnel_id, payload).await {
        // The other party is gone, or its outbound queue is full
        // (backpressure): close the tunnel entirely rather than buffering
        // unboundedly, per relay-protocol.md, and let both sides observe
        // it — including the sender, so it stops writing into a dead tunnel
        // instead of silently blackholing future frames.
        state.registry.tunnels.write().await.remove(&tunnel_id);
        forward_close(state, &tunnel.requester_device_id, tunnel_id).await;
        forward_close(state, &tunnel.target_device_id, tunnel_id).await;
    }
}

/// Best-effort, non-blocking: `try_send`s a data frame to `to_device_id`'s
/// connection. Returns `false` if that connection doesn't exist or its
/// outbound queue is full — either way the caller treats this as
/// "the tunnel cannot be routed" and closes it.
async fn forward_data(
    state: &AppState,
    to_device_id: &str,
    tunnel_id: [u8; TUNNEL_ID_BYTES],
    payload: &[u8],
) -> bool {
    let Some(wire) = wire_tunnel_frame(tunnel_id, payload) else { return false };
    let connections = state.registry.connections.read().await;
    let Some(sender) = connections.get(to_device_id) else { return false };
    sender.try_send(wire).is_ok()
}

/// Wraps a tunnel frame's class-byte-plus-ID-plus-payload body with the
/// outer 4-byte length prefix every frame on the wire needs — the
/// counterpart to [`encode_control`], which already does this for control
/// frames. Anything pushed into a connection's outbound channel MUST go
/// through one of these two, never the raw `choosh_protocol::relay`
/// encode helpers directly, or the receiver's `FrameDecoder` misparses the
/// frame's own bytes as a bogus length header.
fn wire_tunnel_frame(tunnel_id: [u8; TUNNEL_ID_BYTES], payload: &[u8]) -> Option<Vec<u8>> {
    choosh_protocol::framing::encode_frame(
        &encode_tunnel_frame(tunnel_id, payload),
        choosh_protocol::relay::MAX_TUNNEL_FRAME_BYTES,
    )
    .ok()
}

/// Best-effort, non-blocking: relays a zero-payload close signal to
/// `to_device_id` if it's still connected. Never surfaces a failure — the
/// recipient not getting this is equivalent to it discovering the closure
/// on its own next send against the now-removed tunnel ID.
async fn forward_close(state: &AppState, to_device_id: &str, tunnel_id: [u8; TUNNEL_ID_BYTES]) {
    let Some(wire) = wire_tunnel_frame(tunnel_id, &[]) else { return };
    let connections = state.registry.connections.read().await;
    if let Some(sender) = connections.get(to_device_id) {
        let _ = sender.try_send(wire);
    }
}

/// Closes every tunnel where `device_id` was either party and notifies the
/// survivor, per relay-protocol.md's reconnect-discontinuity rule: a
/// dropped connection implicitly closes its tunnels rather than leaving
/// them to be spliced onto a future reconnect.
async fn close_tunnels_for_device(state: &AppState, device_id: &str) {
    let to_notify: Vec<([u8; TUNNEL_ID_BYTES], String)> = {
        let mut tunnels = state.registry.tunnels.write().await;
        let ids: Vec<[u8; TUNNEL_ID_BYTES]> = tunnels
            .iter()
            .filter(|(_, tunnel)| {
                tunnel.requester_device_id == device_id || tunnel.target_device_id == device_id
            })
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter_map(|id| {
                let tunnel = tunnels.remove(&id)?;
                tunnel.other_party(device_id).map(|survivor| (id, survivor.to_string()))
            })
            .collect()
    };
    for (tunnel_id, survivor) in to_notify {
        forward_close(state, &survivor, tunnel_id).await;
    }
}

/// The ids of every tunnel whose `last_activity_unix` is at least
/// `TUNNEL_IDLE_TIMEOUT_SECONDS` behind `now` — the pure boundary decision
/// [`reap_idle_tunnels`] acts on, split out so it's unit-testable without
/// waiting on either the real system clock or that function's own 30-second
/// tick interval.
fn expired_tunnel_ids(
    tunnels: &std::collections::HashMap<[u8; TUNNEL_ID_BYTES], Tunnel>,
    now: u64,
) -> Vec<[u8; TUNNEL_ID_BYTES]> {
    tunnels
        .iter()
        .filter(|(_, tunnel)| now.saturating_sub(tunnel.last_activity_unix) >= TUNNEL_IDLE_TIMEOUT_SECONDS)
        .map(|(id, _)| *id)
        .collect()
}

/// Runs for the lifetime of the process: periodically closes tunnels with
/// no data frames in either direction for `TUNNEL_IDLE_TIMEOUT_SECONDS`,
/// per relay-protocol.md's tunnel lifecycle, notifying both parties.
pub async fn reap_idle_tunnels(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        interval.tick().await;
        let expired: Vec<([u8; TUNNEL_ID_BYTES], Tunnel)> = {
            let mut tunnels = state.registry.tunnels.write().await;
            let ids = expired_tunnel_ids(&tunnels, now_unix());
            ids.into_iter().filter_map(|id| tunnels.remove(&id).map(|tunnel| (id, tunnel))).collect()
        };
        for (tunnel_id, tunnel) in expired {
            forward_close(&state, &tunnel.requester_device_id, tunnel_id).await;
            forward_close(&state, &tunnel.target_device_id, tunnel_id).await;
        }
    }
}

fn generate_tunnel_id() -> [u8; TUNNEL_ID_BYTES] {
    let mut id = [0u8; TUNNEL_ID_BYTES];
    crate::rng::os_rng().fill_bytes(&mut id);
    id
}

/// Capability-scope enforcement per auth-and-enrollment.md's Identity-class
/// table: every target must be a known, non-revoked `devhost` (a `phone`,
/// `laptop-proxy`, or another `devhost`'s tunnels are never routed to a
/// non-devhost target in this system), and `laptop-proxy`/`devhost`
/// requesters are further restricted to one specific `purpose` each.
async fn check_open_tunnel_permitted(
    state: &AppState,
    requester: &Authenticated,
    target_device_id: &str,
    purpose: &str,
) -> Result<(), &'static str> {
    match requester.identity_class {
        IdentityClass::Phone => {}
        IdentityClass::LaptopProxy if purpose == "ssh" => {}
        IdentityClass::Devhost if purpose == "offload" => {}
        IdentityClass::LaptopProxy | IdentityClass::Devhost => return Err("not_permitted"),
    }
    let devices = state.registry.devices.read().await;
    match devices.get(target_device_id) {
        Some(device) if device.identity_class == IdentityClass::Devhost && !device.revoked => Ok(()),
        _ => Err("not_permitted"),
    }
}

async fn registry_touch(registry: &Registry, device_id: &str) {
    if let Some(last_seen) = registry.online_devices.write().await.get_mut(device_id) {
        *last_seen = now_unix();
    }
}

fn not_permitted(request_id: &str, method: &str) -> ControlResponse {
    ControlResponse::Error {
        request_id: request_id.to_string(),
        code: "not_permitted".to_string(),
        message: format!("{method} is not permitted for this Identity class"),
    }
}

/// The `if authenticated.identity_class != want { return not_permitted(...) }`
/// idiom every single-Identity-class capability in [`dispatch`] (and its
/// split-out `handle_*`/`list_*` helpers) starts with, factored out so each
/// site is a one-line `?`-early-return instead of a repeated four-line `if`.
fn require_identity_class(
    authenticated: &Authenticated,
    want: IdentityClass,
    method: &str,
    request_id: &str,
) -> Result<(), ControlResponse> {
    if authenticated.identity_class != want {
        return Err(not_permitted(request_id, method));
    }
    Ok(())
}

async fn dispatch(
    state: &AppState,
    authenticated: &Authenticated,
    request: ControlRequest,
) -> ControlResponse {
    match request {
        ControlRequest::RequestEnrollmentToken { request_id, identity_class } => {
            if let Err(response) =
                require_identity_class(authenticated, IdentityClass::Phone, "request-enrollment-token", &request_id)
            {
                return response;
            }
            let token = generate_token();
            let expires_at_unix = now_unix() + ENROLLMENT_TOKEN_VALIDITY_SECONDS;
            state.registry.tokens.write().await.insert(
                token.clone(),
                EnrollmentToken { identity_class, expires_at_unix, consumed: false },
            );
            ControlResponse::RequestEnrollmentTokenOk {
                request_id,
                token,
                expires_at: format_unix(expires_at_unix),
            }
        }
        ControlRequest::ListDevhosts { request_id } => {
            if let Err(response) =
                require_identity_class(authenticated, IdentityClass::Phone, "list-devhosts", &request_id)
            {
                return response;
            }
            handle_list_devhosts(state, request_id).await
        }
        ControlRequest::ListDevhostSshEndpoints { request_id } => {
            if let Err(response) = require_identity_class(
                authenticated,
                IdentityClass::LaptopProxy,
                "list-devhost-ssh-endpoints",
                &request_id,
            ) {
                return response;
            }
            list_devhost_ssh_endpoints(state, request_id).await
        }
        ControlRequest::OpenTunnel { request_id, target_device_id, purpose } => {
            handle_open_tunnel(state, authenticated, request_id, target_device_id, purpose).await
        }
        ControlRequest::AgentEvent { request_id, event, sequence } => {
            if let Err(response) =
                require_identity_class(authenticated, IdentityClass::Devhost, "agent-event", &request_id)
            {
                return response;
            }
            route_agent_event(state, &authenticated.device_id, event, sequence).await;
            ControlResponse::AgentEventOk { request_id }
        }
        ControlRequest::RegisterFcmToken { request_id, fcm_token } => {
            if let Err(response) =
                require_identity_class(authenticated, IdentityClass::Phone, "register-fcm-token", &request_id)
            {
                return response;
            }
            // Replaces, never duplicates: `insert` on an existing key
            // overwrites, per relay-protocol.md's "at most one FCM token
            // per phone Identity".
            state.registry.fcm_tokens.write().await.insert(authenticated.device_id.clone(), fcm_token);
            ControlResponse::RegisterFcmTokenOk { request_id }
        }
        ControlRequest::RevokeDevice { request_id, device_id } => {
            handle_revoke_device(state, authenticated, request_id, device_id).await
        }
        ControlRequest::RevokePhoneSession { request_id, device_id } => {
            handle_revoke_phone_session(state, authenticated, request_id, device_id).await
        }
        ControlRequest::Enroll { request_id, .. } => {
            // Enroll only runs pre-authentication, per handle_connection.
            not_permitted(&request_id, "enroll")
        }
    }
}

/// The `open-tunnel` capability body, split out of [`dispatch`] purely to
/// keep that function's line count reasonable — permission-checking
/// (`check_open_tunnel_permitted`) and target-presence checking stay exactly
/// as they were, just relocated here so `dispatch` only holds the one-line
/// match arm calling this.
async fn handle_open_tunnel(
    state: &AppState,
    authenticated: &Authenticated,
    request_id: String,
    target_device_id: String,
    purpose: String,
) -> ControlResponse {
    if let Err(code) = check_open_tunnel_permitted(state, authenticated, &target_device_id, &purpose).await {
        return ControlResponse::Error {
            request_id,
            code: code.to_string(),
            message: "open-tunnel is not permitted for this Identity class, purpose, or target".to_string(),
        };
    }
    if !state.registry.online_devices.read().await.contains_key(&target_device_id) {
        return ControlResponse::Error {
            request_id,
            code: "target_offline".to_string(),
            message: "target device is not currently connected".to_string(),
        };
    }
    let tunnel_id = generate_tunnel_id();
    state.registry.tunnels.write().await.insert(
        tunnel_id,
        Tunnel {
            requester_device_id: authenticated.device_id.clone(),
            target_device_id: target_device_id.clone(),
            purpose: purpose.clone(),
            last_activity_unix: now_unix(),
        },
    );
    push_tunnel_offered(state, &target_device_id, tunnel_id, &authenticated.device_id, &purpose).await;
    ControlResponse::OpenTunnelOk { request_id, tunnel_id: encode_tunnel_id_hex(tunnel_id) }
}

/// The `revoke-device` capability body, split out of [`dispatch`] purely to
/// keep that function's line count reasonable — permission-checking stays
/// inline here, mirroring every other capability's own check in `dispatch`
/// itself; this is only ever called after `dispatch` has already matched the
/// frame's `type`. Phone-only, per [`ControlRequest::RevokeDevice`]'s doc
/// comment.
async fn handle_revoke_device(
    state: &AppState,
    authenticated: &Authenticated,
    request_id: String,
    device_id: String,
) -> ControlResponse {
    if let Err(response) =
        require_identity_class(authenticated, IdentityClass::Phone, "revoke-device", &request_id)
    {
        return response;
    }
    let found = revoke_device(state, &device_id).await;
    revoke_response(found, "no enrolled device with this device_id", request_id, device_id, |request_id, device_id| {
        ControlResponse::RevokeDeviceOk { request_id, device_id }
    })
}

/// [`handle_revoke_device`]'s sibling for `revoke-phone-session`. Phone-only,
/// per [`ControlRequest::RevokePhoneSession`]'s doc comment.
async fn handle_revoke_phone_session(
    state: &AppState,
    authenticated: &Authenticated,
    request_id: String,
    device_id: String,
) -> ControlResponse {
    if let Err(response) =
        require_identity_class(authenticated, IdentityClass::Phone, "revoke-phone-session", &request_id)
    {
        return response;
    }
    let found = revoke_phone_session(state, &device_id).await;
    revoke_response(found, "no phone session recorded for this device_id", request_id, device_id, |request_id, device_id| {
        ControlResponse::RevokePhoneSessionOk { request_id, device_id }
    })
}

/// The `unknown_device`-or-`*Ok` response shape shared by `revoke-device` and
/// `revoke-phone-session`, once permission-checking (`require_identity_class`)
/// and the registry-specific revoke logic (`revoke_device`/
/// `revoke_phone_session`) are factored out separately: both capabilities
/// report the same `unknown_device` error code (with an operation-specific
/// message) when `found` is `false`, and otherwise hand `request_id`/
/// `device_id` to `ok` to build their own distinct `*Ok` response variant.
fn revoke_response(
    found: bool,
    not_found_message: &str,
    request_id: String,
    device_id: String,
    ok: impl FnOnce(String, String) -> ControlResponse,
) -> ControlResponse {
    if !found {
        return ControlResponse::Error {
            request_id,
            code: "unknown_device".to_string(),
            message: not_found_message.to_string(),
        };
    }
    ok(request_id, device_id)
}

/// Sets `EnrolledDevice.revoked = true` for `device_id` and disconnects its
/// live connection right now, if it has one. Returns `false` if
/// `device_id` names no enrolled device at all (the caller reports this as
/// `unknown_device` rather than a silent no-op success). Only the registry
/// membership/`revoked` flag is mutated here — the certificate itself stays
/// valid and verifiable; `authenticate_device`'s `entry.revoked` check
/// (`ws.rs`, this same module) is what makes a revoked device's *next*
/// connection attempt fail closed regardless of certificate validity, per
/// auth-and-enrollment.md's "Revocation" section.
async fn revoke_device(state: &AppState, device_id: &str) -> bool {
    let found = {
        let mut devices = state.registry.devices.write().await;
        let Some(device) = devices.get_mut(device_id) else { return false };
        device.revoked = true;
        true
    };
    kill_connection(state, device_id).await;
    found
}

/// Invalidates every `phone_sessions` entry recorded against `device_id`
/// (auth-and-enrollment.md: "may hold multiple passkey credentials, one per
/// enrolled phone/browser" — normally one entry, but this removes all of
/// them defensively) and disconnects that Identity's live connection right
/// now, if it has one. Returns `false` if no session was found for
/// `device_id` at all.
async fn revoke_phone_session(state: &AppState, device_id: &str) -> bool {
    let removed = {
        let mut sessions = state.registry.phone_sessions.write().await;
        let before = sessions.len();
        sessions.retain(|_, session| session.device_id != device_id);
        before != sessions.len()
    };
    kill_connection(state, device_id).await;
    removed
}

/// Fires `device_id`'s kill switch, if it's currently connected — see
/// `Registry::kill_switches`'s doc comment. A `device_id` with no live
/// connection right now (never connected, or already disconnected) simply
/// has nothing to kill; that's not an error here, since the registry-level
/// revocation the caller already performed is what makes its *next*
/// connection attempt fail, independent of whether this finds a live
/// connection to close immediately.
async fn kill_connection(state: &AppState, device_id: &str) {
    if let Some(kill_tx) = state.registry.kill_switches.write().await.remove(device_id) {
        let _ = kill_tx.send(());
    }
}

/// The `list-devhosts` capability body, split out of [`dispatch`] purely to
/// keep that function's line count reasonable — permission-checking stays in
/// `dispatch` itself, alongside every other capability's own check, so this
/// is only ever called after that's already passed.
async fn handle_list_devhosts(state: &AppState, request_id: String) -> ControlResponse {
    let devices = state.registry.devices.read().await;
    let online = state.registry.online_devices.read().await;
    let devhosts = devices
        .iter()
        .filter(|(_, device)| device.identity_class == IdentityClass::Devhost && !device.revoked)
        .map(|(device_id, device)| DevHostPresence {
            device_id: device_id.clone(),
            alias: device.alias.clone(),
            platform: device.platform.clone().unwrap_or_default(),
            account_label: device.account_label.clone(),
            connection_state: if online.contains_key(device_id) {
                ConnectionState::Online
            } else {
                ConnectionState::Offline
            },
            last_seen: online.get(device_id).map_or_else(String::new, |seen| format_unix(*seen)),
        })
        .collect();
    ControlResponse::ListDevhostsOk { request_id, devhosts }
}

/// The `list-devhost-ssh-endpoints` capability body, split out of
/// [`dispatch`] purely to keep that function's line count reasonable —
/// permission-checking stays in `dispatch` itself, alongside every other
/// capability's own check, so this is only ever called after that's
/// already passed. See `ControlRequest::ListDevhostSshEndpoints`'s doc
/// comment for what this restricted read is and why it's `laptop-proxy`-only.
async fn list_devhost_ssh_endpoints(state: &AppState, request_id: String) -> ControlResponse {
    let devices = state.registry.devices.read().await;
    let endpoints = devices
        .iter()
        .filter(|(_, device)| device.identity_class == IdentityClass::Devhost && !device.revoked)
        .filter_map(|(device_id, device)| {
            let host_key = device.host_ssh_public_key.as_ref()?;
            Some(choosh_protocol::relay::DevhostSshEndpoint {
                device_id: device_id.clone(),
                alias: device.alias.clone(),
                ssh_host_public_key: base64_engine.encode(host_key),
            })
        })
        .collect();
    ControlResponse::ListDevhostSshEndpointsOk { request_id, endpoints }
}

/// Forwards `event` (sent by `from_device_id`, a devhost) to every currently
/// connected phone Identity, or dispatches a real FCM v1 push (see
/// `crate::fcm`) for any registered phone that's *not* currently connected
/// — per notifications.md, FCM is the backstop when the phone's persistent
/// connection is closed, not the primary path. Single-tenant per DESIGN.md
/// §5, but the one user may hold multiple enrolled phone/browser
/// credentials (auth-and-enrollment.md: "may hold multiple passkey
/// credentials"), so this fans out to all of them rather than assuming
/// exactly one.
///
/// `sequence` is `choosh-hostd`'s per-workspace spool sequence for `event`
/// (`agent-events.md`'s "Delivery and replay" section) — `relayd` neither
/// assigns nor interprets it, only forwards it verbatim in the matching
/// `ServerPush::AgentEvent` so a connected phone can track its own
/// last-acknowledged sequence per workspace without a separate round trip.
async fn route_agent_event(state: &AppState, from_device_id: &str, event: WireAgentEvent, sequence: Option<u64>) {
    // `phone_sessions` is keyed by session credential, not `device_id` — the
    // real device_id (what `online_devices`/`connections`/`fcm_tokens` are
    // keyed by) lives in `PhoneSession::device_id`. Collect distinct
    // device_ids since one phone can hold multiple session credentials
    // across app reinstalls/renewals.
    let phone_ids: std::collections::HashSet<String> =
        state.registry.phone_sessions.read().await.values().map(|session| session.device_id.clone()).collect();
    let online = state.registry.online_devices.read().await;
    let connections = state.registry.connections.read().await;
    let fcm_tokens = state.registry.fcm_tokens.read().await;

    for phone_id in phone_ids {
        if online.contains_key(&phone_id) {
            if let Some(sender) = connections.get(&phone_id) {
                let push = ServerPush::AgentEvent { from_device_id: from_device_id.to_string(), event: event.clone(), sequence };
                if let Ok(wire) = encode_control(&push) {
                    let _ = sender.try_send(wire);
                }
            }
        } else if let Some(token) = fcm_tokens.get(&phone_id) {
            // Spawned rather than awaited inline: this is a network call
            // (`crate::fcm::FcmClient::dispatch`), and a slow or hung FCM
            // request must never delay routing this event to the *next*
            // phone in this fan-out loop, or delay `dispatch`'s caller from
            // returning `AgentEventOk` to the devhost that reported it.
            let fcm = state.fcm.clone();
            let token = token.clone();
            let from_device_id = from_device_id.to_string();
            let event = event.clone();
            tokio::spawn(async move { fcm.dispatch(&token, &from_device_id, &event).await });
        }
    }
}

/// Sends the unsolicited `tunnel-offered` push to the tunnel's target, per
/// relay-protocol.md — best-effort: if the target has disconnected in the
/// brief window since the online check in `dispatch` above, this silently
/// does nothing, which is fine, since the target's own connection teardown
/// will have already (or will shortly) run [`close_tunnels_for_device`].
async fn push_tunnel_offered(
    state: &AppState,
    to_device_id: &str,
    tunnel_id: [u8; TUNNEL_ID_BYTES],
    from_device_id: &str,
    purpose: &str,
) {
    let push = ServerPush::TunnelOffered {
        tunnel_id: encode_tunnel_id_hex(tunnel_id),
        from_device_id: from_device_id.to_string(),
        purpose: purpose.to_string(),
    };
    let Ok(wire) = encode_control(&push) else { return };
    let connections = state.registry.connections.read().await;
    if let Some(sender) = connections.get(to_device_id) {
        let _ = sender.try_send(wire);
    }
}

async fn handle_enroll(state: &AppState, request: ControlRequest, socket: &mut WebSocket) {
    let ControlRequest::Enroll {
        request_id,
        token,
        identity_class,
        public_key,
        alias,
        platform,
        account_label,
        host_ssh_public_key,
    } = request
    else {
        return;
    };

    if let Err(code) = consume_token(&state.registry, &token, identity_class).await {
        return respond(socket, ControlResponse::Error {
            request_id,
            code: code.to_string(),
            message: "enrollment token is invalid, expired, consumed, or wrong class".to_string(),
        })
        .await;
    }

    let Ok(public_key_bytes) = base64_engine.decode(&public_key) else {
        return respond(socket, ControlResponse::Error {
            request_id,
            code: "invalid_public_key".to_string(),
            message: "public_key is not valid base64".to_string(),
        })
        .await;
    };

    // auth-and-enrollment.md step 6: a devhost's loopback SSH host key is
    // established as trusted at this exact moment, over this same
    // authenticated exchange, never via a separate TOFU prompt. A devhost
    // enrollment MUST carry one; a laptop-proxy (no SSH server of its own)
    // MUST NOT.
    let host_ssh_public_key_bytes = match (identity_class, host_ssh_public_key) {
        (IdentityClass::Devhost, Some(encoded)) => {
            match base64_engine.decode(&encoded) {
                Ok(bytes) if bytes.len() == 32 => Some(bytes),
                _ => {
                    return respond(socket, ControlResponse::Error {
                        request_id,
                        code: "invalid_host_ssh_public_key".to_string(),
                        message: "host_ssh_public_key is not a valid base64-encoded 32-byte Ed25519 public key".to_string(),
                    })
                    .await;
                }
            }
        }
        (IdentityClass::Devhost, None) => {
            return respond(socket, ControlResponse::Error {
                request_id,
                code: "missing_host_ssh_public_key".to_string(),
                message: "devhost enrollment MUST include the loopback SSH server's host public key".to_string(),
            })
            .await;
        }
        // laptop-proxy/phone: no SSH server of their own, so any presented
        // value is simply ignored rather than validated or stored.
        (_, _) => None,
    };

    let device_id = format!("dev-{}", uuid::Uuid::new_v4());
    let certificate = ca::issue(&state.ca_key, &device_id, &public_key_bytes, now_unix());
    state.registry.devices.write().await.insert(
        device_id.clone(),
        EnrolledDevice {
            public_key: public_key_bytes,
            identity_class,
            alias: alias.unwrap_or_else(|| device_id.clone()),
            platform,
            account_label,
            revoked: false,
            host_ssh_public_key: host_ssh_public_key_bytes,
        },
    );
    respond(socket, ControlResponse::EnrollOk { request_id, device_id, certificate }).await;
}

async fn respond(socket: &mut WebSocket, response: ControlResponse) {
    match encode_control(&response) {
        Ok(wire) => {
            let _ = socket.send(Message::Binary(wire.into())).await;
        }
        Err(err) => tracing::warn!(?err, "failed to encode enroll ControlResponse"),
    }
    let _ = socket.close().await;
}

async fn consume_token(
    registry: &Registry,
    token: &str,
    requested_class: IdentityClass,
) -> Result<(), &'static str> {
    let mut tokens = registry.tokens.write().await;
    let Some(entry) = tokens.get_mut(token) else { return Err("token_unknown") };
    if entry.consumed {
        return Err("token_consumed");
    }
    if now_unix() > entry.expires_at_unix {
        return Err("token_expired");
    }
    if entry.identity_class != requested_class {
        return Err("token_wrong_class");
    }
    entry.consumed = true;
    Ok(())
}

fn generate_token() -> String {
    let mut bytes = [0u8; 24];
    crate::rng::os_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn format_unix(unix_seconds: u64) -> String {
    time::OffsetDateTime::from_unix_timestamp(i64::try_from(unix_seconds).unwrap_or(i64::MAX))
        .map_or_else(|_| unix_seconds.to_string(), |dt| dt.to_string())
}

/// Development-only convenience: mints and returns a fresh devhost
/// enrollment token without requiring a passkey-authenticated phone
/// session. Only reachable when `CHOOSH_RELAYD_DEV_MODE=1` — see `lib.rs`.
pub async fn dev_mint_enrollment_token(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let token = generate_token();
    let expires_at_unix = now_unix() + ENROLLMENT_TOKEN_VALIDITY_SECONDS;
    state.registry.tokens.write().await.insert(
        token.clone(),
        EnrollmentToken { identity_class: IdentityClass::Devhost, expires_at_unix, consumed: false },
    );
    axum::Json(serde_json::json!({ "token": token, "expires_at": format_unix(expires_at_unix) }))
}

#[cfg(test)]
mod tests {
    use super::{RateLimiter, Tunnel, expired_tunnel_ids};
    use crate::state::TUNNEL_IDLE_TIMEOUT_SECONDS;
    use std::collections::HashMap;
    use std::time::Duration;

    fn tunnel(last_activity_unix: u64) -> Tunnel {
        Tunnel {
            requester_device_id: "requester".to_string(),
            target_device_id: "target".to_string(),
            purpose: "rpc".to_string(),
            last_activity_unix,
        }
    }

    // `reap_idle_tunnels` itself relies on a real 30-second tick and the
    // real system clock, so it isn't practically unit-testable end to end —
    // this exercises the pure boundary decision it delegates to instead:
    // the exact instant a tunnel crosses from "idle" to "reap it now" per
    // relay-protocol.md's tunnel lifecycle, including the inclusive `>=`
    // boundary at exactly `TUNNEL_IDLE_TIMEOUT_SECONDS`, not just comfortably
    // on either side of it.
    #[test]
    fn tunnels_expire_at_the_idle_timeout_boundary_inclusive() {
        let now = 1_000_000u64;
        let mut tunnels = HashMap::new();
        let fresh = [1u8; 8];
        let boundary = [2u8; 8];
        let just_under = [3u8; 8];
        let long_idle = [4u8; 8];
        tunnels.insert(fresh, tunnel(now));
        tunnels.insert(boundary, tunnel(now - TUNNEL_IDLE_TIMEOUT_SECONDS));
        tunnels.insert(just_under, tunnel(now - TUNNEL_IDLE_TIMEOUT_SECONDS + 1));
        tunnels.insert(long_idle, tunnel(0));

        let mut expired = expired_tunnel_ids(&tunnels, now);
        expired.sort_unstable();
        let mut want = [boundary, long_idle];
        want.sort_unstable();
        assert_eq!(expired, want);
    }

    #[test]
    fn an_empty_registry_expires_nothing() {
        assert!(expired_tunnel_ids(&HashMap::new(), 1_000_000).is_empty());
    }

    // `RateLimiter::try_acquire` itself relies on the real system clock, so
    // — mirroring `expired_tunnel_ids` above — these exercise the pure
    // boundary decision (`try_acquire_after`) it delegates to, with
    // synthetic elapsed durations rather than a real wait.
    #[test]
    fn rate_limiter_allows_a_burst_then_rejects_until_refill() {
        let mut limiter = RateLimiter::new(3, 1);
        assert!(limiter.try_acquire_after(Duration::ZERO));
        assert!(limiter.try_acquire_after(Duration::ZERO));
        assert!(limiter.try_acquire_after(Duration::ZERO));
        assert!(!limiter.try_acquire_after(Duration::ZERO), "burst of 3 exhausted with no time elapsed to refill");
        assert!(limiter.try_acquire_after(Duration::from_secs(1)), "1 token refills after 1s at 1/s");
        assert!(!limiter.try_acquire_after(Duration::ZERO), "the refilled token was just spent");
    }

    #[test]
    fn rate_limiter_refill_never_exceeds_capacity() {
        let mut limiter = RateLimiter::new(2, 100);
        // A huge elapsed duration would refill far past capacity if
        // uncapped — asserts the `.min(self.capacity)` clamp actually
        // bounds the bucket, not just that some tokens refill.
        assert!(limiter.try_acquire_after(Duration::from_secs(10)));
        assert!(limiter.try_acquire_after(Duration::ZERO));
        assert!(!limiter.try_acquire_after(Duration::ZERO), "capacity is 2, both tokens already spent");
    }
}
