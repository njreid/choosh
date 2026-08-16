//! Shared relayd dial/authenticate logic for `choosh-hostd`'s two
//! client-only WebSocket surfaces: `proxy.rs` (`proxy connect`/`proxy
//! sync`) and `dev_exec.rs` (`dev-exec`). Both dial `relayd`'s public
//! WebSocket endpoint and authenticate with an already-persisted device
//! credential in exactly the same three steps — dial, receive
//! `ServerHello`, sign the nonce and send `ClientAuth::Device`, receive
//! `AuthResult` — so that one shared shape lives here instead of twice.
//!
//! **Deliberately not shared with `serve.rs`.** `serve.rs`'s own
//! `dial`/`run_one_connection` are private to that module and built
//! around `serve`'s own devhost-specific state (an `RpcContext`, a
//! long-lived reconnect loop, `pty:`/`web:` tunnel bookkeeping) that
//! neither `proxy.rs` nor `dev_exec.rs` has any of. A prior review judged
//! pulling a shared abstraction out of that already-tested, working
//! module a worse trade than the small amount of connect+auth logic
//! duplicated there — this module only unifies the two *other*,
//! genuinely-identical client-only call sites with each other.

use base64::Engine;
use choosh_protocol::relay::{AuthResult, ClientAuth, DeviceAuth, FRAME_CLASS_CONTROL, ServerHello};

use crate::credential::{Credential, CredentialError};
use crate::frame_channel::{ChannelError, FrameChannel};

const DEFAULT_RELAYD_URL: &str = "ws://127.0.0.1:7443/connect";

/// `relayd`'s WebSocket URL, overridable via `CHOOSH_RELAYD_URL` — both
/// `proxy.rs`'s and `dev_exec.rs`'s tests rely on this override to point
/// at a fake relayd instead of a real one.
pub(crate) fn relay_url() -> String {
    std::env::var("CHOOSH_RELAYD_URL").unwrap_or_else(|_| DEFAULT_RELAYD_URL.to_string())
}

pub(crate) type WsChannel = FrameChannel<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>;

/// Failure modes shared by [`dial`]/[`connect_authenticated`]. Each caller
/// (`proxy::ProxyError`/`dev_exec::DevExecError`) converts this into its
/// own error type via `From`, so both keep their own module-specific error
/// enum and `Display` wording rather than this module dictating either.
#[derive(Debug)]
pub(crate) enum RelayClientError {
    Transport(String),
    AuthFailed(String),
    Credential(CredentialError),
}

impl From<ChannelError> for RelayClientError {
    fn from(error: ChannelError) -> Self {
        Self::Transport(error.to_string())
    }
}

impl From<CredentialError> for RelayClientError {
    fn from(error: CredentialError) -> Self {
        Self::Credential(error)
    }
}

/// # Errors
///
/// [`RelayClientError::Transport`] if the WebSocket connection itself
/// cannot be established.
pub(crate) async fn dial(relay_url: &str) -> Result<WsChannel, RelayClientError> {
    let (stream, _response) =
        tokio_tungstenite::connect_async(relay_url).await.map_err(|error| RelayClientError::Transport(error.to_string()))?;
    Ok(FrameChannel::new(stream))
}

/// Dials `relay_url` and authenticates with `credential` — the shared
/// step 1 for `proxy connect`/`proxy sync` (ssh-bridge-and-zed.md) and
/// `dev-exec` (auth-and-enrollment.md's capability table) alike.
///
/// # Errors
///
/// [`RelayClientError::Transport`] for a failed dial or a transport
/// failure mid-handshake, [`RelayClientError::Credential`] if the stored
/// credential's key material is corrupt, [`RelayClientError::AuthFailed`]
/// if `relayd` rejects the signed challenge.
pub(crate) async fn connect_authenticated(relay_url: &str, credential: &Credential) -> Result<WsChannel, RelayClientError> {
    let mut channel = dial(relay_url).await?;
    let hello: ServerHello = channel.recv().await?;
    let signature = credential.sign(hello.nonce.as_bytes())?;
    let auth = ClientAuth::Device(DeviceAuth {
        device_id: credential.device_id.clone(),
        certificate: credential.certificate.clone(),
        signature: base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
    });
    channel.send(FRAME_CLASS_CONTROL, &auth).await?;
    match channel.recv().await? {
        AuthResult::Ok(_) => Ok(channel),
        AuthResult::Failed(failed) => Err(RelayClientError::AuthFailed(failed.reason)),
    }
}

/// A fresh random id for a `ControlRequest`'s `request_id` field.
pub(crate) fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
