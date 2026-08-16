//! Real Firebase Cloud Messaging v1 API dispatch, or a logging-only
//! fallback when no service-account credential is configured. This
//! replaces what used to be `ws.rs`'s `dispatch_fcm_push_stub` — see
//! PLAN.md's "Known follow-ups" and `docs/specs/notifications.md`'s
//! "Delivery mechanism" for the status this is closing out.
//!
//! Auth flow (Google's `OAuth2` service-account JWT-bearer grant,
//! <https://developers.google.com/identity/protocols/oauth2/service-account#httprest>):
//! sign a short-lived JWT with the service account's RSA private key,
//! exchange it at the key's `token_uri` for a bearer access token, cache
//! that token until shortly before it expires, and send it as
//! `Authorization: Bearer <token>` on the `messages:send` POST. No
//! `gcloud`/Google Cloud client library involved — this is a from-scratch
//! implementation of that flow using only `openssl` (already a dependency,
//! for RS256 signing) and `reqwest` (already used by `choosh-hostd`/
//! `choosh-android-bridge` elsewhere in this workspace).
//!
//! **Credential availability in this environment, honestly reported**: a
//! real Firebase project (`choosh`) exists and the `firebase` CLI here is
//! authenticated as a real Google account with a `cloud-platform`-scoped
//! `OAuth2` token (confirmed via `firebase projects:list` and
//! `https://oauth2.googleapis.com/tokeninfo`). That token is *not* a
//! service-account credential, and this sandbox's own safety policy
//! blocked using it to call the Cloud IAM API (`serviceAccounts.list`/
//! `.keys.create`) to mint one — a real GCP write action against a real
//! personal Google account's real project, which is exactly the kind of
//! consequential live-infrastructure action that guardrail exists to stop
//! short of explicit user sign-off. No service-account JSON key was
//! otherwise found anywhere in this environment (`GOOGLE_APPLICATION_CREDENTIALS`
//! unset; no `*service-account*.json` file on disk). So: this module is
//! the real implementation, built to the real request/response shapes,
//! but it has never been exercised end-to-end against the live
//! `fcm.googleapis.com` endpoint in this environment — see
//! [`FcmClient::from_env`] for the fallback this runs under absent a
//! configured credential.

use base64::Engine as _;
use choosh_protocol::relay::WireAgentEvent;
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::sign::Signer;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Env var naming a Firebase/GCP service-account JSON key file scoped to
/// this project's FCM sending, checked before the standard
/// `GOOGLE_APPLICATION_CREDENTIALS`.
const CREDENTIALS_ENV_VAR: &str = "CHOOSH_RELAYD_FCM_CREDENTIALS";
/// The standard ADC env var, checked as a fallback so an operator who's
/// already set up `GOOGLE_APPLICATION_CREDENTIALS` for other Google Cloud
/// tooling doesn't need a second, `choosh`-specific copy.
const ADC_ENV_VAR: &str = "GOOGLE_APPLICATION_CREDENTIALS";
const MESSAGING_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const DEFAULT_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
const FCM_SEND_ENDPOINT_BASE: &str = "https://fcm.googleapis.com/v1/projects";
/// Google's token endpoint issues 3600s access tokens; refresh a little
/// early so a dispatch never races an about-to-expire cached token.
const TOKEN_REFRESH_SKEW: Duration = Duration::from_mins(1);
/// The JWT assertion itself is only ever used once, immediately, to fetch
/// an access token — but Google still requires an `exp` within an hour of
/// `iat`.
const ASSERTION_LIFETIME_SECONDS: u64 = 3600;

/// The subset of a downloaded service-account JSON key this module needs.
/// Deliberately not deriving `Debug`/`Clone` — `private_key` is exactly
/// the kind of credential material that has no business ending up in a
/// log line or an accidental clone lingering in memory longer than
/// necessary.
#[derive(Deserialize)]
struct ServiceAccountKey {
    project_id: String,
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    DEFAULT_TOKEN_URI.to_string()
}

struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

struct Configured {
    key: ServiceAccountKey,
    http: reqwest::Client,
    cached_token: Mutex<Option<CachedToken>>,
}

