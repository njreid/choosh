//! Persistent Project/Workspace registry for this devhost, per
//! `docs/specs/host-rpc.md`'s "Registry RPCs" and
//! `docs/milestones/M1-workspace-and-jj.md`. Explicit registration only —
//! never filesystem discovery. Persisted atomically (write-then-rename,
//! same discipline as `credential.rs`) so a `choosh-hostd` restart doesn't
//! forget what's registered; the M0 `relayd` fork left its analogous
//! registry in-memory-only as an explicit, noted gap — this one doesn't
//! repeat it, since a devhost losing track of its own workspaces on
//! restart would be a real regression, not a nice-to-have.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use choosh_protocol::host_rpc::{AgentKind, ItemStatus, ItemType};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Item {
    pub item_id: String,
    pub workspace_id: String,
    pub item_type: ItemType,
    pub name: String,
    pub tab_target: String,
    pub status: ItemStatus,
    pub agent: Option<AgentKind>,
    pub port: Option<u16>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub project_id: String,
    pub name: String,
    pub primary_workspace_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workspace {
    pub workspace_id: String,
    pub workspace_name: String,
    pub project_id: String,
    pub root_path: PathBuf,
    pub created_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct RegistryState {
    projects: Vec<Project>,
    workspaces: Vec<Workspace>,
    #[serde(default)]
    items: Vec<Item>,
}

#[derive(Debug)]
pub enum RegistryError {
    Io(io::Error),
    Corrupt(String),
    WorkspaceNameTaken(String),
    ItemNameTaken(String),
    /// `project.set_primary_workspace`'s `project_id` doesn't name a
    /// registered Project.
    ProjectNotFound(String),
    /// `project.set_primary_workspace`'s `workspace_id` doesn't name a
    /// registered Workspace.
    WorkspaceNotFound(String),
    /// `project.set_primary_workspace`'s `workspace_id` names a real
    /// Workspace, but one registered under a different Project — per
    /// `host-rpc.md`, `workspace_id` "MUST already belong to `project_id`".
    /// Kept distinct from [`Self::WorkspaceNotFound`] so `rpc.rs` can pick
    /// the right typed error code for each case (see
    /// `handle_project_set_primary_workspace`'s doc comment for which).
    WorkspaceNotInProject { project_id: String, workspace_id: String },
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "registry file I/O error: {error}"),
            Self::Corrupt(reason) => write!(f, "registry file is corrupt: {reason}"),
            Self::WorkspaceNameTaken(name) => {
                write!(f, "workspace name {name:?} is already registered on this host")
            }
            Self::ItemNameTaken(name) => {
                write!(f, "item name {name:?} is already registered in this workspace")
            }
            Self::ProjectNotFound(project_id) => write!(f, "project {project_id:?} is not registered on this host"),
            Self::WorkspaceNotFound(workspace_id) => write!(f, "workspace {workspace_id:?} is not registered on this host"),
            Self::WorkspaceNotInProject { project_id, workspace_id } => {
                write!(f, "workspace {workspace_id:?} does not belong to project {project_id:?}")
            }
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<io::Error> for RegistryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// In-memory registry, loaded from and saved back to a single JSON file.
/// Not safe for concurrent mutation from multiple processes (single
/// `choosh-hostd` per devhost, per `DESIGN.md`, so this isn't a real
/// constraint) — callers within one process should hold this behind a
/// `tokio::sync::Mutex` if driven from concurrent RPC dispatch.
pub struct Registry {
    path: PathBuf,
    state: RegistryState,
    /// In-memory-only "last agent/service event seen" per `workspace_id`,
    /// used by `rpc.rs`'s `handle_project_list` for `project.list`'s
    /// `active` computation (`host-rpc.md`: "a connected agent/service Item
    /// or a recent event"). Deliberately not part of `RegistryState` / never
    /// persisted: unlike Projects/Workspaces/Items (which must survive a
    /// `choosh-hostd` restart per this module's own doc comment), "was
    /// there recent activity" is a liveness signal, not registered state —
    /// a restart already drops the live Zellij/PTY/agent processes that
    /// would have produced these events, so there is nothing meaningful to
    /// carry across one.
    last_event_at: std::collections::HashMap<String, time::OffsetDateTime>,
    /// `workspace_name`s currently reserved by an in-flight `workspace.create`
    /// that has passed its uniqueness check but not yet (successfully or
    /// unsuccessfully) reached [`Self::register_workspace`] — see
    /// [`Self::reserve_workspace_name`]'s doc comment for the race this
    /// closes. In-memory-only, same reasoning as `last_event_at`: a
    /// reservation only needs to outlive one in-flight RPC call within this
    /// process, never a `choosh-hostd` restart (which drops every in-flight
    /// call anyway).
    reserved_workspace_names: std::collections::HashSet<String>,
    /// The `item.create` analog of `reserved_workspace_names`, keyed by
    /// `(workspace_id, name)` since item names are unique per-Workspace, not
    /// host-wide (see [`Self::find_item_by_name`]).
    reserved_item_names: std::collections::HashSet<(String, String)>,
}

impl Registry {
    /// Loads the registry at `path`, or starts empty if the file doesn't
    /// exist yet (this devhost has never registered a workspace).
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Corrupt`] for a present, malformed file —
    /// deliberately distinct from "file missing", same posture
    /// `credential.rs` takes: a corrupt registry must be a hard error, not
    /// silently treated as "nothing registered yet".
    pub fn load(path: &Path) -> Result<Self, RegistryError> {
        let state = match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| RegistryError::Corrupt(format!("invalid JSON: {error}")))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => RegistryState::default(),
            Err(error) => return Err(RegistryError::Io(error)),
        };
        Ok(Self {
            path: path.to_path_buf(),
            state,
            last_event_at: std::collections::HashMap::new(),
            reserved_workspace_names: std::collections::HashSet::new(),
            reserved_item_names: std::collections::HashSet::new(),
        })
    }

    /// The default per-devhost registry file location:
    /// `<config dir>/choosh-hostd/workspaces.json`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if no config directory can be determined.
    pub fn default_path() -> Result<PathBuf, RegistryError> {
        let dirs = directories::ProjectDirs::from("ai", "choosh", "hostd").ok_or_else(|| {
            RegistryError::Io(io::Error::new(io::ErrorKind::NotFound, "no config directory for choosh-hostd"))
        })?;
        Ok(dirs.config_dir().join("workspaces.json"))
    }

    fn save(&self) -> Result<(), RegistryError> {
        let parent = self.path.parent().ok_or_else(|| {
            RegistryError::Io(io::Error::new(io::ErrorKind::InvalidInput, "registry path has no parent directory"))
        })?;
        fs::create_dir_all(parent)?;
        let tmp_path = parent.join(format!(
            ".{}.tmp-{}",
            self.path.file_name().and_then(|n| n.to_str()).unwrap_or("workspaces.json"),
            std::process::id()
        ));
        let json = serde_json::to_vec_pretty(&self.state)
            .map_err(|error| RegistryError::Io(io::Error::new(io::ErrorKind::InvalidData, error)))?;
        fs::write(&tmp_path, &json)?;
        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    #[must_use]
    pub fn find_workspace_by_name(&self, workspace_name: &str) -> Option<&Workspace> {
        self.state.workspaces.iter().find(|w| w.workspace_name == workspace_name)
    }

    #[must_use]
    pub fn find_workspace(&self, workspace_id: &str) -> Option<&Workspace> {
        self.state.workspaces.iter().find(|w| w.workspace_id == workspace_id)
    }

    #[must_use]
    pub fn list_workspaces(&self) -> &[Workspace] {
        &self.state.workspaces
    }

    #[must_use]
    pub fn find_project_by_source(&self, root_path: &Path) -> Option<&Project> {
        // A Project is identified, for M1's purposes, by its registered
        // Workspaces' root paths sharing a common repo — the simplest
        // correct check available without a richer Project-source model is
        // "does an existing Workspace's root match", which is exactly the
        // adopt-vs-fresh-clone distinction `workspace.create` needs.
        let project_id = self.state.workspaces.iter().find(|w| w.root_path == root_path)?.project_id.clone();
        self.state.projects.iter().find(|p| p.project_id == *project_id.as_str())
    }

    #[must_use]
    pub fn find_project(&self, project_id: &str) -> Option<&Project> {
        self.state.projects.iter().find(|p| p.project_id == project_id)
    }

    /// Every registered Project on this host — `project.list`'s (`rpc.rs`'s
    /// `handle_project_list`) source of rows.
    #[must_use]
    pub fn list_projects(&self) -> &[Project] {
        &self.state.projects
    }

    /// Changes `project_id`'s `primary_workspace_id` to `workspace_id`, per
    /// `host-rpc.md`'s `project.set_primary_workspace`.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::WorkspaceNotFound`] if `workspace_id` isn't
    /// registered at all, or [`RegistryError::WorkspaceNotInProject`] if it
    /// is registered but under a different Project than `project_id` — per
    /// `host-rpc.md`'s "`workspace_id` MUST already belong to
    /// `project_id`". Returns [`RegistryError::ProjectNotFound`] if
    /// `project_id` itself isn't registered (checked after the workspace
    /// lookup so a mismatched-project error is reported precisely rather
    /// than masked by an equally-plausible "no such project" one).
    pub fn set_primary_workspace(&mut self, project_id: &str, workspace_id: &str) -> Result<(), RegistryError> {
        let workspace_project_id = self
            .find_workspace(workspace_id)
            .map(|workspace| workspace.project_id.clone())
            .ok_or_else(|| RegistryError::WorkspaceNotFound(workspace_id.to_string()))?;
        if workspace_project_id != project_id {
            return Err(RegistryError::WorkspaceNotInProject {
                project_id: project_id.to_string(),
                workspace_id: workspace_id.to_string(),
            });
        }
        let project = self
            .state
            .projects
            .iter_mut()
            .find(|p| p.project_id == project_id)
            .ok_or_else(|| RegistryError::ProjectNotFound(project_id.to_string()))?;
        project.primary_workspace_id = Some(workspace_id.to_string());
        self.save()
    }

    /// Atomically reserves `workspace_name` for an in-flight `workspace.create`,
    /// per `rpc.rs`'s `handle_create`: called (under `RpcContext.registry`'s
    /// lock) immediately after the request's own validation, *before*
    /// `handle_create` does any of `workspace.create`'s expensive real work
    /// (`jj_ops::clone`/`workspace_add`, `zellij_ops::create_session`) —
    /// unlike the early check that used to live directly in `handle_create`
    /// (a plain [`Self::find_workspace_by_name`] read with no reservation),
    /// this closes the real race two concurrent `workspace.create`s for the
    /// same name used to have: both could pass a non-authoritative early
    /// check, both would then do the expensive clone/session work, and only
    /// one would win the *authoritative* check that used to run only inside
    /// [`Self::register_workspace`] at the very end — leaving the loser's
    /// clone/session orphaned on disk/in Zellij with no cleanup. Checked
    /// against both already-*registered* names (the same check
    /// [`Self::register_workspace`] itself performs) and already-*reserved*
    /// ones, so two concurrent callers can never both hold a reservation for
    /// the same name — whichever calls this second gets
    /// [`RegistryError::WorkspaceNameTaken`] immediately, before it does any
    /// real work at all, rather than after.
    ///
    /// The caller MUST release this reservation exactly once, via
    /// [`Self::release_workspace_name_reservation`], regardless of whether
    /// its subsequent [`Self::register_workspace`] call (or an earlier
    /// failure that never reaches it) succeeds — this function does not
    /// release it on the caller's behalf, since only the caller knows
    /// whether a rollback (killing an orphaned Zellij session, etc.) needs
    /// to happen first.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::WorkspaceNameTaken`] if `workspace_name` is
    /// already registered, or already reserved by another in-flight
    /// `workspace.create`.
    pub fn reserve_workspace_name(&mut self, workspace_name: &str) -> Result<(), RegistryError> {
        if self.find_workspace_by_name(workspace_name).is_some() || self.reserved_workspace_names.contains(workspace_name) {
            return Err(RegistryError::WorkspaceNameTaken(workspace_name.to_string()));
        }
        self.reserved_workspace_names.insert(workspace_name.to_string());
        Ok(())
    }

    /// Releases a reservation taken by [`Self::reserve_workspace_name`].
    /// Idempotent (removing an absent name is a no-op) — deliberately safe
    /// to call from every one of `handle_create`'s exit paths without each
    /// one having to prove it's the first to release.
    pub fn release_workspace_name_reservation(&mut self, workspace_name: &str) {
        self.reserved_workspace_names.remove(workspace_name);
    }

    /// Registers a new Workspace under `project_id` (creating the Project
    /// record if it doesn't exist yet, with itself as the primary
    /// Workspace, per `DESIGN.md` §4's "defaults to the first Workspace
    /// registered for it").
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::WorkspaceNameTaken`] if `workspace_name` is
    /// already registered on this host, per `host-rpc.md`'s "reject
    /// collisions... rather than silently adopting it". In the normal
    /// `handle_create` flow this is unreachable — the caller already holds
    /// an exclusive [`Self::reserve_workspace_name`] reservation for
    /// `workspace_name` for this call's entire duration — but is kept as a
    /// real, checked invariant rather than an `assert!`/`unwrap`, in case a
    /// future caller ever invokes this directly without reserving first.
    pub fn register_workspace(
        &mut self,
        workspace_id: String,
        workspace_name: String,
        project_id: String,
        project_name: String,
        root_path: PathBuf,
        created_at: String,
    ) -> Result<(), RegistryError> {
        if self.find_workspace_by_name(&workspace_name).is_some() {
            return Err(RegistryError::WorkspaceNameTaken(workspace_name));
        }
        let is_new_project = self.find_project(&project_id).is_none();
        if is_new_project {
            self.state.projects.push(Project {
                project_id: project_id.clone(),
                name: project_name,
                primary_workspace_id: Some(workspace_id.clone()),
            });
        }
        self.state.workspaces.push(Workspace {
            workspace_id,
            workspace_name,
            project_id,
            root_path,
            created_at,
        });
        self.save()
    }

    #[must_use]
    pub fn find_item(&self, item_id: &str) -> Option<&Item> {
        self.state.items.iter().find(|i| i.item_id == item_id)
    }

    #[must_use]
    pub fn find_item_by_name(&self, workspace_id: &str, name: &str) -> Option<&Item> {
        self.state.items.iter().find(|i| i.workspace_id == workspace_id && i.name == name)
    }

    #[must_use]
    pub fn list_items(&self, workspace_id: &str) -> Vec<&Item> {
        self.state.items.iter().filter(|i| i.workspace_id == workspace_id).collect()
    }

    /// The `item.create` analog of [`Self::reserve_workspace_name`] — see
    /// that function's doc comment for the full race it closes and the
    /// reservation-release contract, which applies here identically. Keyed
    /// by `(workspace_id, name)` since item names are only unique
    /// per-Workspace ([`Self::find_item_by_name`]), not host-wide.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::ItemNameTaken`] if `name` is already
    /// registered within `workspace_id`, or already reserved by another
    /// in-flight `item.create` in the same workspace.
    pub fn reserve_item_name(&mut self, workspace_id: &str, name: &str) -> Result<(), RegistryError> {
        if self.find_item_by_name(workspace_id, name).is_some()
            || self.reserved_item_names.contains(&(workspace_id.to_string(), name.to_string()))
        {
            return Err(RegistryError::ItemNameTaken(name.to_string()));
        }
        self.reserved_item_names.insert((workspace_id.to_string(), name.to_string()));
        Ok(())
    }

    /// Releases a reservation taken by [`Self::reserve_item_name`].
    /// Idempotent, same reasoning as [`Self::release_workspace_name_reservation`].
    pub fn release_item_name_reservation(&mut self, workspace_id: &str, name: &str) {
        self.reserved_item_names.remove(&(workspace_id.to_string(), name.to_string()));
    }

    /// # Errors
    ///
    /// Returns [`RegistryError::ItemNameTaken`] if `name` is already
    /// registered within `workspace_id`, per `host-rpc.md`'s "Unique within
    /// the workspace" requirement for `item.create`'s `name` field. In the
    /// normal `handle_item_create` flow this is unreachable for the same
    /// reason documented on [`Self::register_workspace`]'s matching branch —
    /// the caller already holds an exclusive [`Self::reserve_item_name`]
    /// reservation for `(workspace_id, name)`.
    ///
    /// `initial_status` is caller-supplied rather than hardcoded: per
    /// `service-tunnels.md`'s Lifecycle section, a `WebService` item starts
    /// `starting` (its readiness prober, `crate::readiness`, drives it to
    /// `running`/`failed`/`unknown` from there), while `AgentTerminal`/
    /// `Shell` items start `running` immediately — their Zellij tab already
    /// exists synchronously by the time this call happens, so there's no
    /// real `starting` window for them to be in.
    #[allow(clippy::too_many_arguments)]
    pub fn register_item(
        &mut self,
        item_id: String,
        workspace_id: String,
        item_type: ItemType,
        name: String,
        tab_target: String,
        agent: Option<AgentKind>,
        port: Option<u16>,
        initial_status: ItemStatus,
    ) -> Result<(), RegistryError> {
        if self.find_item_by_name(&workspace_id, &name).is_some() {
            return Err(RegistryError::ItemNameTaken(name));
        }
        self.state.items.push(Item {
            item_id,
            workspace_id,
            item_type,
            name,
            tab_target,
            status: initial_status,
            agent,
            port,
        });
        self.save()
    }

    /// Marks an item `stopped` in place; the record and its Zellij tab's
    /// scrollback are retained, per `host-rpc.md`'s `item.stop`.
    ///
    /// # Errors
    ///
    /// See [`Self::set_item_status`].
    pub fn mark_item_stopped(&mut self, item_id: &str) -> Result<(), RegistryError> {
        self.set_item_status(item_id, ItemStatus::Stopped)
    }

    /// Sets `item_id`'s status directly — the general-purpose sibling of
    /// [`Self::mark_item_stopped`], used by the `WebService` readiness
    /// prober (`crate::readiness`) to drive `starting` -> `running`/
    /// `failed`/`unknown` transitions per `service-tunnels.md`'s Lifecycle
    /// section.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError::Corrupt`] (repurposed as "not found" here —
    /// there's no dedicated variant and adding one for two call sites isn't
    /// worth it) if `item_id` isn't registered.
    pub fn set_item_status(&mut self, item_id: &str, status: ItemStatus) -> Result<(), RegistryError> {
        let item = self
            .state
            .items
            .iter_mut()
            .find(|i| i.item_id == item_id)
            .ok_or_else(|| RegistryError::Corrupt(format!("item {item_id:?} not found")))?;
        item.status = status;
        self.save()
    }

    /// Records that a `WireAgentEvent` was just seen for `workspace_id`, at
    /// `at` — the "recent event" half of `project.list`'s `active`
    /// computation (`host-rpc.md`). In-memory only, per this field's own
    /// doc comment; never fails and never calls [`Self::save`].
    pub fn record_event(&mut self, workspace_id: &str, at: time::OffsetDateTime) {
        self.last_event_at.insert(workspace_id.to_string(), at);
    }

    /// Whether `workspace_id` has had a recorded event within `window` of
    /// `now`. `now` is caller-supplied (rather than read internally via
    /// `OffsetDateTime::now_utc()`) so `rpc.rs`'s tests can assert both the
    /// "still recent" and "aged out" boundary deterministically instead of
    /// racing a real clock.
    #[must_use]
    pub fn has_recent_event(&self, workspace_id: &str, now: time::OffsetDateTime, window: time::Duration) -> bool {
        self.last_event_at.get(workspace_id).is_some_and(|at| now - *at <= window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_registry() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.json");
        (dir, path)
    }

    #[test]
    fn missing_file_loads_empty() {
        let (_dir, path) = temp_registry();
        let registry = Registry::load(&path).unwrap();
        assert!(registry.list_workspaces().is_empty());
    }

    #[test]
    fn register_then_reload_round_trips() {
        let (_dir, path) = temp_registry();
        let mut registry = Registry::load(&path).unwrap();
        registry
            .register_workspace(
                "ws-1".to_string(),
                "app".to_string(),
                "proj-1".to_string(),
                "app".to_string(),
                PathBuf::from("/workspaces/app"),
                "2026-08-14T00:00:00Z".to_string(),
            )
            .unwrap();

        let reloaded = Registry::load(&path).unwrap();
        assert_eq!(reloaded.list_workspaces().len(), 1);
        assert_eq!(reloaded.find_workspace("ws-1").unwrap().workspace_name, "app");
        let project = reloaded.find_project("proj-1").unwrap();
        assert_eq!(project.primary_workspace_id.as_deref(), Some("ws-1"));
    }

    #[test]
    fn duplicate_workspace_name_is_rejected() {
        let (_dir, path) = temp_registry();
        let mut registry = Registry::load(&path).unwrap();
        registry
            .register_workspace(
                "ws-1".to_string(),
                "app".to_string(),
                "proj-1".to_string(),
                "app".to_string(),
                PathBuf::from("/workspaces/app"),
                "2026-08-14T00:00:00Z".to_string(),
            )
            .unwrap();
        let result = registry.register_workspace(
            "ws-2".to_string(),
            "app".to_string(),
            "proj-2".to_string(),
            "app-2".to_string(),
            PathBuf::from("/workspaces/app-2"),
            "2026-08-14T00:00:01Z".to_string(),
        );
        assert!(matches!(result, Err(RegistryError::WorkspaceNameTaken(name)) if name == "app"));
    }

    #[test]
    fn corrupt_file_is_a_hard_error() {
        let (_dir, path) = temp_registry();
        fs::write(&path, b"not json").unwrap();
        assert!(matches!(Registry::load(&path), Err(RegistryError::Corrupt(_))));
    }

    #[test]
    fn second_workspace_in_same_project_does_not_change_primary() {
        let (_dir, path) = temp_registry();
        let mut registry = Registry::load(&path).unwrap();
        registry
            .register_workspace(
                "ws-1".to_string(),
                "app".to_string(),
                "proj-1".to_string(),
                "app".to_string(),
                PathBuf::from("/workspaces/app"),
                "2026-08-14T00:00:00Z".to_string(),
            )
            .unwrap();
        registry
            .register_workspace(
                "ws-2".to_string(),
                "app-agent-b".to_string(),
                "proj-1".to_string(),
                "app".to_string(),
                PathBuf::from("/workspaces/app-agent-b"),
                "2026-08-14T00:00:01Z".to_string(),
            )
            .unwrap();
        assert_eq!(registry.find_project("proj-1").unwrap().primary_workspace_id.as_deref(), Some("ws-1"));
        assert_eq!(registry.list_workspaces().len(), 2);
    }

    #[test]
    fn set_primary_workspace_succeeds_and_is_reflected_immediately() {
        let (_dir, path) = temp_registry();
        let mut registry = Registry::load(&path).unwrap();
        registry
            .register_workspace(
                "ws-1".to_string(),
                "app".to_string(),
                "proj-1".to_string(),
                "app".to_string(),
                PathBuf::from("/workspaces/app"),
                "2026-08-14T00:00:00Z".to_string(),
            )
            .unwrap();
        registry
            .register_workspace(
                "ws-2".to_string(),
                "app-agent-b".to_string(),
                "proj-1".to_string(),
                "app".to_string(),
                PathBuf::from("/workspaces/app-agent-b"),
                "2026-08-14T00:00:01Z".to_string(),
            )
            .unwrap();
        assert_eq!(registry.find_project("proj-1").unwrap().primary_workspace_id.as_deref(), Some("ws-1"));

        registry.set_primary_workspace("proj-1", "ws-2").unwrap();
        assert_eq!(registry.find_project("proj-1").unwrap().primary_workspace_id.as_deref(), Some("ws-2"));

        // Persists, same as every other registry mutation.
        let reloaded = Registry::load(&path).unwrap();
        assert_eq!(reloaded.find_project("proj-1").unwrap().primary_workspace_id.as_deref(), Some("ws-2"));
    }

    #[test]
    fn set_primary_workspace_rejects_a_workspace_from_a_different_project() {
        let (_dir, path) = temp_registry();
        let mut registry = Registry::load(&path).unwrap();
        registry
            .register_workspace(
                "ws-1".to_string(),
                "app".to_string(),
                "proj-1".to_string(),
                "app".to_string(),
                PathBuf::from("/workspaces/app"),
                "2026-08-14T00:00:00Z".to_string(),
            )
            .unwrap();
        registry
            .register_workspace(
                "ws-2".to_string(),
                "other".to_string(),
                "proj-2".to_string(),
                "other".to_string(),
                PathBuf::from("/workspaces/other"),
                "2026-08-14T00:00:01Z".to_string(),
            )
            .unwrap();

        let result = registry.set_primary_workspace("proj-1", "ws-2");
        assert!(
            matches!(&result, Err(RegistryError::WorkspaceNotInProject { project_id, workspace_id })
                if project_id == "proj-1" && workspace_id == "ws-2"),
            "expected WorkspaceNotInProject, got {result:?}"
        );
        // Rejected, not silently applied.
        assert_eq!(registry.find_project("proj-1").unwrap().primary_workspace_id.as_deref(), Some("ws-1"));
    }

    #[test]
    fn set_primary_workspace_rejects_an_unknown_workspace_id() {
        let (_dir, path) = temp_registry();
        let mut registry = Registry::load(&path).unwrap();
        registry
            .register_workspace(
                "ws-1".to_string(),
                "app".to_string(),
                "proj-1".to_string(),
                "app".to_string(),
                PathBuf::from("/workspaces/app"),
                "2026-08-14T00:00:00Z".to_string(),
            )
            .unwrap();
        let result = registry.set_primary_workspace("proj-1", "ws-does-not-exist");
        assert!(matches!(result, Err(RegistryError::WorkspaceNotFound(id)) if id == "ws-does-not-exist"));
    }

    /// The core regression test for the `workspace.create` race: while a
    /// name is reserved, a second, concurrent reservation attempt for the
    /// *same* name must fail immediately — proving that two racing
    /// `handle_create` calls can never both proceed to do the expensive
    /// clone/session work this reservation exists to gate. Releasing the
    /// first reservation then frees the name back up, exactly as
    /// `handle_create`'s cleanup path relies on.
    #[test]
    fn reserve_workspace_name_is_exclusive_and_release_frees_it_for_reuse() {
        let (_dir, path) = temp_registry();
        let mut registry = Registry::load(&path).unwrap();

        registry.reserve_workspace_name("app").unwrap();
        let second = registry.reserve_workspace_name("app");
        assert!(
            matches!(&second, Err(RegistryError::WorkspaceNameTaken(name)) if name == "app"),
            "a name already held under reservation must be rejected, got {second:?}"
        );

        registry.release_workspace_name_reservation("app");
        assert!(registry.reserve_workspace_name("app").is_ok(), "releasing a reservation must free the name for reuse");
    }

    /// A reservation must also be rejected against a name that's already
    /// *registered* (not merely reserved) — the reservation step subsumes
    /// the old early, non-authoritative uniqueness check entirely, rather
    /// than only guarding the reserved-but-not-yet-registered window.
    #[test]
    fn reserve_workspace_name_rejects_an_already_registered_name() {
        let (_dir, path) = temp_registry();
        let mut registry = Registry::load(&path).unwrap();
        registry
            .register_workspace(
                "ws-1".to_string(),
                "app".to_string(),
                "proj-1".to_string(),
                "app".to_string(),
                PathBuf::from("/workspaces/app"),
                "2026-08-14T00:00:00Z".to_string(),
            )
            .unwrap();

        let result = registry.reserve_workspace_name("app");
        assert!(matches!(result, Err(RegistryError::WorkspaceNameTaken(name)) if name == "app"));
    }

    /// Releasing a reservation that was never held (e.g. a name that was
    /// never reserved, or was already released) is a safe no-op —
    /// `handle_create`'s exit paths all call this unconditionally without
    /// tracking whether some earlier path already did.
    #[test]
    fn release_workspace_name_reservation_is_idempotent() {
        let (_dir, path) = temp_registry();
        let mut registry = Registry::load(&path).unwrap();
        registry.release_workspace_name_reservation("never-reserved");
        registry.reserve_workspace_name("app").unwrap();
        registry.release_workspace_name_reservation("app");
        registry.release_workspace_name_reservation("app");
        assert!(registry.reserve_workspace_name("app").is_ok());
    }

    /// The `item.create` analog of `reserve_workspace_name_is_exclusive_...`:
    /// item-name reservations are exclusive per `(workspace_id, name)`, but
    /// the *same* name is independently reservable in a different
    /// workspace — matching `find_item_by_name`'s own per-workspace
    /// uniqueness scope.
    #[test]
    fn reserve_item_name_is_exclusive_per_workspace_and_release_frees_it_for_reuse() {
        let (_dir, path) = temp_registry();
        let mut registry = Registry::load(&path).unwrap();

        registry.reserve_item_name("ws-1", "sh").unwrap();
        let second = registry.reserve_item_name("ws-1", "sh");
        assert!(
            matches!(&second, Err(RegistryError::ItemNameTaken(name)) if name == "sh"),
            "an item name already held under reservation in the same workspace must be rejected, got {second:?}"
        );

        // A different workspace reserving the identical name is unaffected.
        assert!(registry.reserve_item_name("ws-2", "sh").is_ok());

        registry.release_item_name_reservation("ws-1", "sh");
        assert!(registry.reserve_item_name("ws-1", "sh").is_ok(), "releasing a reservation must free the name for reuse");
    }

    #[test]
    fn reserve_item_name_rejects_an_already_registered_name() {
        let (_dir, path) = temp_registry();
        let mut registry = Registry::load(&path).unwrap();
        registry
            .register_item(
                "item-1".to_string(),
                "ws-1".to_string(),
                ItemType::Shell,
                "sh".to_string(),
                "sh".to_string(),
                None,
                None,
                ItemStatus::Running,
            )
            .unwrap();

        let result = registry.reserve_item_name("ws-1", "sh");
        assert!(matches!(result, Err(RegistryError::ItemNameTaken(name)) if name == "sh"));
    }

    #[test]
    fn recent_event_is_recognized_within_window_and_not_after() {
        let (_dir, path) = temp_registry();
        let mut registry = Registry::load(&path).unwrap();
        let t0 = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        registry.record_event("ws-1", t0);

        assert!(registry.has_recent_event("ws-1", t0 + time::Duration::minutes(1), time::Duration::minutes(5)));
        assert!(!registry.has_recent_event("ws-1", t0 + time::Duration::minutes(10), time::Duration::minutes(5)));
        assert!(!registry.has_recent_event("ws-unknown", t0, time::Duration::minutes(5)));
    }
}
