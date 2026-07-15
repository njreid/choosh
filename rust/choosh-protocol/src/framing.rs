//! Incremental four-byte big-endian length-prefixed framing.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLimits {
    pub max_frame_bytes: usize,
    pub max_frames_per_feed: usize,
}

impl FrameLimits {
    /// Creates positive limits representable by the four-byte wire header.
    ///
    /// # Errors
    ///
    /// Returns `invalid_limits` for a zero limit or a frame bound above `u32::MAX`.
    pub fn new(max_frame_bytes: usize, max_frames_per_feed: usize) -> Result<Self, FrameError> {
        if max_frame_bytes == 0 || max_frames_per_feed == 0 || max_frame_bytes > u32::MAX as usize {
            return Err(FrameError::InvalidLimits);
        }
        Ok(Self {
            max_frame_bytes,
            max_frames_per_feed,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    InvalidLimits,
    EmptyFrame,
    FrameTooLarge,
    BatchLimitExceeded,
    TruncatedFrame,
    DecoderFailed,
}

impl FrameError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::EmptyFrame => "empty_frame",
            Self::FrameTooLarge => "frame_too_large",
            Self::BatchLimitExceeded => "frame_batch_limit_exceeded",
            Self::TruncatedFrame => "truncated_frame",
            Self::DecoderFailed => "decoder_failed",
        }
    }
}

/// Encodes one non-empty frame without accepting sizes the wire format cannot represent.
///
/// # Errors
///
/// Returns `empty_frame` for an empty payload or `frame_too_large` when the
/// configured or wire-format limit would be exceeded.
pub fn encode_frame(payload: &[u8], max_frame_bytes: usize) -> Result<Vec<u8>, FrameError> {
    if payload.is_empty() {
        return Err(FrameError::EmptyFrame);
    }
    if payload.len() > max_frame_bytes || payload.len() > u32::MAX as usize {
        return Err(FrameError::FrameTooLarge);
    }
    let mut encoded = Vec::with_capacity(4 + payload.len());
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::FrameTooLarge)?;
    encoded.extend_from_slice(&length.to_be_bytes());
    encoded.extend_from_slice(payload);
    Ok(encoded)
}

#[derive(Debug)]
pub struct FrameDecoder {
    limits: FrameLimits,
    header: [u8; 4],
    header_len: usize,
    expected_payload: Option<usize>,
    payload: Vec<u8>,
    failed: bool,
}

impl FrameDecoder {
    #[must_use]
    pub const fn new(limits: FrameLimits) -> Self {
        Self {
            limits,
            header: [0; 4],
            header_len: 0,
            expected_payload: None,
            payload: Vec::new(),
            failed: false,
        }
    }

    #[must_use]
    pub const fn buffered_bytes(&self) -> usize {
        self.header_len + self.payload.len()
    }

    /// Consumes arbitrary stream fragments and returns every complete frame.
    ///
    /// # Errors
    ///
    /// A malformed length or per-call batch overflow permanently fails the decoder.
    /// Subsequent calls return `decoder_failed`, preventing recovery on an ambiguous
    /// byte boundary.
    pub fn feed(&mut self, mut input: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        if self.failed {
            return Err(FrameError::DecoderFailed);
        }
        let mut frames = Vec::new();

        while !input.is_empty() {
            if self.expected_payload.is_none() {
                let needed = 4 - self.header_len;
                let copied = needed.min(input.len());
                self.header[self.header_len..self.header_len + copied]
                    .copy_from_slice(&input[..copied]);
                self.header_len += copied;
                input = &input[copied..];
                if self.header_len < 4 {
                    continue;
                }

                let length = u32::from_be_bytes(self.header) as usize;
                if length == 0 {
                    return self.fail(FrameError::EmptyFrame);
                }
                if length > self.limits.max_frame_bytes {
                    return self.fail(FrameError::FrameTooLarge);
                }
                self.header_len = 0;
                self.header = [0; 4];
                self.expected_payload = Some(length);
                self.payload = Vec::with_capacity(length);
            }

            let Some(expected) = self.expected_payload else {
                return self.fail(FrameError::DecoderFailed);
            };
            let needed = expected - self.payload.len();
            let copied = needed.min(input.len());
            self.payload.extend_from_slice(&input[..copied]);
            input = &input[copied..];

            if self.payload.len() == expected {
                if frames.len() == self.limits.max_frames_per_feed {
                    return self.fail(FrameError::BatchLimitExceeded);
                }
                frames.push(std::mem::take(&mut self.payload));
                self.expected_payload = None;
                self.header_len = 0;
                self.header = [0; 4];
            }
        }
        Ok(frames)
    }

