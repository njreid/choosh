//! Deterministic coordination of workspace, item, and event metadata.
//!
//! This layer performs no filesystem, process, session, clock, or network IO.
//! In particular, unregistering metadata never terminates a Zellij session.

use std::collections::BTreeMap;

use choosh_core::event_spool::{EventSpool, SpoolError, SpoolLimits, SpoolSnapshot, Subscription};
use choosh_core::item::{
    ItemId, ItemRegistry, ItemStatus, ManagedItem, RegistryError as ItemError,
    RegistrySnapshot as ItemSnapshot, WorkspaceId as ItemWorkspaceId,
};
use choosh_core::workspace::{
    RegisterWorkspace, RegistrySnapshot as WorkspaceRegistrySnapshot, WorkspaceError, WorkspaceId,
    WorkspaceLimits, WorkspaceSnapshot,
};

/// Bounds for every collection owned by one coordinator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoordinatorLimits {
    pub workspaces: WorkspaceLimits,
    pub max_items: usize,
    pub events: SpoolLimits,
}

impl CoordinatorLimits {
    /// Validates limits before any state is constructed.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinatorError::InvalidLimits`] for a zero item bound and
    /// forwards invalid workspace or event-spool limits.
    pub fn validate(self) -> Result<Self, CoordinatorError> {
        if self.max_items == 0 {
            return Err(CoordinatorError::InvalidLimits);
        }
        choosh_core::workspace::WorkspaceRegistry::new(self.workspaces)
            .map_err(CoordinatorError::Workspace)?;
        self.events.validate().map_err(CoordinatorError::Event)?;
        Ok(self)
    }
}

/// A consistent in-memory projection for one registered workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceStateSnapshot {
    pub workspace_revision: u64,
    pub workspace: WorkspaceSnapshot,
    pub item_revision: u64,
    pub items: Vec<ManagedItem>,
    pub event_high_water: u64,
}

/// Stable coordinator failures. All errors leave state unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoordinatorError {
    InvalidLimits,
    Workspace(WorkspaceError),
    Item(ItemError),
    Event(SpoolError),
    WorkspaceNotFound,
    WorkspaceHasItems { count: usize },
    ItemWorkspaceMismatch,
}

/// In-memory authority for daemon-owned workspace and item metadata.
pub struct DaemonCoordinator {
    workspaces: choosh_core::workspace::WorkspaceRegistry,
    items: ItemRegistry,
    event_limits: SpoolLimits,
    events: BTreeMap<WorkspaceId, EventSpool>,
}

impl DaemonCoordinator {
    /// Constructs empty state after validating all configured bounds.
    ///
    /// # Errors
    ///
    /// Returns a typed limit error without constructing partial state.
    pub fn new(limits: CoordinatorLimits) -> Result<Self, CoordinatorError> {
        let limits = limits.validate()?;
        Ok(Self {
            workspaces: choosh_core::workspace::WorkspaceRegistry::new(limits.workspaces)
                .map_err(CoordinatorError::Workspace)?,
            items: ItemRegistry::new(limits.max_items),
            event_limits: limits.events,
            events: BTreeMap::new(),
        })
    }

    #[must_use]
    pub fn workspace_snapshot(&self) -> WorkspaceRegistrySnapshot {
        self.workspaces.snapshot()
    }

    /// Registers metadata and creates its empty event spool as one operation.
    ///
    /// # Errors
    ///
    /// Validation occurs before registry mutation. Any error leaves all three
    /// coordinator stores unchanged.
    pub fn register_workspace(
        &mut self,
        expected_revision: u64,
        request: RegisterWorkspace,
    ) -> Result<WorkspaceSnapshot, CoordinatorError> {
        let spool = EventSpool::new(self.event_limits).map_err(CoordinatorError::Event)?;
        let id = request.id.clone();
        let registered = self
            .workspaces
            .register(expected_revision, request)
            .map_err(CoordinatorError::Workspace)?;
        let replaced = self.events.insert(id, spool);
        debug_assert!(
            replaced.is_none(),
            "workspace registry guarantees unique IDs"
        );
        Ok(registered)
    }

