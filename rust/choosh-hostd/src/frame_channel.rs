//! A length-prefixed, class-tagged frame channel over a `WebSocket`, per
//! `docs/specs/relay-protocol.md`'s framing section. Reuses
//! `choosh_protocol::framing`'s decoder rather than re-implementing it, and
//! treats one `WebSocket` binary message as carrying frame bytes generally
//! (not necessarily exactly one frame) so this stays correct regardless of
//! how the peer chooses to chunk frames across messages.

use std::collections::VecDeque;

use choosh_protocol::framing::{FrameDecoder, FrameError, FrameLimits, encode_frame};
use choosh_protocol::relay::MAX_CONTROL_FRAME_BYTES;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio_tungstenite::tungstenite::Message;

/// Frames buffered per `feed` call before a caller drains them; generous
/// enough that a burst never trips the limit under normal operation, small
/// enough that a misbehaving peer can't queue unbounded memory.
const MAX_FRAMES_PER_FEED: usize = 64;

#[derive(Debug)]
pub enum ChannelError {
    Frame(FrameError),
    Json(serde_json::Error),
    Ws(tokio_tungstenite::tungstenite::Error),
    Closed,
    EmptyFrame,
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Frame(error) => write!(f, "frame error: {}", error.code()),
            Self::Json(error) => write!(f, "JSON error: {error}"),
            Self::Ws(error) => write!(f, "WebSocket error: {error}"),
            Self::Closed => write!(f, "connection closed"),
            Self::EmptyFrame => write!(f, "frame carried no class byte"),
        }
    }
}

impl std::error::Error for ChannelError {}

impl From<serde_json::Error> for ChannelError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for ChannelError {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::Ws(error)
    }
}

/// Wraps a `WebSocket` stream/sink with the relay-protocol frame format.
pub struct FrameChannel<S> {
    ws: S,
    decoder: FrameDecoder,
    pending: VecDeque<Vec<u8>>,
}

