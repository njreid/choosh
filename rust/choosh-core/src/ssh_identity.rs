//! Opaque SSH credential metadata at the Android import boundary.
//!
//! This module deliberately has no representation of a private-key document,
//! a passphrase, a file path, or a document URI.  A platform adapter performs
//! the user-mediated import and retains the sensitive material in its
//! keystore-backed store.  The domain receives only the resulting opaque
//! reference and non-secret public-key metadata.

use std::fmt;

const MAX_CREDENTIAL_REF_BYTES: usize = 128;
const SHA256_FINGERPRINT_PREFIX: &str = "SHA256:";
const SHA256_FINGERPRINT_BASE64_BYTES: usize = 43;

/// Opaque handle for a key retained by a platform credential store.
///
/// It is intentionally not a path, URI, private-key value, or passphrase. Its
/// debug representation is redacted so routine diagnostics cannot expose even
/// the store-local handle.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct CredentialRef(String);

impl CredentialRef {
    /// Parses a bounded, store-local opaque handle.
    ///
    /// # Errors
    ///
    /// Returns [`SshIdentityError::InvalidCredentialRef`] when the value is
    /// empty, too long, path-like, or contains a character outside the stable
    /// opaque-handle alphabet.
    pub fn parse(value: impl Into<String>) -> Result<Self, SshIdentityError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_CREDENTIAL_REF_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(SshIdentityError::InvalidCredentialRef);
        }
        Ok(Self(value))
    }

    /// Returns the handle only to an explicit credential-store adapter.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CredentialRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CredentialRef(REDACTED)")
    }
}

/// Public-key algorithm classified by the importing platform adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshPublicKeyAlgorithm {
    Ed25519,
    Ecdsa,
    Rsa,
}

/// Canonical OpenSSH SHA-256 fingerprint of an imported public key.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PublicKeyFingerprint(String);

impl PublicKeyFingerprint {
    /// Parses an unpadded SHA-256 OpenSSH fingerprint.
    ///
    /// # Errors
    ///
    /// Returns [`SshIdentityError::InvalidPublicKeyFingerprint`] unless the
    /// value is exactly `SHA256:` followed by the 43 unpadded Base64 characters
    /// that encode a 32-byte SHA-256 digest.
    pub fn parse(value: impl Into<String>) -> Result<Self, SshIdentityError> {
        let value = value.into();
        let encoded = value
            .strip_prefix(SHA256_FINGERPRINT_PREFIX)
            .ok_or(SshIdentityError::InvalidPublicKeyFingerprint)?;
        if encoded.len() != SHA256_FINGERPRINT_BASE64_BYTES
            || !encoded.bytes().all(is_base64_character)
        {
            return Err(SshIdentityError::InvalidPublicKeyFingerprint);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_base64_character(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/')
}

/// Non-secret metadata used to identify an imported SSH key to the user.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicKeyMetadata {
    algorithm: SshPublicKeyAlgorithm,
    fingerprint: PublicKeyFingerprint,
}

impl PublicKeyMetadata {
    #[must_use]
    pub const fn new(algorithm: SshPublicKeyAlgorithm, fingerprint: PublicKeyFingerprint) -> Self {
        Self {
            algorithm,
            fingerprint,
        }
    }

    #[must_use]
    pub const fn algorithm(&self) -> SshPublicKeyAlgorithm {
        self.algorithm
    }

    #[must_use]
    pub const fn fingerprint(&self) -> &PublicKeyFingerprint {
        &self.fingerprint
    }
}

/// Evidence that a platform adapter completed a user-approved document import.
///
/// Constructing this value requires no private key bytes because the platform
/// adapter has already copied or wrapped them in private keystore-backed
/// storage.  This domain value is safe for profile state, but not for logs:
/// callers should emit only stable outcome codes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserApprovedSshKeyImport {
    credential_ref: CredentialRef,
    public_key: PublicKeyMetadata,
}

impl UserApprovedSshKeyImport {
    /// Records the result of one explicit user-selected document import.
    #[must_use]
    pub const fn new(credential_ref: CredentialRef, public_key: PublicKeyMetadata) -> Self {
        Self {
            credential_ref,
            public_key,
        }
    }

    #[must_use]
    pub const fn credential_ref(&self) -> &CredentialRef {
        &self.credential_ref
    }

    #[must_use]
    pub const fn public_key(&self) -> &PublicKeyMetadata {
        &self.public_key
    }
}

/// Stable validation failures at the credential-import domain boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshIdentityError {
    InvalidCredentialRef,
    InvalidPublicKeyFingerprint,
}

#[cfg(test)]
mod tests {
    use super::{
        CredentialRef, PublicKeyFingerprint, PublicKeyMetadata, SshIdentityError,
        SshPublicKeyAlgorithm, UserApprovedSshKeyImport,
    };

    const FINGERPRINT: &str = "SHA256:AbCdEfGhIjKlMnOpQrStUvWxYz0123456789+/abcde";

    fn metadata() -> PublicKeyMetadata {
        PublicKeyMetadata::new(
            SshPublicKeyAlgorithm::Ed25519,
            PublicKeyFingerprint::parse(FINGERPRINT).expect("fixture fingerprint is valid"),
        )
    }

    #[test]
    fn records_only_opaque_reference_and_public_metadata() {
        let credential_ref = CredentialRef::parse("android_keystore_key_42").unwrap();
        let import = UserApprovedSshKeyImport::new(credential_ref, metadata());

        assert_eq!(import.credential_ref().as_str(), "android_keystore_key_42");
        assert_eq!(import.public_key().algorithm(), SshPublicKeyAlgorithm::Ed25519);
        assert_eq!(import.public_key().fingerprint().as_str(), FINGERPRINT);
    }

    #[test]
    fn opaque_reference_rejects_paths_private_key_text_and_controls() {
        for invalid in [
            "",
            "/data/data/com.termux/files/home/.ssh/id_ed25519",
            "content://example/key",
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "key\nreference",
        ] {
            assert_eq!(
                CredentialRef::parse(invalid),
                Err(SshIdentityError::InvalidCredentialRef),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn opaque_reference_debug_output_is_redacted() {
        let credential_ref = CredentialRef::parse("android_keystore_key_42").unwrap();
        assert_eq!(format!("{credential_ref:?}"), "CredentialRef(REDACTED)");
    }

    #[test]
    fn fingerprint_requires_canonical_unpadded_sha256_form() {
        for invalid in [
            "SHA256:short",
            "sha256:AbCdEfGhIjKlMnOpQrStUvWxYz0123456789+/abcde",
            "SHA256:AbCdEfGhIjKlMnOpQrStUvWxYz0123456789+/abcd=",
            "SHA256:AbCdEfGhIjKlMnOpQrStUvWxYz0123456789+/abcde/",
        ] {
            assert_eq!(
                PublicKeyFingerprint::parse(invalid),
                Err(SshIdentityError::InvalidPublicKeyFingerprint),
                "{invalid:?}"
            );
        }
    }
}
