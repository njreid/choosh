#![cfg(not(target_arch = "wasm32"))]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use choosh_core::ssh_identity::PublicKeyFingerprint;
use choosh_ssh::{ExactHostKeyHandler, presented_fingerprint};
use russh::Channel;
use russh::client;
use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Session};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    HostKeyPresented,
    HostKeyMatched,
    HostKeyMismatch,
    AuthenticationStarted,
    AuthenticationKeyOffered,
    AuthenticationKeyVerified,
    Ready,
    ChannelObserved,
}

type Trace = Arc<Mutex<Vec<Event>>>;

struct ClientHandler {
    verifier: ExactHostKeyHandler,
    trace: Trace,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKey) -> Result<bool, Self::Error> {
        let mut trace = self.trace.lock().expect("trace mutex is not poisoned");
        trace.push(Event::HostKeyPresented);
        let accepted = self.verifier.matches(key);
        trace.push(if accepted {
            Event::HostKeyMatched
        } else {
            Event::HostKeyMismatch
        });
        Ok(accepted)
    }
}

struct ServerHandler {
    trace: Trace,
    expected_client: PublicKeyFingerprint,
}

impl server::Handler for ServerHandler {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        self.trace
            .lock()
            .expect("trace mutex is not poisoned")
            .push(Event::AuthenticationKeyOffered);
        Ok(
            if presented_fingerprint(key) == self.expected_client.as_str() {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }

    async fn auth_publickey(&mut self, _user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        self.trace
            .lock()
            .expect("trace mutex is not poisoned")
            .push(Event::AuthenticationKeyVerified);
        Ok(
            if presented_fingerprint(key) == self.expected_client.as_str() {
                Auth::Accept
            } else {
                Auth::reject()
            },
        )
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.trace
            .lock()
            .expect("trace mutex is not poisoned")
            .push(Event::ChannelObserved);
        reply.accept().await;
        Ok(())
    }
}

fn generated_server_key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
        .expect("runtime Ed25519 fixture key generation succeeds")
}

fn expected(key: &PublicKey) -> PublicKeyFingerprint {
    PublicKeyFingerprint::parse(presented_fingerprint(key))
        .expect("Russh SHA-256 fingerprint is canonical")
}

fn spawn_server(
    key: PrivateKey,
    expected_client: PublicKeyFingerprint,
    trace: Trace,
) -> (tokio::task::JoinHandle<()>, tokio::io::DuplexStream) {
    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    let config = Arc::new(server::Config {
        keys: vec![key],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..server::Config::default()
    });
    let task = tokio::spawn(async move {
        if let Ok(session) = server::run_stream(
            config,
            server_stream,
            ServerHandler {
                trace,
                expected_client,
            },
        )
        .await
        {
            let _ = session.await;
        }
    });
    (task, client_stream)
}

#[tokio::test]
async fn changed_generated_host_key_stops_before_authentication_or_channel_events() {
    let presented = generated_server_key();
    let different = generated_server_key();
    let client_key = generated_server_key();
    let trace = Trace::default();
    let (server, stream) = spawn_server(
        presented,
        expected(client_key.public_key()),
        Arc::clone(&trace),
    );
    let handler = ClientHandler {
        verifier: ExactHostKeyHandler::new(expected(different.public_key())),
        trace: Arc::clone(&trace),
    };

    let result = client::connect_stream(Arc::new(client::Config::default()), stream, handler).await;
    assert!(result.is_err());
    server.abort();
    assert_eq!(
        *trace.lock().expect("trace mutex is not poisoned"),
        [Event::HostKeyPresented, Event::HostKeyMismatch]
    );
}

#[tokio::test]
async fn exact_generated_host_key_precedes_authentication_and_channel_open() {
    let key = generated_server_key();
    let client_key = generated_server_key();
    let fingerprint = expected(key.public_key());
    let trace = Trace::default();
    let (server, stream) = spawn_server(key, expected(client_key.public_key()), Arc::clone(&trace));
    let handler = ClientHandler {
        verifier: ExactHostKeyHandler::new(fingerprint),
        trace: Arc::clone(&trace),
    };
    let mut client = client::connect_stream(Arc::new(client::Config::default()), stream, handler)
        .await
        .expect("exact generated host key connects");

    trace
        .lock()
        .expect("trace mutex is not poisoned")
        .push(Event::AuthenticationStarted);
    assert!(
        client
            .authenticate_publickey(
                "fixture-user",
                PrivateKeyWithHashAlg::new(Arc::new(client_key), None),
            )
            .await
            .unwrap()
            .success()
    );
    trace
        .lock()
        .expect("trace mutex is not poisoned")
        .push(Event::Ready);
    let _channel = client.channel_open_session().await.unwrap();
    drop(client);
    server.abort();

    assert_eq!(
        *trace.lock().expect("trace mutex is not poisoned"),
        [
            Event::HostKeyPresented,
            Event::HostKeyMatched,
            Event::AuthenticationStarted,
            Event::AuthenticationKeyOffered,
            Event::AuthenticationKeyVerified,
            Event::Ready,
            Event::ChannelObserved,
        ]
    );
}

#[tokio::test]
async fn wrong_generated_client_key_stops_before_ready_or_channel_open() {
    let host_key = generated_server_key();
    let allowed_client = generated_server_key();
    let rejected_client = generated_server_key();
    let host_fingerprint = expected(host_key.public_key());
    let trace = Trace::default();
    let (server, stream) = spawn_server(
        host_key,
        expected(allowed_client.public_key()),
        Arc::clone(&trace),
    );
    let handler = ClientHandler {
        verifier: ExactHostKeyHandler::new(host_fingerprint),
        trace: Arc::clone(&trace),
    };
    let mut client = client::connect_stream(Arc::new(client::Config::default()), stream, handler)
        .await
        .expect("exact generated host key connects");
    trace
        .lock()
        .expect("trace mutex is not poisoned")
        .push(Event::AuthenticationStarted);

    assert!(
        !client
            .authenticate_publickey(
                "fixture-user",
                PrivateKeyWithHashAlg::new(Arc::new(rejected_client), None),
            )
            .await
            .unwrap()
            .success()
    );
    drop(client);
    server.abort();
    assert_eq!(
        *trace.lock().expect("trace mutex is not poisoned"),
        [
            Event::HostKeyPresented,
            Event::HostKeyMatched,
            Event::AuthenticationStarted,
            Event::AuthenticationKeyOffered,
        ]
    );
}