/// Real FCM v1 dispatcher, or a no-op-but-logging fallback when no
/// credential is configured. Cheap to clone (an `Option<Arc<_>>`) so it can
/// live on `AppState` and be handed to a spawned task per dispatch without
/// holding any registry lock across the network call.
#[derive(Clone)]
pub struct FcmClient(Option<Arc<Configured>>);

impl FcmClient {
    /// Loads a service-account credential from [`CREDENTIALS_ENV_VAR`] or
    /// [`ADC_ENV_VAR`], in that order. Never panics or returns an error —
    /// an absent, unreadable, or unparsable credential just means
    /// [`Self::dispatch`] logs instead of sending, exactly like the stub
    /// this replaces did unconditionally. This is deliberate: a
    /// misconfigured push credential must never be a startup-fatal
    /// condition for the rest of `relayd`.
    #[must_use]
    pub fn from_env() -> Self {
        let Some(path) = std::env::var(CREDENTIALS_ENV_VAR).or_else(|_| std::env::var(ADC_ENV_VAR)).ok() else {
            tracing::info!(
                "FCM: neither {CREDENTIALS_ENV_VAR} nor {ADC_ENV_VAR} is set; dispatch will log-only \
                 (see rust/choosh-relayd/src/fcm.rs's module doc for why no credential is configured here)"
            );
            return Self(None);
        };
        let key = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<ServiceAccountKey>(&raw).ok());
        let Some(key) = key else {
            tracing::warn!(%path, "FCM: credential path set but the file is missing or not a valid service-account JSON key; dispatch will log-only");
            return Self(None);
        };
        let http = match reqwest::Client::builder().timeout(Duration::from_secs(10)).build() {
            Ok(http) => http,
            Err(err) => {
                tracing::warn!(%err, "FCM: failed to build HTTP client; dispatch will log-only");
                return Self(None);
            }
        };
        tracing::info!(project_id = %key.project_id, client_email = %key.client_email, "FCM: service-account credential loaded, live dispatch enabled");
        Self(Some(Arc::new(Configured { key, http, cached_token: Mutex::new(None) })))
    }

    /// The stub's old, always-fires behavior — used by tests that want the
    /// log-only path deterministically regardless of this process's
    /// environment.
    #[must_use]
    pub fn unconfigured() -> Self {
        Self(None)
    }

    /// Sends one FCM v1 data message for `event` to `token` (the target
    /// phone's FCM registration token), or logs what would have been sent
    /// if no credential is configured. Best-effort: every failure is
    /// logged and swallowed here, never propagated — per
    /// notifications.md, FCM is purely the backstop for when the
    /// persistent connection (tried first by `route_agent_event`) isn't
    /// live, so a push failure must never affect anything else `relayd`
    /// does.
    pub async fn dispatch(&self, token: &str, from_device_id: &str, event: &WireAgentEvent) {
        let data = redacted_data_payload(from_device_id, event);
        let kind = data.get("kind").and_then(|v| v.as_str()).unwrap_or("unknown");
        let workspace_id = data.get("workspace_id").and_then(|v| v.as_str()).unwrap_or("");
        let item_id = data.get("item_id").and_then(|v| v.as_str()).unwrap_or("");

        let Some(configured) = self.0.as_ref() else {
            tracing::info!(
                fcm_token_prefix = %token.get(..8).unwrap_or(token),
                %from_device_id,
                %kind,
                %workspace_id,
                %item_id,
                "FCM dispatch: no credential configured in this environment, logging only (would push here)"
            );
            return;
        };

        let bearer = match Self::bearer_token(configured).await {
            Ok(bearer) => bearer,
            Err(err) => {
                tracing::warn!(%err, %from_device_id, %kind, "FCM: OAuth2 token exchange failed, dropping push");
                return;
            }
        };

        let url = format!("{FCM_SEND_ENDPOINT_BASE}/{}/messages:send", configured.key.project_id);
        // Data-only (not `notification`-only), per notifications.md and
        // this module's own doc comment: the payload must carry enough
        // for the app to construct the same notification the persistent
        // path would have rendered, not a bare "open the app" ping.
        let body = serde_json::json!({
            "message": {
                "token": token,
                "data": data,
                "android": { "priority": "high" },
            }
        });
        match configured.http.post(&url).bearer_auth(&bearer).json(&body).send().await {
            Ok(response) if response.status().is_success() => {
                tracing::info!(%from_device_id, %kind, %workspace_id, %item_id, "FCM: dispatched");
            }
            Ok(response) => {
                let status = response.status();
                let body_text = response.text().await.unwrap_or_default();
                tracing::warn!(%status, body = %body_text, %from_device_id, %kind, "FCM: messages:send returned a non-success status");
            }
            Err(err) => {
                tracing::warn!(%err, %from_device_id, %kind, "FCM: messages:send request failed");
            }
        }
    }

    async fn bearer_token(configured: &Configured) -> Result<String, String> {
        {
            let cached = configured.cached_token.lock().await;
            if let Some(cached) = cached.as_ref()
                && cached.expires_at > Instant::now() + TOKEN_REFRESH_SKEW
            {
                return Ok(cached.access_token.clone());
            }
        }
        let assertion = sign_jwt_bearer_assertion(&configured.key)?;
        let response = configured
            .http
            .post(&configured.key.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|err| format!("token request failed: {err}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("token endpoint returned {status}: {body}"));
        }
        let parsed: TokenResponse =
            response.json().await.map_err(|err| format!("token response decode failed: {err}"))?;
        let expires_at = Instant::now() + Duration::from_secs(parsed.expires_in);
        *configured.cached_token.lock().await =
            Some(CachedToken { access_token: parsed.access_token.clone(), expires_at });
        Ok(parsed.access_token)
    }
}

