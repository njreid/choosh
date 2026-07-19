#![cfg(not(target_arch = "wasm32"))]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use choosh_core::ssh_identity::PublicKeyFingerprint;
use choosh_ssh::{ExactHostKeyHandler, presented_fingerprint};
use russh::client;
use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId, ChannelMsg, Pty};

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
    PtyAccepted,
    PtyRejected,
    ShellAccepted,
}

const MAX_TERM_BYTES: usize = 16;
const MAX_COLUMNS: u32 = 240;
const MAX_ROWS: u32 = 100;
const MAX_PIXELS: u32 = 8192;
const MAX_MODES: usize = 16;

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

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        pix_width: u32,
        pix_height: u32,
        modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let accepted = !term.is_empty()
            && term.len() <= MAX_TERM_BYTES
            && (1..=MAX_COLUMNS).contains(&col_width)
            && (1..=MAX_ROWS).contains(&row_height)
            && pix_width <= MAX_PIXELS
            && pix_height <= MAX_PIXELS
            && modes.len() <= MAX_MODES;
        self.trace
            .lock()
            .expect("trace mutex is not poisoned")
            .push(if accepted {
                Event::PtyAccepted
            } else {
                Event::PtyRejected
            });
        if accepted {
            session.channel_success(channel)?;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.trace
            .lock()
            .expect("trace mutex is not poisoned")
            .push(Event::ShellAccepted);
        session.channel_success(channel)?;
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

#[tokio::test]
async fn authenticated_pty_and_shell_requests_obey_explicit_fixture_limits() {
    let host_key = generated_server_key();
    let client_key = generated_server_key();
    let trace = Trace::default();
    let (server, stream) = spawn_server(
        host_key.clone(),
        expected(client_key.public_key()),
        Arc::clone(&trace),
    );
    let handler = ClientHandler {
        verifier: ExactHostKeyHandler::new(expected(host_key.public_key())),
        trace: Arc::clone(&trace),
    };
    let mut client = client::connect_stream(Arc::new(client::Config::default()), stream, handler)
        .await
        .unwrap();
    trace.lock().unwrap().push(Event::AuthenticationStarted);
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
    trace.lock().unwrap().push(Event::Ready);
    let mut channel = client.channel_open_session().await.unwrap();
    channel
        .request_pty(true, "xterm-256color", 120, 40, 1920, 1080, &[])
        .await
        .unwrap();
    assert!(matches!(channel.wait().await, Some(ChannelMsg::Success)));
    channel.request_shell(true).await.unwrap();
    assert!(matches!(channel.wait().await, Some(ChannelMsg::Success)));
    channel.close().await.unwrap();
    drop(client);
    server.abort();

    assert_eq!(
        *trace.lock().unwrap(),
        [
            Event::HostKeyPresented,
            Event::HostKeyMatched,
            Event::AuthenticationStarted,
            Event::AuthenticationKeyOffered,
            Event::AuthenticationKeyVerified,
            Event::Ready,
            Event::ChannelObserved,
            Event::PtyAccepted,
            Event::ShellAccepted,
        ]
    );
}

#[tokio::test]
async fn oversized_pty_is_rejected_without_a_shell_callback() {
    let host_key = generated_server_key();
    let client_key = generated_server_key();
    let trace = Trace::default();
    let (server, stream) = spawn_server(
        host_key.clone(),
        expected(client_key.public_key()),
        Arc::clone(&trace),
    );
    let handler = ClientHandler {
        verifier: ExactHostKeyHandler::new(expected(host_key.public_key())),
        trace: Arc::clone(&trace),
    };
    let mut client = client::connect_stream(Arc::new(client::Config::default()), stream, handler)
        .await
        .unwrap();
    trace.lock().unwrap().push(Event::AuthenticationStarted);
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
    trace.lock().unwrap().push(Event::Ready);
    let mut channel = client.channel_open_session().await.unwrap();
    channel
        .request_pty(true, "xterm-256color", MAX_COLUMNS + 1, 40, 0, 0, &[])
        .await
        .unwrap();
    assert!(matches!(channel.wait().await, Some(ChannelMsg::Failure)));
    drop(client);
    server.abort();
    assert_eq!(trace.lock().unwrap().last(), Some(&Event::PtyRejected));
    assert!(!trace.lock().unwrap().contains(&Event::ShellAccepted));
}
