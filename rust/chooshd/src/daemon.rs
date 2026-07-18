//! Minimal bounded daemon composition behavior over a private Unix socket.

#![cfg(unix)]

use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

use choosh_protocol::framing::{FrameDecoder, FrameError, FrameLimits, encode_frame};
use choosh_protocol::handshake::{
    Capability, HandshakeError, PeerIdentity, ProtocolLimits, ProtocolVersion, ServerNegotiator,
};
use choosh_protocol::wire::{WireError, decode_hello, encode_server_reply};

use crate::socket::{self, OwnedUnixListener, SocketError, SocketPlan};

pub const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;
const READ_CHUNK_BYTES: usize = 8192;
const MAX_FRAMES_PER_READ: usize = 1;

#[derive(Clone, Debug)]
pub struct HandshakeConfig {
    pub protocol: ProtocolVersion,
    pub daemon: PeerIdentity,
    pub host: PeerIdentity,
    pub capabilities: Vec<Capability>,
    pub limits: ProtocolLimits,
}

impl HandshakeConfig {
    fn negotiator(&self) -> Result<ServerNegotiator, HandshakeError> {
        ServerNegotiator::new(
            self.protocol,
            self.daemon.clone(),
            self.host.clone(),
            self.capabilities.iter().copied(),
            self.limits,
        )
    }
}

#[derive(Debug)]
pub enum DaemonError {
    Socket(SocketError),
    Io(io::Error),
    Frame(FrameError),
    Wire(WireError),
    Handshake(HandshakeError),
    ExpectedHello,
}

impl fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Socket(_) => formatter.write_str("daemon_socket_error"),
            Self::Io(_) => formatter.write_str("daemon_io_error"),
            Self::Frame(error) => write!(formatter, "daemon_frame_{}", error.code()),
            Self::Wire(error) => write!(formatter, "daemon_wire_{}", error.code()),
            Self::Handshake(error) => write!(formatter, "daemon_handshake_{}", error.code()),
            Self::ExpectedHello => formatter.write_str("daemon_expected_hello"),
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Socket(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Frame(_) | Self::Wire(_) | Self::Handshake(_) | Self::ExpectedHello => None,
        }
    }
}

impl From<SocketError> for DaemonError {
    fn from(error: SocketError) -> Self {
        Self::Socket(error)
    }
}

impl From<io::Error> for DaemonError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<FrameError> for DaemonError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<WireError> for DaemonError {
    fn from(error: WireError) -> Self {
        Self::Wire(error)
    }
}

impl From<HandshakeError> for DaemonError {
    fn from(error: HandshakeError) -> Self {
        Self::Handshake(error)
    }
}

/// Binds the injected private socket plan for the outer binary composition root.
///
/// # Errors
///
/// Returns the existing socket boundary's fail-closed path, permission, and I/O
/// errors.
pub fn bind(plan: &SocketPlan) -> Result<OwnedUnixListener, DaemonError> {
    socket::bind(plan).map_err(Into::into)
}

/// Serves accepted connections until the listener fails.
///
/// A malformed connection is closed without terminating the listener. Accept
/// failure is returned to the composition root.
///
/// # Errors
///
/// Returns invalid framing limits or listener accept failure.
pub fn serve(
    listener: &UnixListener,
    config: &HandshakeConfig,
    max_frame_bytes: usize,
) -> Result<(), DaemonError> {
    let limits = limits(max_frame_bytes)?;
    loop {
        let (mut stream, _) = listener.accept()?;
        let _ = serve_stream_with_limits(&mut stream, config, limits);
    }
}

/// Accepts and serves exactly one connection for a deterministic black-box seam.
///
/// # Errors
///
/// Returns invalid framing limits, accept failure, malformed/truncated framing,
/// or stream I/O errors.
pub fn serve_once(
    listener: &UnixListener,
    config: &HandshakeConfig,
    max_frame_bytes: usize,
) -> Result<(), DaemonError> {
    let limits = limits(max_frame_bytes)?;
    let (mut stream, _) = listener.accept()?;
    serve_stream_with_limits(&mut stream, config, limits)
}

