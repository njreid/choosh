//! Typed, bounded client handshake over an injected framed duplex transport.

use std::io::{self, Read, Write};

use choosh_protocol::framing::{FrameDecoder, FrameError, FrameLimits, encode_frame};
use choosh_protocol::handshake::{Hello, ServerReply};
use choosh_protocol::session::{ClientSession, SessionError};
use choosh_protocol::wire::{WireError, decode_server_reply, encode_hello};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HandshakeLimits {
    pub max_frame_bytes: usize,
    pub read_buffer_bytes: usize,
    pub max_tracked_workspaces: usize,
}

impl HandshakeLimits {
    /// Creates positive, framing-compatible handshake limits.
    ///
    /// # Errors
    ///
    /// Returns `invalid_limits` when any bound is zero or the frame bound
    /// cannot be represented by the framing protocol.
    pub fn new(
        max_frame_bytes: usize,
        read_buffer_bytes: usize,
        max_tracked_workspaces: usize,
    ) -> Result<Self, ClientHandshakeError> {
        FrameLimits::new(max_frame_bytes, 2).map_err(ClientHandshakeError::Frame)?;
        if read_buffer_bytes == 0 || max_tracked_workspaces == 0 {
            return Err(ClientHandshakeError::InvalidLimits);
        }
        Ok(Self {
            max_frame_bytes,
            read_buffer_bytes,
            max_tracked_workspaces,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientHandshakeError {
    InvalidLimits,
    TransportRead,
    TransportWrite,
    UnexpectedEof,
    MultipleReplies,
    Frame(FrameError),
    Wire(WireError),
    Session(SessionError),
}

impl ClientHandshakeError {
    /// Returns a stable diagnostic that contains no transport or peer data.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::TransportRead => "handshake_read",
            Self::TransportWrite => "handshake_write",
            Self::UnexpectedEof => "handshake_eof",
            Self::MultipleReplies => "multiple_handshake_replies",
            Self::Frame(error) => error.code(),
            Self::Wire(error) => error.code(),
            Self::Session(error) => error.code(),
        }
    }
}

/// Sends exactly one typed hello and validates one typed server reply.
///
/// The returned session is established and owns the negotiated limits. An
/// incompatible response, malformed reply, reordered message, or unadvertised
/// selection fails closed.
///
/// # Errors
///
/// Returns only stable, content-free transport, framing, wire, or session
/// classifications.
pub fn perform_client_handshake<T: Read + Write>(
    transport: &mut T,
    hello: &Hello,
    limits: HandshakeLimits,
) -> Result<ClientSession, ClientHandshakeError> {
    let frame_limits =
        FrameLimits::new(limits.max_frame_bytes, 2).map_err(ClientHandshakeError::Frame)?;
    if limits.read_buffer_bytes == 0 || limits.max_tracked_workspaces == 0 {
        return Err(ClientHandshakeError::InvalidLimits);
    }

    let mut session = ClientSession::new(hello, limits.max_tracked_workspaces)
        .map_err(ClientHandshakeError::Session)?;
    let payload =
        encode_hello(hello, limits.max_frame_bytes).map_err(ClientHandshakeError::Wire)?;
    let frame =
        encode_frame(&payload, limits.max_frame_bytes).map_err(ClientHandshakeError::Frame)?;
    transport
        .write_all(&frame)
        .and_then(|()| transport.flush())
        .map_err(|_| ClientHandshakeError::TransportWrite)?;
    session
        .send_hello()
        .map_err(ClientHandshakeError::Session)?;

    let mut decoder = FrameDecoder::new(frame_limits);
    let mut buffer = vec![0; limits.read_buffer_bytes];
    loop {
        let read = match transport.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(ClientHandshakeError::TransportRead),
        };
        if read == 0 {
            decoder.finish().map_err(ClientHandshakeError::Frame)?;
            return Err(ClientHandshakeError::UnexpectedEof);
        }
        let frames = decoder
            .feed(&buffer[..read])
            .map_err(ClientHandshakeError::Frame)?;
        if frames.len() > 1 {
            return Err(ClientHandshakeError::MultipleReplies);
        }
        let Some(reply_frame) = frames.into_iter().next() else {
            continue;
        };
        let reply = decode_server_reply(&reply_frame, limits.max_frame_bytes)
            .map_err(ClientHandshakeError::Wire)?;
        match reply {
            ServerReply::Welcome(welcome) => session
                .receive_welcome(&welcome, reply_frame.len())
                .map_err(ClientHandshakeError::Session)?,
            ServerReply::Incompatible(incompatible) => session
                .receive_incompatible(incompatible)
                .map_err(ClientHandshakeError::Session)?,
        }
        return Ok(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_protocol::framing::encode_frame;
    use choosh_protocol::handshake::{
        Capability, PeerIdentity, ProtocolLimits, ProtocolVersion, ServerNegotiator,
    };
    use choosh_protocol::session::SessionPhase;
    use choosh_protocol::wire::{decode_hello, encode_server_reply};
    use std::collections::VecDeque;

    struct ScriptedTransport {
        reads: VecDeque<Vec<u8>>,
        written: Vec<u8>,
    }

    impl Read for ScriptedTransport {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let Some(mut bytes) = self.reads.pop_front() else {
                return Ok(0);
            };
            let count = bytes.len().min(output.len());
            output[..count].copy_from_slice(&bytes[..count]);
            if count < bytes.len() {
                bytes.drain(..count);
                self.reads.push_front(bytes);
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

    fn hello() -> Hello {
        Hello::new(
            ProtocolVersion::new(1, 2),
            PeerIdentity::new("android", "1.0").unwrap(),
            [Capability::Events],
        )
        .unwrap()
    }

    fn limits() -> HandshakeLimits {
        HandshakeLimits::new(1024, 7, 8).unwrap()
    }

    fn reply(hello: &Hello, daemon_version: ProtocolVersion) -> Vec<u8> {
        let mut server = ServerNegotiator::new(
            daemon_version,
            PeerIdentity::new("chooshd", "1.0").unwrap(),
            PeerIdentity::new("choosh-host", "1.0").unwrap(),
            [Capability::Events],
            ProtocolLimits::new(512, 4).unwrap(),
        )
        .unwrap();
        let reply = server.receive_hello(hello).unwrap();
        let payload = encode_server_reply(&reply, 1024).unwrap();
        encode_frame(&payload, 1024).unwrap()
    }

    #[test]
    fn establishes_from_fragmented_typed_reply_and_sends_typed_hello() {
        let hello = hello();
        let response = reply(&hello, ProtocolVersion::new(1, 1));
        let reads = response.chunks(3).map(<[u8]>::to_vec).collect();
        let mut transport = ScriptedTransport {
            reads,
            written: Vec::new(),
        };

        let session = perform_client_handshake(&mut transport, &hello, limits()).unwrap();

        assert_eq!(session.phase(), SessionPhase::Established);
        assert_eq!(
            session.negotiated_limits(),
            ProtocolLimits::new(512, 4).ok()
        );
        let payload_length =
            u32::from_be_bytes(transport.written[..4].try_into().unwrap()) as usize;
        let sent = decode_hello(&transport.written[4..], payload_length).unwrap();
        assert_eq!(sent, hello);
    }

    #[test]
    fn incompatible_reply_is_validated_and_fails_closed() {
        let hello = hello();
        let mut transport = ScriptedTransport {
            reads: VecDeque::from([reply(&hello, ProtocolVersion::new(2, 0))]),
            written: Vec::new(),
        };
        let error = perform_client_handshake(&mut transport, &hello, limits()).unwrap_err();
        assert_eq!(
            error,
            ClientHandshakeError::Session(SessionError::Incompatible)
        );
        assert_eq!(error.code(), "incompatible_protocol");
    }

    #[test]
    fn malformed_and_multiple_replies_are_rejected_without_echoing_content() {
        let hello = hello();
        let secret = b"secret-peer-data";
        let malformed = encode_frame(secret, 1024).unwrap();
        let mut transport = ScriptedTransport {
            reads: VecDeque::from([malformed]),
            written: Vec::new(),
        };
        let error = perform_client_handshake(&mut transport, &hello, limits()).unwrap_err();
        assert_eq!(error.code(), "malformed_json");
        assert!(!error.code().contains("secret"));

        let mut repeated = reply(&hello, ProtocolVersion::new(1, 1));
        repeated.extend(reply(&hello, ProtocolVersion::new(1, 1)));
        let mut transport = ScriptedTransport {
            reads: VecDeque::from([repeated]),
            written: Vec::new(),
        };
        let result = perform_client_handshake(
            &mut transport,
            &hello,
            HandshakeLimits::new(1024, 4096, 8).unwrap(),
        );
        assert!(matches!(result, Err(ClientHandshakeError::MultipleReplies)));
    }
}
