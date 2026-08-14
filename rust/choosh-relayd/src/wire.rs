//! Frame-level helpers built on `choosh_protocol::framing`'s length-prefix
//! codec, adding relay-protocol.md's one-byte class discriminant. Each
//! logical frame this module produces/consumes is sent/received as exactly
//! one `axum::extract::ws::Message::Binary`.

use choosh_protocol::framing::{FrameDecoder, FrameError, FrameLimits};
use choosh_protocol::relay::{FRAME_CLASS_CONTROL, MAX_CONTROL_FRAME_BYTES};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[must_use]
pub fn control_frame_limits() -> FrameLimits {
    FrameLimits::new(MAX_CONTROL_FRAME_BYTES, 1)
        .expect("MAX_CONTROL_FRAME_BYTES and a batch size of 1 are always valid limits")
}

#[must_use]
pub fn new_decoder() -> FrameDecoder {
    FrameDecoder::new(control_frame_limits())
}

// Variant payloads are read via `Debug` at every call site (`tracing::warn!(?err, ...)`)
// for diagnostics, which rustc's dead_code lint doesn't count as real usage —
// this is intentional, not dead.
#[derive(Debug)]
#[allow(dead_code)]
pub enum EncodeError {
    Serialize(serde_json::Error),
    Frame(FrameError),
}

/// Encodes `body` as a length-prefixed, class-`0x01` control frame ready to
/// send as one WS binary message.
///
/// # Errors
///
/// Returns [`EncodeError::Serialize`] if `body` cannot be serialized to
/// JSON, or [`EncodeError::Frame`] if the encoded payload exceeds
/// `MAX_CONTROL_FRAME_BYTES`.
pub fn encode_control<T: Serialize>(body: &T) -> Result<Vec<u8>, EncodeError> {
    let json = serde_json::to_vec(body).map_err(EncodeError::Serialize)?;
    let mut payload = Vec::with_capacity(1 + json.len());
    payload.push(FRAME_CLASS_CONTROL);
    payload.extend_from_slice(&json);
    choosh_protocol::framing::encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).map_err(EncodeError::Frame)
}

// See the `EncodeError` note above: these fields are read via `Debug` for
// diagnostics, which the dead_code lint doesn't recognize as usage.
#[derive(Debug)]
#[allow(dead_code)]
pub enum DecodeError {
    Frame(FrameError),
    WrongClass(u8),
    Deserialize(serde_json::Error),
}

/// Decodes one already-length-unwrapped frame payload (as yielded by
/// [`choosh_protocol::framing::FrameDecoder::feed`]) as a class-`0x01`
/// control frame body.
///
/// # Errors
///
/// Returns [`DecodeError::WrongClass`] if the frame's class byte is not
/// `0x01`, or [`DecodeError::Deserialize`] if the remaining bytes are not
/// valid JSON for `T`.
pub fn decode_control<T: DeserializeOwned>(frame: &[u8]) -> Result<T, DecodeError> {
    let (class, body) = frame.split_first().ok_or(DecodeError::Frame(FrameError::EmptyFrame))?;
    if *class != FRAME_CLASS_CONTROL {
        return Err(DecodeError::WrongClass(*class));
    }
    serde_json::from_slice(body).map_err(DecodeError::Deserialize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        value: u32,
    }

    #[test]
    fn control_frame_round_trips_through_decoder() {
        let wire = encode_control(&Sample { value: 7 }).unwrap();
        let mut decoder = new_decoder();
        let frames = decoder.feed(&wire).unwrap();
        assert_eq!(frames.len(), 1);
        let round_tripped: Sample = decode_control(&frames[0]).unwrap();
        assert_eq!(round_tripped, Sample { value: 7 });
    }

    #[test]
    fn wrong_class_byte_is_rejected() {
        let mut payload = vec![0x02u8];
        payload.extend_from_slice(b"{}");
        assert!(matches!(
            decode_control::<Sample>(&payload),
            Err(DecodeError::WrongClass(0x02))
        ));
    }
}
