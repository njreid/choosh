//! `relayd` as a `WebAuthn` relying party. Single-tenant: exactly one
//! registrable user who may hold multiple passkeys (one per enrolled
//! phone/browser), per auth-and-enrollment.md.

use crate::AppState;
use crate::state::{PhoneSession, SESSION_CREDENTIAL_VALIDITY_SECONDS, now_unix};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use getrandom::rand_core::Rng as _;
use std::sync::Arc;
use webauthn_rs::prelude::*;

pub struct WebauthnState {
    pub webauthn: Webauthn,
    pub user_id: Uuid,
    pub passkeys: tokio::sync::RwLock<Vec<Passkey>>,
    pub in_flight_registration: tokio::sync::RwLock<Option<PasskeyRegistration>>,
    pub in_flight_authentication: tokio::sync::RwLock<Option<PasskeyAuthentication>>,
}

/// # Panics
///
/// Panics if `rp_id`/`rp_origin_str` cannot build a valid `Webauthn`
/// instance — this only happens for a malformed
/// `CHOOSH_RELAYD_RP_ID`/`_ORIGIN`, which is a startup configuration error,
/// not a runtime condition to recover from.
#[must_use]
pub fn build(rp_id: &str, rp_origin_str: &str) -> WebauthnState {
    let rp_origin = Url::parse(rp_origin_str).expect("CHOOSH_RELAYD_RP_ORIGIN must be a valid URL");
    let webauthn = WebauthnBuilder::new(rp_id, &rp_origin)
        .expect("valid rp_id/rp_origin")
        .build()
        .expect("webauthn builder succeeds with valid rp_id/rp_origin");
    WebauthnState {
        webauthn,
        // Fixed: single-tenant, exactly one registrable user, per
        // auth-and-enrollment.md.
        user_id: Uuid::from_u128(1),
        passkeys: tokio::sync::RwLock::new(Vec::new()),
        in_flight_registration: tokio::sync::RwLock::new(None),
        in_flight_authentication: tokio::sync::RwLock::new(None),
    }
}

pub async fn register_start(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let existing = state.webauthn.passkeys.read().await;
    let exclude: Vec<CredentialID> = existing.iter().map(Passkey::cred_id).cloned().collect();
    drop(existing);
    match state.webauthn.webauthn.start_passkey_registration(
        state.webauthn.user_id,
        "choosh",
        "Choosh",
        Some(exclude),
    ) {
        Ok((ccr, reg_state)) => {
            *state.webauthn.in_flight_registration.write().await = Some(reg_state);
            (StatusCode::OK, Json(ccr)).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

pub async fn register_finish(
    State(state): State<Arc<AppState>>,
    Json(credential): Json<RegisterPublicKeyCredential>,
) -> impl IntoResponse {
    let Some(reg_state) = state.webauthn.in_flight_registration.write().await.take() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "no registration in progress" })),
        )
            .into_response();
    };
    match state.webauthn.webauthn.finish_passkey_registration(&credential, &reg_state) {
        Ok(passkey) => {
            state.webauthn.passkeys.write().await.push(passkey);
            let (credential, expires_at) = mint_session(&state).await;
            (StatusCode::OK, Json(serde_json::json!({
                "session_credential": credential,
                "expires_at": expires_at,
            })))
                .into_response()
        }
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

pub async fn login_start(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let passkeys = state.webauthn.passkeys.read().await.clone();
    if passkeys.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "no passkeys registered yet" })),
        )
            .into_response();
    }
    match state.webauthn.webauthn.start_passkey_authentication(&passkeys) {
        Ok((rcr, auth_state)) => {
            *state.webauthn.in_flight_authentication.write().await = Some(auth_state);
            (StatusCode::OK, Json(rcr)).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

pub async fn login_finish(
    State(state): State<Arc<AppState>>,
    Json(credential): Json<PublicKeyCredential>,
) -> impl IntoResponse {
    let Some(auth_state) = state.webauthn.in_flight_authentication.write().await.take() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "no authentication in progress" })),
        )
            .into_response();
    };
    match state.webauthn.webauthn.finish_passkey_authentication(&credential, &auth_state) {
        Ok(auth_result) => {
            // Passkey counters can change across an authentication; persist
            // that so a future ceremony's clone-detection stays accurate.
            let mut passkeys = state.webauthn.passkeys.write().await;
            for passkey in passkeys.iter_mut() {
                passkey.update_credential(&auth_result);
            }
            drop(passkeys);
            let (credential, expires_at) = mint_session(&state).await;
            (StatusCode::OK, Json(serde_json::json!({
                "session_credential": credential,
                "expires_at": expires_at,
            })))
                .into_response()
        }
        Err(err) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

async fn mint_session(state: &AppState) -> (String, u64) {
    let mut bytes = [0u8; 32];
    crate::rng::os_rng().fill_bytes(&mut bytes);
    let credential = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes);
    let expires_at = now_unix() + SESSION_CREDENTIAL_VALIDITY_SECONDS;
    let device_id = format!("phone-{}", uuid::Uuid::new_v4());
    state.registry.phone_sessions.write().await.insert(
        credential.clone(),
        PhoneSession { device_id, expires_at_unix: expires_at },
    );
    (credential, expires_at)
}
