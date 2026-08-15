//! Conversions between this crate's own `ed25519_dalek` key material and
//! `russh`'s SSH-wire key types, per `docs/specs/ssh-bridge-and-zed.md`'s
//! "Loopback SSH server" section and `docs/specs/auth-and-enrollment.md`
//! step 6.
//!
//! **Design decision: the devhost's SSH host key is the same Ed25519 key
//! as its enrollment credential, not a separate one.** `auth-and-enrollment.md`
//! names "the loopback SSH server's host public key" as a distinct concept
//! from the enrollment `public_key`, but doesn't mandate a distinct
//! *keypair* — and generating one would mean this crate has to generate,
//! persist, protect, and eventually rotate a second private key with
//! exactly the same security properties (an Ed25519 key living in the same
//! `Credential` file, protected by the same `0600` permissions, on the
//! same device) as the one it already has. Reusing the enrollment key:
//! - Costs nothing in security, since both keys would be equally
//!   sensitive and equally protected — a compromise of one is a
//!   compromise of the devhost either way.
//! - Keeps the trust chain auth-and-enrollment.md already describes
//!   intact: "the SSH host key it now writes to `known_hosts` was itself
//!   established the same way, at that devhost's own enrollment" reads
//!   most naturally as *the* enrollment key, not a second key
//!   incidentally introduced alongside it.
//! - Avoids a second keypair with no independent lifecycle: it would
//!   always be generated, persisted, and revoked in lockstep with the
//!   enrollment key anyway (there is no scenario where a devhost keeps
//!   its enrollment key but wants a different SSH host key, or vice
//!   versa), so splitting them buys no real rotation flexibility.
//!
//! Ed25519 SSH host keys are an ordinary, fully supported SSH key type
//! (`ssh-ed25519`), so there's no protocol-level reason to prefer a
//! different algorithm here either.

use ed25519_dalek::SigningKey;
use russh::keys::PrivateKey;
use russh::keys::ssh_key::private::{Ed25519Keypair, KeypairData};
use russh::keys::ssh_key::public::{Ed25519PublicKey, KeyData};
use russh::keys::ssh_key::{Error as SshKeyFormatError, PublicKey};

#[derive(Debug)]
pub enum SshKeyError {
    Format(SshKeyFormatError),
    /// A caller-supplied byte slice isn't exactly 32 bytes — a raw Ed25519
    /// public key's fixed length.
    WrongLength,
}

impl std::fmt::Display for SshKeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format(error) => write!(f, "SSH key formatting error: {error}"),
            Self::WrongLength => write!(f, "expected exactly 32 bytes for an Ed25519 public key"),
        }
    }
}

impl std::error::Error for SshKeyError {}

impl From<SshKeyFormatError> for SshKeyError {
    fn from(error: SshKeyFormatError) -> Self {
        Self::Format(error)
    }
}

/// Builds `russh`'s server host-key type directly from this devhost's own
/// enrollment signing key — see this module's doc comment for why that's
/// the same key rather than a second, independently generated one.
///
/// # Errors
///
/// Returns [`SshKeyError`] only if `ssh_key`'s internal key construction
/// fails, which does not happen for a well-formed Ed25519 keypair in
/// practice — kept fallible because the underlying `PrivateKey::new`
/// constructor is.
pub fn host_private_key(signing_key: &SigningKey) -> Result<PrivateKey, SshKeyError> {
    let keypair: Ed25519Keypair = signing_key.clone().into();
    Ok(PrivateKey::new(KeypairData::Ed25519(keypair), "choosh-hostd")?)
}

/// The raw 32 Ed25519 public-key bytes for `signing_key` — the same
/// encoding [`ControlRequest::Enroll`](choosh_protocol::relay::ControlRequest::Enroll)'s
/// `public_key`/`host_ssh_public_key` fields already use (base64 of these
/// bytes), so enrollment can send this host's SSH host key with no extra
/// wire format of its own.
#[must_use]
pub fn raw_public_key_bytes(signing_key: &SigningKey) -> [u8; 32] {
    signing_key.verifying_key().to_bytes()
}

/// Formats a raw 32-byte Ed25519 public key as an OpenSSH wire-format
/// public-key line (`ssh-ed25519 AAAA...`), the shape both `known_hosts`
/// and `authorized_keys`-style files use. `comment` becomes the trailing,
/// purely cosmetic comment field (e.g. a devhost's alias).
///
/// # Errors
///
/// Returns [`SshKeyError::WrongLength`] if `raw` isn't exactly 32 bytes,
/// or [`SshKeyError::Format`] if `ssh_key` itself fails to encode it
/// (not expected for well-formed input).
pub fn openssh_public_key_line(raw: &[u8], comment: &str) -> Result<String, SshKeyError> {
    let bytes: [u8; 32] = raw.try_into().map_err(|_| SshKeyError::WrongLength)?;
    let key_data = KeyData::Ed25519(Ed25519PublicKey(bytes));
    let public_key = PublicKey::new(key_data, comment);
    Ok(public_key.to_openssh()?)
}

/// The `known_hosts` key-field portion only (`ssh-ed25519 AAAA...`, no
/// trailing comment) — `proxy sync` appends its own choosh-managed marker
/// as that file's comment field instead, so this returns just the part
/// that's actually the key.
///
/// # Errors
///
/// See [`openssh_public_key_line`].
pub fn openssh_public_key_algorithm_and_base64(raw: &[u8]) -> Result<String, SshKeyError> {
    Ok(openssh_public_key_line(raw, "")?.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_private_key_derives_from_the_enrollment_signing_key_deterministically() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let private = host_private_key(&signing_key).unwrap();
        let expected_public = openssh_public_key_line(&raw_public_key_bytes(&signing_key), "choosh-hostd").unwrap();
        assert_eq!(private.public_key().to_openssh().unwrap(), expected_public);

        // Deterministic: the same signing key always yields the same SSH
        // host key, so a devhost's `known_hosts` entry (captured once at
        // enrollment) keeps matching what the SSH server actually
        // presents across every `serve` restart.
        let private_again = host_private_key(&signing_key).unwrap();
        assert_eq!(private.public_key(), private_again.public_key());
    }

    #[test]
    fn openssh_public_key_line_round_trips_through_parsing() {
        let signing_key = SigningKey::from_bytes(&[3u8; 32]);
        let line = openssh_public_key_line(&raw_public_key_bytes(&signing_key), "build-box").unwrap();
        assert!(line.starts_with("ssh-ed25519 "));
        assert!(line.ends_with("build-box"));
        let parsed: PublicKey = line.parse().expect("formatted line must parse back as a public key");
        assert_eq!(parsed.key_data(), &KeyData::Ed25519(Ed25519PublicKey(raw_public_key_bytes(&signing_key))));
    }

    #[test]
    fn openssh_public_key_line_rejects_wrong_length_input() {
        assert!(matches!(openssh_public_key_line(&[0u8; 31], "x"), Err(SshKeyError::WrongLength)));
    }
}