    /// Confirms that EOF occurred exactly between frames.
    ///
    /// # Errors
    ///
    /// Returns `truncated_frame` for a partial header or payload, and permanently
    /// fails the decoder. A previously failed decoder returns `decoder_failed`.
    pub fn finish(&mut self) -> Result<(), FrameError> {
        if self.failed {
            return Err(FrameError::DecoderFailed);
        }
        if self.header_len != 0 || self.expected_payload.is_some() {
            return self.fail(FrameError::TruncatedFrame);
        }
        Ok(())
    }

    fn fail<T>(&mut self, error: FrameError) -> Result<T, FrameError> {
        self.failed = true;
        self.header_len = 0;
        self.expected_payload = None;
        self.payload.clear();
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameDecoder, FrameError, FrameLimits, encode_frame};

    fn limits() -> FrameLimits {
        FrameLimits::new(64, 8).unwrap()
    }

    #[test]
    fn every_fragmentation_boundary_decodes_identically() {
        let encoded = encode_frame(b"fragment me", 64).unwrap();
        for boundary in 0..=encoded.len() {
            let mut decoder = FrameDecoder::new(limits());
            let mut frames = decoder.feed(&encoded[..boundary]).unwrap();
            frames.extend(decoder.feed(&encoded[boundary..]).unwrap());
            assert_eq!(frames, [b"fragment me".to_vec()]);
            assert_eq!(decoder.buffered_bytes(), 0);
            assert_eq!(decoder.finish(), Ok(()));
        }
    }

    #[test]
    fn byte_at_a_time_decoding_is_bounded() {
        let encoded = encode_frame(&[7; 64], 64).unwrap();
        let mut decoder = FrameDecoder::new(limits());
        let mut frames = Vec::new();
        for byte in encoded {
            frames.extend(decoder.feed(&[byte]).unwrap());
            assert!(decoder.buffered_bytes() <= 64);
        }
        assert_eq!(frames, [vec![7; 64]]);
    }

    #[test]
    fn coalesced_frames_preserve_order() {
        let mut bytes = encode_frame(b"one", 64).unwrap();
        bytes.extend(encode_frame(b"two", 64).unwrap());
        bytes.extend(encode_frame(b"three", 64).unwrap());
        let mut decoder = FrameDecoder::new(limits());
        assert_eq!(
            decoder.feed(&bytes),
            Ok(vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()])
        );
    }

    #[test]
    fn empty_and_oversized_lengths_fail_closed() {
        let mut empty = FrameDecoder::new(limits());
        assert_eq!(empty.feed(&[0, 0, 0, 0]), Err(FrameError::EmptyFrame));
        assert_eq!(empty.feed(&[]), Err(FrameError::DecoderFailed));

        let mut oversized = FrameDecoder::new(limits());
        assert_eq!(
            oversized.feed(&65_u32.to_be_bytes()),
            Err(FrameError::FrameTooLarge)
        );
        assert_eq!(oversized.buffered_bytes(), 0);
        assert_eq!(oversized.finish(), Err(FrameError::DecoderFailed));
    }

    #[test]
    fn exact_maximum_encodes_and_maximum_plus_one_is_rejected() {
        assert_eq!(encode_frame(&[1; 64], 64).unwrap().len(), 68);
        assert_eq!(encode_frame(&[1; 65], 64), Err(FrameError::FrameTooLarge));
        assert_eq!(encode_frame(&[], 64), Err(FrameError::EmptyFrame));
    }

    #[test]
    fn eof_detects_every_partial_header_and_payload() {
        let encoded = encode_frame(b"payload", 64).unwrap();
        for boundary in 1..encoded.len() {
            let mut decoder = FrameDecoder::new(limits());
            assert!(decoder.feed(&encoded[..boundary]).unwrap().is_empty());
            assert_eq!(decoder.finish(), Err(FrameError::TruncatedFrame));
            assert_eq!(decoder.finish(), Err(FrameError::DecoderFailed));
        }
    }

    #[test]
    fn per_feed_batch_limit_fails_instead_of_growing_output() {
        let limits = FrameLimits::new(4, 2).unwrap();
        let mut bytes = encode_frame(b"a", 4).unwrap();
        bytes.extend(encode_frame(b"b", 4).unwrap());
        bytes.extend(encode_frame(b"c", 4).unwrap());
        let mut decoder = FrameDecoder::new(limits);
        assert_eq!(decoder.feed(&bytes), Err(FrameError::BatchLimitExceeded));
        assert_eq!(decoder.feed(&[]), Err(FrameError::DecoderFailed));
    }

    #[test]
    fn invalid_configuration_is_rejected() {
        assert_eq!(FrameLimits::new(0, 1), Err(FrameError::InvalidLimits));
        assert_eq!(FrameLimits::new(1, 0), Err(FrameError::InvalidLimits));
    }
}
