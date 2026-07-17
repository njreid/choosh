//! Bounded raw stdio framing for the daemon bridge.

use std::io::{self, Read, Write};

use choosh_protocol::framing::{FrameDecoder, FrameError, FrameLimits, encode_frame};

pub const MAX_CONTROL_FRAME_BYTES: usize = 1_048_576;
const DEFAULT_READ_BUFFER_BYTES: usize = 16 * 1_024;
const DEFAULT_FRAMES_PER_READ: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BridgeLimits {
    pub max_frame_bytes: usize,
    pub read_buffer_bytes: usize,
    pub max_frames_per_read: usize,
}

impl BridgeLimits {
    /// Creates bounded bridge limits. Control frames may never exceed one MiB.
    ///
    /// # Errors
    ///
    /// Returns [`BridgeError::InvalidLimits`] when any limit is zero or the
    /// frame limit exceeds the protocol's one-MiB ceiling.
    pub fn new(
        max_frame_bytes: usize,
        read_buffer_bytes: usize,
        max_frames_per_read: usize,
    ) -> Result<Self, BridgeError> {
        if max_frame_bytes == 0
            || max_frame_bytes > MAX_CONTROL_FRAME_BYTES
            || read_buffer_bytes == 0
            || max_frames_per_read == 0
        {
            return Err(BridgeError::InvalidLimits);
        }
        Ok(Self {
            max_frame_bytes,
            read_buffer_bytes,
            max_frames_per_read,
        })
    }
}

impl Default for BridgeLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: MAX_CONTROL_FRAME_BYTES,
            read_buffer_bytes: DEFAULT_READ_BUFFER_BYTES,
            max_frames_per_read: DEFAULT_FRAMES_PER_READ,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandlerFailure {
    Rejected,
    Unavailable,
    Internal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeError {
    InvalidLimits,
    Input,
    Output,
    EmptyFrame,
    FrameTooLarge,
    FrameBatchLimit,
    TruncatedFrame,
    DecoderFailed,
    Handler(HandlerFailure),
}

impl BridgeError {
    /// Stable bounded diagnostic classification. It contains no input or payload data.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::Input => "bridge_input",
            Self::Output => "bridge_output",
            Self::EmptyFrame => "empty_frame",
            Self::FrameTooLarge => "frame_too_large",
            Self::FrameBatchLimit => "frame_batch_limit_exceeded",
            Self::TruncatedFrame => "truncated_frame",
            Self::DecoderFailed => "decoder_failed",
            Self::Handler(HandlerFailure::Rejected) => "handler_rejected",
            Self::Handler(HandlerFailure::Unavailable) => "handler_unavailable",
            Self::Handler(HandlerFailure::Internal) => "handler_internal",
        }
    }
}

impl From<FrameError> for BridgeError {
    fn from(error: FrameError) -> Self {
        match error {
            FrameError::InvalidLimits => Self::InvalidLimits,
            FrameError::EmptyFrame => Self::EmptyFrame,
            FrameError::FrameTooLarge => Self::FrameTooLarge,
            FrameError::BatchLimitExceeded => Self::FrameBatchLimit,
            FrameError::TruncatedFrame => Self::TruncatedFrame,
            FrameError::DecoderFailed => Self::DecoderFailed,
        }
    }
}

pub trait FrameHandler {
    /// Consumes one complete frame and may synchronously emit bounded responses.
    ///
    /// # Errors
    ///
    /// Returns a content-free classification when the frame cannot be handled.
    fn handle_frame<W: Write>(
        &mut self,
        frame: &[u8],
        responses: &mut FrameWriter<'_, W>,
    ) -> Result<(), HandlerFailure>;
}

pub struct FrameWriter<'a, W> {
    output: &'a mut W,
    max_frame_bytes: usize,
}

