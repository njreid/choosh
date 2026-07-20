#![cfg(not(target_arch = "wasm32"))]

//! Generated-key, in-memory proof that SSH channel types remain separately
//! admitted after one exact-host-key/public-key-authenticated transport.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use choosh_core::ssh_identity::PublicKeyFingerprint;
use choosh_ssh::presented_fingerprint;
use russh::client;
use russh::keys::{Algorithm, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId, ChannelMsg};

const DISPATCHER_COMMAND: &[u8] = b"choosh-host --exec-stdio-v1";
const LOOPBACK: &str = "127.0.0.1";
const LOOPBACK_PORT: u32 = 7_777;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Event {
    HostKeyAccepted,
    PublicKeyAccepted,
    FixedExecAccepted,
    SftpAccepted,
    LoopbackForwardAccepted,
    NonLoopbackForwardRejected,
}

type Trace = Arc<Mutex<Vec<Event>>>;

struct ClientHandler {
    expected: PublicKeyFingerprint,
    trace: Trace,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKey) -> Result<bool, Self::Error> {
        let accepted = presented_fingerprint(key) == self.expected.as_str();
        if accepted {
            self.trace.lock().unwrap().push(Event::HostKeyAccepted);
        }
        Ok(accepted)
    }
}

struct ServerHandler {
    expected_client: PublicKeyFingerprint,
    trace: Trace,
}

impl server::Handler for ServerHandler {
    type Error = russh::Error;

    async fn auth_publickey(&mut self, _user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        let accepted = presented_fingerprint(key) == self.expected_client.as_str();
        if accepted {
            self.trace.lock().unwrap().push(Event::PublicKeyAccepted);
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if command == DISPATCHER_COMMAND {
            self.trace.lock().unwrap().push(Event::FixedExecAccepted);
            session.channel_success(channel)?;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            self.trace.lock().unwrap().push(Event::SftpAccepted);
            session.channel_success(channel)?;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        _channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        originator_address: &str,
        originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if host_to_connect == LOOPBACK
            && port_to_connect == LOOPBACK_PORT
            && originator_address == LOOPBACK
            && originator_port == 0
        {
            self.trace
                .lock()
                .unwrap()
                .push(Event::LoopbackForwardAccepted);
            reply.accept().await;
        } else {
            self.trace
                .lock()
                .unwrap()
                .push(Event::NonLoopbackForwardRejected);
        }
        Ok(())
    }
}

fn generated_key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
}

fn fingerprint(key: &PublicKey) -> PublicKeyFingerprint {
    PublicKeyFingerprint::parse(presented_fingerprint(key)).unwrap()
}

fn spawn_server(
    host_key: PrivateKey,
    client_key: PublicKeyFingerprint,
    trace: Trace,
) -> (tokio::task::JoinHandle<()>, tokio::io::DuplexStream) {
    let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
    let config = Arc::new(server::Config {
        keys: vec![host_key],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..server::Config::default()
    });
    let server = tokio::spawn(async move {
        if let Ok(session) = server::run_stream(
            config,
            server_stream,
            ServerHandler {
                expected_client: client_key,
                trace,
            },
        )
        .await
        {
            let _ = session.await;
        }
    });
    (server, client_stream)
}

#[tokio::test]
async fn one_authenticated_transport_admits_fixed_exec_sftp_and_loopback_only_forwarding() {
    let host_key = generated_key();
    let client_key = generated_key();
    let trace = Trace::default();
    let (server, stream) = spawn_server(
        host_key.clone(),
        fingerprint(client_key.public_key()),
        Arc::clone(&trace),
    );
    let mut client = client::connect_stream(
        Arc::new(client::Config::default()),
        stream,
        ClientHandler {
            expected: fingerprint(host_key.public_key()),
            trace: Arc::clone(&trace),
        },
    )
    .await
    .unwrap();
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

    let mut exec = client.channel_open_session().await.unwrap();
    exec.exec(true, DISPATCHER_COMMAND).await.unwrap();
    assert!(matches!(exec.wait().await, Some(ChannelMsg::Success)));

    let mut sftp = client.channel_open_session().await.unwrap();
    sftp.request_subsystem(true, "sftp").await.unwrap();
    assert!(matches!(sftp.wait().await, Some(ChannelMsg::Success)));

    let forward = client
        .channel_open_direct_tcpip(LOOPBACK, LOOPBACK_PORT, LOOPBACK, 0)
        .await;
    assert!(forward.is_ok());

    drop(exec);
    drop(sftp);
    drop(client);
    server.abort();
    assert_eq!(
        *trace.lock().unwrap(),
        [
            Event::HostKeyAccepted,
            Event::PublicKeyAccepted,
            Event::FixedExecAccepted,
            Event::SftpAccepted,
            Event::LoopbackForwardAccepted,
        ]
    );
}

#[tokio::test]
async fn authenticated_transport_rejects_non_loopback_forwarding() {
    let host_key = generated_key();
    let client_key = generated_key();
    let trace = Trace::default();
    let (server, stream) = spawn_server(
        host_key.clone(),
        fingerprint(client_key.public_key()),
        Arc::clone(&trace),
    );
    let mut client = client::connect_stream(
        Arc::new(client::Config::default()),
        stream,
        ClientHandler {
            expected: fingerprint(host_key.public_key()),
            trace: Arc::clone(&trace),
        },
    )
    .await
    .unwrap();
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

    assert!(
        client
            .channel_open_direct_tcpip("198.51.100.1", LOOPBACK_PORT, LOOPBACK, 0)
            .await
            .is_err()
    );
    drop(client);
    server.abort();
    assert!(
        trace
            .lock()
            .unwrap()
            .contains(&Event::NonLoopbackForwardRejected)
    );
}
