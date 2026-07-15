//! Deterministic client-side coordination of handshake and envelope lifecycles.
//!
//! Serialization and transport EOF detection remain adapter responsibilities. The
//! coordinator consumes already validated typed messages and explicit frame sizes.

use crate::envelope::{
    EnvelopeError, Event, EventDisposition, EventTracker, Method, Request, RequestTracker, Response,
};
use crate::handshake::{
    ClientNegotiator, HandshakeError, Hello, Incompatible, ProtocolLimits, Welcome,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPhase {
    HelloPending,
    WelcomePending,
    Established,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionError {
    ExpectedHello,
    ExpectedWelcome,
    SessionClosed,
    Incompatible,
    ControlFrameTooLarge,
    Handshake(HandshakeError),
    Envelope(EnvelopeError),
}

impl SessionError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ExpectedHello => "expected_hello",
            Self::ExpectedWelcome => "expected_welcome",
            Self::SessionClosed => "session_closed",
            Self::Incompatible => "incompatible_protocol",
            Self::ControlFrameTooLarge => "control_frame_too_large",
            Self::Handshake(error) => error.code(),
            Self::Envelope(error) => error.code(),
        }
    }
}

/// Coordinates one client connection without clocks, I/O, or wire serialization.
#[derive(Debug)]
pub struct ClientSession {
    phase: SessionPhase,
    negotiator: ClientNegotiator,
    max_tracked_workspaces: usize,
    limits: Option<ProtocolLimits>,
    requests: Option<RequestTracker>,
    events: Option<EventTracker>,
}

impl ClientSession {
    /// Creates a session whose only valid first action is sending its hello.
    ///
    /// # Errors
    ///
    /// Returns `invalid_limit` when the event cursor bound is zero.
    pub fn new(hello: &Hello, max_tracked_workspaces: usize) -> Result<Self, SessionError> {
        if max_tracked_workspaces == 0 {
            return Err(SessionError::Envelope(EnvelopeError::InvalidLimit));
        }
        Ok(Self {
            phase: SessionPhase::HelloPending,
            negotiator: ClientNegotiator::new(hello),
            max_tracked_workspaces,
            limits: None,
            requests: None,
            events: None,
        })
    }

    #[must_use]
    pub const fn phase(&self) -> SessionPhase {
        self.phase
    }