impl<S> FrameChannel<S>
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    /// # Panics
    ///
    /// Never, in practice: the limits below are fixed positive constants.
    #[must_use]
    pub fn new(ws: S) -> Self {
        let limits = FrameLimits::new(MAX_CONTROL_FRAME_BYTES, MAX_FRAMES_PER_FEED)
            .expect("MAX_CONTROL_FRAME_BYTES and MAX_FRAMES_PER_FEED are fixed positive constants");
        Self {
            ws,
            decoder: FrameDecoder::new(limits),
            pending: VecDeque::new(),
        }
    }

    /// Sends one frame: `class` as the leading byte, then `value` serialized
    /// as JSON.
    ///
    /// # Errors
    ///
    /// Returns [`ChannelError::Json`] if `value` cannot be serialized,
    /// [`ChannelError::Frame`] if the encoded payload exceeds the control
    /// frame bound, or [`ChannelError::Ws`] if the send fails.
    pub async fn send<T: Serialize>(&mut self, class: u8, value: &T) -> Result<(), ChannelError> {
        let mut payload = Vec::with_capacity(1 + 128);
        payload.push(class);
        serde_json::to_writer(&mut payload, value)?;
        let framed = encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).map_err(ChannelError::Frame)?;
        self.ws.send(Message::Binary(framed.into())).await?;
        Ok(())
    }

    /// Sends one frame whose payload is already fully encoded (`class` as
    /// the leading byte, `body` as the rest) — for cases like a tunnel
    /// frame, where the payload is `tunnel_id` bytes plus a nested,
    /// separately-encoded control frame, not a single `Serialize` value.
    ///
    /// # Errors
    ///
    /// Returns [`ChannelError::Frame`] if the encoded payload exceeds the
    /// control-frame bound, or [`ChannelError::Ws`] if the send fails.
    pub async fn send_bytes(&mut self, class: u8, body: &[u8]) -> Result<(), ChannelError> {
        let mut payload = Vec::with_capacity(1 + body.len());
        payload.push(class);
        payload.extend_from_slice(body);
        let framed = encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).map_err(ChannelError::Frame)?;
        self.ws.send(Message::Binary(framed.into())).await?;
        Ok(())
    }

    /// Receives the next complete frame as `(class, payload_bytes)`.
    ///
    /// # Errors
    ///
    /// Returns [`ChannelError::Closed`] if the peer closes the connection
    /// (or the stream ends) before a full frame arrives, or
    /// [`ChannelError::Frame`] on a malformed frame — per relay-protocol.md,
    /// a malformed frame is unrecoverable; callers MUST NOT keep using a
    /// channel after this variant.
    pub async fn recv_raw(&mut self) -> Result<(u8, Vec<u8>), ChannelError> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                let (class, body) = frame.split_first().ok_or(ChannelError::EmptyFrame)?;
                return Ok((*class, body.to_vec()));
            }
            match self.ws.next().await {
                Some(Ok(Message::Binary(bytes))) => {
                    let frames = self.decoder.feed(&bytes).map_err(ChannelError::Frame)?;
                    self.pending.extend(frames);
                }
                // relay-protocol.md is binary-only; ignore stray text/control frames.
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_) | Message::Text(_))) => {}
                Some(Ok(Message::Close(_))) | None => return Err(ChannelError::Closed),
                Some(Err(error)) => return Err(ChannelError::Ws(error)),
            }
        }
    }

    /// Receives the next frame and deserializes its body as `T`, discarding
    /// the class byte. Use [`Self::recv_raw`] instead when the caller needs
    /// to branch on the class.
    ///
    /// # Errors
    ///
    /// See [`Self::recv_raw`]; additionally returns [`ChannelError::Json`]
    /// if the body does not deserialize as `T`.
    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<T, ChannelError> {
        let (_class, body) = self.recv_raw().await?;
        Ok(serde_json::from_slice(&body)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;
    use serde::Deserialize;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct Ping {
        value: u32,
    }

    /// A fake duplex "WebSocket": reads from a fixed inbound message list,
    /// records outbound sends, never actually touches a network — this is
    /// what lets `send`/`recv_raw` be tested without a real server.
    struct FakeWs {
        inbound: VecDeque<Result<Message, tokio_tungstenite::tungstenite::Error>>,
        outbound: Vec<Message>,
    }

    impl Stream for FakeWs {
        type Item = Result<Message, tokio_tungstenite::tungstenite::Error>;
        fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            Poll::Ready(self.inbound.pop_front())
        }
    }

    impl Sink<Message> for FakeWs {
        type Error = tokio_tungstenite::tungstenite::Error;
        fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            self.outbound.push(item);
            Ok(())
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn send_then_recv_round_trips_through_real_framing() {
        // Build the "wire bytes" the same way `send` would, then hand them
        // back in as a single inbound message, proving encode/decode agree.
        let mut payload = vec![7u8];
        payload.extend(serde_json::to_vec(&Ping { value: 42 }).unwrap());
        let framed = encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).unwrap();

        let fake = FakeWs { inbound: VecDeque::from([Ok(Message::Binary(framed.into()))]), outbound: Vec::new() };
        let mut channel = FrameChannel::new(fake);

        let (class, ping): (u8, Ping) = {
            let (class, body) = channel.recv_raw().await.unwrap();
            (class, serde_json::from_slice(&body).unwrap())
        };
        assert_eq!(class, 7);
        assert_eq!(ping, Ping { value: 42 });
    }

    #[tokio::test]
    async fn a_frame_split_across_two_messages_still_decodes() {
        let mut payload = vec![1u8];
        payload.extend(serde_json::to_vec(&Ping { value: 9 }).unwrap());
        let framed = encode_frame(&payload, MAX_CONTROL_FRAME_BYTES).unwrap();
        let (first_half, second_half) = framed.split_at(3);

        let fake = FakeWs {
            inbound: VecDeque::from([
                Ok(Message::Binary(first_half.to_vec().into())),
                Ok(Message::Binary(second_half.to_vec().into())),
            ]),
            outbound: Vec::new(),
        };
        let mut channel = FrameChannel::new(fake);
        let ping: Ping = channel.recv().await.unwrap();
        assert_eq!(ping, Ping { value: 9 });
    }

    #[tokio::test]
    async fn closed_stream_is_reported_as_closed_not_silently_hung() {
        let fake = FakeWs { inbound: VecDeque::from([Ok(Message::Close(None))]), outbound: Vec::new() };
        let mut channel = FrameChannel::new(fake);
        assert!(matches!(channel.recv_raw().await, Err(ChannelError::Closed)));
    }

    #[tokio::test]
    async fn end_of_stream_with_no_close_frame_is_also_closed() {
        let fake = FakeWs { inbound: VecDeque::new(), outbound: Vec::new() };
        let mut channel = FrameChannel::new(fake);
        assert!(matches!(channel.recv_raw().await, Err(ChannelError::Closed)));
    }

    #[tokio::test]
    async fn send_produces_correctly_length_prefixed_bytes() {
        let fake = FakeWs { inbound: VecDeque::new(), outbound: Vec::new() };
        let mut channel = FrameChannel::new(fake);
        channel.send(3, &Ping { value: 1 }).await.unwrap();

        let Message::Binary(bytes) = &channel.ws.outbound[0] else {
            panic!("expected a binary message");
        };
        let mut decoder = FrameDecoder::new(FrameLimits::new(MAX_CONTROL_FRAME_BYTES, 1).unwrap());
        let frames = decoder.feed(bytes).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0][0], 3);
        let _ = stream::empty::<()>(); // keep `stream` import used across cfg variations
    }
}
