//! Debug-build-only software `WebAuthn` authenticator, for devices with no
//! real platform passkey provider — e.g. this project's own Genymotion test
//! instances, which have no Google Play Services and so fail
//! [`androidx.credentials.CredentialManager`]'s `createCredential`/
//! `getCredential` with `CreateCredentialNoCreateOptionException: No create
//! options available.` before the ceremony can even start.
//!
//! This module exists only when the `dev-passkey` Cargo feature is enabled
//! — `android/app/build.gradle.kts`'s `buildRustAndroid` task only ever
//! passes `--features dev-passkey` for the `debug` Android build type, so a
//! release `.so` never contains this code (or the JNI symbols
//! [`crate::lib`]'s `native_method!`-generated exports for it) at all, not
//! just at a runtime-checked flag. The Kotlin side mirrors this: only
//! `android/app/src/debug/java/ai/choosh/connection/DevPasskeyHooks.kt`
//! ever calls the native methods this module backs; the `src/main` version
//! of that same file (which is what a release build actually compiles) is
//! a stub that always reports itself unavailable.
//!
//! **This does not weaken `relayd`'s `WebAuthn` verification in any way.**
//! It only replaces the *client-side* ceremony fulfillment normally done by
//! the OS's platform authenticator — the actual cryptography is real: a
//! fresh P-256 keypair is generated per ceremony via
//! [`webauthn_authenticator_rs::softpasskey::SoftPasskey`] (from the same
//! upstream project, `kanidm/webauthn-rs`, as the `webauthn-rs`/
//! `webauthn-rs-core` crates `choosh-relayd` itself uses server-side), and
//! `relayd` performs the exact same signature/attestation verification it
//! would against a real hardware passkey. Nothing is hardcoded or shared
//! across installs — each [`register`] call generates its own fresh
//! keypair via a throwaway `SoftPasskey`, held only for the duration of
//! that single ceremony; there is deliberately no persistence across app
//! restarts (this module exists to unblock a device with no credential
//! provider, not to build a parallel credential-storage system). Only
//! registration is implemented — nothing in the Android UI ever re-runs a
//! `WebAuthn` login ceremony (a stored `relayd` session credential is
//! reused directly instead, per `ConnectionViewModel`), so there is no real
//! caller for a dev-mode login path to serve.

use webauthn_authenticator_rs::WebauthnAuthenticator as _;
use webauthn_authenticator_rs::prelude::{CreationChallengeResponse, RegisterPublicKeyCredential, Url};
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

/// Fulfills one `WebAuthn` registration ceremony against `origin` (the
/// relay's own configured HTTP origin — the same value already threaded
/// through as `Engine::http_base_url`, never a hardcoded domain, so this
/// works against whatever relay a debug build happens to point at) given
/// `creation_options_json` (the exact JSON `Engine::webauthn_register_start`
/// already returns — the `CreationChallengeResponse` shape `relayd`'s
/// `/webauthn/register/start` produces). Returns the JSON
/// `RegisterPublicKeyCredential` body `Engine::webauthn_register_finish`
/// expects, byte-for-byte the same shape Android's own Credential Manager
/// would have produced.
///
/// # Errors
///
/// A plain `String` message on a malformed origin/JSON, or the underlying
/// [`webauthn_authenticator_rs::error::WebauthnCError`] formatted for
/// display — never a panic, matching every other engine-facing function in
/// this crate.
pub fn register(origin: &str, creation_options_json: &str) -> Result<String, String> {
    let origin = Url::parse(origin).map_err(|error| format!("invalid relay origin: {error}"))?;
    let ccr: CreationChallengeResponse =
        serde_json::from_str(creation_options_json).map_err(|error| format!("invalid creation options: {error}"))?;
    // `falsify_uv: true` — `SoftPasskey`'s own doc/test usage: a software
    // authenticator has no biometric/PIN of its own to actually verify, but
    // asserting the UV flag lets registration succeed against an RP that
    // requests (without strictly requiring) user verification, exactly as
    // `relayd`'s default `webauthn-rs` builder does.
    let mut authenticator = SoftPasskey::new(true);
    let credential: RegisterPublicKeyCredential = authenticator
        .do_registration(origin, ccr)
        .map_err(|error| format!("dev passkey registration failed: {error:?}"))?;
    serde_json::to_string(&credential).map_err(|error| format!("failed to serialize dev passkey credential: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A real, minimal `CreationChallengeResponse` shape (per
    /// `webauthn-rs-proto`'s JSON serialization — base64url-encoded
    /// `challenge`/`user.id`, `pubKeyCredParams` naming ES256 (`alg: -7`)),
    /// built by hand rather than round-tripped through a live `relayd`
    /// instance, since this test only needs to prove [`register`] can drive
    /// a real ceremony end to end against a well-formed options payload —
    /// `PLAN.md`'s follow-up notes cover live-`relayd` verification
    /// separately.
    fn sample_creation_options_json() -> String {
        json!({
            "publicKey": {
                "rp": {"name": "Choosh", "id": "example.test"},
                "user": {"id": "AQIDBA", "name": "choosh", "displayName": "Choosh"},
                "challenge": "AAECAwQFBgcICQoLDA0ODw",
                "pubKeyCredParams": [{"type": "public-key", "alg": -7}],
                "timeout": 60000,
                "excludeCredentials": [],
                "authenticatorSelection": {"userVerification": "preferred", "requireResidentKey": false},
                "attestation": "none",
                "extensions": {},
            }
        })
        .to_string()
    }

    #[test]
    fn register_produces_a_well_formed_credential_json_for_a_real_ceremony() {
        let body = register("https://example.test", &sample_creation_options_json()).expect("registration should succeed");
        let parsed: serde_json::Value = serde_json::from_str(&body).expect("output must be valid JSON");
        assert_eq!(parsed["type"], "public-key");
        assert!(parsed["id"].as_str().is_some_and(|id| !id.is_empty()), "{body}");
        assert!(parsed["response"]["attestationObject"].as_str().is_some_and(|s| !s.is_empty()), "{body}");
        assert!(parsed["response"]["clientDataJSON"].as_str().is_some_and(|s| !s.is_empty()), "{body}");

        // The real clientDataJSON the ceremony embedded must carry the
        // exact origin and challenge it was given — this is the crux of a
        // genuine ceremony, not a stubbed-out response.
        let client_data_b64 = parsed["response"]["clientDataJSON"].as_str().unwrap();
        let client_data_bytes =
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, client_data_b64).unwrap();
        let client_data: serde_json::Value = serde_json::from_slice(&client_data_bytes).unwrap();
        assert_eq!(client_data["type"], "webauthn.create");
        // `Url::parse("https://example.test")` normalizes to
        // `"https://example.test/"` (trailing slash) — both this
        // ceremony's client side and `relayd`'s own `webauthn.rs::build`
        // parse `CHOOSH_RELAYD_RP_ORIGIN` through the same `url` crate, so
        // this is consistent on both ends, not an interop bug; asserting
        // exact string identity here would just be testing the `url`
        // crate's own normalization rules.
        assert_eq!(client_data["origin"], "https://example.test/");
    }

    #[test]
    fn register_rejects_an_invalid_origin() {
        let error = register("not a url", &sample_creation_options_json()).unwrap_err();
        assert!(error.contains("invalid relay origin"), "{error}");
    }

    #[test]
    fn register_rejects_malformed_creation_options() {
        let error = register("https://example.test", "{not json").unwrap_err();
        assert!(error.contains("invalid creation options"), "{error}");
    }
}
