//! Deterministic connection lifecycle for a host-key-verified SSH transport.
//!
//! This module owns policy, not I/O. An outer adapter performs DNS and SSH work,
//! then feeds its typed outcomes into this state machine. Time and retry jitter
//! are likewise supplied as logical values so tests never need wall-clock sleeps.

pub type ConnectionGeneration = u64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    VerifyingHostKey,
    TrustDecisionRequired { fingerprint: String },
    Authenticating,
    Ready { generation: ConnectionGeneration },
    Reconnecting { attempt: u32, deadline_millis: u64 },
    Failed(ConnectionFailure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionFailure {
    HostKeyRejected,
    HostKeyMismatch,
    AuthenticationFailed,
    TransportFailed,
    RetryExhausted,
    GenerationOverflow,
    InvalidTransition,
}

impl ConnectionFailure {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::HostKeyRejected => "host_key_rejected",
            Self::HostKeyMismatch => "host_key_mismatch",
            Self::AuthenticationFailed => "authentication_failed",
            Self::TransportFailed => "transport_failed",
            Self::RetryExhausted => "retry_exhausted",
            Self::GenerationOverflow => "connection_generation_overflow",
            Self::InvalidTransition => "invalid_connection_transition",
        }
    }

    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(self, Self::TransportFailed | Self::RetryExhausted)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
}

