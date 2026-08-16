//! `choosh-relayd`: presence registry, tunnel broker, and `WebAuthn` relying
//! party for the Choosh fleet. See `docs/specs/relay-protocol.md` and
//! `docs/specs/auth-and-enrollment.md` for the protocol this crate
//! implements, and `DESIGN.md` §5 for the surrounding architecture.
//!
//! Implements: `WebAuthn` registration/login, devhost enrollment, presence
//! (`list-devhosts`), enrollment-token issuance, and `open-tunnel` with
//! `0x02` tunnel-frame routing (M1 — see `docs/milestones/M1-workspace-and-jj.md`,
//! whose RPC surface rides over an `rpc`-purpose tunnel). Also `agent-event`
//! and `register-fcm-token` dispatch/routing (M2), the laptop-proxy-only
//! `list-devhost-ssh-endpoints` read (M6), and the `devhost`-to-`devhost`
//! `offload`-purpose tunnel capability (M7) — see `crate::ws::dispatch` for
//! the full control-frame catalog this now serves. Also, per the M8
//! threat-model review's named follow-ups: phone-gated device/phone-session
//! revocation that closes an already-live connection immediately
//! (`crate::ws::dispatch`'s `RevokeDevice`/`RevokePhoneSession` arms), a
//! per-Identity control-frame rate limit (`crate::ws::RateLimiter`), and a
//! connection-wide idle timeout (`AppState::connection_idle_timeout`). Also,
//! per the unauthenticated-registration finding this crate's own
//! `webauthn.rs` previously left open: `/webauthn/register/start` now
//! requires a `CHOOSH_RELAYD_BOOTSTRAP_SECRET`-gated header before it will
//! begin a registration ceremony (see `webauthn.rs`'s module doc), and the
//! in-flight registration/login ceremony state is keyed per-ceremony rather
//! than held in a single global slot.

mod ca;
mod fcm;
#[cfg(test)]
mod integration_tests;
mod rng;
mod state;
mod webauthn;
mod wire;
mod ws;

use axum::Router;
use axum::routing::{get, post};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use webauthn::WebauthnState;

/// Default bind address; overridden by the `CHOOSH_RELAYD_BIND` env var.
pub const DEFAULT_BIND: &str = "127.0.0.1:7443";
/// Default `WebAuthn` RP ID; overridden by `CHOOSH_RELAYD_RP_ID`. This is a
/// genuinely open question (DESIGN.md §14) — not meant to be the final
/// production domain.
pub const DEFAULT_RP_ID: &str = "choosh.local";

pub struct AppState {
    pub registry: Arc<state::Registry>,
    pub ca_key: ed25519_dalek::SigningKey,
    pub webauthn: WebauthnState,
    /// See `state::CONNECTION_IDLE_TIMEOUT_SECONDS`. A plain field (not the
    /// constant used directly) so a test can shrink it to something an
    /// `#[tokio::test]` can actually wait out in real time, by mutating it
    /// via `Arc::get_mut` before the state is cloned into `router`/spawned —
    /// mirroring how `CHOOSH_RELAYD_STATE_DIR`/`build_state_in` already let
    /// tests substitute an isolated directory without an env var.
    pub connection_idle_timeout: std::time::Duration,
    /// Real Firebase Cloud Messaging v1 dispatcher, or a logging-only
    /// fallback — see `crate::fcm`'s module doc for this environment's
    /// credential-availability finding.
    pub fcm: fcm::FcmClient,
}

async fn healthz() -> &'static str {
    "ok"
}

fn state_dir() -> PathBuf {
    std::env::var("CHOOSH_RELAYD_STATE_DIR")
        .map_or_else(|_| PathBuf::from("./relayd-state"), PathBuf::from)
}

#[must_use]
pub fn build_state() -> Arc<AppState> {
    build_state_in(&state_dir())
}

/// Builds `AppState` rooted at an explicit state directory, bypassing the
/// `CHOOSH_RELAYD_STATE_DIR` env var — this is what lets tests use an
/// isolated directory per test without `std::env::set_var`, which this
/// workspace's `unsafe_code = "forbid"` lint (correctly) rejects.
///
/// # Panics
///
/// Panics if the enrollment CA key cannot be loaded from or generated into
/// `dir`, or if `CHOOSH_RELAYD_RP_ORIGIN`/the default origin is not a valid
/// URL — both are startup configuration failures, not conditions to
/// recover from at runtime.
#[must_use]
pub fn build_state_in(dir: &Path) -> Arc<AppState> {
    let ca_key = ca::load_or_generate(dir).expect("enrollment CA key must load or generate");
    let rp_id = std::env::var("CHOOSH_RELAYD_RP_ID").unwrap_or_else(|_| DEFAULT_RP_ID.to_string());
    let rp_origin_str = std::env::var("CHOOSH_RELAYD_RP_ORIGIN")
        .unwrap_or_else(|_| format!("https://{rp_id}"));
    // No default: an unset `CHOOSH_RELAYD_BOOTSTRAP_SECRET` means
    // `register_start` refuses every request (fail closed) rather than
    // falling back to some baked-in value — see `webauthn.rs`'s module doc.
    let bootstrap_secret = std::env::var("CHOOSH_RELAYD_BOOTSTRAP_SECRET").ok();
    Arc::new(AppState {
        registry: Arc::new(state::Registry::new()),
        ca_key,
        webauthn: webauthn::build(&rp_id, &rp_origin_str, bootstrap_secret),
        connection_idle_timeout: std::time::Duration::from_secs(state::CONNECTION_IDLE_TIMEOUT_SECONDS),
        fcm: fcm::FcmClient::from_env(),
    })
}

pub fn router(state: Arc<AppState>) -> Router {
    let dev_mode = std::env::var("CHOOSH_RELAYD_DEV_MODE").as_deref() == Ok("1");
    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .route("/connect", get(ws::connect))
        .route("/webauthn/register/start", post(webauthn::register_start))
        .route("/webauthn/register/finish", post(webauthn::register_finish))
        .route("/webauthn/login/start", post(webauthn::login_start))
        .route("/webauthn/login/finish", post(webauthn::login_finish));
    if dev_mode {
        tracing::warn!(
            "CHOOSH_RELAYD_DEV_MODE=1: /dev/mint-enrollment-token is enabled with no authentication"
        );
        router = router.route("/dev/mint-enrollment-token", post(ws::dev_mint_enrollment_token));
    }
    router.with_state(state)
}

/// Runs the relay server until the process receives a shutdown signal.
///
/// # Errors
///
/// Returns an error if the bind address cannot be parsed or the listener
/// cannot be bound.
pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();

    let bind = std::env::var("CHOOSH_RELAYD_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "choosh-relayd listening");

    let state = build_state();
    tokio::spawn(ws::reap_idle_tunnels(Arc::clone(&state)));
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_reports_ok() {
        let dir = std::env::temp_dir().join(format!("choosh-relayd-test-{}", uuid::Uuid::new_v4()));
        let response = router(build_state_in(&dir))
            .oneshot(Request::builder().uri("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        std::fs::remove_dir_all(&dir).ok();
    }
}
