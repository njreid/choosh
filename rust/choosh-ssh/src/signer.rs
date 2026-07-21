//! Public-key authentication signing boundary.

use std::future::Future;

use russh::Signer;
use russh::keys::HashAlg;
use russh::keys::agent::AgentIdentity;

/// Platform capability that signs an SSH authentication challenge.
///
/// Implementations locate a key through an opaque credential reference outside
/// this crate. They receive only public-key identity metadata, the negotiated
/// hash algorithm, and the SSH protocol's signing payload. They must never
/// return, log, or serialize private-key bytes.
pub trait CredentialSigner: Send {
    type Error: From<russh::SendError>;

    /// Returns the non-secret public key paired with this signing capability.
    fn public_key(&self) -> russh::keys::PublicKey;

    /// Appends one SSH wire-encoded signature to the supplied signing buffer.
    ///
    /// The returned buffer MUST retain `payload` unchanged and append the
    /// signature as an SSH `string` (a four-byte length followed by the
    /// algorithm-and-signature encoding). This is Russh's custom-signer
    /// contract and keeps the platform callback responsible only for a single
    /// public-key proof.
    fn sign(
        &mut self,
        identity: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        payload: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send;
}

/// Adapts an injected credential capability to Russh's public-key signer API.
pub struct CredentialSignerAdapter<S> {
    signer: S,
}

impl<S> CredentialSignerAdapter<S> {
    #[must_use]
    pub const fn new(signer: S) -> Self {
        Self { signer }
    }

    #[must_use]
    pub fn into_inner(self) -> S {
        self.signer
    }
}

impl<S: CredentialSigner> CredentialSignerAdapter<S> {
    /// Returns the public key that the injected capability will prove control of.
    #[must_use]
    pub fn public_key(&self) -> russh::keys::PublicKey {
        self.signer.public_key()
    }
}

impl<S: CredentialSigner> Signer for CredentialSignerAdapter<S> {
    type Error = S::Error;

    async fn auth_sign(
        &mut self,
        key: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> Result<Vec<u8>, Self::Error> {
        self.signer.sign(key, hash_alg, to_sign).await
    }
}

#[cfg(test)]
mod tests {
    use super::{CredentialSigner, CredentialSignerAdapter};
    use russh::Signer;
    use russh::keys::PublicKey;
    use russh::keys::agent::AgentIdentity;

    const FIXTURE_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILM+rvN+ot98qgEN796jTiQfZfG1KaT0PtFDJ/XFSqti";

    #[derive(Debug)]
    enum FakeError {
        Send(russh::SendError),
    }

    impl From<russh::SendError> for FakeError {
        fn from(value: russh::SendError) -> Self {
            Self::Send(value)
        }
    }

    struct FakeSigner {
        seen_payload: Vec<u8>,
    }

    impl CredentialSigner for FakeSigner {
        type Error = FakeError;

        fn public_key(&self) -> PublicKey {
            FIXTURE_KEY.parse().expect("fixture public key is valid")
        }

        async fn sign(
            &mut self,
            identity: &AgentIdentity,
            hash_alg: Option<russh::keys::HashAlg>,
            payload: Vec<u8>,
        ) -> Result<Vec<u8>, Self::Error> {
            assert!(matches!(identity, AgentIdentity::PublicKey { .. }));
            assert!(hash_alg.is_none());
            self.seen_payload = payload;
            Ok(vec![1, 2, 3])
        }
    }

    #[tokio::test]
    async fn adapter_delegates_only_public_identity_and_signing_payload() {
        let key: PublicKey = FIXTURE_KEY.parse().expect("fixture public key is valid");
        let mut adapter = CredentialSignerAdapter::new(FakeSigner {
            seen_payload: Vec::new(),
        });

        assert_eq!(adapter.public_key(), key);
        let signature = adapter
            .auth_sign(&AgentIdentity::from(key), None, vec![9, 8, 7])
            .await
            .expect("fake signer succeeds");
        assert_eq!(signature, [1, 2, 3]);
        assert_eq!(adapter.into_inner().seen_payload, [9, 8, 7]);
    }
}