    #[must_use]
    pub const fn negotiated_limits(&self) -> Option<ProtocolLimits> {
        self.limits
    }

    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.requests.as_ref().map_or(0, RequestTracker::in_flight)
    }

    /// Records transmission of the configured hello.
    ///
    /// # Errors
    ///
    /// Any reordered action closes the session.
    pub fn send_hello(&mut self) -> Result<(), SessionError> {
        match self.phase {
            SessionPhase::HelloPending => {
                self.phase = SessionPhase::WelcomePending;
                Ok(())
            }
            SessionPhase::Closed => Err(SessionError::SessionClosed),
            _ => self.fail(SessionError::ExpectedWelcome),
        }
    }

    /// Validates a welcome and installs its negotiated resource limits atomically.
    ///
    /// # Errors
    ///
    /// Invalid negotiation or ordering closes the session.
    pub fn receive_welcome(
        &mut self,
        welcome: &Welcome,
        frame_bytes: usize,
    ) -> Result<(), SessionError> {
        self.expect_welcome()?;
        if frame_bytes > welcome.limits.max_control_frame_bytes as usize {
            return self.fail(SessionError::ControlFrameTooLarge);
        }
        if let Err(error) = self.negotiator.receive_welcome(welcome) {
            return self.fail(SessionError::Handshake(error));
        }
        let requests = RequestTracker::new(usize::from(welcome.limits.max_in_flight_requests))
            .map_err(SessionError::Envelope)?;
        let events =
            EventTracker::new(self.max_tracked_workspaces).map_err(SessionError::Envelope)?;
        self.limits = Some(welcome.limits);
        self.requests = Some(requests);
        self.events = Some(events);
        self.phase = SessionPhase::Established;
        Ok(())
    }

    /// Accepts a valid incompatibility response and makes the session terminal.
    ///
    /// # Errors
    ///
    /// Returns `incompatible_protocol` after validating the response. Invalid
    /// negotiation or ordering also closes the session.
    pub fn receive_incompatible(&mut self, incompatible: Incompatible) -> Result<(), SessionError> {
        self.expect_welcome()?;
        if let Err(error) = self.negotiator.receive_incompatible(incompatible) {
            return self.fail(SessionError::Handshake(error));
        }
        self.fail(SessionError::Incompatible)
    }

    /// Registers an outgoing request under the negotiated frame and in-flight bounds.
    ///
    /// # Errors
    ///
    /// Capacity exhaustion is recoverable. Reordered messages, oversized frames,
    /// and duplicate IDs close the session.
    pub fn send_request<P>(
        &mut self,
        request: &Request<P>,
        frame_bytes: usize,
    ) -> Result<(), SessionError> {
        self.ensure_established()?;
        self.check_frame(frame_bytes)?;
        let Some(requests) = self.requests.as_mut() else {
            return self.fail(SessionError::SessionClosed);
        };
        let result = requests.register(request);
        match result {
            Ok(()) => Ok(()),
            Err(EnvelopeError::InFlightLimitExceeded) => {
                Err(SessionError::Envelope(EnvelopeError::InFlightLimitExceeded))
            }
            Err(error) => self.fail(SessionError::Envelope(error)),
        }
    }

    /// Correlates one incoming terminal response, allowing completion in any order.
    ///
    /// # Errors
    ///
    /// Unknown or duplicate responses close the session.
    pub fn receive_response<R, E>(
        &mut self,
        response: &Response<R, E>,
        frame_bytes: usize,
    ) -> Result<Method, SessionError> {
        self.ensure_established()?;
        self.check_frame(frame_bytes)?;
        let Some(requests) = self.requests.as_mut() else {
            return self.fail(SessionError::SessionClosed);
        };
        let result = requests.receive(response);
        match result {
            Ok(method) => Ok(method),
            Err(error) => self.fail(SessionError::Envelope(error)),
        }
    }

    /// Applies the existing per-workspace event ordering rules.
    ///
    /// # Errors
    ///
    /// Invalid events or exhausted workspace tracking close the session.
    pub fn receive_event<P>(
        &mut self,
        event: &Event<P>,
        frame_bytes: usize,
    ) -> Result<EventDisposition, SessionError> {
        self.ensure_established()?;
        self.check_frame(frame_bytes)?;
        let Some(events) = self.events.as_mut() else {
            return self.fail(SessionError::SessionClosed);
        };
        let result = events.observe(event);
        match result {
            Ok(disposition) => Ok(disposition),
            Err(error) => self.fail(SessionError::Envelope(error)),
        }
    }

    /// Marks transport EOF as a terminal condition and discards connection state.
    pub fn receive_eof(&mut self) {
        self.close();
    }

    fn expect_welcome(&mut self) -> Result<(), SessionError> {
        match self.phase {
            SessionPhase::WelcomePending => Ok(()),
            SessionPhase::Closed => Err(SessionError::SessionClosed),
            SessionPhase::HelloPending => self.fail(SessionError::ExpectedHello),
            SessionPhase::Established => self.fail(SessionError::ExpectedWelcome),
        }
    }

    fn ensure_established(&mut self) -> Result<(), SessionError> {
        match self.phase {
            SessionPhase::Established => Ok(()),
            SessionPhase::Closed => Err(SessionError::SessionClosed),
            SessionPhase::HelloPending => self.fail(SessionError::ExpectedHello),
            SessionPhase::WelcomePending => self.fail(SessionError::ExpectedWelcome),
        }
    }

    fn check_frame(&mut self, frame_bytes: usize) -> Result<(), SessionError> {
        let Some(limits) = self.limits else {
            return self.fail(SessionError::SessionClosed);
        };
        let limit = limits.max_control_frame_bytes as usize;
        if frame_bytes > limit {
            self.fail(SessionError::ControlFrameTooLarge)
        } else {
            Ok(())
        }
    }

    fn fail<T>(&mut self, error: SessionError) -> Result<T, SessionError> {
        self.close();
        Err(error)
    }

    fn close(&mut self) {
        self.phase = SessionPhase::Closed;
        self.limits = None;
        self.requests = None;
        self.events = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{EnvelopeId, Terminal};
    use crate::handshake::{
        Capability, PeerIdentity, ProtocolVersion, ServerNegotiator, ServerReply,
    };

    fn identity(name: &str) -> PeerIdentity {
        PeerIdentity::new(name, "0.1.0").unwrap()
    }

    fn hello() -> Hello {
        Hello::new(
            ProtocolVersion::new(1, 1),
            identity("choosh-android"),
            [Capability::Events],
        )
        .unwrap()
    }

    fn welcome(max_frame: u32, max_in_flight: u16) -> Welcome {
        let hello = hello();
        let mut server = ServerNegotiator::new(
            ProtocolVersion::new(1, 0),
            identity("chooshd"),
            identity("test-host"),
            [Capability::Events],
            ProtocolLimits::new(max_frame, max_in_flight).unwrap(),
        )
        .unwrap();
        let ServerReply::Welcome(welcome) = server.receive_hello(&hello).unwrap() else {
            panic!("matching major must negotiate");
        };
        welcome
    }

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

    fn established(max_frame: u32, max_in_flight: u16) -> ClientSession {
        let hello = hello();
        let mut session = ClientSession::new(&hello, 4).unwrap();
        session.send_hello().unwrap();
        session
            .receive_welcome(&welcome(max_frame, max_in_flight), 8)
            .unwrap();
        session
    }

    #[test]
    fn ordering_violation_closes_permanently() {
        let hello = hello();
        let mut session = ClientSession::new(&hello, 4).unwrap();
        assert_eq!(
            session.send_request(&request(1), 1),
            Err(SessionError::ExpectedHello)
        );
        assert_eq!(session.phase(), SessionPhase::Closed);
        assert_eq!(session.send_hello(), Err(SessionError::SessionClosed));
    }

    #[test]
    fn welcome_propagates_both_negotiated_limits() {
        let mut session = established(16, 1);
        assert_eq!(
            session.negotiated_limits(),
            Some(ProtocolLimits::new(16, 1).unwrap())
        );
        session.send_request(&request(1), 16).unwrap();
        assert_eq!(
            session.send_request(&request(2), 16),
            Err(SessionError::Envelope(EnvelopeError::InFlightLimitExceeded))
        );
        assert_eq!(session.phase(), SessionPhase::Established);
        assert_eq!(
            session.receive_response(
                &Response::<(), ()> {
                    id: id(1),
                    terminal: Terminal::Result(())
                },
                17,
            ),
            Err(SessionError::ControlFrameTooLarge)
        );
        assert_eq!(session.phase(), SessionPhase::Closed);
    }

    #[test]
    fn responses_complete_out_of_order() {
        let mut session = established(64, 2);
        session.send_request(&request(1), 10).unwrap();
        session.send_request(&request(2), 10).unwrap();
        for n in [2, 1] {
            let method = session
                .receive_response(
                    &Response::<(), ()> {
                        id: id(n),
                        terminal: Terminal::Result(()),
                    },
                    10,
                )
                .unwrap();
            assert_eq!(method.as_str(), "workspace.open");
        }
        assert_eq!(session.in_flight(), 0);
    }

    #[test]
    fn incompatible_and_eof_are_terminal() {
        let hello = hello();
        let mut incompatible = ClientSession::new(&hello, 4).unwrap();
        incompatible.send_hello().unwrap();
        assert_eq!(
            incompatible.receive_incompatible(Incompatible {
                client: ProtocolVersion::new(1, 1),
                daemon: ProtocolVersion::new(2, 0),
            }),
            Err(SessionError::Incompatible)
        );
        assert_eq!(incompatible.phase(), SessionPhase::Closed);

        let mut eof = established(64, 1);
        eof.receive_eof();
        assert_eq!(
            eof.send_request(&request(1), 1),
            Err(SessionError::SessionClosed)
        );
    }

    #[test]
    fn unknown_response_closes_whole_session() {
        let mut session = established(64, 1);
        assert_eq!(
            session.receive_response(
                &Response::<(), ()> {
                    id: id(9),
                    terminal: Terminal::Error(())
                },
                10,
            ),
            Err(SessionError::Envelope(EnvelopeError::UnknownResponse))
        );
        assert_eq!(session.phase(), SessionPhase::Closed);
    }
}
