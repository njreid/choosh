//! Local persistence for the devhost's enrollment credential: the Ed25519
//! keypair generated at first enrollment (never leaves this process except
//! written to this file) plus the `device_id`/`certificate` `relayd` issued
//! for it. See `docs/specs/auth-and-enrollment.md`'s "Devhost enrollment".

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signature, SigningKey, Signer};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Credential {
    pub device_id: String,
    /// Base64-encoded certificate `relayd` issued at enrollment.
    pub certificate: String,
    /// Base64-encoded 32-byte Ed25519 secret key.
    signing_key_b64: String,
}

impl Credential {
    #[must_use]
    pub fn new(device_id: String, certificate: String, signing_key: &SigningKey) -> Self {
        Self {
            device_id,
            certificate,
            signing_key_b64: BASE64.encode(signing_key.to_bytes()),
        }
    }

    /// # Errors
    ///
    /// Returns [`CredentialError::Corrupt`] if the stored key material is
    /// not a valid base64-encoded 32-byte Ed25519 secret key.
    pub fn signing_key(&self) -> Result<SigningKey, CredentialError> {
        let bytes = BASE64
            .decode(&self.signing_key_b64)
            .map_err(|error| CredentialError::Corrupt(format!("signing key is not valid base64: {error}")))?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| CredentialError::Corrupt("signing key is not 32 bytes".to_string()))?;
        Ok(SigningKey::from_bytes(&bytes))
    }

    /// Signs `message` with the stored private key, for the connect-time
    /// challenge/signature step in auth-and-enrollment.md.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialError::Corrupt`] if the stored key material is
    /// invalid.
    pub fn sign(&self, message: &[u8]) -> Result<Signature, CredentialError> {
        Ok(self.signing_key()?.sign(message))
    }
}

#[derive(Debug)]
pub enum CredentialError {
    Io(io::Error),
    /// The credential file exists but its contents are not a valid,
    /// well-formed credential. This is deliberately distinct from "no file"
    /// (see [`load`]) — a corrupt credential MUST be a hard error, not
    /// silently treated as "not yet enrolled", since that would mask
    /// whatever wrote (or partially wrote, or truncated) the file.
    Corrupt(String),
    NoConfigDir,
}

impl std::fmt::Display for CredentialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "credential file I/O error: {error}"),
            Self::Corrupt(reason) => write!(f, "credential file is corrupt: {reason}"),
            Self::NoConfigDir => write!(f, "could not determine a config directory for choosh-hostd"),
        }
    }
}

impl std::error::Error for CredentialError {}

impl From<io::Error> for CredentialError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// The default per-user credential file location:
/// `<config dir>/choosh-hostd/device-credential.json`.
///
/// # Errors
///
/// Returns [`CredentialError::NoConfigDir`] if no config directory can be
/// determined for the current user (e.g. `$HOME` is unset).
pub fn default_path() -> Result<PathBuf, CredentialError> {
    let dirs = directories::ProjectDirs::from("ai", "choosh", "hostd").ok_or(CredentialError::NoConfigDir)?;
    Ok(dirs.config_dir().join("device-credential.json"))
}

/// Loads the credential at `path`. Returns `Ok(None)` if the file does not
/// exist (this device has not enrolled yet) and `Err` for every other
/// failure, including a present-but-malformed file.
///
/// # Errors
///
/// Returns [`CredentialError::Corrupt`] for a present, unreadable-as-JSON,
/// or structurally invalid file, and [`CredentialError::Io`] for any I/O
/// failure other than "file does not exist".
pub fn load(path: &Path) -> Result<Option<Credential>, CredentialError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(CredentialError::Io(error)),
    };
    let credential: Credential = serde_json::from_slice(&bytes)
        .map_err(|error| CredentialError::Corrupt(format!("invalid JSON: {error}")))?;
    // Re-validate the embedded key material eagerly, so a caller that only
    // checks `load(..).is_ok()` still fails closed on a truncated key
    // rather than deferring the failure to first use.
    credential.signing_key()?;
    Ok(Some(credential))
}

/// Persists `credential` to `path` atomically (write to a sibling temp file,
/// then rename) with `0600` permissions on Unix, so the private key is
/// never briefly world-readable and a crash mid-write never leaves a
/// truncated file at the real path.
///
/// # Errors
///
/// Returns [`CredentialError::Io`] if the parent directory cannot be
/// created or the write/rename fails.
pub fn save(path: &Path, credential: &Credential) -> Result<(), CredentialError> {
    let parent = path.parent().ok_or_else(|| {
        CredentialError::Io(io::Error::new(io::ErrorKind::InvalidInput, "credential path has no parent directory"))
    })?;
    fs::create_dir_all(parent)?;

    let tmp_path = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("device-credential.json"),
        std::process::id()
    ));
    let json = serde_json::to_vec_pretty(credential)
        .map_err(|error| CredentialError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))?;
    fs::write(&tmp_path, &json)?;
    set_owner_only_permissions(&tmp_path)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample_credential() -> Credential {
        let signing_key = SigningKey::generate(&mut rand::rng());
        Credential::new("dev-1".to_string(), "cert-bytes".to_string(), &signing_key)
    }

    #[test]
    fn missing_file_loads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-credential.json");
        assert!(matches!(load(&path), Ok(None)));
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("device-credential.json");
        let credential = sample_credential();

        save(&path, &credential).unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded, credential);
    }

    #[test]
    #[cfg(unix)]
    fn saved_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-credential.json");
        save(&path, &sample_credential()).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn corrupt_json_is_a_hard_error_not_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-credential.json");
        fs::write(&path, b"not json").unwrap();

        match load(&path) {
            Err(CredentialError::Corrupt(_)) => {}
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn truncated_signing_key_is_corrupt_not_silently_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-credential.json");
        let mut credential = sample_credential();
        credential.signing_key_b64 = BASE64.encode(b"too-short");
        fs::write(&path, serde_json::to_vec(&credential).unwrap()).unwrap();

        match load(&path) {
            Err(CredentialError::Corrupt(_)) => {}
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }

    #[test]
    fn sign_produces_a_verifiable_signature() {
        use ed25519_dalek::Verifier;

        let credential = sample_credential();
        let message = b"nonce-bytes";
        let signature = credential.sign(message).unwrap();
        let verifying_key = credential.signing_key().unwrap().verifying_key();
        assert!(verifying_key.verify(message, &signature).is_ok());
    }
}
