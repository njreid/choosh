//! Bounded framed stdio relay to the injected per-user daemon Unix socket.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use choosh_protocol::framing::{FrameDecoder, FrameLimits, encode_frame};
use choosh_protocol::handshake::{Hello, PeerIdentity, ProtocolVersion};

use crate::bridge::{
    BridgeError, BridgeLimits, BridgeStats, FrameHandler, FrameWriter, HandlerFailure, run_bridge,
};
use crate::handshake::{HandshakeLimits, perform_client_handshake};

/// Runs `rpc --stdio` over one explicitly injected daemon socket path.
///
/// This layer forwards opaque frames and deliberately assigns no JSON semantics.
/// The socket path and payloads never appear in returned errors.
///
/// # Errors
///
/// Returns stable bridge classifications for connection, framing, input/output,
/// or daemon response failures.
pub fn run_unix_socket_relay<R: Read, W: Write>(
    input: R,
    output: W,
    socket_path: &Path,
    limits: BridgeLimits,
) -> Result<BridgeStats, BridgeError> {
    let mut stream = UnixStream::connect(socket_path)
        .map_err(|_| BridgeError::Handler(HandlerFailure::Unavailable))?;
    establish_daemon_session(&mut stream, limits)?;
    let mut relay = SocketFrameRelay::new(stream, limits)?;
    run_bridge(input, output, &mut relay, limits)
}

/// Completes the daemon protocol handshake before any untrusted RPC frame is
/// relayed. The daemon welcome is consumed at this local bridge boundary: the
/// SSH caller sends and receives only terminal RPC frames.
fn establish_daemon_session(
    stream: &mut UnixStream,
    limits: BridgeLimits,
) -> Result<(), BridgeError> {
    let hello = Hello::new(
        ProtocolVersion::new(1, 0),
        PeerIdentity::new("choosh-host", env!("CARGO_PKG_VERSION"))
            .map_err(|_| BridgeError::Handler(HandlerFailure::Internal))?,
        [],
    )
    .map_err(|_| BridgeError::Handler(HandlerFailure::Internal))?;
    let handshake_limits =
        HandshakeLimits::new(limits.max_frame_bytes, limits.read_buffer_bytes, 64)
            .map_err(|_| BridgeError::Handler(HandlerFailure::Internal))?;
    perform_client_handshake(stream, &hello, handshake_limits)
        .map(|_| ())
        .map_err(|_| BridgeError::Handler(HandlerFailure::Unavailable))
}

struct SocketFrameRelay<T> {
    transport: T,
    limits: BridgeLimits,
    read_buffer: Vec<u8>,
}

impl<T> SocketFrameRelay<T> {
    fn new(transport: T, limits: BridgeLimits) -> Result<Self, BridgeError> {
        let _ = FrameLimits::new(limits.max_frame_bytes, 2)?;
        if limits.read_buffer_bytes == 0 {
            return Err(BridgeError::InvalidLimits);
        }
        Ok(Self {
            transport,
            limits,
            read_buffer: vec![0; limits.read_buffer_bytes],
        })
    }
}

