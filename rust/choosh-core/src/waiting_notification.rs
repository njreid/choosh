//! Deterministic, redacted notifications derived from agent waiting state.
//!
//! Untrusted agent payload text is never accepted by this projection. Notifications use fixed
//! bounded copy and carry only exact workspace/item activation identities.

use std::collections::BTreeMap;

pub const WAITING_SNAPSHOT_VERSION: u16 = 1;
pub const WAITING_NOTIFICATION_TEXT: &str = "Agent is waiting for input";
const MAX_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AgentIdentity {
    workspace: String,
    item: String,
}

impl AgentIdentity {
    /// Constructs an exact bounded workspace/item identity.
    ///
    /// # Errors
    ///
    /// Returns a typed validation error for empty, oversized, or control-bearing components.
    pub fn new(workspace: String, item: String) -> Result<Self, NotificationError> {
        validate_identity(&workspace)?;
        validate_identity(&item)?;
        Ok(Self { workspace, item })
    }

    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    #[must_use]
    pub fn item(&self) -> &str {
        &self.item
    }
}

fn validate_identity(value: &str) -> Result<(), NotificationError> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(NotificationError::InvalidIdentity);
    }
    if value.len() > MAX_ID_BYTES {
        return Err(NotificationError::IdentityTooLong);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentProjectionState {
    Waiting,
    Active,
    Removed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentStateEvent {
    pub sequence: u64,
    pub agent: AgentIdentity,
    pub state: AgentProjectionState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationIntent {
    pub workspace: String,
    pub item: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaitingNotification {
    pub agent: AgentIdentity,
    pub sequence: u64,
    pub text: &'static str,
    pub activation: ActivationIntent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredAgentProjection {
    pub agent: AgentIdentity,
    pub last_sequence: u64,
    pub waiting: bool,
    pub acknowledged_sequence: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaitingSnapshot {
    pub version: u16,
    pub agents: Vec<StoredAgentProjection>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotificationError {
    CapacityExceeded,
    InvalidIdentity,
    IdentityTooLong,
    InvalidSequence,
    StaleSequence { last: u64, received: u64 },
    UnknownAgent,
    NotWaiting,
    UnsupportedSnapshotVersion(u16),
    DuplicateAgent,
    InvalidSnapshot,
}

#[derive(Clone, Debug)]
struct Projection {
    last_sequence: u64,
    waiting: bool,
    acknowledged_sequence: Option<u64>,
}

#[derive(Debug)]
pub struct WaitingNotifications {
    capacity: usize,
    agents: BTreeMap<AgentIdentity, Projection>,
}

impl WaitingNotifications {
    /// Creates an empty bounded projection.
    ///
    /// # Errors
    ///
    /// Returns [`NotificationError::CapacityExceeded`] for zero capacity.
    pub fn new(capacity: usize) -> Result<Self, NotificationError> {
        if capacity == 0 {
            return Err(NotificationError::CapacityExceeded);
        }
        Ok(Self {
            capacity,
            agents: BTreeMap::new(),
        })
    }

    /// Applies one normalized agent state event in strictly increasing per-agent sequence order.
    ///
    /// # Errors
    ///
    /// Returns a typed sequence or capacity error without changing existing state.
    pub fn apply(&mut self, event: AgentStateEvent) -> Result<(), NotificationError> {
        if event.sequence == 0 {
            return Err(NotificationError::InvalidSequence);
        }
        if let Some(current) = self.agents.get(&event.agent) {
            if event.sequence <= current.last_sequence {
                return Err(NotificationError::StaleSequence {
                    last: current.last_sequence,
                    received: event.sequence,
                });
            }
        } else if self.agents.len() >= self.capacity {
            return Err(NotificationError::CapacityExceeded);
        }

        let projection = self.agents.entry(event.agent).or_insert(Projection {
            last_sequence: 0,
            waiting: false,
            acknowledged_sequence: None,
        });
        projection.last_sequence = event.sequence;
        projection.waiting = event.state == AgentProjectionState::Waiting;
        if !projection.waiting {
            projection.acknowledged_sequence = None;
        }
        Ok(())
    }

    /// Acknowledges the current waiting notification without changing remote agent state.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown agent or one that is not currently waiting.
    pub fn acknowledge(&mut self, agent: &AgentIdentity) -> Result<(), NotificationError> {
        let projection = self
            .agents
            .get_mut(agent)
            .ok_or(NotificationError::UnknownAgent)?;
        if !projection.waiting {
            return Err(NotificationError::NotWaiting);
        }
        projection.acknowledged_sequence = Some(projection.last_sequence);
        Ok(())
    }

    #[must_use]
    pub fn notifications(&self) -> Vec<WaitingNotification> {
        self.agents
            .iter()
            .filter(|(_, projection)| {
                projection.waiting
                    && projection.acknowledged_sequence != Some(projection.last_sequence)
            })
            .map(|(agent, projection)| WaitingNotification {
                agent: agent.clone(),
                sequence: projection.last_sequence,
                text: WAITING_NOTIFICATION_TEXT,
                activation: ActivationIntent {
                    workspace: agent.workspace.clone(),
                    item: agent.item.clone(),
                },
            })
            .collect()
    }

    /// Projects durable waiting state into bounded notification intents.
    ///
    /// The payload is fixed and redacted; agent-provided text is never forwarded.
    #[must_use]
    pub fn notification_intents(&self) -> Vec<crate::ports::NotificationIntent> {
        self.notifications()
            .into_iter()
            .map(|notification| crate::ports::NotificationIntent::Upsert {
                stable_id: format!("{}:{}", notification.agent.workspace(), notification.agent.item()),
                coarse_reason: notification.text.to_owned(),
            })
            .collect()
    }

    #[must_use]
    pub fn snapshot(&self) -> WaitingSnapshot {
        WaitingSnapshot {
            version: WAITING_SNAPSHOT_VERSION,
            agents: self
                .agents
                .iter()
                .map(|(agent, projection)| StoredAgentProjection {
                    agent: agent.clone(),
                    last_sequence: projection.last_sequence,
                    waiting: projection.waiting,
                    acknowledged_sequence: projection.acknowledged_sequence,
                })
                .collect(),
        }
    }

    /// Restores projection and acknowledgement state transactionally.
    ///
    /// # Errors
    ///
    /// Returns a version, capacity, duplicate, or invariant error without returning partial state.
    pub fn restore(capacity: usize, snapshot: WaitingSnapshot) -> Result<Self, NotificationError> {
        if snapshot.version != WAITING_SNAPSHOT_VERSION {
            return Err(NotificationError::UnsupportedSnapshotVersion(
                snapshot.version,
            ));
        }
        let mut restored = Self::new(capacity)?;
        if snapshot.agents.len() > capacity {
            return Err(NotificationError::CapacityExceeded);
        }
        for stored in snapshot.agents {
            if stored.last_sequence == 0
                || stored
                    .acknowledged_sequence
                    .is_some_and(|sequence| !stored.waiting || sequence != stored.last_sequence)
            {
                return Err(NotificationError::InvalidSnapshot);
            }
            let projection = Projection {
                last_sequence: stored.last_sequence,
                waiting: stored.waiting,
                acknowledged_sequence: stored.acknowledged_sequence,
            };
            if restored.agents.insert(stored.agent, projection).is_some() {
                return Err(NotificationError::DuplicateAgent);
            }
        }
        Ok(restored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(item: &str) -> AgentIdentity {
        AgentIdentity::new("workspace".into(), item.into()).unwrap()
    }

    fn event(sequence: u64, item: &str, state: AgentProjectionState) -> AgentStateEvent {
        AgentStateEvent {
            sequence,
            agent: agent(item),
            state,
        }
    }

    #[test]
    fn projects_one_redacted_notification_per_waiting_agent() {
        let mut projection = WaitingNotifications::new(2).unwrap();
        projection
            .apply(event(1, "agent-a", AgentProjectionState::Waiting))
            .unwrap();

        let notifications = projection.notifications();
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].text, WAITING_NOTIFICATION_TEXT);
        assert_eq!(notifications[0].activation.workspace, "workspace");
        assert_eq!(notifications[0].activation.item, "agent-a");
    }

    #[test]
    fn notification_intents_are_redacted_and_stable() {
        let mut projection = WaitingNotifications::new(1).unwrap();
        projection
            .apply(event(4, "agent", AgentProjectionState::Waiting))
            .unwrap();
        assert_eq!(
            projection.notification_intents(),
            vec![crate::ports::NotificationIntent::Upsert {
                stable_id: "workspace:agent".into(),
                coarse_reason: WAITING_NOTIFICATION_TEXT.into(),
            }]
        );
    }

    #[test]
    fn stale_events_cannot_resurrect_or_clear_notifications() {
        let mut projection = WaitingNotifications::new(1).unwrap();
        projection
            .apply(event(3, "agent", AgentProjectionState::Waiting))
            .unwrap();
        assert_eq!(
            projection.apply(event(2, "agent", AgentProjectionState::Active)),
            Err(NotificationError::StaleSequence {
                last: 3,
                received: 2
            })
        );
        assert_eq!(projection.notifications().len(), 1);
    }

    #[test]
    fn acknowledgement_survives_process_recreation() {
        let mut projection = WaitingNotifications::new(1).unwrap();
        projection
            .apply(event(4, "agent", AgentProjectionState::Waiting))
            .unwrap();
        projection.acknowledge(&agent("agent")).unwrap();

        let mut restored = WaitingNotifications::restore(1, projection.snapshot()).unwrap();
        assert!(restored.notifications().is_empty());
        restored
            .apply(event(5, "agent", AgentProjectionState::Waiting))
            .unwrap();
        assert_eq!(restored.notifications().len(), 1);
    }

    #[test]
    fn active_transition_clears_notification() {
        let mut projection = WaitingNotifications::new(1).unwrap();
        projection
            .apply(event(1, "agent", AgentProjectionState::Waiting))
            .unwrap();
        projection
            .apply(event(2, "agent", AgentProjectionState::Active))
            .unwrap();
        assert!(projection.notifications().is_empty());
    }

    #[test]
    fn restore_rejects_invalid_acknowledgement_transactionally() {
        let snapshot = WaitingSnapshot {
            version: WAITING_SNAPSHOT_VERSION,
            agents: vec![StoredAgentProjection {
                agent: agent("agent"),
                last_sequence: 7,
                waiting: false,
                acknowledged_sequence: Some(7),
            }],
        };
        assert_eq!(
            WaitingNotifications::restore(1, snapshot).unwrap_err(),
            NotificationError::InvalidSnapshot
        );
    }

    #[test]
    fn capacity_is_bounded_without_eviction() {
        let mut projection = WaitingNotifications::new(1).unwrap();
        projection
            .apply(event(1, "agent-a", AgentProjectionState::Waiting))
            .unwrap();
        assert_eq!(
            projection.apply(event(1, "agent-b", AgentProjectionState::Waiting)),
            Err(NotificationError::CapacityExceeded)
        );
        assert_eq!(projection.notifications()[0].agent, agent("agent-a"));
    }
}