    /// Removes metadata only when no managed items remain.
    ///
    /// This method never communicates with Zellij and therefore cannot stop or
    /// terminate any process or session.
    ///
    /// # Errors
    ///
    /// Returns a typed revision, existence, identity, or remaining-item error.
    pub fn unregister_workspace(
        &mut self,
        expected_revision: u64,
        id: &WorkspaceId,
    ) -> Result<WorkspaceSnapshot, CoordinatorError> {
        self.require_workspace(id)?;
        let item_id = item_workspace_id(id)?;
        let count = self.items.snapshot(&item_id).items.len();
        if count != 0 {
            return Err(CoordinatorError::WorkspaceHasItems { count });
        }
        let removed = self
            .workspaces
            .unregister(expected_revision, id)
            .map_err(CoordinatorError::Workspace)?;
        self.events.remove(id);
        Ok(removed)
    }

    /// Inserts item metadata only for an existing, exactly matching workspace.
    ///
    /// # Errors
    ///
    /// Returns an existence, identity, capacity, duplicate, or revision error.
    pub fn insert_item(&mut self, item: ManagedItem) -> Result<(), CoordinatorError> {
        let workspace = workspace_id(item.workspace_id())?;
        self.require_workspace(&workspace)?;
        self.items.insert(item).map_err(CoordinatorError::Item)
    }

    /// Applies an item lifecycle transition without performing process IO.
    ///
    /// # Errors
    ///
    /// Forwards typed item lookup, transition, and revision errors.
    pub fn transition_item(
        &mut self,
        id: &ItemId,
        next: ItemStatus,
    ) -> Result<(), CoordinatorError> {
        self.items
            .transition(id, next)
            .map_err(CoordinatorError::Item)
    }

    /// Removes stopped item metadata without performing process IO.
    ///
    /// # Errors
    ///
    /// Forwards typed item lookup, status, and revision errors.
    pub fn remove_item(&mut self, id: &ItemId) -> Result<ManagedItem, CoordinatorError> {
        self.items.remove(id).map_err(CoordinatorError::Item)
    }

    /// Appends to the event sequence belonging to an existing workspace.
    ///
    /// # Errors
    ///
    /// Returns a workspace lookup or bounded spool error.
    pub fn append_event(
        &mut self,
        workspace: &WorkspaceId,
        received_at: u64,
        payload: Vec<u8>,
    ) -> Result<u64, CoordinatorError> {
        self.require_workspace(workspace)?;
        self.events
            .get_mut(workspace)
            .ok_or(CoordinatorError::WorkspaceNotFound)?
            .append(received_at, payload)
            .map_err(CoordinatorError::Event)
    }

    /// Advances one client's acknowledgement in an existing workspace spool.
    ///
    /// # Errors
    ///
    /// Returns a workspace lookup or acknowledgement validation error.
    pub fn acknowledge_event(
        &mut self,
        workspace: &WorkspaceId,
        client_id: &str,
        sequence: u64,
    ) -> Result<(), CoordinatorError> {
        self.require_workspace(workspace)?;
        self.events
            .get_mut(workspace)
            .ok_or(CoordinatorError::WorkspaceNotFound)?
            .acknowledge(client_id, sequence)
            .map_err(CoordinatorError::Event)
    }

    /// Selects replay or snapshot recovery for an existing workspace.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinatorError::WorkspaceNotFound`] for an unknown workspace.
    pub fn subscribe(
        &self,
        workspace: &WorkspaceId,
        after_sequence: u64,
    ) -> Result<Subscription, CoordinatorError> {
        self.require_workspace(workspace)?;
        Ok(self
            .events
            .get(workspace)
            .ok_or(CoordinatorError::WorkspaceNotFound)?
            .subscribe(after_sequence))
    }