impl<T: Read + Write> FrameHandler for SocketFrameRelay<T> {
    fn handle_frame<W: Write>(
        &mut self,
        frame: &[u8],
        responses: &mut FrameWriter<'_, W>,
    ) -> Result<(), HandlerFailure> {
        let request = encode_frame(frame, self.limits.max_frame_bytes)
            .map_err(|_| HandlerFailure::Rejected)?;
        self.transport
            .write_all(&request)
            .map_err(|_| HandlerFailure::Unavailable)?;
        self.transport
            .flush()
            .map_err(|_| HandlerFailure::Unavailable)?;

        let frame_limits = FrameLimits::new(self.limits.max_frame_bytes, 2)
            .map_err(|_| HandlerFailure::Internal)?;
        let mut decoder = FrameDecoder::new(frame_limits);
        loop {
            let read = self
                .transport
                .read(&mut self.read_buffer)
                .map_err(|_| HandlerFailure::Unavailable)?;
            if read == 0 {
                decoder.finish().map_err(|_| HandlerFailure::Unavailable)?;
                return Err(HandlerFailure::Unavailable);
            }
            let frames = decoder
                .feed(&self.read_buffer[..read])
                .map_err(|_| HandlerFailure::Rejected)?;
            if frames.len() > 1 {
                return Err(HandlerFailure::Rejected);
            }
            if let Some(response) = frames.into_iter().next() {
                return responses
                    .write_frame(&response)
                    .map_err(|_| HandlerFailure::Internal);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_protocol::framing::encode_frame;
    use std::collections::VecDeque;
    use std::io;

    struct ScriptedTransport {
        written: Vec<u8>,
        reads: VecDeque<io::Result<Vec<u8>>>,
    }

    impl Read for ScriptedTransport {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let Some(read) = self.reads.pop_front() else {
                return Ok(0);
            };
            let bytes = read?;
            let count = bytes.len().min(output.len());
            output[..count].copy_from_slice(&bytes[..count]);
            if count < bytes.len() {
                self.reads.push_front(Ok(bytes[count..].to_vec()));
            }
            Ok(count)
        }
    }

    impl Write for ScriptedTransport {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn limits() -> BridgeLimits {
        BridgeLimits::new(64, 3, 4).unwrap()
    }

    #[test]
    fn forwards_one_opaque_request_and_fragmented_response() {
        let response = encode_frame(b"opaque-response", 64).unwrap();
        let reads = response.chunks(2).map(|chunk| Ok(chunk.to_vec())).collect();
        let transport = ScriptedTransport {
            written: Vec::new(),
            reads,
        };
        let mut relay = SocketFrameRelay::new(transport, limits()).unwrap();
        let request = encode_frame(b"opaque-request", 64).unwrap();
        let mut output = Vec::new();
        let stats = run_bridge(request.as_slice(), &mut output, &mut relay, limits()).unwrap();
        assert_eq!(stats.input_frames, 1);
        assert_eq!(relay.transport.written, request);
        assert_eq!(output, response);
    }

    #[test]
    fn truncated_daemon_response_is_secret_free_unavailable() {
        let secret = b"do-not-report";
        let partial = encode_frame(secret, 64).unwrap()[..6].to_vec();
        let transport = ScriptedTransport {
            written: Vec::new(),
            reads: VecDeque::from([Ok(partial)]),
        };
        let mut relay = SocketFrameRelay::new(transport, limits()).unwrap();
        let input = encode_frame(b"request", 64).unwrap();
        let error = run_bridge(input.as_slice(), Vec::new(), &mut relay, limits()).unwrap_err();
        assert_eq!(error, BridgeError::Handler(HandlerFailure::Unavailable));
        assert_eq!(error.code(), "handler_unavailable");
        assert!(!error.code().contains("do-not-report"));
    }

    #[test]
    fn oversized_daemon_frame_is_rejected_without_forwarding() {
        let transport = ScriptedTransport {
            written: Vec::new(),
            reads: VecDeque::from([Ok(vec![0, 0, 0, 65])]),
        };
        let mut relay = SocketFrameRelay::new(transport, limits()).unwrap();
        let input = encode_frame(b"request", 64).unwrap();
        assert_eq!(
            run_bridge(input.as_slice(), Vec::new(), &mut relay, limits()),
            Err(BridgeError::Handler(HandlerFailure::Rejected))
        );
    }

    #[test]
    fn multiple_daemon_responses_for_one_request_are_rejected() {
        let coalesced_limits = BridgeLimits::new(64, 64, 4).unwrap();
        let mut coalesced = encode_frame(b"first", 64).unwrap();
        coalesced.extend(encode_frame(b"second", 64).unwrap());
        let transport = ScriptedTransport {
            written: Vec::new(),
            reads: VecDeque::from([Ok(coalesced)]),
        };
        let mut relay = SocketFrameRelay::new(transport, coalesced_limits).unwrap();
        let input = encode_frame(b"request", 64).unwrap();
        let mut output = Vec::new();
        assert_eq!(
            run_bridge(input.as_slice(), &mut output, &mut relay, coalesced_limits),
            Err(BridgeError::Handler(HandlerFailure::Rejected))
        );
        assert!(output.is_empty());
    }

    #[test]
    fn missing_socket_error_does_not_expose_injected_path() {
        let secret_path = Path::new("/definitely-missing/secret-token.sock");
        let error = run_unix_socket_relay(&[][..], Vec::new(), secret_path, limits()).unwrap_err();
        assert_eq!(error.code(), "handler_unavailable");
        assert!(!error.code().contains("secret-token"));
    }
}
