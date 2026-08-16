//! `relayd` as a `WebAuthn` relying party. Single-tenant: exactly one
//! registrable user who may hold multiple passkeys (one per enrolled
//! phone/browser), per auth-and-enrollment.md.
//!
//! **Registration is bootstrap-secret-gated.** `register_start`/
//! `register_finish` are the only path in this whole system that mints a
//! brand-new, fully-trusted `phone` Identity from nothing — every other
//! credential (a devhost's, a laptop-proxy's) can only be minted by an
//! already-authenticated `phone` connection via an enrollment token
//! (auth-and-enrollment.md). Left ungated, anyone who can reach `relayd`'s
//! HTTP surface (meant to be reachable at a stable public DNS name per
//! DESIGN.md) could register themselves as the owner. `register_start`
//! therefore requires the caller to present `CHOOSH_RELAYD_BOOTSTRAP_SECRET`
//! (a header, see [`BOOTSTRAP_SECRET_HEADER`]) via a constant-time compare
//! ([`bootstrap_secret_matches`]); if the env var isn't set at all,
//! registration is refused outright (fail closed, no default). The
//! system's owner sets this once at `relayd` deploy time and provides it to
//! their own phone out of band during first-time setup — it is never a
//! standing credential presented on every request, only once, to open the
//! single registration window that establishes the first (and every
//! subsequent) passkey. `login_start`/`login_finish` need no such gate:
//! logging in only ever re-asserts an *already-registered* passkey, which
//! by definition means someone already passed through the bootstrap gate
//! once.

use crate::AppState;
use crate::state::{PhoneSession, SESSION_CREDENTIAL_VALIDITY_SECONDS, now_unix};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use getrandom::rand_core::Rng as _;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use webauthn_rs::prelude::*;

/// HTTP header a `register_start` caller must present
/// `CHOOSH_RELAYD_BOOTSTRAP_SECRET` in, verbatim, to be allowed to begin a
/// registration ceremony. Header (not a query parameter) so the secret
/// doesn't land in server access logs or browser history the way a query
/// string routinely does.
pub const BOOTSTRAP_SECRET_HEADER: &str = "x-choosh-bootstrap-secret";

/// How long an in-flight registration/authentication ceremony's server-side
/// challenge state is kept before being pruned as abandoned (e.g. the user
/// cancelled the OS biometric prompt and never called the matching
/// `finish` endpoint). Generous relative to how long a real `WebAuthn`
/// ceremony takes (seconds), but short enough that an unbounded number of
/// started-and-abandoned ceremonies can't accumulate forever in memory —
/// pruning runs inline on every `start`/`finish` call rather than needing a
/// background task, since this system's ceremony volume is low enough
/// (single-tenant, human-paced) that an O(n) sweep on each call is not a
/// performance concern.
const IN_FLIGHT_CEREMONY_TTL_SECONDS: u64 = 5 * 60;

pub struct WebauthnState {
    pub webauthn: Webauthn,
    pub user_id: Uuid,
    pub passkeys: tokio::sync::RwLock<Vec<Passkey>>,
    /// Keyed by a fresh, unguessable per-ceremony correlation id minted in
    /// [`register_start`] and required back (as a `correlation_id` query
    /// parameter) on the matching [`register_finish`] call, alongside the
    /// unix-time each entry was inserted (for TTL pruning, see
    /// [`IN_FLIGHT_CEREMONY_TTL_SECONDS`]).
    ///
    /// **Not** a single global slot: an earlier version of this state was
    /// `RwLock<Option<PasskeyRegistration>>`, which meant two ceremonies
    /// started close together (e.g. two browser tabs, or a client retry
    /// racing its own first attempt) would silently clobber each other's
    /// challenge — the second `start` call would overwrite the first's
    /// entry, so the first ceremony's `finish` would fail (or, worse,
    /// succeed against the *second* ceremony's state if the two calls
    /// interleaved just right). Keying by an unguessable per-ceremony id
    /// makes concurrent ceremonies independent: each `finish` call can only
    /// ever observe the exact challenge state its own `start` call created.
    pub in_flight_registration: tokio::sync::RwLock<HashMap<String, (PasskeyRegistration, u64)>>,
    /// [`Self::in_flight_registration`]'s sibling for the login/assertion
    /// ceremony, same keying and same reasoning.
    pub in_flight_authentication: tokio::sync::RwLock<HashMap<String, (PasskeyAuthentication, u64)>>,
    /// `CHOOSH_RELAYD_BOOTSTRAP_SECRET`, read once at startup. `None` means
    /// the env var was unset (or set to an empty string, treated the same
    /// way) — [`register_start`] then refuses every request outright rather
    /// than falling back to any default, since there is no safe default for
    /// "who is allowed to become the owner."
    pub bootstrap_secret: Option<String>,
}

