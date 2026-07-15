//! Typed metadata for daemon-managed Zellij items.
//!
//! This module deliberately has no pinning API: pins are client-owned presentation
//! state and must not start, stop, or remove a managed process.

use std::collections::BTreeMap;

const MAX_ID_BYTES: usize = 128;
const MAX_NAME_BYTES: usize = 256;
const MAX_TARGET_COMPONENT_BYTES: usize = 256;

macro_rules! bounded_identity {
    ($name:ident, $max:expr) => {
        #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            /// Parses a non-empty, bounded identity without control characters.
            ///
            /// # Errors
            ///
            /// Returns [`IdentityError`] when the value is empty, exceeds the
            /// identity byte limit, or contains a control character.
            pub fn parse(value: impl Into<String>) -> Result<Self, IdentityError> {
                let value = value.into();
                validate_text(&value, $max)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

bounded_identity!(ItemId, MAX_ID_BYTES);
bounded_identity!(WorkspaceId, MAX_ID_BYTES);
bounded_identity!(AgentAdapterId, MAX_ID_BYTES);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityError {
    Empty,
    TooLong,
    ControlCharacter,
}

fn validate_text(value: &str, max_bytes: usize) -> Result<(), IdentityError> {
    if value.is_empty() {
        return Err(IdentityError::Empty);
    }
    if value.len() > max_bytes {
        return Err(IdentityError::TooLong);
    }
    if value.chars().any(char::is_control) {
        return Err(IdentityError::ControlCharacter);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZellijTarget {
    session: String,
    tab: String,
    pane: String,
}

impl ZellijTarget {
    /// Constructs the exact Zellij session, tab, and pane identity.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError`] if any component is empty, oversized, or
    /// contains a control character.
    pub fn new(session: String, tab: String, pane: String) -> Result<Self, IdentityError> {
        validate_text(&session, MAX_TARGET_COMPONENT_BYTES)?;
        validate_text(&tab, MAX_TARGET_COMPONENT_BYTES)?;
        validate_text(&pane, MAX_TARGET_COMPONENT_BYTES)?;
        Ok(Self { session, tab, pane })
    }

    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    #[must_use]
    pub fn tab(&self) -> &str {
        &self.tab
    }

    #[must_use]
    pub fn pane(&self) -> &str {
        &self.pane
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceProtocol {
    Http,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoopbackPort(u16);

impl LoopbackPort {
    /// Constructs a declared, nonzero TCP port.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::Empty`] for port zero.
    pub fn new(value: u16) -> Result<Self, IdentityError> {
        if value == 0 {
            return Err(IdentityError::Empty);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn get(self) -> u16 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ItemKind {
    Agent {
        adapter: AgentAdapterId,
    },
    Service {
        protocol: ServiceProtocol,
        port: LoopbackPort,
    },
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemStatus {
    Starting,
    Running,
    Waiting,
    Stopped,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManagedItem {
    id: ItemId,
    workspace_id: WorkspaceId,
    display_name: String,
    kind: ItemKind,
    target: ZellijTarget,
    status: ItemStatus,
}

impl ManagedItem {
    /// Constructs starting metadata for one daemon-managed item.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError`] if the display name is empty, oversized, or
    /// contains a control character.
    pub fn new(
        id: ItemId,
        workspace_id: WorkspaceId,
        display_name: String,
        kind: ItemKind,
        target: ZellijTarget,
    ) -> Result<Self, IdentityError> {
        validate_text(&display_name, MAX_NAME_BYTES)?;
        Ok(Self {
            id,
            workspace_id,
            display_name,
            kind,
            target,
            status: ItemStatus::Starting,
        })
    }

    #[must_use]
    pub fn id(&self) -> &ItemId {
        &self.id
    }
    #[must_use]
    pub fn workspace_id(&self) -> &WorkspaceId {
        &self.workspace_id
    }
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
    #[must_use]
    pub fn kind(&self) -> &ItemKind {
        &self.kind
    }
    #[must_use]
    pub fn target(&self) -> &ZellijTarget {
        &self.target
    }
    #[must_use]
    pub fn status(&self) -> ItemStatus {
        self.status
    }

    fn transition(&mut self, next: ItemStatus) -> Result<(), RegistryError> {
        if transition_allowed(&self.kind, self.status, next) {
            self.status = next;
            Ok(())
        } else {
            Err(RegistryError::InvalidTransition {
                from: self.status,
                to: next,
            })
        }
    }
}

fn transition_allowed(kind: &ItemKind, from: ItemStatus, to: ItemStatus) -> bool {
    use ItemStatus::{Failed, Running, Starting, Stopped, Unknown, Waiting};
    if from == to {
        return true;
    }
    match kind {
        ItemKind::Agent { .. } => matches!(
            (from, to),
            (Starting, Running | Waiting | Failed | Stopped | Unknown)
                | (Running, Waiting | Failed | Stopped | Unknown)
                | (Waiting, Running | Failed | Stopped | Unknown)
                | (Unknown, Running | Waiting | Failed | Stopped)
                | (Failed | Stopped, Starting)
                | (Failed, Stopped)
        ),
        ItemKind::Service { .. } | ItemKind::Terminal => matches!(
            (from, to),
            (Starting, Running | Failed | Stopped | Unknown)
                | (Running, Failed | Stopped | Unknown)
                | (Unknown, Running | Failed | Stopped)
                | (Failed | Stopped, Starting)
                | (Failed, Stopped)
        ),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrySnapshot {
    pub revision: u64,
    pub items: Vec<ManagedItem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryError {
    CapacityExceeded,
    DuplicateItemId,
    DuplicateTarget,
    DuplicateServiceName,
    ItemNotFound,
    RevisionExhausted,
    RemovalRequiresStopped,
    InvalidTransition { from: ItemStatus, to: ItemStatus },
}

pub struct ItemRegistry {
    capacity: usize,
    revision: u64,
    items: BTreeMap<ItemId, ManagedItem>,
}

impl ItemRegistry {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            revision: 0,
            items: BTreeMap::new(),
        }
    }

    /// Inserts unique managed-item metadata without performing process IO.
    ///
    /// # Errors
    ///
    /// Returns a stable [`RegistryError`] if capacity or revision is exhausted,
    /// or if the item ID, target, or workspace service name is already present.
    pub fn insert(&mut self, item: ManagedItem) -> Result<(), RegistryError> {
        if self.items.contains_key(item.id()) {
            return Err(RegistryError::DuplicateItemId);
        }
        if self.items.len() >= self.capacity {
            return Err(RegistryError::CapacityExceeded);
        }
        if self
            .items
            .values()
            .any(|existing| existing.target() == item.target())
        {
            return Err(RegistryError::DuplicateTarget);
        }
        if matches!(item.kind(), ItemKind::Service { .. })
            && self.items.values().any(|existing| {
                existing.workspace_id() == item.workspace_id()
                    && matches!(existing.kind(), ItemKind::Service { .. })
                    && existing.display_name() == item.display_name()
            })
        {
            return Err(RegistryError::DuplicateServiceName);
        }
        let revision = self.next_revision()?;
        self.items.insert(item.id().clone(), item);
        self.revision = revision;
        Ok(())
    }

    /// Applies a lifecycle transition to the exact stable item identity.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::ItemNotFound`],
    /// [`RegistryError::InvalidTransition`], or
    /// [`RegistryError::RevisionExhausted`] without partially mutating state.
    pub fn transition(&mut self, id: &ItemId, next: ItemStatus) -> Result<(), RegistryError> {
        let revision = self.next_revision()?;
        let item = self.items.get_mut(id).ok_or(RegistryError::ItemNotFound)?;
        if item.status() == next {
            return Ok(());
        }
        item.transition(next)?;
        self.revision = revision;
        Ok(())
    }

    /// Stops metadata only. The outer Zellij adapter performs process termination.
    ///
    /// # Errors
    ///
    /// Returns the same typed errors as [`Self::transition`].
    pub fn mark_stopped(&mut self, id: &ItemId) -> Result<(), RegistryError> {
        self.transition(id, ItemStatus::Stopped)
    }

    /// Removal is deliberately separate from stopping and never implies process IO.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::ItemNotFound`] when absent,
    /// [`RegistryError::RemovalRequiresStopped`] while active, or
    /// [`RegistryError::RevisionExhausted`] before removal.
    pub fn remove(&mut self, id: &ItemId) -> Result<ManagedItem, RegistryError> {
        let item = self.items.get(id).ok_or(RegistryError::ItemNotFound)?;
        if item.status() != ItemStatus::Stopped {
            return Err(RegistryError::RemovalRequiresStopped);
        }
        let revision = self.next_revision()?;
        let Some(removed) = self.items.remove(id) else {
            return Err(RegistryError::ItemNotFound);
        };
        self.revision = revision;
        Ok(removed)
    }

    #[must_use]
    pub fn get(&self, id: &ItemId) -> Option<&ManagedItem> {
        self.items.get(id)
    }

    #[must_use]
    pub fn snapshot(&self, workspace_id: &WorkspaceId) -> RegistrySnapshot {
        RegistrySnapshot {
            revision: self.revision,
            items: self
                .items
                .values()
                .filter(|item| item.workspace_id() == workspace_id)
                .cloned()
                .collect(),
        }
    }

    fn next_revision(&self) -> Result<u64, RegistryError> {
        self.revision
            .checked_add(1)
            .ok_or(RegistryError::RevisionExhausted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> ItemId {
        ItemId::parse(value).unwrap()
    }
    fn workspace(value: &str) -> WorkspaceId {
        WorkspaceId::parse(value).unwrap()
    }
    fn target(tab: &str) -> ZellijTarget {
        ZellijTarget::new("workspace".into(), tab.into(), "pane-1".into()).unwrap()
    }
    fn terminal(item_id: &str, tab: &str) -> ManagedItem {
        ManagedItem::new(
            id(item_id),
            workspace("ws"),
            item_id.into(),
            ItemKind::Terminal,
            target(tab),
        )
        .unwrap()
    }
    fn service(item_id: &str, name: &str, tab: &str) -> ManagedItem {
        ManagedItem::new(
            id(item_id),
            workspace("ws"),
            name.into(),
            ItemKind::Service {
                protocol: ServiceProtocol::Http,
                port: LoopbackPort::new(8080).unwrap(),
            },
            target(tab),
        )
        .unwrap()
    }

    #[test]
    fn identities_and_targets_reject_unbounded_or_control_input() {
        assert_eq!(ItemId::parse(""), Err(IdentityError::Empty));
        assert_eq!(
            WorkspaceId::parse("x\n"),
            Err(IdentityError::ControlCharacter)
        );
        assert_eq!(
            AgentAdapterId::parse("x".repeat(129)),
            Err(IdentityError::TooLong)
        );
        assert_eq!(
            ZellijTarget::new("s".into(), String::new(), "p".into()),
            Err(IdentityError::Empty)
        );
        assert_eq!(LoopbackPort::new(0), Err(IdentityError::Empty));
    }

    #[test]
    fn bounded_registry_rejects_duplicates_without_mutation() {
        let mut registry = ItemRegistry::new(2);
        registry.insert(terminal("b", "tab-b")).unwrap();
        let before = registry.snapshot(&workspace("ws"));
        assert_eq!(
            registry.insert(terminal("b", "other")),
            Err(RegistryError::DuplicateItemId)
        );
        assert_eq!(
            registry.insert(terminal("c", "tab-b")),
            Err(RegistryError::DuplicateTarget)
        );
        assert_eq!(registry.snapshot(&workspace("ws")), before);
        registry.insert(terminal("c", "tab-c")).unwrap();
        assert_eq!(
            registry.insert(terminal("d", "tab-d")),
            Err(RegistryError::CapacityExceeded)
        );
    }

    #[test]
    fn service_names_are_unique_only_among_services_in_a_workspace() {
        let mut registry = ItemRegistry::new(4);
        registry.insert(service("one", "preview", "one")).unwrap();
        assert_eq!(
            registry.insert(service("two", "preview", "two")),
            Err(RegistryError::DuplicateServiceName)
        );
        let ordinary = ManagedItem::new(
            id("three"),
            workspace("ws"),
            "preview".into(),
            ItemKind::Terminal,
            target("three"),
        )
        .unwrap();
        registry.insert(ordinary).unwrap();
    }

    #[test]
    fn kind_specific_lifecycle_is_fail_closed_and_idempotent() {
        let mut registry = ItemRegistry::new(2);
        registry.insert(service("svc", "service", "svc")).unwrap();
        let svc = id("svc");
        assert_eq!(
            registry.transition(&svc, ItemStatus::Waiting),
            Err(RegistryError::InvalidTransition {
                from: ItemStatus::Starting,
                to: ItemStatus::Waiting
            })
        );
        assert_eq!(registry.get(&svc).unwrap().status(), ItemStatus::Starting);
        registry.transition(&svc, ItemStatus::Running).unwrap();
        let revision = registry.snapshot(&workspace("ws")).revision;
        registry.transition(&svc, ItemStatus::Running).unwrap();
        assert_eq!(registry.snapshot(&workspace("ws")).revision, revision);
    }

    #[test]
    fn agent_can_wait_without_naming_a_specific_agent_product() {
        let agent = ManagedItem::new(
            id("agent"),
            workspace("ws"),
            "assistant".into(),
            ItemKind::Agent {
                adapter: AgentAdapterId::parse("adapter-v1").unwrap(),
            },
            target("agent"),
        )
        .unwrap();
        let mut registry = ItemRegistry::new(1);
        registry.insert(agent).unwrap();
        registry
            .transition(&id("agent"), ItemStatus::Waiting)
            .unwrap();
        assert_eq!(
            registry.get(&id("agent")).unwrap().status(),
            ItemStatus::Waiting
        );
    }

    #[test]
    fn stopping_and_removal_are_distinct_explicit_mutations() {
        let mut registry = ItemRegistry::new(1);
        registry.insert(terminal("shell", "shell")).unwrap();
        let shell = id("shell");
        assert_eq!(
            registry.remove(&shell),
            Err(RegistryError::RemovalRequiresStopped)
        );
        assert!(registry.get(&shell).is_some());
        registry.mark_stopped(&shell).unwrap();
        assert!(registry.get(&shell).is_some());
        let removed = registry.remove(&shell).unwrap();
        assert_eq!(removed.id(), &shell);
        assert!(registry.get(&shell).is_none());
    }

    #[test]
    fn snapshots_are_workspace_scoped_and_stably_ordered_by_item_id() {
        let mut registry = ItemRegistry::new(3);
        registry.insert(terminal("z", "z")).unwrap();
        registry.insert(terminal("a", "a")).unwrap();
        let other = ManagedItem::new(
            id("m"),
            workspace("other"),
            "m".into(),
            ItemKind::Terminal,
            ZellijTarget::new("other".into(), "m".into(), "p".into()).unwrap(),
        )
        .unwrap();
        registry.insert(other).unwrap();
        let snapshot = registry.snapshot(&workspace("ws"));
        assert_eq!(
            snapshot
                .items
                .iter()
                .map(|item| item.id().as_str())
                .collect::<Vec<_>>(),
            vec!["a", "z"]
        );
    }
}