impl RetryPolicy {
    #[must_use]
    pub const fn new(max_attempts: u32) -> Self {
        Self { max_attempts }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionMachine {
    state: ConnectionState,
    generation: ConnectionGeneration,
    retry_policy: RetryPolicy,
    reconnect_attempt: Option<u32>,
}

impl ConnectionMachine {
    #[must_use]
    pub const fn new(retry_policy: RetryPolicy) -> Self {
        Self {
            state: ConnectionState::Disconnected,
            generation: 0,
            retry_policy,
            reconnect_attempt: None,
        }
    }

    #[must_use]
    pub const fn state(&self) -> &ConnectionState {
        &self.state
    }

    #[must_use]
    pub const fn generation(&self) -> ConnectionGeneration {
        self.generation
    }

    /// # Errors
    /// Returns `InvalidTransition` unless disconnected.
    pub fn connect(&mut self) -> Result<(), ConnectionFailure> {
        Self::require_state(matches!(self.state, ConnectionState::Disconnected))?;
        self.state = ConnectionState::VerifyingHostKey;
        Ok(())
    }

    /// Records that the presented key exactly matches the stored trusted key.
    ///
    /// # Errors
    /// Returns `InvalidTransition` unless verifying a key.
    pub fn host_key_trusted(&mut self) -> Result<(), ConnectionFailure> {
        Self::require_state(matches!(self.state, ConnectionState::VerifyingHostKey))?;
        self.state = ConnectionState::Authenticating;
        Ok(())
    }

    /// # Errors
    /// Returns `InvalidTransition` unless verifying a key.
    pub fn host_key_unknown(&mut self, fingerprint: String) -> Result<(), ConnectionFailure> {
        Self::require_state(matches!(self.state, ConnectionState::VerifyingHostKey))?;
        self.state = ConnectionState::TrustDecisionRequired { fingerprint };
        Ok(())
    }

    /// Accepts first trust only when the consent result names the exact pending key.
    ///
    /// # Errors
    /// Returns `InvalidTransition` for a stale or different consent result.
    pub fn accept_unknown(&mut self, fingerprint: &str) -> Result<(), ConnectionFailure> {
        let matches_pending = matches!(
            &self.state,
            ConnectionState::TrustDecisionRequired { fingerprint: pending } if pending == fingerprint
        );
        Self::require_state(matches_pending)?;
        self.state = ConnectionState::Authenticating;
        Ok(())
    }

    /// # Errors
    /// Returns `InvalidTransition` unless a first-trust decision is pending.
    pub fn reject_unknown(&mut self) -> Result<(), ConnectionFailure> {
        Self::require_state(matches!(
            self.state,
            ConnectionState::TrustDecisionRequired { .. }
        ))?;
        self.state = ConnectionState::Failed(ConnectionFailure::HostKeyRejected);
        Ok(())
    }

    /// # Errors
    /// Returns `InvalidTransition` unless verifying a key.
    pub fn host_key_mismatch(&mut self) -> Result<(), ConnectionFailure> {
        Self::require_state(matches!(self.state, ConnectionState::VerifyingHostKey))?;
        self.state = ConnectionState::Failed(ConnectionFailure::HostKeyMismatch);
        Ok(())
    }

    /// # Errors
    /// Returns `InvalidTransition` unless authenticating, or `GenerationOverflow`.
    pub fn authenticated(&mut self) -> Result<ConnectionGeneration, ConnectionFailure> {
        Self::require_state(matches!(self.state, ConnectionState::Authenticating))?;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(ConnectionFailure::GenerationOverflow)?;
        self.generation = generation;
        self.reconnect_attempt = None;
        self.state = ConnectionState::Ready { generation };
        Ok(generation)
    }

    /// # Errors
    /// Returns `InvalidTransition` unless authenticating.
    pub fn authentication_failed(&mut self) -> Result<(), ConnectionFailure> {
        Self::require_state(matches!(self.state, ConnectionState::Authenticating))?;
        self.state = ConnectionState::Failed(ConnectionFailure::AuthenticationFailed);
        Ok(())
    }

    /// Schedules a retry using an already-computed monotonic deadline.
    ///
    /// # Errors
    /// Returns `InvalidTransition` unless ready.
    pub fn transport_lost(&mut self, deadline_millis: u64) -> Result<(), ConnectionFailure> {
        Self::require_state(matches!(self.state, ConnectionState::Ready { .. }))?;
        if self.retry_policy.max_attempts == 0 {
            self.state = ConnectionState::Failed(ConnectionFailure::RetryExhausted);
        } else {
            self.reconnect_attempt = Some(1);
            self.state = ConnectionState::Reconnecting {
                attempt: 1,
                deadline_millis,
            };
        }
        Ok(())
    }

    /// Starts verification when the logical clock reaches the retry deadline.
    #[must_use]
    pub fn retry_due(&mut self, now_millis: u64) -> bool {
        let ConnectionState::Reconnecting {
            deadline_millis, ..
        } = self.state
        else {
            return false;
        };
        if now_millis < deadline_millis {
            return false;
        }
        self.state = ConnectionState::VerifyingHostKey;
        true
    }

    /// Records a failed reconnect and either schedules the next attempt or stops.
    ///
    /// # Errors
    /// Returns `InvalidTransition` when no reconnect attempt is active.
    pub fn reconnect_failed(&mut self, next_deadline_millis: u64) -> Result<(), ConnectionFailure> {
        let Some(attempt) = self.reconnect_attempt else {
            return Err(ConnectionFailure::InvalidTransition);
        };
        Self::require_state(matches!(
            self.state,
            ConnectionState::Reconnecting { .. }
                | ConnectionState::VerifyingHostKey
                | ConnectionState::Authenticating
        ))?;
        if attempt >= self.retry_policy.max_attempts {
            self.reconnect_attempt = None;
            self.state = ConnectionState::Failed(ConnectionFailure::RetryExhausted);
        } else {
            self.reconnect_attempt = Some(attempt + 1);
            self.state = ConnectionState::Reconnecting {
                attempt: attempt + 1,
                deadline_millis: next_deadline_millis,
            };
        }
        Ok(())
    }

    pub fn disconnect(&mut self) {
        self.reconnect_attempt = None;
        self.state = ConnectionState::Disconnected;
    }

    /// Rejects channel work unless the transport is ready at the exact generation.
    ///
    /// # Errors
    /// Returns `NotReady` or `StaleGeneration` when channel work must be rejected.
    pub fn validate_channel(&self, generation: ConnectionGeneration) -> Result<(), ChannelFailure> {
        match self.state {
            ConnectionState::Ready {
                generation: current,
            } if generation == current => Ok(()),
            ConnectionState::Ready { .. } => Err(ChannelFailure::StaleGeneration),
            _ => Err(ChannelFailure::NotReady),
        }
    }

    fn require_state(valid: bool) -> Result<(), ConnectionFailure> {
        if valid {
            Ok(())
        } else {
            Err(ConnectionFailure::InvalidTransition)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelFailure {
    NotReady,
    StaleGeneration,
}

impl ChannelFailure {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotReady => "connection_not_ready",
            Self::StaleGeneration => "stale_connection_generation",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authenticating(machine: &mut ConnectionMachine) {
        machine.connect().unwrap();
        machine.host_key_trusted().unwrap();
    }

    fn ready(machine: &mut ConnectionMachine) -> ConnectionGeneration {
        authenticating(machine);
        machine.authenticated().unwrap()
    }

    #[test]
    fn known_key_and_authentication_reach_first_generation() {
        let mut machine = ConnectionMachine::new(RetryPolicy::new(3));
        assert_eq!(ready(&mut machine), 1);
        assert_eq!(machine.state(), &ConnectionState::Ready { generation: 1 });
        assert_eq!(machine.validate_channel(1), Ok(()));
    }

    #[test]
    fn unknown_key_requires_exact_explicit_consent() {
        let mut machine = ConnectionMachine::new(RetryPolicy::new(3));
        machine.connect().unwrap();
        machine.host_key_unknown("SHA256:first".into()).unwrap();
        assert_eq!(
            machine.accept_unknown("SHA256:other"),
            Err(ConnectionFailure::InvalidTransition)
        );
        assert_eq!(machine.accept_unknown("SHA256:first"), Ok(()));
        assert_eq!(machine.authenticated(), Ok(1));
    }

    #[test]
    fn rejecting_first_trust_fails_closed() {
        let mut machine = ConnectionMachine::new(RetryPolicy::new(3));
        machine.connect().unwrap();
        machine.host_key_unknown("SHA256:first".into()).unwrap();
        machine.reject_unknown().unwrap();
        assert_eq!(
            machine.state(),
            &ConnectionState::Failed(ConnectionFailure::HostKeyRejected)
        );
        assert_eq!(machine.validate_channel(0), Err(ChannelFailure::NotReady));
    }

    #[test]
    fn changed_key_is_never_treated_as_first_trust() {
        let mut machine = ConnectionMachine::new(RetryPolicy::new(3));
        machine.connect().unwrap();
        machine.host_key_mismatch().unwrap();
        assert_eq!(
            machine.state(),
            &ConnectionState::Failed(ConnectionFailure::HostKeyMismatch)
        );
        assert_eq!(
            machine.accept_unknown("SHA256:new"),
            Err(ConnectionFailure::InvalidTransition)
        );
    }

    #[test]
    fn authentication_failure_is_distinct_and_non_retryable() {
        let mut machine = ConnectionMachine::new(RetryPolicy::new(3));
        authenticating(&mut machine);
        machine.authentication_failed().unwrap();
        let failure = ConnectionFailure::AuthenticationFailed;
        assert_eq!(failure.code(), "authentication_failed");
        assert!(!failure.retryable());
    }

    #[test]
    fn retry_deadline_is_driven_only_by_supplied_logical_time() {
        let mut machine = ConnectionMachine::new(RetryPolicy::new(3));
        ready(&mut machine);
        machine.transport_lost(500).unwrap();
        assert!(!machine.retry_due(499));
        assert!(machine.retry_due(500));
        assert_eq!(machine.state(), &ConnectionState::VerifyingHostKey);
    }

    #[test]
    fn reconnect_advances_generation_and_rejects_old_channels() {
        let mut machine = ConnectionMachine::new(RetryPolicy::new(3));
        let old = ready(&mut machine);
        machine.transport_lost(10).unwrap();
        assert!(machine.retry_due(10));
        machine.host_key_trusted().unwrap();
        let current = machine.authenticated().unwrap();
        assert_eq!(current, old + 1);
        assert_eq!(
            machine.validate_channel(old),
            Err(ChannelFailure::StaleGeneration)
        );
        assert_eq!(machine.validate_channel(current), Ok(()));
    }

    #[test]
    fn bounded_reconnect_attempts_end_in_stable_failure() {
        let mut machine = ConnectionMachine::new(RetryPolicy::new(2));
        ready(&mut machine);
        machine.transport_lost(10).unwrap();
        assert!(machine.retry_due(10));
        machine.reconnect_failed(20).unwrap();
        assert_eq!(
            machine.state(),
            &ConnectionState::Reconnecting {
                attempt: 2,
                deadline_millis: 20
            }
        );
        assert!(machine.retry_due(20));
        machine.reconnect_failed(30).unwrap();
        assert_eq!(
            machine.state(),
            &ConnectionState::Failed(ConnectionFailure::RetryExhausted)
        );
    }

    #[test]
    fn explicit_disconnect_cancels_pending_retry() {
        let mut machine = ConnectionMachine::new(RetryPolicy::new(3));
        ready(&mut machine);
        machine.transport_lost(10).unwrap();
        machine.disconnect();
        assert!(!machine.retry_due(100));
        assert_eq!(machine.state(), &ConnectionState::Disconnected);
    }

    #[test]
    fn invalid_transitions_do_not_mutate_state() {
        let mut machine = ConnectionMachine::new(RetryPolicy::new(3));
        assert_eq!(
            machine.authenticated(),
            Err(ConnectionFailure::InvalidTransition)
        );
        assert_eq!(machine.state(), &ConnectionState::Disconnected);
        assert_eq!(machine.validate_channel(0), Err(ChannelFailure::NotReady));
    }
}
