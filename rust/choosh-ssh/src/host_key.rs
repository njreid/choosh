//! Exact host-key verification at the Russh callback boundary.

use choosh_core::ssh_identity::PublicKeyFingerprint;
use russh::client;
use russh::keys::{HashAlg, PublicKey};

/// Converts a Russh host key into the canonical SHA-256 fingerprint used by
/// the domain known-host policy.
#[must_use]
pub fn presented_fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Russh client callback that accepts exactly one persisted host key.
///
/// The callback deliberately has no consent, authentication, credential, or
/// channel authority. An unknown or changed key returns `false`, causing
/// Russh to abort the connection before authentication can be attempted.
pub struct ExactHostKeyHandler {
    expected: PublicKeyFingerprint,
}

impl ExactHostKeyHandler {
    #[must_use]
    pub const fn new(expected: PublicKeyFingerprint) -> Self {
        Self { expected }
    }

    #[must_use]
    pub fn matches(&self, key: &PublicKey) -> bool {
        presented_fingerprint(key) == self.expected.as_str()
    }
}

impl client::Handler for ExactHostKeyHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(self.matches(server_public_key))
    }
}

#[cfg(test)]
mod tests {
    use super::{ExactHostKeyHandler, presented_fingerprint};
    use choosh_core::ssh_identity::PublicKeyFingerprint;
    use russh::keys::PublicKey;

    const FIXTURE_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti";

    fn fixture_key() -> PublicKey {
        FIXTURE_KEY
            .parse()
            .expect("repository fixture public key is valid")
    }

    #[test]
    fn exact_fingerprint_is_the_only_accepted_host_key() {
        let key = fixture_key();
        let expected = PublicKeyFingerprint::parse(presented_fingerprint(&key)).unwrap();
        let handler = ExactHostKeyHandler::new(expected);
        assert!(handler.matches(&key));

        let different =
            PublicKeyFingerprint::parse("SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
                .unwrap();
        assert!(!ExactHostKeyHandler::new(different).matches(&key));
    }
}