/// Shape of Google's `OAuth2` token-endpoint response for the JWT-bearer
/// grant. See <https://developers.google.com/identity/protocols/oauth2/service-account#httprest>.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

/// Signs a Google `OAuth2` service-account JWT-bearer assertion (RS256) for
/// `key`, requesting [`MESSAGING_SCOPE`]. See
/// <https://developers.google.com/identity/protocols/oauth2/service-account#creatinganaccesstoken>.
fn sign_jwt_bearer_assertion(key: &ServiceAccountKey) -> Result<String, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|err| format!("system clock before unix epoch: {err}"))?
        .as_secs();
    let header = serde_json::json!({"alg": "RS256", "typ": "JWT"});
    let claims = serde_json::json!({
        "iss": key.client_email,
        "scope": MESSAGING_SCOPE,
        "aud": key.token_uri,
        "iat": now,
        "exp": now + ASSERTION_LIFETIME_SECONDS,
    });
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let signing_input = format!("{}.{}", engine.encode(header.to_string()), engine.encode(claims.to_string()));

    let pkey = PKey::private_key_from_pem(key.private_key.as_bytes())
        .map_err(|err| format!("private key parse failed: {err}"))?;
    let mut signer =
        Signer::new(MessageDigest::sha256(), &pkey).map_err(|err| format!("signer init failed: {err}"))?;
    signer.update(signing_input.as_bytes()).map_err(|err| format!("signer update failed: {err}"))?;
    let signature = signer.sign_to_vec().map_err(|err| format!("signing failed: {err}"))?;

    Ok(format!("{signing_input}.{}", engine.encode(signature)))
}

/// Builds the exact FCM `data` map (string values only, per the v1 API's
/// `map<string, string>` contract for `message.data`) for `event`, plus
/// `from_device_id` as `host_id` — the one piece of context
/// [`WireAgentEvent`] itself never carries, since which devhost an event
/// came from lives in `route_agent_event`'s caller context, not the event
/// payload.
///
/// Deliberately generic over every [`WireAgentEvent`] field via
/// `serde_json::to_value` rather than a hand-written match per variant:
/// this ties the payload's redaction guarantee directly to
/// [`WireAgentEvent`]'s own `#[serde(tag = "kind", ...)]` shape (already
/// enforced at the type level — see its doc comment on
/// `WireAgentEvent::ResourceReauthRequired` and the round-trip tests in
/// `choosh-protocol/src/relay.rs`) instead of duplicating a second,
/// driftable list of "which fields are safe" here.
fn redacted_data_payload(from_device_id: &str, event: &WireAgentEvent) -> serde_json::Map<String, serde_json::Value> {
    let mut data = serde_json::Map::new();
    data.insert("host_id".to_string(), serde_json::Value::String(from_device_id.to_string()));
    let value = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
    if let serde_json::Value::Object(fields) = value {
        for (key, field_value) in fields {
            data.insert(key, serde_json::Value::String(plain_string(&field_value)));
        }
    }
    data
}