    /// Returns workspace/item revisions and the event high-water mark together.
    ///
    /// # Errors
    ///
    /// Returns a workspace lookup or cross-domain identity error.
    pub fn snapshot(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<WorkspaceStateSnapshot, CoordinatorError> {
        let record = self.require_workspace(workspace)?;
        let item_workspace = item_workspace_id(workspace)?;
        let ItemSnapshot { revision, items } = self.items.snapshot(&item_workspace);
        let event_high_water = self
            .events
            .get(workspace)
            .ok_or(CoordinatorError::WorkspaceNotFound)?
            .committed_high();
        Ok(WorkspaceStateSnapshot {
            workspace_revision: self.workspaces.revision(),
            workspace: record,
            item_revision: revision,
            items,
            event_high_water,
        })
    }

    /// Returns the durable event-spool projection for one workspace.
    ///
    /// # Errors
    ///
    /// Returns [`CoordinatorError::WorkspaceNotFound`] for an unknown workspace.
    pub fn event_checkpoint(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<SpoolSnapshot, CoordinatorError> {
        self.require_workspace(workspace)?;
        Ok(self
            .events
            .get(workspace)
            .ok_or(CoordinatorError::WorkspaceNotFound)?
            .checkpoint())
    }

    fn require_workspace(&self, id: &WorkspaceId) -> Result<WorkspaceSnapshot, CoordinatorError> {
        self.workspaces
            .get(id)
            .ok_or(CoordinatorError::WorkspaceNotFound)
    }
}

fn item_workspace_id(id: &WorkspaceId) -> Result<ItemWorkspaceId, CoordinatorError> {
    ItemWorkspaceId::parse(id.as_str()).map_err(|_| CoordinatorError::ItemWorkspaceMismatch)
}

fn workspace_id(id: &ItemWorkspaceId) -> Result<WorkspaceId, CoordinatorError> {
    WorkspaceId::parse(id.as_str(), 128).map_err(|_| CoordinatorError::ItemWorkspaceMismatch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use choosh_core::item::{ItemKind, ZellijTarget};
    use choosh_core::workspace::{CanonicalRoot, SessionIntent, WorkspaceName};

    fn limits() -> CoordinatorLimits {
        CoordinatorLimits {
            workspaces: WorkspaceLimits::new(3, 16, 16, 64),
            max_items: 4,
            events: SpoolLimits {
                max_events: 3,
                max_retained_bytes: 16,
                max_payload_bytes: 8,
                max_receipt_age: 20,
                max_clients: 2,
                max_client_id_bytes: 8,
            },
        }
    }

    fn workspace(id: &str) -> WorkspaceId {
        WorkspaceId::parse(id, 16).unwrap()
    }

    fn register(id: &str) -> RegisterWorkspace {
        RegisterWorkspace {
            id: workspace(id),
            name: WorkspaceName::parse(id, 16).unwrap(),
            canonical_root: CanonicalRoot::from_adapter(&format!("/srv/{id}"), 64).unwrap(),
            session_intent: SessionIntent::Create,
        }
    }

    fn item(id: &str, workspace: &str) -> ManagedItem {
        ManagedItem::new(
            ItemId::parse(id).unwrap(),
            ItemWorkspaceId::parse(workspace).unwrap(),
            id.to_owned(),
            ItemKind::Terminal,
            ZellijTarget::new(workspace.into(), id.into(), "pane-1".into()).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn composes_revisions_and_per_workspace_event_sequences() {
        let mut state = DaemonCoordinator::new(limits()).unwrap();
        state.register_workspace(0, register("alpha")).unwrap();
        state.register_workspace(1, register("beta")).unwrap();
        state.insert_item(item("terminal", "alpha")).unwrap();
        assert_eq!(
            state
                .append_event(&workspace("alpha"), 10, vec![1])
                .unwrap(),
            1
        );
        assert_eq!(
            state.append_event(&workspace("beta"), 10, vec![2]).unwrap(),
            1
        );

        let snapshot = state.snapshot(&workspace("alpha")).unwrap();
        assert_eq!(snapshot.workspace_revision, 2);
        assert_eq!(snapshot.item_revision, 1);
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.event_high_water, 1);
    }

    #[test]
    fn rejects_unknown_workspace_items_and_events_without_mutation() {
        let mut state = DaemonCoordinator::new(limits()).unwrap();
        state.register_workspace(0, register("alpha")).unwrap();
        let before = state.snapshot(&workspace("alpha")).unwrap();

        assert_eq!(
            state.insert_item(item("orphan", "missing")),
            Err(CoordinatorError::WorkspaceNotFound)
        );
        assert_eq!(
            state.append_event(&workspace("missing"), 1, vec![1]),
            Err(CoordinatorError::WorkspaceNotFound)
        );
        assert_eq!(state.snapshot(&workspace("alpha")).unwrap(), before);
    }

    #[test]
    fn unregister_refuses_items_and_never_discards_metadata_partially() {
        let mut state = DaemonCoordinator::new(limits()).unwrap();
        state.register_workspace(0, register("alpha")).unwrap();
        state.insert_item(item("terminal", "alpha")).unwrap();
        state.append_event(&workspace("alpha"), 2, vec![7]).unwrap();
        let before = state.snapshot(&workspace("alpha")).unwrap();

        assert_eq!(
            state.unregister_workspace(1, &workspace("alpha")),
            Err(CoordinatorError::WorkspaceHasItems { count: 1 })
        );
        assert_eq!(state.snapshot(&workspace("alpha")).unwrap(), before);
    }

    #[test]
    fn stale_unregister_preserves_workspace_and_event_spool() {
        let mut state = DaemonCoordinator::new(limits()).unwrap();
        state.register_workspace(0, register("alpha")).unwrap();
        state.append_event(&workspace("alpha"), 1, vec![1]).unwrap();
        let checkpoint = state.event_checkpoint(&workspace("alpha")).unwrap();

        assert!(matches!(
            state.unregister_workspace(0, &workspace("alpha")),
            Err(CoordinatorError::Workspace(WorkspaceError::StaleRevision {
                current: 1
            }))
        ));
        assert_eq!(
            state.event_checkpoint(&workspace("alpha")).unwrap(),
            checkpoint
        );
    }

    #[test]
    fn successful_unregister_removes_registration_and_spool_only() {
        let mut state = DaemonCoordinator::new(limits()).unwrap();
        state.register_workspace(0, register("alpha")).unwrap();
        state.append_event(&workspace("alpha"), 1, vec![1]).unwrap();

        state.unregister_workspace(1, &workspace("alpha")).unwrap();
        assert_eq!(
            state.snapshot(&workspace("alpha")),
            Err(CoordinatorError::WorkspaceNotFound)
        );
        assert!(state.workspace_snapshot().workspaces.is_empty());
    }

    #[test]
    fn subscription_replay_and_ack_survive_reconnect_cursor() {
        let mut state = DaemonCoordinator::new(limits()).unwrap();
        state.register_workspace(0, register("alpha")).unwrap();
        state.append_event(&workspace("alpha"), 1, vec![1]).unwrap();
        state.append_event(&workspace("alpha"), 2, vec![2]).unwrap();

        let first = state.subscribe(&workspace("alpha"), 0).unwrap();
        assert!(
            matches!(first, Subscription::Replay { committed_high: 2, ref events, .. } if events.len() == 2)
        );
        state
            .acknowledge_event(&workspace("alpha"), "android", 2)
            .unwrap();

        // A reconnect resumes strictly after the durable cursor and does not
        // redeliver already acknowledged events.
        state.append_event(&workspace("alpha"), 3, vec![3]).unwrap();
        let resumed = state.subscribe(&workspace("alpha"), 2).unwrap();
        assert!(
            matches!(resumed, Subscription::Replay { committed_high: 3, ref events, .. } if events.iter().map(|e| e.sequence).collect::<Vec<_>>() == vec![3])
        );
        assert_eq!(
            state
                .event_checkpoint(&workspace("alpha"))
                .unwrap()
                .client_acks
                .get("android"),
            Some(&2)
        );
    }
}
