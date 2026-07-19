#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use choosh_core::ssh_identity::PublicKeyFingerprint;
use choosh_ssh::{
    CredentialSigner, CredentialSignerAdapter, PreAuthenticationSession, SessionLimits,
    SshUsername, VerifiedConnection, VerifiedConnectionError, presented_fingerprint,
};
use russh::keys::agent::AgentIdentity;
use russh::keys::{Algorithm, HashAlg, PrivateKey, PublicKey};
use russh::server;

struct FixtureServer;

impl server::Handler for FixtureServer {
    type Error = russh::Error;
}

#[derive(Debug)]
struct FixtureSignerError;

impl From<russh::SendError> for FixtureSignerError {
    fn from(_: russh::SendError) -> Self {
        Self
    }
}

struct CountingSigner {
    public_key: PublicKey,
    sign_calls: Arc<AtomicUsize>,
}

impl CredentialSigner for CountingSigner {
    type Error = FixtureSignerError;

    fn public_key(&self) -> PublicKey {
        self.public_key.clone()
    }

    async fn sign(
        &mut self,
        _identity: &AgentIdentity,
        _hash_alg: Option<HashAlg>,
        _payload: Vec<u8>,
    ) -> Result<Vec<u8>, Self::Error> {
        self.sign_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }
}

fn generated_key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
        .expect("runtime Ed25519 fixture key generation succeeds")
}

fn fingerprint(key: &PublicKey) -> PublicKeyFingerprint {
    PublicKeyFingerprint::parse(presented_fingerprint(key))
        .expect("Russh SHA-256 fingerprint is canonical")
}

#[tokio::test]
async fn changed_host_key_does_not_invoke_injected_credential_signer() {
    let presented_host_key = generated_key();
    let different_host_key = generated_key();
    let credential_key = generated_key();
    let calls = Arc::new(AtomicUsize::new(0));
    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    let server_config = Arc::new(server::Config {
        keys: vec![presented_host_key],
        ..server::Config::default()
    });
    let server = tokio::spawn(async move {
        let _ = server::run_stream(server_config, server_stream, FixtureServer).await;
    });

    let session = PreAuthenticationSession::new(
        fingerprint(different_host_key.public_key()),
        SessionLimits::admission_default(),
    );
    let signer = CredentialSignerAdapter::new(CountingSigner {
        public_key: credential_key.public_key().clone(),
        sign_calls: Arc::clone(&calls),
    });
    let outcome = VerifiedConnection::connect_stream(
        session,
        client_stream,
        SshUsername::parse("fixture-user").unwrap(),
        signer,
    )
    .await;

    assert!(matches!(
        outcome,
        Err(VerifiedConnectionError::TransportFailed)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.abort();
}
