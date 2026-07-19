//! One-shot typed RPC exchange over the fixed host dispatcher.
//!
//! This intentionally exposes no raw stdio pipe. Each call sends one framed,
//! schema-shaped request to the fixed `choosh-host rpc --stdio` argv and
//! accepts exactly one matching terminal response frame.

use choosh_protocol::envelope::{EnvelopeId, Method, Response, Terminal};
use choosh_protocol::framing::{FrameDecoder, FrameError, FrameLimits, encode_frame};
use choosh_protocol::wire::{WireEnvelope, WireError, decode_envelope};
use serde_json::Value;

use crate::{FixedCommand, FixedCommandError, FixedExecError, FixedExecOutput, VerifiedConnection};

const MAX_RPC_FRAME_BYTES: usize = 256 * 1024 - 4;

/// A typed, version-one RPC request that can be carried by the fixed host dispatcher.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcRequest {
    id: EnvelopeId,
    method: Method,
    params: Value,
}

impl RpcRequest {
    /// Builds a schema-shaped RPC request from already untrusted JSON parameters.
    #[must_use]
    pub const fn new(id: EnvelopeId, method: Method, params: Value) -> Self {
        Self { id, method, params }
    }

    #[must_use]
    pub fn id(&self) -> &EnvelopeId {
        &self.id
    }
}

/// The only terminal response shape accepted from the fixed RPC bridge.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcResponse {
    pub id: EnvelopeId,
    pub terminal: Terminal<Value, Value>,
}

/// Typed RPC framing and bridge failures without payload-bearing diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RpcError {
    RequestFrame(FrameError),
    FixedCommand(FixedCommandError),
    Transport(FixedExecError),
    RemoteExit,
    ResponseFrame(FrameError),
    ResponseWire(WireError),
    ResponseCount,
    ResponseKind,
    ResponseIdMismatch,
}

impl VerifiedConnection {
    /// Executes one bounded, framed RPC request through `choosh-host rpc --stdio`.
    ///
    /// # Errors
    ///
    /// Returns typed request-frame, fixed-dispatch, SSH, remote-exit,
    /// response-frame, wire-shape, response-count, response-kind, or ID-match
    /// failures. Standard error is deliberately not parsed as protocol data.
    pub async fn request_rpc(&self, request: RpcRequest) -> Result<RpcResponse, RpcError> {
        let frame = encode_request(&request)?;
        let output = self
            .execute_fixed(
                FixedCommand::new(
                    "choosh-host",
                    vec!["rpc".to_owned(), "--stdio".to_owned()],
                    frame,
                )
                .map_err(RpcError::FixedCommand)?,
            )
            .await
            .map_err(RpcError::Transport)?;
        decode_response(&request.id, &output)
    }
}

fn encode_request(request: &RpcRequest) -> Result<Vec<u8>, RpcError> {
    let payload = serde_json::to_vec(&serde_json::json!({
        "kind": "request",
        "id": request.id.as_str(),
        "method": request.method.as_str(),
        "params": request.params,
    }))
    .map_err(|_| RpcError::RequestFrame(FrameError::FrameTooLarge))?;
    encode_frame(&payload, MAX_RPC_FRAME_BYTES).map_err(RpcError::RequestFrame)
}

fn decode_response(
    expected_id: &EnvelopeId,
    output: &FixedExecOutput,
) -> Result<RpcResponse, RpcError> {
    if output.exit_status != 0 {
        return Err(RpcError::RemoteExit);
    }
    let limits = FrameLimits::new(MAX_RPC_FRAME_BYTES, 2).expect("constant RPC limits are valid");
    let mut decoder = FrameDecoder::new(limits);
    let frames = decoder
        .feed(&output.stdout)
        .map_err(RpcError::ResponseFrame)?;
    decoder.finish().map_err(RpcError::ResponseFrame)?;
    let [frame] = frames.as_slice() else {
        return Err(RpcError::ResponseCount);
    };
    let WireEnvelope::Response(Response { id, terminal }) =
        decode_envelope(frame, MAX_RPC_FRAME_BYTES).map_err(RpcError::ResponseWire)?
    else {
        return Err(RpcError::ResponseKind);
    };
    if id != *expected_id {
        return Err(RpcError::ResponseIdMismatch);
    }
    Ok(RpcResponse { id, terminal })
}

#[cfg(test)]
mod tests {
    use choosh_protocol::envelope::{EnvelopeId, Method};
    use choosh_protocol::framing::encode_frame;
    use serde_json::json;

    use super::{FixedExecOutput, RpcError, RpcRequest, decode_response, encode_request};

    fn request() -> RpcRequest {
        RpcRequest::new(
            EnvelopeId::new("00000000-0000-4000-8000-000000000001").unwrap(),
            Method::new("host.describe").unwrap(),
            json!({"fixture": true}),
        )
    }

    #[test]
    fn request_is_one_bounded_protocol_frame_for_fixed_rpc_argv() {
        let encoded = encode_request(&request()).unwrap();
        assert_eq!(
            u32::from_be_bytes(encoded[..4].try_into().unwrap()) as usize,
            encoded.len() - 4
        );
        assert_eq!(
            &encoded[4..],
            br#"{"id":"00000000-0000-4000-8000-000000000001","kind":"request","method":"host.describe","params":{"fixture":true}}"#
        );
    }

    #[test]
    fn response_requires_one_matching_terminal_frame() {
        let request = request();
        let payload = br#"{"id":"00000000-0000-4000-8000-000000000001","kind":"response","result":{"ok":true}}"#;
        let output = FixedExecOutput {
            stdout: encode_frame(payload, 1024).unwrap(),
            stderr: b"untrusted diagnostic".to_vec(),
            exit_status: 0,
        };
        let response = decode_response(request.id(), &output).unwrap();
        assert_eq!(response.id, *request.id());

        let wrong_id = FixedExecOutput {
            stdout: encode_frame(
                br#"{"id":"00000000-0000-4000-4000-000000000002","kind":"response","result":null}"#,
                1024,
            )
            .unwrap(),
            stderr: Vec::new(),
            exit_status: 0,
        };
        assert_eq!(
            decode_response(request.id(), &wrong_id),
            Err(RpcError::ResponseIdMismatch)
        );
    }

    #[test]
    fn response_rejects_multiple_or_truncated_frames_before_json_use() {
        let request = request();
        let frame = encode_frame(
            br#"{"id":"00000000-0000-4000-8000-000000000001","kind":"response","result":null}"#,
            1024,
        )
        .unwrap();
        let mut multiple = frame.clone();
        multiple.extend(&frame);
        assert_eq!(
            decode_response(
                request.id(),
                &FixedExecOutput {
                    stdout: multiple,
                    stderr: Vec::new(),
                    exit_status: 0,
                }
            ),
            Err(RpcError::ResponseCount)
        );
        assert_eq!(
            decode_response(
                request.id(),
                &FixedExecOutput {
                    stdout: frame[..frame.len() - 1].to_vec(),
                    stderr: Vec::new(),
                    exit_status: 0,
                }
            ),
            Err(RpcError::ResponseFrame(
                choosh_protocol::framing::FrameError::TruncatedFrame
            ))
        );
    }
}
