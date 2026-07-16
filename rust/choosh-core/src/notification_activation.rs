//! Resumable exact-target activation for a waiting-agent notification.

const MAX_ID_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationTarget {
    pub host: String,
    pub workspace: String,
    pub item: String,
    pub event_sequence: u64,
}

impl ActivationTarget {
    /// Validates a bounded exact activation identity.
    /// # Errors
    /// Returns [`ActivationError::InvalidTarget`] for invalid identity or zero sequence.
    pub fn validate(&self) -> Result<(), ActivationError<()>> {
        if self.event_sequence == 0
            || [&self.host, &self.workspace, &self.item]
                .into_iter()
                .any(|value| {
                    value.is_empty()
                        || value.len() > MAX_ID_BYTES
                        || value.chars().any(char::is_control)
                })
        {
            return Err(ActivationError::InvalidTarget);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ActivationPhase {
    Connect,
    OpenWorkspace,
    FocusPin,
    Acknowledge,
    Complete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationCheckpoint {
    pub target: ActivationTarget,
    pub next_phase: ActivationPhase,
}

pub trait ActivationActions {
    type Error;
    /// Connects the exact host.
    ///
    /// # Errors
    /// Returns an adapter failure.
    fn connect(&mut self, host: &str) -> Result<(), Self::Error>;
    /// Opens the exact workspace without substitution.
    ///
    /// # Errors
    /// Returns an adapter failure.
    fn open_workspace(&mut self, host: &str, workspace: &str) -> Result<bool, Self::Error>;
    /// Focuses the exact item pin without substitution.
    ///
    /// # Errors
    /// Returns an adapter failure.
    fn focus_pin(&mut self, workspace: &str, item: &str) -> Result<bool, Self::Error>;
    /// Acknowledges the exact originating sequence.
    ///
    /// # Errors
    /// Returns an adapter failure.
    fn acknowledge(&mut self, target: &ActivationTarget) -> Result<bool, Self::Error>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivationError<E> {
    InvalidTarget,
    MissingWorkspace,
    MissingItem,
    StaleEvent,
    Action(E),
}

/// Advances ordered activation from its durable checkpoint.
///
/// The checkpoint advances only after each successful idempotent action, so persisting it after
/// any return makes retries and process recreation resume without substituting identities.
/// # Errors
/// Returns typed missing/stale target errors or an adapter failure at the current phase.
pub fn activate<A: ActivationActions>(
    actions: &mut A,
    checkpoint: &mut ActivationCheckpoint,
) -> Result<ActivationPhase, ActivationError<A::Error>> {
    checkpoint
        .target
        .validate()
        .map_err(|_| ActivationError::InvalidTarget)?;
    loop {
        checkpoint.next_phase = match checkpoint.next_phase {
            ActivationPhase::Connect => {
                actions
                    .connect(&checkpoint.target.host)
                    .map_err(ActivationError::Action)?;
                ActivationPhase::OpenWorkspace
            }
            ActivationPhase::OpenWorkspace => {
                if !actions
                    .open_workspace(&checkpoint.target.host, &checkpoint.target.workspace)
                    .map_err(ActivationError::Action)?
                {
                    return Err(ActivationError::MissingWorkspace);
                }
                ActivationPhase::FocusPin
            }
            ActivationPhase::FocusPin => {
                if !actions
                    .focus_pin(&checkpoint.target.workspace, &checkpoint.target.item)
                    .map_err(ActivationError::Action)?
                {
                    return Err(ActivationError::MissingItem);
                }
                ActivationPhase::Acknowledge
            }
            ActivationPhase::Acknowledge => {
                if !actions
                    .acknowledge(&checkpoint.target)
                    .map_err(ActivationError::Action)?
                {
                    return Err(ActivationError::StaleEvent);
                }
                ActivationPhase::Complete
            }
            ActivationPhase::Complete => return Ok(ActivationPhase::Complete),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Fake {
        calls: Vec<&'static str>,
        missing_item: bool,
        stale: bool,
        fail_connect: bool,
    }
    impl ActivationActions for Fake {
        type Error = &'static str;
        fn connect(&mut self, _: &str) -> Result<(), Self::Error> {
            self.calls.push("connect");
            if self.fail_connect {
                Err("offline")
            } else {
                Ok(())
            }
        }
        fn open_workspace(&mut self, _: &str, _: &str) -> Result<bool, Self::Error> {
            self.calls.push("workspace");
            Ok(true)
        }
        fn focus_pin(&mut self, _: &str, _: &str) -> Result<bool, Self::Error> {
            self.calls.push("focus");
            Ok(!self.missing_item)
        }
        fn acknowledge(&mut self, _: &ActivationTarget) -> Result<bool, Self::Error> {
            self.calls.push("ack");
            Ok(!self.stale)
        }
    }
    fn checkpoint() -> ActivationCheckpoint {
        ActivationCheckpoint {
            target: ActivationTarget {
                host: "host".into(),
                workspace: "ws".into(),
                item: "agent".into(),
                event_sequence: 7,
            },
            next_phase: ActivationPhase::Connect,
        }
    }

    #[test]
    fn exact_actions_run_in_order() {
        let mut f = Fake::default();
        let mut c = checkpoint();
        assert_eq!(activate(&mut f, &mut c), Ok(ActivationPhase::Complete));
        assert_eq!(f.calls, ["connect", "workspace", "focus", "ack"]);
    }
    #[test]
    fn retry_resumes_after_durable_phase() {
        let mut f = Fake {
            fail_connect: true,
            ..Fake::default()
        };
        let mut c = checkpoint();
        assert_eq!(
            activate(&mut f, &mut c),
            Err(ActivationError::Action("offline"))
        );
        assert_eq!(c.next_phase, ActivationPhase::Connect);
        f.fail_connect = false;
        activate(&mut f, &mut c).unwrap();
        assert_eq!(f.calls, ["connect", "connect", "workspace", "focus", "ack"]);
    }
    #[test]
    fn missing_item_never_acknowledges_or_substitutes() {
        let mut f = Fake {
            missing_item: true,
            ..Fake::default()
        };
        let mut c = checkpoint();
        assert_eq!(activate(&mut f, &mut c), Err(ActivationError::MissingItem));
        assert_eq!(c.next_phase, ActivationPhase::FocusPin);
        assert!(!f.calls.contains(&"ack"));
    }
    #[test]
    fn stale_event_is_not_acknowledged_as_success() {
        let mut f = Fake {
            stale: true,
            ..Fake::default()
        };
        let mut c = checkpoint();
        assert_eq!(activate(&mut f, &mut c), Err(ActivationError::StaleEvent));
        assert_eq!(c.next_phase, ActivationPhase::Acknowledge);
    }
    #[test]
    fn completed_checkpoint_is_idempotent() {
        let mut f = Fake::default();
        let mut c = checkpoint();
        c.next_phase = ActivationPhase::Complete;
        assert_eq!(activate(&mut f, &mut c), Ok(ActivationPhase::Complete));
        assert!(f.calls.is_empty());
    }
}