/// # Panics
///
/// Panics if `rp_id`/`rp_origin_str` cannot build a valid `Webauthn`
/// instance — this only happens for a malformed
/// `CHOOSH_RELAYD_RP_ID`/`_ORIGIN`, which is a startup configuration error,
/// not a runtime condition to recover from.
#[must_use]
pub fn build(rp_id: &str, rp_origin_str: &str, bootstrap_secret: Option<String>) -> WebauthnState {
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
        in_flight_registration: tokio::sync::RwLock::new(HashMap::new()),
        in_flight_authentication: tokio::sync::RwLock::new(HashMap::new()),
        bootstrap_secret: bootstrap_secret.filter(|secret| !secret.is_empty()),
    }
}

/// Constant-time equality check for the bootstrap secret — deliberately not
/// `==`, which short-circuits on the first differing byte and can leak
/// timing information about how much of a guessed secret matched. Backed by
/// `subtle::ConstantTimeEq`'s `[u8]` impl, which itself short-circuits only
/// on *length* (not content) before comparing in constant time; that's
/// acceptable here since the secret's length isn't the sensitive part.
fn bootstrap_secret_matches(expected: &str, provided: &str) -> bool {
    expected.as_bytes().ct_eq(provided.as_bytes()).into()
}

/// 24 bytes of OS CSPRNG entropy, base64url-encoded — same construction as
/// [`mint_session`]'s session credential. Used as the per-ceremony
/// correlation id keying [`WebauthnState::in_flight_registration`]/
/// [`WebauthnState::in_flight_authentication`]; unguessable, so a caller
/// can only ever complete the specific ceremony their own `start` call
/// began.
fn generate_correlation_id() -> String {
    let mut bytes = [0u8; 24];
    crate::rng::os_rng().fill_bytes(&mut bytes);
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes)
}

/// Drops any entry older than [`IN_FLIGHT_CEREMONY_TTL_SECONDS`] — run
/// inline at the start of every `start`/`finish` call that touches the map,
/// so an abandoned ceremony's state doesn't accumulate forever.
fn prune_expired_ceremonies<T>(map: &mut HashMap<String, (T, u64)>, now: u64) {
    map.retain(|_, (_, started_at_unix)| now.saturating_sub(*started_at_unix) < IN_FLIGHT_CEREMONY_TTL_SECONDS);
}

/// Merges `correlation_id` into a serialized `CreationChallengeResponse`/
/// `RequestChallengeResponse` body as a sibling top-level field. Both types
/// serialize to a JSON object (`{"publicKey": {...}}`), and neither denies
/// unknown fields on deserialization (checked directly against
/// `webauthn-rs-proto`'s source), so a caller that decodes the response
/// straight into either type — as every real caller in this codebase does —
/// simply ignores the extra field unless it's specifically looking for it.
fn with_correlation_id<T: serde::Serialize>(value: &T, correlation_id: &str) -> serde_json::Value {
    let mut body = serde_json::to_value(value).unwrap_or_else(|_| serde_json::json!({}));
    if let serde_json::Value::Object(ref mut map) = body {
        map.insert("correlation_id".to_string(), serde_json::json!(correlation_id));
    }
    body
}

#[derive(Deserialize)]
pub struct CeremonyQuery {
    correlation_id: String,
}

