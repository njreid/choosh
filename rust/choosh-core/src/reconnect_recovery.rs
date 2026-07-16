//! Deterministic per-component reconnect recovery decisions.

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Component {
    Workspace,
    Items,
    EventSpool,
    Pins,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ComponentCheckpoint {
    pub component: Component,
    pub local_revision: u64,
    pub remote_revision: u64,
    pub oldest_replay_revision: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryAction {
    Current {
        component: Component,
    },
    Replay {
        component: Component,
        after_revision: u64,
    },
    Snapshot {
        component: Component,
        expected_revision: u64,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryError {
    InvalidLimits,
    DuplicateComponent,
    MissingComponent,
    RemoteRegressed,
    GenerationExhausted,
    RetryLimit,
    StaleGeneration,
}

#[derive(Debug)]
pub struct RecoveryCoordinator {
    generation: u64,
    max_retries: u16,
    retries: u16,
}
impl RecoveryCoordinator {
    /// Creates bounded recovery coordination.
    /// # Errors
    /// Returns invalid limits for a zero or excessive retry bound.
    pub fn new(max_retries: u16) -> Result<Self, RecoveryError> {
        if max_retries == 0 || max_retries > 64 {
            return Err(RecoveryError::InvalidLimits);
        }
        Ok(Self {
            generation: 0,
            max_retries,
            retries: 0,
        })
    }
    /// Starts one reconnect generation and produces exact per-component actions.
    /// # Errors
    /// Rejects duplicate/missing components, regressed remote state, or generation exhaustion.
    pub fn begin(
        &mut self,
        checkpoints: impl IntoIterator<Item = ComponentCheckpoint>,
    ) -> Result<(u64, Vec<RecoveryAction>), RecoveryError> {
        let mut values = checkpoints.into_iter().collect::<Vec<_>>();
        values.sort_by_key(|v| v.component);
        if values
            .windows(2)
            .any(|pair| pair[0].component == pair[1].component)
        {
            return Err(RecoveryError::DuplicateComponent);
        }
        if values.iter().map(|v| v.component).ne([
            Component::Workspace,
            Component::Items,
            Component::EventSpool,
            Component::Pins,
        ]) {
            return Err(RecoveryError::MissingComponent);
        }
        let mut actions = Vec::with_capacity(4);
        for value in values {
            if value.remote_revision < value.local_revision {
                return Err(RecoveryError::RemoteRegressed);
            }
            actions.push(if value.remote_revision == value.local_revision {
                RecoveryAction::Current {
                    component: value.component,
                }
            } else if value.local_revision >= value.oldest_replay_revision.saturating_sub(1) {
                RecoveryAction::Replay {
                    component: value.component,
                    after_revision: value.local_revision,
                }
            } else {
                RecoveryAction::Snapshot {
                    component: value.component,
                    expected_revision: value.remote_revision,
                }
            });
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(RecoveryError::GenerationExhausted)?;
        self.retries = 0;
        Ok((self.generation, actions))
    }
    /// Records a failed attempt and authorizes a bounded retry in the same generation.
    /// # Errors
    /// Rejects stale generations or exhausted retry allowance.
    pub fn retry(&mut self, generation: u64) -> Result<u16, RecoveryError> {
        if generation != self.generation {
            return Err(RecoveryError::StaleGeneration);
        }
        if self.retries >= self.max_retries {
            return Err(RecoveryError::RetryLimit);
        }
        self.retries += 1;
        Ok(self.retries)
    }
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn c(component: Component, local: u64, remote: u64, oldest: u64) -> ComponentCheckpoint {
        ComponentCheckpoint {
            component,
            local_revision: local,
            remote_revision: remote,
            oldest_replay_revision: oldest,
        }
    }
    fn all() -> Vec<ComponentCheckpoint> {
        vec![
            c(Component::Pins, 5, 5, 1),
            c(Component::EventSpool, 2, 8, 7),
            c(Component::Items, 6, 8, 5),
            c(Component::Workspace, 0, 4, 3),
        ]
    }
    #[test]
    fn shuffled_components_get_deterministic_independent_actions() {
        let mut r = RecoveryCoordinator::new(2).unwrap();
        let (_, a) = r.begin(all()).unwrap();
        assert_eq!(
            a,
            [
                RecoveryAction::Snapshot {
                    component: Component::Workspace,
                    expected_revision: 4
                },
                RecoveryAction::Replay {
                    component: Component::Items,
                    after_revision: 6
                },
                RecoveryAction::Snapshot {
                    component: Component::EventSpool,
                    expected_revision: 8
                },
                RecoveryAction::Current {
                    component: Component::Pins
                }
            ]
        );
    }
    #[test]
    fn replay_gap_uses_snapshot_not_partial_replay() {
        let mut r = RecoveryCoordinator::new(1).unwrap();
        let (_, a) = r.begin(all()).unwrap();
        assert_eq!(
            a[2],
            RecoveryAction::Snapshot {
                component: Component::EventSpool,
                expected_revision: 8
            }
        );
    }
    #[test]
    fn retries_are_bounded_and_generation_safe() {
        let mut r = RecoveryCoordinator::new(1).unwrap();
        let (g, _) = r.begin(all()).unwrap();
        assert_eq!(r.retry(g), Ok(1));
        assert_eq!(r.retry(g), Err(RecoveryError::RetryLimit));
        let (g2, _) = r.begin(all()).unwrap();
        assert_eq!(r.retry(g), Err(RecoveryError::StaleGeneration));
        assert_eq!(g2, g + 1);
    }
    #[test]
    fn malformed_or_regressed_inputs_fail_closed() {
        let mut r = RecoveryCoordinator::new(1).unwrap();
        let mut missing = all();
        missing.pop();
        assert_eq!(r.begin(missing), Err(RecoveryError::MissingComponent));
        let mut regressed = all();
        regressed[0].local_revision = 9;
        assert_eq!(r.begin(regressed), Err(RecoveryError::RemoteRegressed));
    }
    #[test]
    fn actions_have_no_remote_process_stop_variant() {
        let mut r = RecoveryCoordinator::new(1).unwrap();
        let (_, actions) = r.begin(all()).unwrap();
        assert_eq!(actions.len(), 4);
        assert_eq!(r.generation(), 1);
    }
}
