//! `relayd`'s internal enrollment CA: a single Ed25519 signing key that
//! binds a device's public key to a freshly assigned `device_id`, per
//! auth-and-enrollment.md's devhost/laptop-proxy enrollment sequence. This
//! is not a public CA and issues no certificates outside this system.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as base64_engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey, ed25519::signature::Verifier};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Certificate validity: 1 year, per auth-and-enrollment.md.
pub const CERTIFICATE_VALIDITY_SECONDS: u64 = 365 * 24 * 60 * 60;

#[derive(Debug)]
pub enum CaError {
    Io(std::io::Error),
    Corrupt,
}

impl std::fmt::Display for CaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "enrollment CA key I/O error: {err}"),
            Self::Corrupt => write!(f, "enrollment CA key file is corrupt"),
        }
    }
}

impl std::error::Error for CaError {}

/// Loads the enrollment CA's signing key from `state_dir/enrollment-ca.key`,
/// generating and persisting a fresh one on first run. Persisting (rather
/// than regenerating per-process) is deliberate: every certificate issued
/// against a CA key becomes unverifiable the moment that key changes, which
/// would invalidate every enrolled device's stored credential on a routine
/// `relayd` restart.
pub fn load_or_generate(state_dir: &Path) -> Result<SigningKey, CaError> {
    let key_path = state_dir.join("enrollment-ca.key");
    if let Ok(bytes) = std::fs::read(&key_path) {
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| CaError::Corrupt)?;
        return Ok(SigningKey::from_bytes(&bytes));
    }
    std::fs::create_dir_all(state_dir).map_err(CaError::Io)?;
    let mut rng = crate::rng::os_rng();
    let signing_key = SigningKey::generate(&mut rng);
    std::fs::write(&key_path, signing_key.to_bytes()).map_err(CaError::Io)?;
    Ok(signing_key)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CertificateBody {
    device_id: String,
    /// Base64-encoded Ed25519 public key.
    public_key: String,
    issued_at_unix: u64,
    not_after_unix: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SignedCertificate {
    body: CertificateBody,
    /// Base64-encoded Ed25519 signature by the enrollment CA over
    /// `serde_json::to_vec(&body)`.
    ca_signature: String,
}

#[derive(Debug, Eq, PartialEq)]
pub enum CertificateError {
    Malformed,
    BadSignature,
    Expired,
}

/// Issues a certificate binding `public_key` to `device_id`, valid for
/// [`CERTIFICATE_VALIDITY_SECONDS`] from `issued_at_unix`. Returns the
/// base64-encoded, wire-ready certificate string.
///
/// # Panics
///
/// Panics if `serde_json` fails to serialize the certificate body, which
/// cannot happen for this fixed, non-recursive shape.
#[must_use]
pub fn issue(
    ca_key: &SigningKey,
    device_id: &str,
    public_key_bytes: &[u8],
    issued_at_unix: u64,
) -> String {
    let body = CertificateBody {
        device_id: device_id.to_string(),
        public_key: base64_engine.encode(public_key_bytes),
        issued_at_unix,
        not_after_unix: issued_at_unix + CERTIFICATE_VALIDITY_SECONDS,
    };
    let body_bytes = serde_json::to_vec(&body).expect("fixed shape always serializes");
    let signature: Signature = ca_key.sign(&body_bytes);
    let signed = SignedCertificate {
        body,
        ca_signature: base64_engine.encode(signature.to_bytes()),
    };
    base64_engine.encode(serde_json::to_vec(&signed).expect("fixed shape always serializes"))
}

/// Verifies a wire-format certificate against the enrollment CA's public
/// key and returns the `device_id` and public key it binds, if valid and
/// unexpired as of `now_unix`.
///
/// # Errors
///
/// Returns [`CertificateError::Malformed`] if the certificate cannot be
/// decoded, [`CertificateError::BadSignature`] if the CA signature does not
/// verify, or [`CertificateError::Expired`] if `now_unix` is past
/// `not_after_unix`.
pub fn verify(
    ca_verifying_key: &VerifyingKey,
    certificate_b64: &str,
    now_unix: u64,
) -> Result<(String, Vec<u8>), CertificateError> {
    let signed_bytes = base64_engine
        .decode(certificate_b64)
        .map_err(|_| CertificateError::Malformed)?;
    let signed: SignedCertificate =
        serde_json::from_slice(&signed_bytes).map_err(|_| CertificateError::Malformed)?;
    let body_bytes = serde_json::to_vec(&signed.body).map_err(|_| CertificateError::Malformed)?;
    let signature_bytes = base64_engine
        .decode(&signed.ca_signature)
        .map_err(|_| CertificateError::Malformed)?;
    let signature_bytes: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| CertificateError::Malformed)?;
    let signature = Signature::from_bytes(&signature_bytes);
    ca_verifying_key
        .verify(&body_bytes, &signature)
        .map_err(|_| CertificateError::BadSignature)?;
    if now_unix > signed.body.not_after_unix {
        return Err(CertificateError::Expired);
    }
    let public_key = base64_engine
        .decode(&signed.body.public_key)
        .map_err(|_| CertificateError::Malformed)?;
    Ok((signed.body.device_id, public_key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ca_key() -> SigningKey {
        SigningKey::generate(&mut crate::rng::os_rng())
    }

    #[test]
    fn issued_certificate_verifies_and_round_trips_device_id() {
        let ca = ca_key();
        let device_key = SigningKey::generate(&mut crate::rng::os_rng());
        let public_key_bytes = device_key.verifying_key().to_bytes();
        let cert = issue(&ca, "dev-1", &public_key_bytes, 1_000);
        let (device_id, public_key) = verify(&ca.verifying_key(), &cert, 1_001).unwrap();
        assert_eq!(device_id, "dev-1");
        assert_eq!(public_key, public_key_bytes.to_vec());
    }

    #[test]
    fn certificate_signed_by_a_different_ca_fails() {
        let ca = ca_key();
        let other_ca = ca_key();
        let device_key = SigningKey::generate(&mut crate::rng::os_rng());
        let cert = issue(&ca, "dev-1", &device_key.verifying_key().to_bytes(), 1_000);
        assert_eq!(
            verify(&other_ca.verifying_key(), &cert, 1_001),
            Err(CertificateError::BadSignature)
        );
    }

    #[test]
    fn expired_certificate_is_rejected() {
        let ca = ca_key();
        let device_key = SigningKey::generate(&mut crate::rng::os_rng());
        let cert = issue(&ca, "dev-1", &device_key.verifying_key().to_bytes(), 1_000);
        let far_future = 1_000 + CERTIFICATE_VALIDITY_SECONDS + 1;
        assert_eq!(
            verify(&ca.verifying_key(), &cert, far_future),
            Err(CertificateError::Expired)
        );
    }

    #[test]
    fn tampered_certificate_fails() {
        let ca = ca_key();
        let device_key = SigningKey::generate(&mut crate::rng::os_rng());
        let cert = issue(&ca, "dev-1", &device_key.verifying_key().to_bytes(), 1_000);
        let mut tampered_bytes = base64_engine.decode(&cert).unwrap();
        *tampered_bytes.last_mut().unwrap() ^= 0xFF;
        let tampered = base64_engine.encode(tampered_bytes);
        assert!(verify(&ca.verifying_key(), &tampered, 1_001).is_err());
    }

    #[test]
    fn load_or_generate_persists_across_calls() {
        let dir = std::env::temp_dir().join(format!("choosh-relayd-ca-test-{}", uuid::Uuid::new_v4()));
        let first = load_or_generate(&dir).unwrap();
        let second = load_or_generate(&dir).unwrap();
        assert_eq!(first.to_bytes(), second.to_bytes());
        std::fs::remove_dir_all(&dir).ok();
    }
}