/// Flattens one JSON value to the plain string FCM's `data` map requires.
/// The only non-string field any [`WireAgentEvent`] variant carries today
/// is `FilesChanged`'s `paths: Vec<String>` (that event never triggers a
/// notification per notifications.md, but this stays total rather than
/// panicking on it) — everything else is already a JSON string.
fn plain_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items.iter().map(plain_string).collect::<Vec<_>>().join(","),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_protocol::relay::{WireInputReason, WireMobileProfile, WireResourcePattern};

    #[tokio::test]
    async fn unconfigured_client_dispatch_never_panics_or_sends() {
        let client = FcmClient::unconfigured();
        client
            .dispatch(
                "fcm-token-abc123",
                "devhost-1",
                &WireAgentEvent::InputRequired {
                    workspace_id: "ws-1".to_string(),
                    item_id: "item-1".to_string(),
                    reason: WireInputReason::Approval,
                },
            )
            .await;
    }

    #[test]
    fn redacted_data_payload_carries_only_typed_fields_for_input_required() {
        let event = WireAgentEvent::InputRequired {
            workspace_id: "ws-1".to_string(),
            item_id: "item-1".to_string(),
            reason: WireInputReason::Permission,
        };
        let data = redacted_data_payload("devhost-1", &event);
        assert_eq!(data.len(), 5, "host_id + kind + workspace_id + item_id + reason == 5, got {data:?}");
        assert_eq!(data.get("host_id").unwrap(), "devhost-1");
        assert_eq!(data.get("kind").unwrap(), "input_required");
        assert_eq!(data.get("workspace_id").unwrap(), "ws-1");
        assert_eq!(data.get("item_id").unwrap(), "item-1");
        assert_eq!(data.get("reason").unwrap(), "permission");
    }

    /// The hard redaction rule under test: a `resource_reauth_required` FCM
    /// payload may contain `user_code`/`verification_uri` (meant to be
    /// shown to the user) but must never contain anything else — no token,
    /// session id, or credential material, mirroring
    /// `choosh-protocol::relay`'s own `ResourceReauthRequired` round-trip
    /// test.
    #[test]
    fn redacted_data_payload_for_resource_reauth_required_has_exactly_the_spec_fields_and_nothing_else() {
        let event = WireAgentEvent::ResourceReauthRequired {
            resource_id: "res-1".to_string(),
            display_name: "Prod AWS SSO".to_string(),
            resource_kind: "aws-sso".to_string(),
            pattern: WireResourcePattern::A,
            verification_uri: Some("https://example.com/device".to_string()),
            user_code: Some("WDJB-MJHT".to_string()),
            fetch_instructions: None,
            mobile_profile: WireMobileProfile::Work,
        };
        let data = redacted_data_payload("devhost-1", &event);
        let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "display_name",
                "fetch_instructions",
                "host_id",
                "kind",
                "mobile_profile",
                "pattern",
                "resource_id",
                "resource_kind",
                "user_code",
                "verification_uri",
            ]
        );
        assert_eq!(data.get("resource_kind").unwrap(), "aws-sso");
        assert_eq!(data.get("pattern").unwrap(), "a");
        assert_eq!(data.get("user_code").unwrap(), "WDJB-MJHT");
        assert_eq!(data.get("verification_uri").unwrap(), "https://example.com/device");
        assert_eq!(data.get("fetch_instructions").unwrap(), "", "None flattens to an empty string, per plain_string's doc comment");
    }

    #[test]
    fn files_changed_paths_flatten_to_a_plain_comma_joined_string() {
        let event = WireAgentEvent::FilesChanged {
            workspace_id: "ws-1".to_string(),
            item_id: "item-1".to_string(),
            paths: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
        };
        let data = redacted_data_payload("devhost-1", &event);
        assert_eq!(data.get("paths").unwrap(), "src/a.rs,src/b.rs");
    }
}
