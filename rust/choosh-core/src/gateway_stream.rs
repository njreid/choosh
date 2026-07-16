//! Bounded lifecycle accounting for long-lived WebSocket and SSE gateway streams.

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamKind {
    WebSocket,
    ServerSentEvents,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    ClientToHost,
    HostToClient,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebSocketFrameKind {
    Text,
    Binary,
    Continuation,
    Ping,
    Pong,
    Close,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Enqueue {
    pub id: u64,
    pub generation: u64,
    pub direction: Direction,
    pub bytes: usize,
    pub frame: Option<WebSocketFrameKind>,
    pub final_fragment: bool,
    pub now_ms: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamLimits {
    pub max_connections: usize,
    pub max_queued_per_direction: usize,
    pub max_frame_bytes: usize,
    pub max_control_bytes: usize,
    pub idle_ms: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamError {
    InvalidLimits,
    ConnectionLimit,
    UnknownConnection,
    StaleGeneration,
    QueueLimit,
    InvalidFrame,
    FrameTooLarge,
    ControlFrameTooLarge,
    AccountingUnderflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Connection {
    kind: StreamKind,
    generation: u64,
    client_queue: usize,
    host_queue: usize,
    last_activity_ms: u64,
}

#[derive(Debug)]
pub struct StreamRegistry {
    limits: StreamLimits,
    next_id: u64,
    connections: BTreeMap<u64, Connection>,
}

impl StreamRegistry {
    /// Creates bounded stream accounting.
    /// # Errors
    /// Returns [`StreamError::InvalidLimits`] for any unusable zero limit.
    pub fn new(limits: StreamLimits) -> Result<Self, StreamError> {
        if limits.max_connections == 0
            || limits.max_queued_per_direction == 0
            || limits.max_frame_bytes == 0
            || limits.max_control_bytes == 0
            || limits.idle_ms == 0
        {
            return Err(StreamError::InvalidLimits);
        }
        Ok(Self {
            limits,
            next_id: 1,
            connections: BTreeMap::new(),
        })
    }
    /// Opens a long-lived stream with generation one.
    /// # Errors
    /// Returns connection limit or identifier exhaustion as a stable limit error.
    pub fn open(&mut self, kind: StreamKind, now_ms: u64) -> Result<(u64, u64), StreamError> {
        if self.connections.len() >= self.limits.max_connections {
            return Err(StreamError::ConnectionLimit);
        }
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(StreamError::ConnectionLimit)?;
        self.connections.insert(
            id,
            Connection {
                kind,
                generation: 1,
                client_queue: 0,
                host_queue: 0,
                last_activity_ms: now_ms,
            },
        );
        Ok((id, 1))
    }
    /// Reserves bounded bytes in one direction, implementing backpressure.
    /// # Errors
    /// Returns unknown/stale, frame validation, overflow, or queue-limit errors without mutation.
    pub fn enqueue(&mut self, request: Enqueue) -> Result<(), StreamError> {
        let Enqueue {
            id,
            generation,
            direction,
            bytes,
            frame,
            final_fragment,
            now_ms,
        } = request;
        let c = self
            .connections
            .get_mut(&id)
            .ok_or(StreamError::UnknownConnection)?;
        check_generation(c, generation)?;
        if bytes > self.limits.max_frame_bytes {
            return Err(StreamError::FrameTooLarge);
        }
        if c.kind == StreamKind::WebSocket {
            let kind = frame.ok_or(StreamError::InvalidFrame)?;
            if matches!(
                kind,
                WebSocketFrameKind::Ping | WebSocketFrameKind::Pong | WebSocketFrameKind::Close
            ) {
                if !final_fragment {
                    return Err(StreamError::InvalidFrame);
                }
                if bytes > self.limits.max_control_bytes {
                    return Err(StreamError::ControlFrameTooLarge);
                }
            }
        } else if frame.is_some() {
            return Err(StreamError::InvalidFrame);
        }
        let queued = match direction {
            Direction::ClientToHost => &mut c.client_queue,
            Direction::HostToClient => &mut c.host_queue,
        };
        let next = queued.checked_add(bytes).ok_or(StreamError::QueueLimit)?;
        if next > self.limits.max_queued_per_direction {
            return Err(StreamError::QueueLimit);
        }
        *queued = next;
        c.last_activity_ms = now_ms;
        Ok(())
    }
    /// Releases forwarded bytes from one directional queue.
    /// # Errors
    /// Returns unknown/stale or underflow errors.
    pub fn release(
        &mut self,
        id: u64,
        generation: u64,
        direction: Direction,
        bytes: usize,
    ) -> Result<(), StreamError> {
        let c = self
            .connections
            .get_mut(&id)
            .ok_or(StreamError::UnknownConnection)?;
        check_generation(c, generation)?;
        let queued = match direction {
            Direction::ClientToHost => &mut c.client_queue,
            Direction::HostToClient => &mut c.host_queue,
        };
        if bytes > *queued {
            return Err(StreamError::AccountingUnderflow);
        }
        *queued -= bytes;
        Ok(())
    }
    /// Closes only the matching stream generation.
    /// # Errors
    /// Returns unknown or stale generation errors.
    pub fn cancel(&mut self, id: u64, generation: u64) -> Result<(), StreamError> {
        let c = self
            .connections
            .get(&id)
            .ok_or(StreamError::UnknownConnection)?;
        check_generation(c, generation)?;
        self.connections.remove(&id);
        Ok(())
    }
    /// Removes streams idle at least the configured duration. SSE has no separate short timeout.
    #[must_use]
    pub fn expire_idle(&mut self, now_ms: u64) -> Vec<u64> {
        let idle = self.limits.idle_ms;
        let expired = self
            .connections
            .iter()
            .filter(|(_, c)| now_ms.saturating_sub(c.last_activity_ms) >= idle)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in &expired {
            self.connections.remove(id);
        }
        expired
    }
    #[must_use]
    pub fn queued(&self, id: u64, direction: Direction) -> Option<usize> {
        self.connections.get(&id).map(|c| match direction {
            Direction::ClientToHost => c.client_queue,
            Direction::HostToClient => c.host_queue,
        })
    }
}
fn check_generation(c: &Connection, generation: u64) -> Result<(), StreamError> {
    if c.generation == generation {
        Ok(())
    } else {
        Err(StreamError::StaleGeneration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn enqueue(
        id: u64,
        generation: u64,
        direction: Direction,
        bytes: usize,
        frame: Option<WebSocketFrameKind>,
        final_fragment: bool,
        now_ms: u64,
    ) -> Enqueue {
        Enqueue {
            id,
            generation,
            direction,
            bytes,
            frame,
            final_fragment,
            now_ms,
        }
    }
    fn registry() -> StreamRegistry {
        StreamRegistry::new(StreamLimits {
            max_connections: 1,
            max_queued_per_direction: 4,
            max_frame_bytes: 4,
            max_control_bytes: 2,
            idle_ms: 100,
        })
        .unwrap()
    }
    #[test]
    fn direction_queues_apply_independent_backpressure() {
        let mut r = registry();
        let (id, g) = r.open(StreamKind::WebSocket, 0).unwrap();
        r.enqueue(enqueue(
            id,
            g,
            Direction::ClientToHost,
            4,
            Some(WebSocketFrameKind::Binary),
            true,
            1,
        ))
        .unwrap();
        r.enqueue(enqueue(
            id,
            g,
            Direction::HostToClient,
            4,
            Some(WebSocketFrameKind::Binary),
            true,
            1,
        ))
        .unwrap();
        assert_eq!(
            r.enqueue(enqueue(
                id,
                g,
                Direction::ClientToHost,
                1,
                Some(WebSocketFrameKind::Binary),
                true,
                1
            )),
            Err(StreamError::QueueLimit)
        );
    }
    #[test]
    fn websocket_control_frames_are_bounded_and_unfragmented() {
        let mut r = registry();
        let (id, g) = r.open(StreamKind::WebSocket, 0).unwrap();
        assert_eq!(
            r.enqueue(enqueue(
                id,
                g,
                Direction::ClientToHost,
                3,
                Some(WebSocketFrameKind::Ping),
                true,
                1
            )),
            Err(StreamError::ControlFrameTooLarge)
        );
        assert_eq!(
            r.enqueue(enqueue(
                id,
                g,
                Direction::ClientToHost,
                1,
                Some(WebSocketFrameKind::Close),
                false,
                1
            )),
            Err(StreamError::InvalidFrame)
        );
    }
    #[test]
    fn sse_is_long_lived_until_idle_limit() {
        let mut r = registry();
        let (id, g) = r.open(StreamKind::ServerSentEvents, 0).unwrap();
        r.enqueue(enqueue(id, g, Direction::HostToClient, 1, None, true, 90))
            .unwrap();
        assert!(r.expire_idle(150).is_empty());
        assert_eq!(r.expire_idle(190), [id]);
    }
    #[test]
    fn cancelled_generation_rejects_late_activity() {
        let mut r = registry();
        let (id, g) = r.open(StreamKind::WebSocket, 0).unwrap();
        r.cancel(id, g).unwrap();
        assert_eq!(
            r.enqueue(enqueue(
                id,
                g,
                Direction::ClientToHost,
                1,
                Some(WebSocketFrameKind::Text),
                true,
                1
            )),
            Err(StreamError::UnknownConnection)
        );
    }
    #[test]
    fn connection_and_release_bounds_are_stable() {
        let mut r = registry();
        let (id, g) = r.open(StreamKind::WebSocket, 0).unwrap();
        assert_eq!(
            r.open(StreamKind::ServerSentEvents, 0),
            Err(StreamError::ConnectionLimit)
        );
        assert_eq!(
            r.release(id, g, Direction::ClientToHost, 1),
            Err(StreamError::AccountingUnderflow)
        );
    }
}
