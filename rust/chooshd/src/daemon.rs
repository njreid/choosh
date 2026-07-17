//! Minimal bounded daemon composition behavior over a private Unix socket.

#![cfg(unix)]

use std::fmt;
use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

use choosh_protocol::framing::{FrameDecoder, FrameError, FrameLimits, encode_frame};

use crate::socket::{self, OwnedUnixListener, SocketError, SocketPlan};

pub const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;
const READ_CHUNK_BYTES: usize = 8192;
const MAX_FRAMES_PER_READ: usize = 16;

#[derive(Debug)]
pub enum DaemonError {
    Socket(SocketError),
    Io(io::Error),
    Frame(FrameError),
}

impl fmt::Display for DaemonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Socket(_) => formatter.write_str("daemon_socket_error"),
            Self::Io(_) => formatter.write_str("daemon_io_error"),
            Self::Frame(error) => write!(formatter, "daemon_frame_{}", error.code()),
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Socket(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Frame(_) => None,
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
pub fn serve(listener: &UnixListener, max_frame_bytes: usize) -> Result<(), DaemonError> {
    let limits = limits(max_frame_bytes)?;
    loop {
        let (mut stream, _) = listener.accept()?;
        let _ = serve_stream_with_limits(&mut stream, limits);
    }
}

/// Accepts and serves exactly one connection for a deterministic black-box seam.
///
/// # Errors
///
/// Returns invalid framing limits, accept failure, malformed/truncated framing,
/// or stream I/O errors.
pub fn serve_once(listener: &UnixListener, max_frame_bytes: usize) -> Result<(), DaemonError> {
    let limits = limits(max_frame_bytes)?;
    let (mut stream, _) = listener.accept()?;
    serve_stream_with_limits(&mut stream, limits)
}

fn limits(max_frame_bytes: usize) -> Result<FrameLimits, DaemonError> {
    FrameLimits::new(max_frame_bytes, MAX_FRAMES_PER_READ).map_err(Into::into)
}

fn serve_stream_with_limits(
    stream: &mut UnixStream,
    limits: FrameLimits,
) -> Result<(), DaemonError> {
    let mut decoder = FrameDecoder::new(limits);
    let mut buffer = [0_u8; READ_CHUNK_BYTES];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            decoder.finish()?;
            return Ok(());
        }
        for payload in decoder.feed(&buffer[..read])? {
            let response = if payload == b"health" {
                b"healthy".as_slice()
            } else {
                payload.as_slice()
            };
            let encoded = encode_frame(response, limits.max_frame_bytes)?;
            stream.write_all(&encoded)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn health_and_binary_echo_share_bounded_raw_framing() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            serve_stream_with_limits(&mut server, limits(32).unwrap()).unwrap();
        });
        client
            .write_all(&encode_frame(b"health", 32).unwrap())
            .unwrap();
        client
            .write_all(&encode_frame(&[0, 1, 2], 32).unwrap())
            .unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();

        let mut output = Vec::new();
        client.read_to_end(&mut output).unwrap();
        let mut decoder = FrameDecoder::new(FrameLimits::new(32, 2).unwrap());
        assert_eq!(
            decoder.feed(&output).unwrap(),
            [b"healthy".to_vec(), vec![0, 1, 2]]
        );
        decoder.finish().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn oversized_frame_is_rejected_without_response() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            assert!(matches!(
                serve_stream_with_limits(&mut server, limits(4).unwrap()),
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
