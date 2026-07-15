//! Deterministic, bounded storage for sequenced workspace events.
//!
//! Receipt times are logical values supplied by the caller. Acknowledgements are
//! useful durable client cursors, but retention is governed by configured count,
//! byte, and age bounds so one client cannot control cleanup for another.

use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpoolLimits {
    pub max_events: usize,
    pub max_retained_bytes: usize,
    pub max_payload_bytes: usize,
    pub max_receipt_age: u64,
    pub max_clients: usize,
    pub max_client_id_bytes: usize,
}

impl SpoolLimits {
    /// Validates that every configured bound is nonzero.
    ///
    /// # Errors
    ///
    /// Returns [`SpoolError::InvalidLimits`] when any bound is zero.
    pub fn validate(self) -> Result<Self, SpoolError> {
        if self.max_events == 0
            || self.max_retained_bytes == 0
            || self.max_payload_bytes == 0
            || self.max_receipt_age == 0
            || self.max_clients == 0
            || self.max_client_id_bytes == 0
        {
            return Err(SpoolError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpoolEvent {
    pub sequence: u64,
    pub received_at: u64,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpoolSnapshot {
    /// The next sequence to assign. This high-water mark must be checkpointed
    /// even when no events remain, preventing reuse after restart.
    pub next_sequence: u64,
    pub events: Vec<SpoolEvent>,
    pub client_acks: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Subscription {
    Replay {
        retained_low: Option<u64>,
        committed_high: u64,
        events: Vec<SpoolEvent>,
    },
    SnapshotRequired {
        retained_low: Option<u64>,
        committed_high: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SpoolError {
    InvalidLimits,
    PayloadTooLarge {
        actual: usize,
        limit: usize,
    },
    SequenceExhausted,
    TimeRegression,
    InvalidCheckpoint,
    EmptyClientId,
    ClientIdTooLong,
    TooManyClients,
    AckAhead {
        acknowledged: u64,
        committed_high: u64,
    },
    AckRegression {
        current: u64,
        attempted: u64,
    },
}

#[derive(Clone, Debug)]
pub struct EventSpool {
    limits: SpoolLimits,
    next_sequence: u64,
    retained_bytes: usize,
    events: VecDeque<SpoolEvent>,
    client_acks: BTreeMap<String, u64>,
}

impl EventSpool {
    /// Creates an empty spool whose first event has sequence one.
    ///
    /// # Errors
    ///
    /// Returns [`SpoolError::InvalidLimits`] when any configured bound is zero.
    pub fn new(limits: SpoolLimits) -> Result<Self, SpoolError> {
        Ok(Self {
            limits: limits.validate()?,
            next_sequence: 1,
            retained_bytes: 0,
            events: VecDeque::new(),
            client_acks: BTreeMap::new(),
        })
    }

    /// Restores durable state. Cleanup remains explicit so callers can restore
    /// and checkpoint without inventing a wall-clock value.
    ///
    /// # Errors
    ///
    /// Returns [`SpoolError::InvalidLimits`] for zero bounds or
    /// [`SpoolError::InvalidCheckpoint`] for malformed or out-of-bounds state.
    pub fn restore(limits: SpoolLimits, snapshot: SpoolSnapshot) -> Result<Self, SpoolError> {
        let limits = limits.validate()?;
        let mut previous: Option<&SpoolEvent> = None;
        let mut retained_bytes = 0usize;
        for event in &snapshot.events {
            if event.sequence == 0
                || previous.is_some_and(|value| {
                    event.sequence != value.sequence + 1 || event.received_at < value.received_at
                })
                || event.sequence >= snapshot.next_sequence
                || event.payload.len() > limits.max_payload_bytes
            {
                return Err(SpoolError::InvalidCheckpoint);
            }
            retained_bytes = retained_bytes
                .checked_add(event.payload.len())
                .ok_or(SpoolError::InvalidCheckpoint)?;
            previous = Some(event);
        }
        if snapshot.next_sequence == 0
            || snapshot.client_acks.iter().any(|(id, ack)| {
                id.is_empty()
                    || id.len() > limits.max_client_id_bytes
                    || *ack >= snapshot.next_sequence
            })
            || snapshot.client_acks.len() > limits.max_clients
        {
            return Err(SpoolError::InvalidCheckpoint);
        }
        Ok(Self {
            limits,
            next_sequence: snapshot.next_sequence,
            retained_bytes,
            events: snapshot.events.into(),
            client_acks: snapshot.client_acks,
        })
    }

    #[must_use]
    pub fn checkpoint(&self) -> SpoolSnapshot {
        SpoolSnapshot {
            next_sequence: self.next_sequence,
            events: self.events.iter().cloned().collect(),
            client_acks: self.client_acks.clone(),
        }
    }

    /// Appends an event after deterministic age cleanup and returns its sequence.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload exceeds its bound, receipt time moves
    /// backwards, or the sequence space is exhausted.
    pub fn append(&mut self, received_at: u64, payload: Vec<u8>) -> Result<u64, SpoolError> {
        if payload.len() > self.limits.max_payload_bytes {
            return Err(SpoolError::PayloadTooLarge {
                actual: payload.len(),
                limit: self.limits.max_payload_bytes,
            });
        }
        if self
            .events
            .back()
            .is_some_and(|event| received_at < event.received_at)
        {
            return Err(SpoolError::TimeRegression);
        }
        let following_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(SpoolError::SequenceExhausted)?;
        self.cleanup(received_at)?;
        let sequence = self.next_sequence;
        self.next_sequence = following_sequence;
        self.retained_bytes += payload.len();
        self.events.push_back(SpoolEvent {
            sequence,
            received_at,
            payload,
        });
        self.enforce_size_bounds();
        Ok(sequence)
    }

    /// Removes expired events and enforces count and byte bounds.
    ///
    /// # Errors
    ///
    /// Returns [`SpoolError::TimeRegression`] when `now` precedes the newest
    /// retained event's receipt time.
    pub fn cleanup(&mut self, now: u64) -> Result<(), SpoolError> {
        if self
            .events
            .back()
            .is_some_and(|event| now < event.received_at)
        {
            return Err(SpoolError::TimeRegression);
        }
        while self.events.front().is_some_and(|event| {
            now.saturating_sub(event.received_at) > self.limits.max_receipt_age
        }) {
            self.evict_front();
        }
        self.enforce_size_bounds();
        Ok(())
    }

    #[must_use]
    pub fn subscribe(&self, after_sequence: u64) -> Subscription {
        let high = self.committed_high();
        let low = self.retained_low();
        let cursor_too_old = low.map_or(after_sequence < high, |value| {
            after_sequence.saturating_add(1) < value
        });
        if after_sequence > high || cursor_too_old {
            return Subscription::SnapshotRequired {
                retained_low: low,
                committed_high: high,
            };
        }
        Subscription::Replay {
            retained_low: low,
            committed_high: high,
            events: self
                .events
                .iter()
                .filter(|event| event.sequence > after_sequence)
                .cloned()
                .collect(),
        }
    }

    /// Advances a registered client's durable acknowledgement cursor.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or excess client, a cursor beyond the
    /// committed high-water mark, or an acknowledgement regression.
    pub fn acknowledge(&mut self, client_id: &str, sequence: u64) -> Result<(), SpoolError> {
        if client_id.is_empty() {
            return Err(SpoolError::EmptyClientId);
        }
        if client_id.len() > self.limits.max_client_id_bytes {
            return Err(SpoolError::ClientIdTooLong);
        }
        let high = self.committed_high();
        if sequence > high {
            return Err(SpoolError::AckAhead {
                acknowledged: sequence,
                committed_high: high,
            });
        }
        if let Some(current) = self.client_acks.get(client_id) {
            if sequence < *current {
                return Err(SpoolError::AckRegression {
                    current: *current,
                    attempted: sequence,
                });
            }
        } else if self.client_acks.len() == self.limits.max_clients {
            return Err(SpoolError::TooManyClients);
        }
        self.client_acks.insert(client_id.to_owned(), sequence);
        Ok(())
    }

    #[must_use]
    pub fn retained_low(&self) -> Option<u64> {
        self.events.front().map(|event| event.sequence)
    }

    #[must_use]
    pub fn retained_high(&self) -> Option<u64> {
        self.events.back().map(|event| event.sequence)
    }

    #[must_use]
    pub fn committed_high(&self) -> u64 {
        self.next_sequence - 1
    }

    fn enforce_size_bounds(&mut self) {
        while self.events.len() > self.limits.max_events
            || self.retained_bytes > self.limits.max_retained_bytes
        {
            self.evict_front();
        }
    }

    fn evict_front(&mut self) {
        if let Some(event) = self.events.pop_front() {
            self.retained_bytes -= event.payload.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> SpoolLimits {
        SpoolLimits {
            max_events: 3,
            max_retained_bytes: 8,
            max_payload_bytes: 4,
            max_receipt_age: 10,
            max_clients: 2,
            max_client_id_bytes: 8,
        }
    }

    #[test]
    fn replays_strictly_after_cursor_in_sequence_order() {
        let mut spool = EventSpool::new(limits()).unwrap();
        spool.append(10, vec![1]).unwrap();
        spool.append(11, vec![2]).unwrap();
        assert_eq!(
            spool.subscribe(1),
            Subscription::Replay {
                retained_low: Some(1),
                committed_high: 2,
                events: vec![SpoolEvent {
                    sequence: 2,
                    received_at: 11,
                    payload: vec![2]
                }]
            }
        );
    }

    #[test]
    fn count_eviction_requires_snapshot_for_gap_but_not_boundary_cursor() {
        let mut spool = EventSpool::new(limits()).unwrap();
        for time in 1..=4 {
            spool.append(time, vec![1]).unwrap();
        }
        assert!(matches!(
            spool.subscribe(0),
            Subscription::SnapshotRequired { .. }
        ));
        assert!(matches!(spool.subscribe(1), Subscription::Replay { .. }));
        assert_eq!(spool.retained_low(), Some(2));
    }

    #[test]
    fn byte_eviction_is_independent_of_acknowledgements() {
        let mut spool = EventSpool::new(limits()).unwrap();
        spool.append(1, vec![1; 4]).unwrap();
        spool.acknowledge("slow", 0).unwrap();
        spool.append(2, vec![2; 4]).unwrap();
        spool.append(3, vec![3; 4]).unwrap();
        assert_eq!(spool.retained_low(), Some(2));
        assert!(matches!(
            spool.subscribe(0),
            Subscription::SnapshotRequired { .. }
        ));
    }

    #[test]
    fn supplied_time_expires_only_events_older_than_age_bound() {
        let mut spool = EventSpool::new(limits()).unwrap();
        spool.append(5, vec![1]).unwrap();
        spool.append(6, vec![2]).unwrap();
        spool.cleanup(15).unwrap();
        assert_eq!(spool.retained_low(), Some(1));
        spool.cleanup(16).unwrap();
        assert_eq!(spool.retained_low(), Some(2));
    }

    #[test]
    fn rejected_payload_and_time_do_not_consume_sequences() {
        let mut spool = EventSpool::new(limits()).unwrap();
        spool.append(10, vec![1]).unwrap();
        assert!(matches!(
            spool.append(11, vec![0; 5]),
            Err(SpoolError::PayloadTooLarge { .. })
        ));
        assert_eq!(spool.append(9, vec![2]), Err(SpoolError::TimeRegression));
        assert_eq!(spool.append(12, vec![3]).unwrap(), 2);
    }

    #[test]
    fn acknowledgement_is_monotonic_idempotent_and_bounded_by_high_water() {
        let mut spool = EventSpool::new(limits()).unwrap();
        spool.append(1, vec![1]).unwrap();
        spool.acknowledge("phone", 1).unwrap();
        spool.acknowledge("phone", 1).unwrap();
        assert!(matches!(
            spool.acknowledge("phone", 0),
            Err(SpoolError::AckRegression { .. })
        ));
        assert!(matches!(
            spool.acknowledge("tablet", 2),
            Err(SpoolError::AckAhead { .. })
        ));
        spool.acknowledge("tablet", 1).unwrap();
        assert_eq!(
            spool.acknowledge("third", 1),
            Err(SpoolError::TooManyClients)
        );
        assert_eq!(
            spool.acknowledge("identifier-too-long", 1),
            Err(SpoolError::ClientIdTooLong)
        );
    }

    #[test]
    fn checkpoint_preserves_high_water_after_complete_eviction() {
        let mut spool = EventSpool::new(limits()).unwrap();
        spool.append(1, vec![1]).unwrap();
        spool.cleanup(20).unwrap();
        let restored = EventSpool::restore(limits(), spool.checkpoint()).unwrap();
        let mut restored = restored;
        assert_eq!(restored.append(21, vec![2]).unwrap(), 2);
    }

    #[test]
    fn ahead_cursor_and_empty_retained_gap_require_snapshot() {
        let mut spool = EventSpool::new(limits()).unwrap();
        spool.append(1, vec![1]).unwrap();
        spool.cleanup(20).unwrap();
        assert!(matches!(
            spool.subscribe(2),
            Subscription::SnapshotRequired { .. }
        ));
        // With no retained events, a cursor behind committed history has a gap.
        assert!(matches!(
            spool.subscribe(0),
            Subscription::SnapshotRequired { .. }
        ));
    }
}
