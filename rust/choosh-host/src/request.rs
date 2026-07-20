//! One bounded typed request/response lifecycle over an injected transport.

use std::io::{self, Read, Write};

use choosh_protocol::envelope::{Request, Response};
use choosh_protocol::framing::{FrameDecoder, FrameError, FrameLimits, encode_frame};
use choosh_protocol::session::{ClientSession, SessionError};
use choosh_protocol::wire::{WireEnvelope, WireError, decode_envelope};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestLimits {
    pub max_frame_bytes: usize,
    pub read_buffer_bytes: usize,
}

impl RequestLimits {
    /// Creates positive, framing-compatible local safety limits.
    ///
    /// The established session's negotiated frame limit is enforced separately.
    ///
    /// # Errors
    ///
    /// Returns `invalid_limits` for a zero read bound or invalid frame bound.
    pub fn new(
        max_frame_bytes: usize,
        read_buffer_bytes: usize,
    ) -> Result<Self, ClientRequestError> {
        FrameLimits::new(max_frame_bytes, 2).map_err(ClientRequestError::Frame)?;
        if read_buffer_bytes == 0 {
            return Err(ClientRequestError::InvalidLimits);
        }
        Ok(Self {
            max_frame_bytes,
            read_buffer_bytes,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientRequestError {
    InvalidLimits,
    InvalidParams,
    TransportRead,
    TransportWrite,
    UnexpectedEof,
    UnexpectedEnvelope,
    MultipleResponses,
    Frame(FrameError),
    Wire(WireError),
    Session(SessionError),
}

impl ClientRequestError {
    /// Returns a stable classification containing no request or response data.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::InvalidParams => "invalid_params",
            Self::TransportRead => "request_read",
            Self::TransportWrite => "request_write",
            Self::UnexpectedEof => "request_eof",
            Self::UnexpectedEnvelope => "expected_response",
            Self::MultipleResponses => "multiple_responses",
            Self::Frame(error) => error.code(),
            Self::Wire(error) => error.code(),
            Self::Session(error) => error.code(),
        }
    }
}

/// Sends one typed request and returns its correlated typed terminal response.
///
/// The request is registered with the established session before transport I/O.
/// Any transport or response protocol failure closes the session. Local encoding
/// failures leave it unchanged.
///
/// # Errors
///
/// Returns stable, content-free validation, transport, framing, wire, or
/// session classifications.
pub fn perform_request<T: Read + Write>(
    transport: &mut T,
    session: &mut ClientSession,
    request: &Request<Value>,
    limits: RequestLimits,
) -> Result<Response<Value, Value>, ClientRequestError> {
    FrameLimits::new(limits.max_frame_bytes, 2).map_err(ClientRequestError::Frame)?;
    if limits.read_buffer_bytes == 0 {
        return Err(ClientRequestError::InvalidLimits);
    }
    if !request.params.is_object() {
        return Err(ClientRequestError::InvalidParams);
    }
    let payload = encode_request(request, limits.max_frame_bytes)?;
    let frame =
        encode_frame(&payload, limits.max_frame_bytes).map_err(ClientRequestError::Frame)?;
    session
        .send_request(request, payload.len())
        .map_err(ClientRequestError::Session)?;
    if transport
        .write_all(&frame)
        .and_then(|()| transport.flush())
        .is_err()
    {
        session.receive_eof();
        return Err(ClientRequestError::TransportWrite);
    }

    let response_frame = read_response_frame(transport, session, limits)?;
    let envelope = match decode_envelope(&response_frame, limits.max_frame_bytes) {
        Ok(envelope) => envelope,
        Err(error) => {
            session.receive_eof();
            return Err(ClientRequestError::Wire(error));
        }
    };
    let WireEnvelope::Response(response) = envelope else {
        session.receive_eof();
        return Err(ClientRequestError::UnexpectedEnvelope);
    };
    session
        .receive_response(&response, response_frame.len())
        .map_err(ClientRequestError::Session)?;
    Ok(response)
}

fn encode_request(
    request: &Request<Value>,
    max_frame_bytes: usize,
) -> Result<Vec<u8>, ClientRequestError> {
    if max_frame_bytes == 0 {
        return Err(ClientRequestError::InvalidLimits);
    }
    let value = serde_json::json!({
        "kind": "request",
        "id": request.id.as_str(),
        "method": request.method.as_str(),
        "params": request.params,
    });
    let encoded = serde_json::to_vec(&value)
        .map_err(|_| ClientRequestError::Wire(WireError::MalformedJson))?;
    if encoded.len() > max_frame_bytes {
        Err(ClientRequestError::Wire(WireError::PayloadTooLarge))
    } else {
        Ok(encoded)
    }
}

fn read_response_frame<T: Read>(
    transport: &mut T,
    session: &mut ClientSession,
    limits: RequestLimits,
) -> Result<Vec<u8>, ClientRequestError> {
    let frame_limits =
        FrameLimits::new(limits.max_frame_bytes, 2).map_err(ClientRequestError::Frame)?;
    let mut decoder = FrameDecoder::new(frame_limits);
    let mut buffer = vec![0; limits.read_buffer_bytes];
    loop {
        let read = match transport.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                session.receive_eof();
                return Err(ClientRequestError::TransportRead);
            }
        };
        if read == 0 {
            let error = decoder
                .finish()
                .err()
                .map_or(ClientRequestError::UnexpectedEof, ClientRequestError::Frame);
            session.receive_eof();
            return Err(error);
        }
        match decoder.feed(&buffer[..read]) {
            Ok(frames) => {
                if frames.len() > 1 {
                    session.receive_eof();
                    return Err(ClientRequestError::MultipleResponses);
                }
                if let Some(frame) = frames.into_iter().next() {
                    return Ok(frame);
                }
            }
            Err(error) => {
                session.receive_eof();
                return Err(ClientRequestError::Frame(error));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_protocol::envelope::{EnvelopeId, Method, Terminal};
    use choosh_protocol::handshake::{
        Capability, Hello, PeerIdentity, ProtocolLimits, ProtocolVersion, Welcome,
    };
    use choosh_protocol::session::SessionPhase;
    use choosh_protocol::wire::{decode_envelope, encode_response};
    use std::collections::VecDeque;

    struct ScriptedTransport {
        reads: VecDeque<Vec<u8>>,
        written: Vec<u8>,
        fail_write: bool,
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
            if self.fail_write {
                return Err(io::Error::other("injected"));
            }
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn established_session() -> ClientSession {
        let hello = Hello::new(
            ProtocolVersion::new(1, 0),
            PeerIdentity::new("android", "test").unwrap(),
            [Capability::Events],
        )
        .unwrap();
        let mut session = ClientSession::new(&hello, 8).unwrap();
        session.send_hello().unwrap();
        let welcome = Welcome::new(
            ProtocolVersion::new(1, 0),
            PeerIdentity::new("chooshd", "test").unwrap(),
            PeerIdentity::new("host", "test").unwrap(),
            [Capability::Events],
            ProtocolLimits::new(512, 2).unwrap(),
        )
        .unwrap();
        session.receive_welcome(&welcome, 100).unwrap();
        session
    }

    fn request(id: &str) -> Request<Value> {
        Request {
            id: EnvelopeId::new(id).unwrap(),
            method: Method::new("host.describe").unwrap(),
            params: serde_json::json!({}),
        }
    }

    fn framed_response(id: &str) -> Vec<u8> {
        let response = Response {
            id: EnvelopeId::new(id).unwrap(),
            terminal: Terminal::<Value, Value>::Result(serde_json::json!({"ready": true})),
        };
        let payload = encode_response(&response, 512).unwrap();
        encode_frame(&payload, 512).unwrap()
    }

    #[test]
    fn sends_canonical_request_and_correlates_fragmented_response() {
        let id = "00000000-0000-0000-0000-000000000001";
        let framed = framed_response(id);
        let mut transport = ScriptedTransport {
            reads: framed.chunks(2).map(<[u8]>::to_vec).collect(),
            written: Vec::new(),
            fail_write: false,
        };
        let mut session = established_session();

        let response = perform_request(
            &mut transport,
            &mut session,
            &request(id),
            RequestLimits::new(512, 3).unwrap(),
        )
        .unwrap();

        assert!(matches!(response.terminal, Terminal::Result(_)));
        assert_eq!(session.in_flight(), 0);
        let length = u32::from_be_bytes(transport.written[..4].try_into().unwrap()) as usize;
        assert_eq!(length, transport.written.len() - 4);
        assert!(matches!(
            decode_envelope(&transport.written[4..], 512),
            Ok(WireEnvelope::Request(_))
        ));
    }

    #[test]
    fn mismatched_response_id_closes_session_without_exposing_payload() {
        let mut transport = ScriptedTransport {
            reads: VecDeque::from([framed_response("00000000-0000-0000-0000-000000000099")]),
            written: Vec::new(),
            fail_write: false,
        };
        let mut session = established_session();
        let error = perform_request(
            &mut transport,
            &mut session,
            &request("00000000-0000-0000-0000-000000000001"),
            RequestLimits::new(512, 512).unwrap(),
        )
        .unwrap_err();

        assert_eq!(error.code(), "unknown_response");
        assert_eq!(session.phase(), SessionPhase::Closed);
    }

    #[test]
    fn coalesced_responses_close_the_session_before_one_can_be_accepted() {
        let id = "00000000-0000-0000-0000-000000000001";
        let mut responses = framed_response(id);
        responses.extend(framed_response(id));
        let mut transport = ScriptedTransport {
            reads: VecDeque::from([responses]),
            written: Vec::new(),
            fail_write: false,
        };
        let mut session = established_session();

        assert_eq!(
            perform_request(
                &mut transport,
                &mut session,
                &request(id),
                RequestLimits::new(512, 4096).unwrap(),
            ),
            Err(ClientRequestError::MultipleResponses)
        );
        assert_eq!(session.phase(), SessionPhase::Closed);
    }

    #[test]
    fn write_failure_closes_session_deterministically() {
        let mut transport = ScriptedTransport {
            reads: VecDeque::new(),
            written: Vec::new(),
            fail_write: true,
        };
        let mut session = established_session();
        let error = perform_request(
            &mut transport,
            &mut session,
            &request("00000000-0000-0000-0000-000000000001"),
            RequestLimits::new(512, 16).unwrap(),
        )
        .unwrap_err();

        assert_eq!(error, ClientRequestError::TransportWrite);
        assert_eq!(session.phase(), SessionPhase::Closed);
    }
}