fn limits(max_frame_bytes: usize) -> Result<FrameLimits, DaemonError> {
    FrameLimits::new(max_frame_bytes, MAX_FRAMES_PER_READ).map_err(Into::into)
}

fn serve_stream_with_limits(
    stream: &mut UnixStream,
    config: &HandshakeConfig,
    limits: FrameLimits,
) -> Result<(), DaemonError> {
    let mut decoder = FrameDecoder::new(limits);
    let mut buffer = [0_u8; READ_CHUNK_BYTES];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            decoder.finish()?;
            return Err(DaemonError::ExpectedHello);
        }
        if let Some(payload) = decoder.feed(&buffer[..read])?.into_iter().next() {
            let hello = decode_hello(&payload, limits.max_frame_bytes)?;
            let reply = config.negotiator()?.receive_hello(&hello)?;
            let response = encode_server_reply(&reply, limits.max_frame_bytes)?;
            let encoded = encode_frame(&response, limits.max_frame_bytes)?;
            stream.write_all(&encoded)?;
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    fn config(max_frame_bytes: u32) -> HandshakeConfig {
        HandshakeConfig {
            protocol: ProtocolVersion::new(1, 2),
            daemon: PeerIdentity::new("chooshd", "test").unwrap(),
            host: PeerIdentity::new("local-host", "test").unwrap(),
            capabilities: vec![Capability::Events],
            limits: ProtocolLimits::new(max_frame_bytes, 8).unwrap(),
        }
    }

    #[test]
    fn hello_receives_one_canonical_welcome_then_connection_closes() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            serve_stream_with_limits(&mut server, &config(512), limits(512).unwrap()).unwrap();
        });
        let hello = br#"{"kind":"hello","protocol":{"major":1,"minor":1},"client":{"name":"test-client","version":"1"},"capabilities":["events"]}"#;
        client
            .write_all(&encode_frame(hello, 512).unwrap())
            .unwrap();

        let mut output = Vec::new();
        client.read_to_end(&mut output).unwrap();
        let mut decoder = FrameDecoder::new(FrameLimits::new(512, 1).unwrap());
        assert_eq!(
            decoder.feed(&output).unwrap(),
            [br#"{"capabilities":["events"],"daemon":{"name":"chooshd","version":"test"},"host":{"name":"local-host","version":"test"},"kind":"welcome","limits":{"max_control_frame_bytes":512,"max_in_flight_requests":8},"protocol":{"major":1,"minor":1}}"#.to_vec()]
        );
        decoder.finish().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn raw_health_is_rejected_without_response() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            assert!(matches!(
                serve_stream_with_limits(&mut server, &config(32), limits(32).unwrap()),
                Err(DaemonError::Wire(WireError::MalformedJson))
            ));
        });
        client
            .write_all(&encode_frame(b"health", 32).unwrap())
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert!(response.is_empty());
        worker.join().unwrap();
    }

    #[test]
    fn incompatible_major_receives_canonical_terminal_reply() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            serve_stream_with_limits(&mut server, &config(256), limits(256).unwrap()).unwrap();
        });
        let hello = br#"{"kind":"hello","protocol":{"major":2,"minor":0},"client":{"name":"test-client","version":"1"},"capabilities":[]}"#;
        client
            .write_all(&encode_frame(hello, 256).unwrap())
            .unwrap();
        let mut output = Vec::new();
        client.read_to_end(&mut output).unwrap();
        let mut decoder = FrameDecoder::new(FrameLimits::new(256, 1).unwrap());
        assert_eq!(
            decoder.feed(&output).unwrap(),
            [br#"{"client":{"major":2,"minor":0},"daemon":{"major":1,"minor":2},"kind":"incompatible"}"#.to_vec()]
        );
        decoder.finish().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn oversized_frame_is_rejected_without_response() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            assert!(matches!(
                serve_stream_with_limits(&mut server, &config(4), limits(4).unwrap()),
                Err(DaemonError::Frame(FrameError::FrameTooLarge))
            ));
        });
        client.write_all(&5_u32.to_be_bytes()).unwrap();
        client.write_all(b"12345").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert!(response.is_empty());
        worker.join().unwrap();
    }
}
