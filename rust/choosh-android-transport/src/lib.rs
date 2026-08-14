//! The relay-protocol client for the *phone* Identity, per
//! `docs/specs/relay-protocol.md` and `docs/specs/auth-and-enrollment.md`.
//! Parallel to `choosh-hostd`'s devhost-side client, but phone-authenticated
//! (a stored bearer session credential, never a signed challenge) rather
//! than device-certificate-authenticated, and with no enrollment step of
//! its own — a phone's credential comes from `WebAuthn`, which is
//! `choosh-android-bridge`'s job, not this crate's.
//!
//! This crate has no `main`/CLI: `choosh-android-bridge` is its only
//! consumer, driving it from JNI call sites.

#![forbid(unsafe_code)]

pub mod backoff;
mod frame_channel;

use choosh_protocol::relay::{
    AuthResult, ClientAuth, ControlRequest, ControlResponse, DevHostPresence, FRAME_CLASS_CONTROL,
    PhoneAuth, ServerHello,
};
use frame_channel::{ChannelError, FrameChannel};

type WsChannel = FrameChannel<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>;

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

/// A live, phone-authenticated connection to `relayd`. Holds the socket
/// open; the caller decides when to drop it (and, per relay-protocol.md,
/// MUST reconnect+re-authenticate rather than expect any in-flight state to
/// survive a drop — see [`PhoneConnection::run_with_reconnect`] for that
/// loop already built).
pub struct PhoneConnection {
    channel: WsChannel,
}

impl PhoneConnection {
    /// Dials `relayd`, completes the `ServerHello`/`ClientAuth::Phone`
    /// handshake with the given session credential, and returns a live
    /// connection on `AuthResult::Ok`.
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
            AuthResult::Ok(_) => Ok(Self { channel }),
            AuthResult::Failed(failed) => Err(ConnectError::Rejected(failed.reason)),
        }
    }

    /// Requests the current fleet presence list.
    ///
    /// # Errors
    ///
    /// See [`CallError`].
    pub async fn list_devhosts(&mut self) -> Result<Vec<DevHostPresence>, CallError> {
        let request_id = new_request_id();
        self.channel
            .send(FRAME_CLASS_CONTROL, &ControlRequest::ListDevhosts { request_id: request_id.clone() })
            .await?;
        match self.channel.recv::<ControlResponse>().await? {
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
        self.channel
            .send(
                FRAME_CLASS_CONTROL,
                &ControlRequest::RequestEnrollmentToken { request_id: request_id.clone(), identity_class },
            )
            .await?;
        match self.channel.recv::<ControlResponse>().await? {
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
        self.channel
            .send(
                FRAME_CLASS_CONTROL,
                &ControlRequest::RegisterFcmToken { request_id: request_id.clone(), fcm_token: fcm_token.to_string() },
            )
            .await?;
        match self.channel.recv::<ControlResponse>().await? {
            ControlResponse::RegisterFcmTokenOk { .. } => Ok(()),
            ControlResponse::Error { code, message, .. } => Err(CallError::Server { code, message }),
            _ => Err(CallError::UnexpectedResponse),
        }
    }

    /// Blocks until the connection drops (or a malformed frame terminates
    /// it, per relay-protocol.md). M0 has no server-pushed frame to react
    /// to on this connection yet (no tunnels, no agent events), so this is
    /// just liveness — the caller's reconnect loop decides what happens
    /// next. Never returns `Ok`.
    async fn hold_until_closed(&mut self) -> ChannelError {
        loop {
            if let Err(error) = self.channel.recv_raw().await {
                return error;
            }
            // An unexpected-but-well-framed control frame arriving here
            // (there are none in M0) would be logged and ignored rather
            // than treated as fatal — matching choosh-hostd's posture that
            // an unrecognized frame isn't itself a connection-ending event,
            // only a malformed one (already handled by recv_raw's Err path).
        }
    }
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
    use choosh_protocol::relay::{AuthOk, ConnectionState, IdentityClass, MAX_CONTROL_FRAME_BYTES};
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    async fn send_control<T: serde::Serialize>(
        ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        value: &T,
    ) {
        let mut payload = vec![FRAME_CLASS_CONTROL];
        payload.extend(serde_json::to_vec(value).unwrap());
        let wire = encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).unwrap();
        ws.send(Message::Binary(wire.into())).await.unwrap();
    }

    async fn recv_control<T: serde::de::DeserializeOwned>(
        ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) -> T {
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

    async fn bind_fake_relayd() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, format!("ws://{addr}/connect"))
    }

    #[tokio::test]
    async fn phone_connects_and_lists_devhosts() {
        let (listener, url) = bind_fake_relayd().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            send_control(&mut ws, &ServerHello { nonce: "n".to_string() }).await;

            let auth: ClientAuth = recv_control(&mut ws).await;
            assert!(matches!(auth, ClientAuth::Phone(PhoneAuth { session_credential }) if session_credential == "good-cred"));
            send_control(
                &mut ws,
                &AuthResult::Ok(AuthOk { identity_class: IdentityClass::Phone, device_id: "phone-1".to_string() }),
            )
            .await;

            let _request: ControlRequest = recv_control(&mut ws).await;
            send_control(
                &mut ws,
                &ControlResponse::ListDevhostsOk {
                    request_id: "ignored-by-client-side-of-this-test".to_string(),
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
            send_control(&mut ws, &ServerHello { nonce: "n".to_string() }).await;
            let _auth: ClientAuth = recv_control(&mut ws).await;
            send_control(
                &mut ws,
                &AuthResult::Ok(AuthOk { identity_class: IdentityClass::Phone, device_id: "phone-1".to_string() }),
            )
            .await;
            let _request: ControlRequest = recv_control(&mut ws).await;
            send_control(
                &mut ws,
                &ControlResponse::RequestEnrollmentTokenOk {
                    request_id: "ignored".to_string(),
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
