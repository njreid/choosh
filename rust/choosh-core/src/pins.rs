//! Deterministic client-owned pin ordering and restoration.
//!
//! Pins describe presentation targets only. They do not own or mutate remote process
//! lifecycle, and unavailable targets remain in place during reconciliation.

use std::collections::{BTreeMap, BTreeSet};

pub const PIN_DESCRIPTOR_VERSION: u16 = 1;
const MAX_ID_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PinTarget {
    Agent { workspace: String, item: String },
    Document { workspace: String, path: String },
    Diff { workspace: String, path: String },
    Service { workspace: String, item: String },
}

impl PinTarget {
    /// Validates every component of an exact compound pin identity.
    ///
    /// # Errors
    ///
    /// Returns a typed error when a component is empty, oversized, or contains a control
    /// character. Paths are opaque identities here; host path validation belongs elsewhere.
    pub fn validate(&self) -> Result<(), PinError> {
        let (workspace, target) = match self {
            Self::Agent { workspace, item } | Self::Service { workspace, item } => {
                (workspace, item)
            }
            Self::Document { workspace, path } | Self::Diff { workspace, path } => {
                (workspace, path)
            }
        };
        validate_component(workspace)?;
        validate_component(target)
    }
}

fn validate_component(value: &str) -> Result<(), PinError> {
    if value.is_empty() {
        return Err(PinError::InvalidIdentity);
    }
    if value.len() > MAX_ID_BYTES {
        return Err(PinError::IdentityTooLong);
    }
    if value.chars().any(char::is_control) {
        return Err(PinError::InvalidIdentity);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinDescriptor {
    pub version: u16,
    pub target: PinTarget,
}

impl PinDescriptor {
    #[must_use]
    pub const fn current(target: PinTarget) -> Self {
        Self {
            version: PIN_DESCRIPTOR_VERSION,
            target,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredPin {
    pub ordinal: usize,
    pub descriptor: PinDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PinView {
    pub ordinal: usize,
    pub descriptor: PinDescriptor,
    pub available: bool,
    pub focused: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToggleOutcome {
    Pinned { ordinal: usize },
    Unpinned { former_ordinal: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PinError {
    CapacityExceeded,
    UnsupportedVersion(u16),
    InvalidIdentity,
    IdentityTooLong,
    DuplicateTarget,
    DuplicateOrdinal,
    SparseOrdinals,
    InvalidFocus,
}

#[derive(Debug)]
pub struct PinSet {
    capacity: usize,
    pins: Vec<PinDescriptor>,
    available: BTreeSet<PinTarget>,
    focused: Option<usize>,
}

impl PinSet {
    /// Creates an empty bounded pin set.
    ///
    /// # Errors
    ///
    /// Returns [`PinError::CapacityExceeded`] when the capacity is zero.
    pub fn new(capacity: usize) -> Result<Self, PinError> {
        if capacity == 0 {
            return Err(PinError::CapacityExceeded);
        }
        Ok(Self {
            capacity,
            pins: Vec::new(),
            available: BTreeSet::new(),
            focused: None,
        })
    }

    /// Restores a complete snapshot transactionally, independent of input ordering.
    ///
    /// `focused_ordinal` is interpreted after sorting and dense-ordinal validation. Target
    /// availability is deliberately restored later by [`Self::reconcile`].
    ///
    /// # Errors
    ///
    /// Returns a validation, version, duplicate, density, capacity, or focus error. No partial
    /// set is returned.
    pub fn restore(
        capacity: usize,
        stored: Vec<StoredPin>,
        focused_ordinal: Option<usize>,
    ) -> Result<Self, PinError> {
        let mut restored = Self::new(capacity)?;
        if stored.len() > capacity {
            return Err(PinError::CapacityExceeded);
        }

        let mut by_ordinal = BTreeMap::new();
        let mut targets = BTreeSet::new();
        for pin in stored {
            validate_descriptor(&pin.descriptor)?;
            if !targets.insert(pin.descriptor.target.clone()) {
                return Err(PinError::DuplicateTarget);
            }
            if by_ordinal.insert(pin.ordinal, pin.descriptor).is_some() {
                return Err(PinError::DuplicateOrdinal);
            }
        }
        if by_ordinal.keys().copied().ne(0..by_ordinal.len()) {
            return Err(PinError::SparseOrdinals);
        }
        if focused_ordinal.is_some_and(|ordinal| ordinal >= by_ordinal.len()) {
            return Err(PinError::InvalidFocus);
        }

        restored.pins = by_ordinal.into_values().collect();
        restored.focused = focused_ordinal;
        Ok(restored)
    }

    /// Toggles one exact target. New pins append and receive focus; removal compacts ordinals.
    ///
    /// # Errors
    ///
    /// Returns a validation or capacity error without changing the set.
    pub fn toggle(&mut self, target: PinTarget) -> Result<ToggleOutcome, PinError> {
        target.validate()?;
        if let Some(ordinal) = self.pins.iter().position(|pin| pin.target == target) {
            self.pins.remove(ordinal);
            self.available.remove(&target);
            self.focused = match self.focused {
                Some(focused) if focused == ordinal => {
                    (!self.pins.is_empty()).then_some(ordinal.min(self.pins.len() - 1))
                }
                Some(focused) if focused > ordinal => Some(focused - 1),
                other => other,
            };
            return Ok(ToggleOutcome::Unpinned {
                former_ordinal: ordinal,
            });
        }
        if self.pins.len() >= self.capacity {
            return Err(PinError::CapacityExceeded);
        }
        let ordinal = self.pins.len();
        self.pins.push(PinDescriptor::current(target));
        self.focused = Some(ordinal);
        Ok(ToggleOutcome::Pinned { ordinal })
    }

    /// Replaces only availability observations, preserving identities, order, and focus.
    pub fn reconcile(&mut self, available: impl IntoIterator<Item = PinTarget>) {
        self.available = available
            .into_iter()
            .filter(|target| self.pins.iter().any(|pin| pin.target == *target))
            .collect();
    }

    /// Focuses an existing dense ordinal.
    ///
    /// # Errors
    ///
    /// Returns [`PinError::InvalidFocus`] without changing focus for an absent ordinal.
    pub fn focus(&mut self, ordinal: usize) -> Result<(), PinError> {
        if ordinal >= self.pins.len() {
            return Err(PinError::InvalidFocus);
        }
        self.focused = Some(ordinal);
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<StoredPin> {
        self.pins
            .iter()
            .cloned()
            .enumerate()
            .map(|(ordinal, descriptor)| StoredPin {
                ordinal,
                descriptor,
            })
            .collect()
    }

    #[must_use]
    pub fn views(&self) -> Vec<PinView> {
        self.pins
            .iter()
            .cloned()
            .enumerate()
            .map(|(ordinal, descriptor)| PinView {
                available: self.available.contains(&descriptor.target),
                focused: self.focused == Some(ordinal),
                ordinal,
                descriptor,
            })
            .collect()
    }
}

fn validate_descriptor(descriptor: &PinDescriptor) -> Result<(), PinError> {
    if descriptor.version != PIN_DESCRIPTOR_VERSION {
        return Err(PinError::UnsupportedVersion(descriptor.version));
    }
    descriptor.target.validate()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(item: &str) -> PinTarget {
        PinTarget::Agent {
            workspace: "workspace".into(),
            item: item.into(),
        }
    }

    fn stored(ordinal: usize, target: PinTarget) -> StoredPin {
        StoredPin {
            ordinal,
            descriptor: PinDescriptor::current(target),
        }
    }

    #[test]
    fn shuffled_snapshot_restores_dense_order_and_focus() {
        let pins = PinSet::restore(
            3,
            vec![
                stored(2, agent("c")),
                stored(0, agent("a")),
                stored(1, agent("b")),
            ],
            Some(1),
        )
        .unwrap();

        let views = pins.views();
        assert_eq!(views[0].descriptor.target, agent("a"));
        assert_eq!(views[1].descriptor.target, agent("b"));
        assert_eq!(views[2].descriptor.target, agent("c"));
        assert!(views[1].focused);
        assert!(views.iter().all(|view| !view.available));
    }

    #[test]
    fn reconciliation_preserves_unavailable_placeholder_and_focus() {
        let mut pins = PinSet::restore(
            3,
            vec![stored(0, agent("a")), stored(1, agent("b"))],
            Some(1),
        )
        .unwrap();
        pins.reconcile([agent("a"), agent("not-pinned")]);

        let views = pins.views();
        assert!(views[0].available);
        assert!(!views[1].available);
        assert!(views[1].focused);
        assert_eq!(pins.snapshot()[1].descriptor.target, agent("b"));
    }

    #[test]
    fn toggle_compacts_ordinals_and_moves_focus_deterministically() {
        let mut pins = PinSet::new(3).unwrap();
        pins.toggle(agent("a")).unwrap();
        pins.toggle(agent("b")).unwrap();
        pins.toggle(agent("c")).unwrap();
        pins.focus(1).unwrap();

        assert_eq!(
            pins.toggle(agent("b")).unwrap(),
            ToggleOutcome::Unpinned { former_ordinal: 1 }
        );
        let views = pins.views();
        assert_eq!(views.len(), 2);
        assert_eq!(views[1].descriptor.target, agent("c"));
        assert!(views[1].focused);
        assert_eq!(pins.snapshot()[1].ordinal, 1);
    }

    #[test]
    fn invalid_restore_is_rejected_as_a_whole() {
        assert_eq!(
            PinSet::restore(
                3,
                vec![stored(0, agent("a")), stored(2, agent("b"))],
                Some(0)
            )
            .unwrap_err(),
            PinError::SparseOrdinals
        );
        assert_eq!(
            PinSet::restore(3, vec![stored(0, agent("a")), stored(1, agent("a"))], None)
                .unwrap_err(),
            PinError::DuplicateTarget
        );
    }

    #[test]
    fn variants_with_same_text_remain_distinct_compound_identities() {
        let service = PinTarget::Service {
            workspace: "workspace".into(),
            item: "same".into(),
        };
        let mut pins = PinSet::new(2).unwrap();
        pins.toggle(agent("same")).unwrap();
        pins.toggle(service).unwrap();
        assert_eq!(pins.views().len(), 2);
    }
}