pub async fn register_start(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    let Some(expected_secret) = state.webauthn.bootstrap_secret.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "registration is disabled: CHOOSH_RELAYD_BOOTSTRAP_SECRET is not set"
            })),
        )
            .into_response();
    };
    let provided_secret = headers.get(BOOTSTRAP_SECRET_HEADER).and_then(|value| value.to_str().ok());
    if !provided_secret.is_some_and(|provided| bootstrap_secret_matches(expected_secret, provided)) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "missing or invalid bootstrap secret" })),
        )
            .into_response();
    }

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
            let correlation_id = generate_correlation_id();
            let now = now_unix();
            {
                let mut in_flight = state.webauthn.in_flight_registration.write().await;
                prune_expired_ceremonies(&mut in_flight, now);
                in_flight.insert(correlation_id.clone(), (reg_state, now));
            }
            (StatusCode::OK, Json(with_correlation_id(&ccr, &correlation_id))).into_response()
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
    Query(query): Query<CeremonyQuery>,
    Json(credential): Json<RegisterPublicKeyCredential>,
) -> impl IntoResponse {
    let reg_state = {
        let mut in_flight = state.webauthn.in_flight_registration.write().await;
        prune_expired_ceremonies(&mut in_flight, now_unix());
        in_flight.remove(&query.correlation_id)
    };
    let Some((reg_state, _)) = reg_state else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "no registration in progress for this correlation_id" })),
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
            let correlation_id = generate_correlation_id();
            let now = now_unix();
            {
                let mut in_flight = state.webauthn.in_flight_authentication.write().await;
                prune_expired_ceremonies(&mut in_flight, now);
                in_flight.insert(correlation_id.clone(), (auth_state, now));
            }
            (StatusCode::OK, Json(with_correlation_id(&rcr, &correlation_id))).into_response()
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
    Query(query): Query<CeremonyQuery>,
    Json(credential): Json<PublicKeyCredential>,
) -> impl IntoResponse {
    let auth_state = {
        let mut in_flight = state.webauthn.in_flight_authentication.write().await;
        prune_expired_ceremonies(&mut in_flight, now_unix());
        in_flight.remove(&query.correlation_id)
    };
    let Some((auth_state, _)) = auth_state else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "no authentication in progress for this correlation_id" })),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_state_in, router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    use webauthn_authenticator_rs::WebauthnAuthenticator as _;
    use webauthn_authenticator_rs::prelude::{
        CreationChallengeResponse, PublicKeyCredential, RequestChallengeResponse,
    };
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;

    const TEST_SECRET: &str = "correct-horse-battery-staple";
    const TEST_ORIGIN: &str = "https://choosh.local";

    /// Builds isolated `AppState` with [`TEST_SECRET`] wired up as the
    /// bootstrap secret, bypassing `CHOOSH_RELAYD_BOOTSTRAP_SECRET`
    /// entirely (mutating process env vars from tests is unsound and this
    /// workspace's `unsafe_code = "forbid"` lint correctly rejects it — see
    /// `lib.rs`'s `build_state_in` doc comment for the established pattern
    /// this mirrors: override the field directly via `Arc::get_mut` while
    /// still the sole owner, before the state is shared).
    fn test_state_with_secret(secret: Option<&str>) -> Arc<AppState> {
        let dir = std::env::temp_dir().join(format!("choosh-relayd-webauthn-test-{}", uuid::Uuid::new_v4()));
        let mut state = build_state_in(&dir);
        Arc::get_mut(&mut state)
            .expect("sole owner of state before it's cloned into the router")
            .webauthn
            .bootstrap_secret = secret.map(str::to_string);
        state
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.expect("readable body");
        serde_json::from_slice(&bytes).expect("valid json body")
    }

    async fn post(app: &axum::Router, path: &str, secret_header: Option<&str>, body: serde_json::Value) -> axum::response::Response {
        let mut builder = Request::builder().method("POST").uri(path).header("content-type", "application/json");
        if let Some(secret) = secret_header {
            builder = builder.header(BOOTSTRAP_SECRET_HEADER, secret);
        }
        app.clone()
            .oneshot(builder.body(Body::from(body.to_string())).unwrap())
            .await
            .expect("request completes")
    }

    #[tokio::test]
    async fn register_start_is_refused_when_no_bootstrap_secret_is_configured() {
        let state = test_state_with_secret(None);
        let app = router(state);
        let response = post(&app, "/webauthn/register/start", Some(TEST_SECRET), serde_json::json!({})).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_json(response).await;
        assert!(body["error"].as_str().unwrap().contains("CHOOSH_RELAYD_BOOTSTRAP_SECRET"));
    }

    #[tokio::test]
    async fn register_start_without_a_header_is_rejected() {
        let state = test_state_with_secret(Some(TEST_SECRET));
        let app = router(state);
        let response = post(&app, "/webauthn/register/start", None, serde_json::json!({})).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn register_start_with_the_wrong_secret_is_rejected() {
        let state = test_state_with_secret(Some(TEST_SECRET));
        let app = router(state);
        let response = post(&app, "/webauthn/register/start", Some("not-the-secret"), serde_json::json!({})).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn register_start_with_a_secret_that_is_a_prefix_of_the_real_one_is_rejected() {
        // Guards against an accidental non-constant-time compare that
        // short-circuits (and so behaves differently, timing aside) on a
        // partial match — a prefix is the case most likely to slip through
        // a naive `starts_with`-style bug.
        let state = test_state_with_secret(Some(TEST_SECRET));
        let app = router(state);
        let prefix = &TEST_SECRET[..TEST_SECRET.len() - 1];
        let response = post(&app, "/webauthn/register/start", Some(prefix), serde_json::json!({})).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn register_start_with_the_correct_secret_succeeds_and_returns_a_correlation_id() {
        let state = test_state_with_secret(Some(TEST_SECRET));
        let app = router(state);
        let response = post(&app, "/webauthn/register/start", Some(TEST_SECRET), serde_json::json!({})).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert!(body["correlation_id"].as_str().is_some_and(|id| !id.is_empty()), "{body}");
        assert!(body["publicKey"].is_object(), "{body}");
    }

    /// Drives a real registration ceremony end to end (start -> software
    /// authenticator -> finish), exactly the same "real cryptography, fake
    /// hardware" approach `choosh-android-bridge::dev_passkey` uses against
    /// a live `relayd` — here exercised in-process against the router
    /// directly, so this test suite doesn't need a real TCP listener.
    ///
    /// Returns the [`SoftPasskey`] that performed the ceremony (not a fresh
    /// one — a login ceremony against this same credential later must reuse
    /// this exact instance, since `SoftPasskey` holds its registered
    /// credentials in its own local map, the same way a real hardware
    /// authenticator holds its own keys; a *different* `SoftPasskey`
    /// instance has no idea this credential exists) alongside the minted
    /// session credential.
    async fn complete_registration(app: &axum::Router, secret: &str) -> (SoftPasskey, String) {
        let start_response = post(app, "/webauthn/register/start", Some(secret), serde_json::json!({})).await;
        assert_eq!(start_response.status(), StatusCode::OK);
        let start_body = body_json(start_response).await;
        let correlation_id = start_body["correlation_id"].as_str().expect("correlation_id present").to_string();

        let ccr: CreationChallengeResponse = serde_json::from_value(start_body).expect("valid creation challenge response");
        let mut authenticator = SoftPasskey::new(true);
        let credential: webauthn_authenticator_rs::prelude::RegisterPublicKeyCredential = authenticator
            .do_registration(Url::parse(TEST_ORIGIN).unwrap(), ccr)
            .expect("software authenticator completes registration");
        let credential_json = serde_json::to_value(&credential).expect("credential serializes");

        let finish_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webauthn/register/finish?correlation_id={correlation_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(credential_json.to_string()))
                    .unwrap(),
            )
            .await
            .expect("request completes");
        assert_eq!(finish_response.status(), StatusCode::OK, "registration finish should succeed");
        let finish_body = body_json(finish_response).await;
        let session_credential = finish_body["session_credential"].as_str().expect("session_credential present").to_string();
        (authenticator, session_credential)
    }

    #[tokio::test]
    async fn register_start_and_finish_round_trips_a_real_ceremony() {
        let state = test_state_with_secret(Some(TEST_SECRET));
        let app = router(state.clone());
        let (_authenticator, session_credential) = complete_registration(&app, TEST_SECRET).await;
        assert!(!session_credential.is_empty());
        assert_eq!(state.webauthn.passkeys.read().await.len(), 1);
        assert!(state.registry.phone_sessions.read().await.contains_key(&session_credential));
    }

    #[tokio::test]
    async fn register_finish_with_an_unknown_correlation_id_fails() {
        let state = test_state_with_secret(Some(TEST_SECRET));
        let app = router(state);
        // A structurally valid credential (a real, completed ceremony) —
        // this test is specifically about the correlation-id lookup, not
        // about malformed-body handling, so the body must be well-formed
        // enough that `Json<RegisterPublicKeyCredential>` extraction itself
        // succeeds and the handler's own `correlation_id` check is what
        // actually rejects the request.
        let start_response = post(&app, "/webauthn/register/start", Some(TEST_SECRET), serde_json::json!({})).await;
        let start_body = body_json(start_response).await;
        let ccr: CreationChallengeResponse = serde_json::from_value(start_body).expect("valid creation challenge response");
        let mut authenticator = SoftPasskey::new(true);
        let credential = authenticator.do_registration(Url::parse(TEST_ORIGIN).unwrap(), ccr).expect("ceremony completes");

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webauthn/register/finish?correlation_id=bogus-correlation-id-that-was-never-issued")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&credential).unwrap()))
                    .unwrap(),
            )
            .await
            .expect("request completes");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert!(body["error"].as_str().unwrap().contains("correlation_id"));
    }

    #[tokio::test]
    async fn two_concurrent_registrations_do_not_clobber_each_other() {
        let state = test_state_with_secret(Some(TEST_SECRET));
        let app = router(state.clone());

        // Start BOTH ceremonies before finishing either — this is exactly
        // the scenario the old single-`Option` slot got wrong: the second
        // `start` used to silently overwrite the first ceremony's state.
        let start_1 = post(&app, "/webauthn/register/start", Some(TEST_SECRET), serde_json::json!({})).await;
        let start_1_body = body_json(start_1).await;
        let correlation_id_1 = start_1_body["correlation_id"].as_str().unwrap().to_string();
        let ccr_1: CreationChallengeResponse = serde_json::from_value(start_1_body).unwrap();

        let start_2 = post(&app, "/webauthn/register/start", Some(TEST_SECRET), serde_json::json!({})).await;
        let start_2_body = body_json(start_2).await;
        let correlation_id_2 = start_2_body["correlation_id"].as_str().unwrap().to_string();
        let ccr_2: CreationChallengeResponse = serde_json::from_value(start_2_body).unwrap();

        assert_ne!(correlation_id_1, correlation_id_2, "each ceremony must get its own correlation id");
        assert_eq!(state.webauthn.in_flight_registration.read().await.len(), 2, "both ceremonies' state must coexist");

        // Complete ceremony 2 first, then ceremony 1 — order shouldn't
        // matter, and each must succeed against its own challenge.
        let mut authenticator_2 = SoftPasskey::new(true);
        let credential_2 = authenticator_2.do_registration(Url::parse(TEST_ORIGIN).unwrap(), ccr_2).expect("ceremony 2 completes");
        let finish_2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webauthn/register/finish?correlation_id={correlation_id_2}"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&credential_2).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finish_2.status(), StatusCode::OK, "ceremony 2 must succeed against its own challenge");

        let mut authenticator_1 = SoftPasskey::new(true);
        let credential_1 = authenticator_1.do_registration(Url::parse(TEST_ORIGIN).unwrap(), ccr_1).expect("ceremony 1 completes");
        let finish_1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webauthn/register/finish?correlation_id={correlation_id_1}"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&credential_1).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finish_1.status(), StatusCode::OK, "ceremony 1 must still succeed against its own challenge, unclobbered by ceremony 2");

        assert_eq!(state.webauthn.passkeys.read().await.len(), 2, "both ceremonies must have registered a distinct passkey");
        assert!(state.webauthn.in_flight_registration.read().await.is_empty(), "both entries must be consumed, not leaked");
    }

    #[tokio::test]
    async fn login_start_and_finish_round_trips_a_real_ceremony_after_registration() {
        let state = test_state_with_secret(Some(TEST_SECRET));
        let app = router(state.clone());
        // Reuse the SAME `SoftPasskey` instance that registered — it holds
        // the credential's key material in its own local map, exactly like
        // a real hardware authenticator would; a fresh instance has no way
        // to sign a login assertion for a credential it never registered.
        let (mut authenticator, _) = complete_registration(&app, TEST_SECRET).await;

        let start_response =
            app.clone().oneshot(Request::builder().method("POST").uri("/webauthn/login/start").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(start_response.status(), StatusCode::OK);
        let start_body = body_json(start_response).await;
        let correlation_id = start_body["correlation_id"].as_str().unwrap().to_string();
        let rcr: RequestChallengeResponse = serde_json::from_value(start_body).unwrap();

        let credential: PublicKeyCredential =
            authenticator.do_authentication(Url::parse(TEST_ORIGIN).unwrap(), rcr).expect("software authenticator completes login");

        let finish_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webauthn/login/finish?correlation_id={correlation_id}"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&credential).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finish_response.status(), StatusCode::OK);
        let finish_body = body_json(finish_response).await;
        assert!(finish_body["session_credential"].as_str().is_some_and(|s| !s.is_empty()));
    }

    #[tokio::test]
    async fn two_concurrent_logins_do_not_clobber_each_other() {
        let state = test_state_with_secret(Some(TEST_SECRET));
        let app = router(state.clone());
        // Same `SoftPasskey` instance for both login attempts, deliberately
        // — this is one real passkey being asked to sign two different
        // in-flight challenges in sequence (a fresh instance per attempt
        // would fail with "credential not found", since it never
        // registered anything).
        let (mut authenticator, _) = complete_registration(&app, TEST_SECRET).await;

        let start_1 =
            app.clone().oneshot(Request::builder().method("POST").uri("/webauthn/login/start").body(Body::empty()).unwrap()).await.unwrap();
        let start_1_body = body_json(start_1).await;
        let correlation_id_1 = start_1_body["correlation_id"].as_str().unwrap().to_string();
        let rcr_1: RequestChallengeResponse = serde_json::from_value(start_1_body).unwrap();

        let start_2 =
            app.clone().oneshot(Request::builder().method("POST").uri("/webauthn/login/start").body(Body::empty()).unwrap()).await.unwrap();
        let start_2_body = body_json(start_2).await;
        let correlation_id_2 = start_2_body["correlation_id"].as_str().unwrap().to_string();
        let rcr_2: RequestChallengeResponse = serde_json::from_value(start_2_body).unwrap();

        assert_ne!(correlation_id_1, correlation_id_2);
        assert_eq!(state.webauthn.in_flight_authentication.read().await.len(), 2);

        let credential_2 = authenticator.do_authentication(Url::parse(TEST_ORIGIN).unwrap(), rcr_2).expect("login 2 completes");
        let finish_2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webauthn/login/finish?correlation_id={correlation_id_2}"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&credential_2).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finish_2.status(), StatusCode::OK);

        let credential_1 = authenticator.do_authentication(Url::parse(TEST_ORIGIN).unwrap(), rcr_1).expect("login 1 completes");
        let finish_1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/webauthn/login/finish?correlation_id={correlation_id_1}"))
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&credential_1).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finish_1.status(), StatusCode::OK, "login 1 must still succeed, unclobbered by login 2");
    }

    #[test]
    fn bootstrap_secret_matches_is_true_only_for_an_exact_match() {
        assert!(bootstrap_secret_matches("abc123", "abc123"));
        assert!(!bootstrap_secret_matches("abc123", "abc124"));
        assert!(!bootstrap_secret_matches("abc123", "abc12"));
        assert!(!bootstrap_secret_matches("abc123", "abc1234"));
        assert!(!bootstrap_secret_matches("abc123", ""));
    }

    #[test]
    fn prune_expired_ceremonies_removes_only_stale_entries() {
        let now = 1_000_000u64;
        let mut map: HashMap<String, (u32, u64)> = HashMap::new();
        map.insert("fresh".to_string(), (1, now - 10));
        map.insert("stale".to_string(), (2, now - IN_FLIGHT_CEREMONY_TTL_SECONDS - 1));
        prune_expired_ceremonies(&mut map, now);
        assert!(map.contains_key("fresh"));
        assert!(!map.contains_key("stale"));
    }
}