impl<W: Write> FrameWriter<'_, W> {
    /// Encodes and fully writes one response before accepting another.
    ///
    /// # Errors
    ///
    /// Returns a framing classification for an invalid payload or
    /// [`BridgeError::Output`] when the injected stream cannot complete it.
    pub fn write_frame(&mut self, payload: &[u8]) -> Result<(), BridgeError> {
        let encoded = encode_frame(payload, self.max_frame_bytes)?;
        self.output
            .write_all(&encoded)
            .map_err(|_| BridgeError::Output)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BridgeStats {
    pub input_frames: u64,
}

/// Pumps framed input until clean EOF, applying stream backpressure synchronously.
///
/// # Errors
///
/// Returns a stable content-free classification for invalid limits, malformed
/// or truncated framing, stream failure, or handler rejection.
pub fn run_bridge<R, W, H>(
    mut input: R,
    mut output: W,
    handler: &mut H,
    limits: BridgeLimits,
) -> Result<BridgeStats, BridgeError>
where
    R: Read,
    W: Write,
    H: FrameHandler,
{
    let frame_limits = FrameLimits::new(limits.max_frame_bytes, limits.max_frames_per_read)?;
    if limits.max_frame_bytes > MAX_CONTROL_FRAME_BYTES || limits.read_buffer_bytes == 0 {
        return Err(BridgeError::InvalidLimits);
    }
    let mut decoder = FrameDecoder::new(frame_limits);
    let mut buffer = vec![0_u8; limits.read_buffer_bytes];
    let mut stats = BridgeStats::default();
    loop {
        let read = match input.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(BridgeError::Input),
        };
        if read == 0 {
            decoder.finish()?;
            return Ok(stats);
        }
        for frame in decoder.feed(&buffer[..read])? {
            let mut responses = FrameWriter {
                output: &mut output,
                max_frame_bytes: limits.max_frame_bytes,
            };
            handler
                .handle_frame(&frame, &mut responses)
                .map_err(BridgeError::Handler)?;
            stats.input_frames = stats
                .input_frames
                .checked_add(1)
                .ok_or(BridgeError::DecoderFailed)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    #[cfg(unix)]
    use std::os::unix::net::UnixStream;
    #[cfg(unix)]
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct Echo;

    impl FrameHandler for Echo {
        fn handle_frame<W: Write>(
            &mut self,
            frame: &[u8],
            responses: &mut FrameWriter<'_, W>,
        ) -> Result<(), HandlerFailure> {
            responses
                .write_frame(frame)
                .map_err(|_| HandlerFailure::Internal)
        }
    }

    #[derive(Default)]
    struct ShortWriter {
        bytes: Vec<u8>,
        writes: usize,
    }

    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            let count = bytes.len().min(2);
            self.bytes.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Chunks(VecDeque<Vec<u8>>);

    impl Read for Chunks {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let Some(mut chunk) = self.0.pop_front() else {
                return Ok(0);
            };
            let count = chunk.len().min(output.len());
            output[..count].copy_from_slice(&chunk[..count]);
            if count < chunk.len() {
                chunk.drain(..count);
                self.0.push_front(chunk);
            }
            Ok(count)
        }
    }

    fn limits(read: usize) -> BridgeLimits {
        BridgeLimits::new(64, read, 16).unwrap()
    }

    #[test]
    fn fragmented_and_coalesced_frames_round_trip() {
        let mut encoded = encode_frame(b"one", 64).unwrap();
        encoded.extend(encode_frame(b"two", 64).unwrap());
        let chunks = encoded.into_iter().map(|byte| vec![byte]).collect();
        let mut output = Vec::new();
        let stats = run_bridge(Chunks(chunks), &mut output, &mut Echo, limits(3)).unwrap();
        assert_eq!(stats.input_frames, 2);
        assert_eq!(
            output,
            [
                encode_frame(b"one", 64).unwrap(),
                encode_frame(b"two", 64).unwrap()
            ]
            .concat()
        );
    }

    #[test]
    fn short_writes_are_completed_before_more_input() {
        let input = encode_frame(b"response", 64).unwrap();
        let mut output = ShortWriter::default();
        run_bridge(input.as_slice(), &mut output, &mut Echo, limits(64)).unwrap();
        assert_eq!(output.bytes, input);
        assert!(output.writes > 1);
    }

    #[test]
    fn malformed_oversized_and_truncated_inputs_are_classified_without_content() {
        let secret = b"do-not-log-this";
        let cases = [
            (vec![0, 0, 0, 0], BridgeError::EmptyFrame),
            (vec![0, 0, 0, 65], BridgeError::FrameTooLarge),
            (
                encode_frame(secret, 64).unwrap()[..7].to_vec(),
                BridgeError::TruncatedFrame,
            ),
        ];
        for (input, expected) in cases {
            let error =
                run_bridge(input.as_slice(), Vec::new(), &mut Echo, limits(64)).unwrap_err();
            assert_eq!(error, expected);
            assert!(!error.code().contains("do-not-log-this"));
            assert!(error.code().len() <= 32);
        }
    }

    #[test]
    fn limits_reject_zero_and_more_than_one_mibibyte() {
        assert_eq!(BridgeLimits::new(0, 1, 1), Err(BridgeError::InvalidLimits));
        assert_eq!(
            BridgeLimits::new(MAX_CONTROL_FRAME_BYTES + 1, 1, 1),
            Err(BridgeError::InvalidLimits)
        );
    }

    /// Models the SSH-exec stdio boundary with a local full-duplex stream. The handler's shared
    /// state represents daemon/Zellij ownership and deliberately outlives this one bridge.
    #[cfg(unix)]
    #[test]
    fn clean_stdio_eof_ends_bridge_without_stopping_daemon_owned_state() {
        struct DurableHandler {
            handled: Arc<AtomicUsize>,
            remote_process_stopped: Arc<AtomicBool>,
        }

        impl FrameHandler for DurableHandler {
            fn handle_frame<W: Write>(
                &mut self,
                frame: &[u8],
                responses: &mut FrameWriter<'_, W>,
            ) -> Result<(), HandlerFailure> {
                self.handled.fetch_add(1, Ordering::SeqCst);
                responses
                    .write_frame(frame)
                    .map_err(|_| HandlerFailure::Internal)
            }
        }

        let handled = Arc::new(AtomicUsize::new(0));
        let remote_process_stopped = Arc::new(AtomicBool::new(false));
        let (mut client, server) = UnixStream::pair().unwrap();
        let server_output = server.try_clone().unwrap();
        let bridge_handled = Arc::clone(&handled);
        let bridge_stopped = Arc::clone(&remote_process_stopped);
        let bridge = std::thread::spawn(move || {
            let mut handler = DurableHandler {
                handled: bridge_handled,
                remote_process_stopped: bridge_stopped,
            };
            let result = run_bridge(server, server_output, &mut handler, limits(7));
            assert!(!handler.remote_process_stopped.load(Ordering::SeqCst));
            result
        });

        let request = encode_frame(b"hello", 64).unwrap();
        client.write_all(&request).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();

        assert_eq!(bridge.join().unwrap(), Ok(BridgeStats { input_frames: 1 }));
        assert_eq!(response, request);
        assert_eq!(handled.load(Ordering::SeqCst), 1);
        assert!(!remote_process_stopped.load(Ordering::SeqCst));
    }
}
