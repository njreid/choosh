//! Wire-independent validation and lifecycle tracking for post-handshake envelopes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const MAX_METHOD_BYTES: usize = 128;
pub const DEFAULT_MAX_TRACKED_WORKSPACES: usize = 1024;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EnvelopeId(String);

impl EnvelopeId {
    /// Validates and retains a canonical UUID without assigning meaning to it.
    ///
    /// # Errors
    ///
    /// Returns `invalid_id` unless the value is canonical lowercase UUID syntax.
    pub fn new(value: impl Into<String>) -> Result<Self, EnvelopeError> {
        let value = value.into();
        if !is_canonical_uuid(&value) {
            return Err(EnvelopeError::InvalidId);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
        })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Method(String);

impl Method {
    /// Validates a bounded protocol method or event name.
    ///
    /// # Errors
    ///
    /// Returns `invalid_method` for values outside the schema pattern or bound.
    pub fn new(value: impl Into<String>) -> Result<Self, EnvelopeError> {
        let value = value.into();
        let mut bytes = value.bytes();
        if value.len() > MAX_METHOD_BYTES
            || !matches!(bytes.next(), Some(b'a'..=b'z'))
            || !bytes.all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(&byte)
            })
        {
            return Err(EnvelopeError::InvalidMethod);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Request<P> {
    pub id: EnvelopeId,
    pub method: Method,
    pub params: P,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Terminal<R, E> {
    Result(R),
    Error(E),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Response<R, E> {
    pub id: EnvelopeId,
    pub terminal: Terminal<R, E>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event<P> {
    pub workspace_id: EnvelopeId,
    pub sequence: u64,
    pub event: Method,
    pub payload: P,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvelopeError {
    InvalidId,
    InvalidMethod,
    InvalidSequence,
    InvalidLimit,
    InFlightLimitExceeded,
    DuplicateRequest,
    UnknownResponse,
    DuplicateResponse,
    WorkspaceLimitExceeded,
    TrackerClosed,
}

impl EnvelopeError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidId => "invalid_id",
            Self::InvalidMethod => "invalid_method",
            Self::InvalidSequence => "invalid_sequence",
            Self::InvalidLimit => "invalid_limit",
            Self::InFlightLimitExceeded => "in_flight_limit_exceeded",
            Self::DuplicateRequest => "duplicate_request",
            Self::UnknownResponse => "unknown_response",
            Self::DuplicateResponse => "duplicate_response",
            Self::WorkspaceLimitExceeded => "workspace_limit_exceeded",
            Self::TrackerClosed => "tracker_closed",
        }
    }
}

/// Tracks one connection's request terminals. Protocol violations permanently close it.
#[derive(Debug)]
pub struct RequestTracker {
    max_in_flight: usize,
    outstanding: BTreeMap<EnvelopeId, Method>,
    completed: BTreeSet<EnvelopeId>,
    completed_order: VecDeque<EnvelopeId>,
    closed: bool,
}

impl RequestTracker {
    /// Creates a tracker with a positive in-flight request bound.
    ///
    /// # Errors
    ///
    /// Returns `invalid_limit` when the bound is zero.
    pub fn new(max_in_flight: usize) -> Result<Self, EnvelopeError> {
        if max_in_flight == 0 {
            return Err(EnvelopeError::InvalidLimit);
        }
        Ok(Self {
            max_in_flight,
            outstanding: BTreeMap::new(),
            completed: BTreeSet::new(),
            completed_order: VecDeque::new(),
            closed: false,
        })
    }

    /// Registers a request ID before it is sent.
    ///
    /// # Errors
    ///
    /// Returns a stable error when closed, full, or given a reused request ID.
    pub fn register<P>(&mut self, request: &Request<P>) -> Result<(), EnvelopeError> {
        self.ensure_open()?;
        if self.outstanding.contains_key(&request.id) || self.completed.contains(&request.id) {
            return self.fail(EnvelopeError::DuplicateRequest);
        }
        if self.outstanding.len() == self.max_in_flight {
            return Err(EnvelopeError::InFlightLimitExceeded);
        }
        self.outstanding
            .insert(request.id.clone(), request.method.clone());
        Ok(())
    }

    /// Consumes exactly one terminal response for an outstanding request.
    ///
    /// # Errors
    ///
    /// Unknown or duplicate responses fail the tracker closed. A previously
    /// closed tracker returns `tracker_closed`.
    pub fn receive<R, E>(&mut self, response: &Response<R, E>) -> Result<Method, EnvelopeError> {
        self.ensure_open()?;
        if let Some(method) = self.outstanding.remove(&response.id) {
            if self.completed.len() == self.max_in_flight
                && let Some(oldest) = self.completed_order.pop_front()
            {
                self.completed.remove(&oldest);
            }
            self.completed.insert(response.id.clone());
            self.completed_order.push_back(response.id.clone());
            return Ok(method);
        }
        let error = if self.completed.contains(&response.id) {
            EnvelopeError::DuplicateResponse
        } else {
            EnvelopeError::UnknownResponse
        };
        self.fail(error)
    }

    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.outstanding.len()
    }

    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    fn ensure_open(&self) -> Result<(), EnvelopeError> {
        if self.closed {
            Err(EnvelopeError::TrackerClosed)
        } else {
            Ok(())
        }
    }

    fn fail<T>(&mut self, error: EnvelopeError) -> Result<T, EnvelopeError> {
        self.closed = true;
        Err(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventDisposition {
    Applied,
    Duplicate,
    Gap { expected: u64, received: u64 },
}

#[derive(Debug)]
pub struct EventTracker {
    max_workspaces: usize,
    cursors: BTreeMap<EnvelopeId, u64>,
}

impl Default for EventTracker {
    fn default() -> Self {
        Self {
            max_workspaces: DEFAULT_MAX_TRACKED_WORKSPACES,
            cursors: BTreeMap::new(),
        }
    }
}

impl EventTracker {
    /// Creates a tracker with a positive workspace bound.
    ///
    /// # Errors
    ///
    /// Returns `invalid_limit` when the bound is zero.
    pub fn new(max_workspaces: usize) -> Result<Self, EnvelopeError> {
        if max_workspaces == 0 {
            return Err(EnvelopeError::InvalidLimit);
        }
        Ok(Self {
            max_workspaces,
            cursors: BTreeMap::new(),
        })
    }

    /// Sets the atomically installed snapshot high-water mark for a workspace.
    ///
    /// # Errors
    ///
    /// Returns `workspace_limit_exceeded` when a new cursor exceeds the bound.
    pub fn set_cursor(
        &mut self,
        workspace_id: EnvelopeId,
        sequence: u64,
    ) -> Result<(), EnvelopeError> {
        self.ensure_workspace_capacity(&workspace_id)?;
        self.cursors.insert(workspace_id, sequence);
        Ok(())
    }

    /// Classifies an event as applied, duplicate, or a sequence gap.
    ///
    /// # Errors
    ///
    /// Returns a stable error for sequence zero, overflow, or workspace exhaustion.
    pub fn observe<P>(&mut self, event: &Event<P>) -> Result<EventDisposition, EnvelopeError> {
        if event.sequence == 0 {
            return Err(EnvelopeError::InvalidSequence);
        }
        let cursor = self.cursors.get(&event.workspace_id).copied().unwrap_or(0);
        if event.sequence <= cursor {
            return Ok(EventDisposition::Duplicate);
        }
        let expected = cursor
            .checked_add(1)
            .ok_or(EnvelopeError::InvalidSequence)?;
        if event.sequence != expected {
            return Ok(EventDisposition::Gap {
                expected,
                received: event.sequence,
            });
        }
        self.ensure_workspace_capacity(&event.workspace_id)?;
        self.cursors
            .insert(event.workspace_id.clone(), event.sequence);
        Ok(EventDisposition::Applied)
    }

    fn ensure_workspace_capacity(&self, workspace_id: &EnvelopeId) -> Result<(), EnvelopeError> {
        if self.cursors.len() == self.max_workspaces && !self.cursors.contains_key(workspace_id) {
            Err(EnvelopeError::WorkspaceLimitExceeded)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> EnvelopeId {
        EnvelopeId::new(format!("00000000-0000-0000-0000-{n:012x}")).unwrap()
    }

    fn request(n: u8) -> Request<()> {
        Request {
            id: id(n),
            method: Method::new("workspace.open").unwrap(),
            params: (),
        }
    }

    #[test]
    fn validates_schema_compatible_names_and_canonical_ids() {
        assert!(Method::new("events.subscribe-v1").is_ok());
        assert_eq!(Method::new("Events").unwrap_err().code(), "invalid_method");
        assert_eq!(
            EnvelopeId::new("00000000-0000-0000-0000-00000000000A").unwrap_err(),
            EnvelopeError::InvalidId
        );
    }

    #[test]
    fn responses_may_complete_out_of_order_exactly_once() {
        let mut tracker = RequestTracker::new(2).unwrap();
        tracker.register(&request(1)).unwrap();
        tracker.register(&request(2)).unwrap();
        for n in [2, 1] {
            let response = Response::<(), ()> {
                id: id(n),
                terminal: Terminal::Result(()),
            };
            assert_eq!(
                tracker.receive(&response).unwrap().as_str(),
                "workspace.open"
            );
        }
        assert_eq!(tracker.in_flight(), 0);
    }

    #[test]
    fn capacity_rejection_does_not_close_tracker() {
        let mut tracker = RequestTracker::new(1).unwrap();
        tracker.register(&request(1)).unwrap();
        assert_eq!(
            tracker.register(&request(2)),
            Err(EnvelopeError::InFlightLimitExceeded)
        );
        assert!(!tracker.is_closed());
    }

    #[test]
    fn unknown_and_duplicate_terminals_fail_closed() {
        let mut unknown = RequestTracker::new(1).unwrap();
        let response = Response::<(), ()> {
            id: id(9),
            terminal: Terminal::Error(()),
        };
        assert_eq!(
            unknown.receive(&response),
            Err(EnvelopeError::UnknownResponse)
        );
        assert_eq!(
            unknown.register(&request(1)),
            Err(EnvelopeError::TrackerClosed)
        );

        let mut duplicate = RequestTracker::new(1).unwrap();
        duplicate.register(&request(1)).unwrap();
        let response = Response::<(), ()> {
            id: id(1),
            terminal: Terminal::Result(()),
        };
        duplicate.receive(&response).unwrap();
        assert_eq!(
            duplicate.receive(&response),
            Err(EnvelopeError::DuplicateResponse)
        );
        assert!(duplicate.is_closed());
    }

    #[test]
    fn event_sequences_are_independent_and_gaps_do_not_advance() {
        let mut tracker = EventTracker::default();
        let event = |workspace, sequence| Event {
            workspace_id: id(workspace),
            sequence,
            event: Method::new("agent.status").unwrap(),
            payload: (),
        };
        assert_eq!(
            tracker.observe(&event(1, 1)).unwrap(),
            EventDisposition::Applied
        );
        assert_eq!(
            tracker.observe(&event(1, 1)).unwrap(),
            EventDisposition::Duplicate
        );
        assert_eq!(
            tracker.observe(&event(1, 3)).unwrap(),
            EventDisposition::Gap {
                expected: 2,
                received: 3
            }
        );
        assert_eq!(
            tracker.observe(&event(2, 1)).unwrap(),
            EventDisposition::Applied
        );
        assert_eq!(
            tracker.observe(&event(1, 2)).unwrap(),
            EventDisposition::Applied
        );
    }

    #[test]
    fn snapshot_cursor_defines_next_expected_sequence() {
        let mut tracker = EventTracker::default();
        tracker.set_cursor(id(1), 41).unwrap();
        let event = Event {
            workspace_id: id(1),
            sequence: 42,
            event: Method::new("item.changed").unwrap(),
            payload: (),
        };
        assert_eq!(tracker.observe(&event).unwrap(), EventDisposition::Applied);
    }
}
