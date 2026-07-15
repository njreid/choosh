//! Pure, deterministic workspace registration model.
//!
//! The host adapter is responsible for canonicalizing and checking roots, and
//! for creating or adopting Zellij sessions transactionally. This model does
//! no discovery, filesystem I/O, or session control.

use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt;

pub type RegistryRevision = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceLimits {
    pub max_workspaces: usize,
    pub max_id_bytes: usize,
    pub max_name_bytes: usize,
    pub max_root_bytes: usize,
}

impl WorkspaceLimits {
    #[must_use]
    pub const fn new(
        max_workspaces: usize,
        max_id_bytes: usize,
        max_name_bytes: usize,
        max_root_bytes: usize,
    ) -> Self {
        Self {
            max_workspaces,
            max_id_bytes,
            max_name_bytes,
            max_root_bytes,
        }
    }
}

impl Default for WorkspaceLimits {
    fn default() -> Self {
        Self::new(1_024, 128, 64, 4_096)
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    /// Parses a bounded, protocol-safe stable workspace identifier.
    ///
    /// # Errors
    ///
    /// Returns a stable validation error when the bound is zero, the input is
    /// empty or too long, or it contains characters outside the supported
    /// ASCII alphanumeric, hyphen, and underscore subset.
    pub fn parse(input: &str, max_bytes: usize) -> Result<Self, WorkspaceError> {
        if max_bytes == 0 {
            return Err(WorkspaceError::InvalidLimits);
        }
        if input.is_empty() {
            return Err(WorkspaceError::InvalidId);
        }
        if input.len() > max_bytes {
            return Err(WorkspaceError::IdTooLong);
        }
        if !input
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(WorkspaceError::InvalidId);
        }
        Ok(Self(input.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A deliberately conservative, shell-inert subset for Zellij session names.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceName(String);

impl WorkspaceName {
    /// Parses a bounded name from the supported Zellij session-name subset.
    ///
    /// # Errors
    ///
    /// Returns a stable validation error when the bound is zero, the name is
    /// empty or too long, does not start with an ASCII alphanumeric character,
    /// or contains characters outside the supported subset.
    pub fn parse(input: &str, max_bytes: usize) -> Result<Self, WorkspaceError> {
        if max_bytes == 0 {
            return Err(WorkspaceError::InvalidLimits);
        }
        if input.is_empty() {
            return Err(WorkspaceError::InvalidName);
        }
        if input.len() > max_bytes {
            return Err(WorkspaceError::NameTooLong);
        }
        let mut bytes = input.bytes();
        let Some(first) = bytes.next() else {
            return Err(WorkspaceError::InvalidName);
        };
        if !first.is_ascii_alphanumeric()
            || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(WorkspaceError::InvalidName);
        }
        Ok(Self(input.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkspaceName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An absolute root that an outer adapter has already canonicalized.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CanonicalRoot(String);

impl CanonicalRoot {
    /// Accepts an absolute root that an outer adapter has already canonicalized.
    ///
    /// # Errors
    ///
    /// Returns a stable validation error when the bound is zero or exceeded,
    /// or when the input is not an absolute, normalized, control-free path.
    pub fn from_adapter(input: &str, max_bytes: usize) -> Result<Self, WorkspaceError> {
        if max_bytes == 0 {
            return Err(WorkspaceError::InvalidLimits);
        }
        if input.len() > max_bytes {
            return Err(WorkspaceError::RootTooLong);
        }
        if !input.starts_with('/') || input.chars().any(char::is_control) {
            return Err(WorkspaceError::InvalidCanonicalRoot);
        }
        if input != "/"
            && (input.ends_with('/')
                || input
                    .split('/')
                    .skip(1)
                    .any(|part| part.is_empty() || part == "." || part == ".."))
        {
            return Err(WorkspaceError::InvalidCanonicalRoot);
        }
        Ok(Self(input.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CanonicalRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionIntent {
    Create,
    Adopt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterWorkspace {
    pub id: WorkspaceId,
    pub name: WorkspaceName,
    pub canonical_root: CanonicalRoot,
    pub session_intent: SessionIntent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSnapshot {
    pub id: WorkspaceId,
    pub name: WorkspaceName,
    pub canonical_root: CanonicalRoot,
    pub session_intent: SessionIntent,
    pub registered_revision: RegistryRevision,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrySnapshot {
    pub revision: RegistryRevision,
    /// Deterministically ordered by stable workspace ID.
    pub workspaces: Vec<WorkspaceSnapshot>,
}

#[derive(Debug)]
pub struct WorkspaceRegistry {
    limits: WorkspaceLimits,
    revision: RegistryRevision,
    by_id: BTreeMap<WorkspaceId, WorkspaceSnapshot>,
    ids_by_name: HashMap<WorkspaceName, WorkspaceId>,
    ids_by_root: HashMap<CanonicalRoot, WorkspaceId>,
}

impl WorkspaceRegistry {
    /// Creates an empty bounded registry.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError::InvalidLimits`] when any configured bound is
    /// zero.
    pub fn new(limits: WorkspaceLimits) -> Result<Self, WorkspaceError> {
        if limits.max_workspaces == 0
            || limits.max_id_bytes == 0
            || limits.max_name_bytes == 0
            || limits.max_root_bytes == 0
        {
            return Err(WorkspaceError::InvalidLimits);
        }
        Ok(Self {
            limits,
            revision: 0,
            by_id: BTreeMap::new(),
            ids_by_name: HashMap::new(),
            ids_by_root: HashMap::new(),
        })
    }

    #[must_use]
    pub const fn revision(&self) -> RegistryRevision {
        self.revision
    }

    #[must_use]
    pub fn snapshot(&self) -> RegistrySnapshot {
        RegistrySnapshot {
            revision: self.revision,
            workspaces: self.by_id.values().cloned().collect(),
        }
    }

    #[must_use]
    pub fn get(&self, id: &WorkspaceId) -> Option<WorkspaceSnapshot> {
        self.by_id.get(id).cloned()
    }

    /// Registers an explicitly supplied workspace without filesystem discovery.
    ///
    /// # Errors
    ///
    /// Returns a stable error for a stale revision, invalid or over-limit
    /// identity, duplicate ID, name, or canonical root, a full registry, or
    /// revision overflow. Errors leave the registry unchanged.
    pub fn register(
        &mut self,
        expected_revision: RegistryRevision,
        request: RegisterWorkspace,
    ) -> Result<WorkspaceSnapshot, WorkspaceError> {
        self.require_revision(expected_revision)?;
        self.revalidate(&request)?;
        if self.by_id.contains_key(&request.id) {
            return Err(WorkspaceError::IdAlreadyRegistered);
        }
        if self.ids_by_name.contains_key(&request.name) {
            return Err(WorkspaceError::NameAlreadyRegistered);
        }
        if self.ids_by_root.contains_key(&request.canonical_root) {
            return Err(WorkspaceError::RootAlreadyRegistered);
        }
        if self.by_id.len() >= self.limits.max_workspaces {
            return Err(WorkspaceError::RegistryFull);
        }
        let revision = self.next_revision()?;
        let snapshot = WorkspaceSnapshot {
            id: request.id,
            name: request.name,
            canonical_root: request.canonical_root,
            session_intent: request.session_intent,
            registered_revision: revision,
        };
        self.ids_by_name
            .insert(snapshot.name.clone(), snapshot.id.clone());
        self.ids_by_root
            .insert(snapshot.canonical_root.clone(), snapshot.id.clone());
        self.by_id.insert(snapshot.id.clone(), snapshot.clone());
        self.revision = revision;
        Ok(snapshot)
    }

    /// Removes registry metadata only. It deliberately has no session action.
    ///
    /// # Errors
    ///
    /// Returns a stable error for a stale revision, an unknown workspace ID,
    /// or revision overflow. Errors leave the registry unchanged.
    pub fn unregister(
        &mut self,
        expected_revision: RegistryRevision,
        id: &WorkspaceId,
    ) -> Result<WorkspaceSnapshot, WorkspaceError> {
        self.require_revision(expected_revision)?;
        let removed = self
            .by_id
            .get(id)
            .cloned()
            .ok_or(WorkspaceError::WorkspaceNotFound)?;
        let revision = self.next_revision()?;
        self.by_id.remove(id);
        self.ids_by_name.remove(&removed.name);
        self.ids_by_root.remove(&removed.canonical_root);
        self.revision = revision;
        Ok(removed)
    }

    fn require_revision(&self, expected: RegistryRevision) -> Result<(), WorkspaceError> {
        if expected == self.revision {
            Ok(())
        } else {
            Err(WorkspaceError::StaleRevision {
                current: self.revision,
            })
        }
    }

    fn next_revision(&self) -> Result<RegistryRevision, WorkspaceError> {
        self.revision
            .checked_add(1)
            .ok_or(WorkspaceError::RevisionOverflow)
    }

    fn revalidate(&self, request: &RegisterWorkspace) -> Result<(), WorkspaceError> {
        WorkspaceId::parse(request.id.as_str(), self.limits.max_id_bytes)?;
        WorkspaceName::parse(request.name.as_str(), self.limits.max_name_bytes)?;
        CanonicalRoot::from_adapter(request.canonical_root.as_str(), self.limits.max_root_bytes)?;
        Ok(())
    }
}

impl Default for WorkspaceRegistry {
    fn default() -> Self {
        Self::new(WorkspaceLimits::default()).expect("default workspace limits are valid")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceError {
    InvalidLimits,
    InvalidId,
    IdTooLong,
    InvalidName,
    NameTooLong,
    InvalidCanonicalRoot,
    RootTooLong,
    RegistryFull,
    IdAlreadyRegistered,
    NameAlreadyRegistered,
    RootAlreadyRegistered,
    WorkspaceNotFound,
    StaleRevision { current: RegistryRevision },
    RevisionOverflow,
}

impl WorkspaceError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::InvalidId => "invalid_workspace_id",
            Self::IdTooLong => "workspace_id_too_long",
            Self::InvalidName => "invalid_workspace_name",
            Self::NameTooLong => "workspace_name_too_long",
            Self::InvalidCanonicalRoot => "invalid_canonical_root",
            Self::RootTooLong => "canonical_root_too_long",
            Self::RegistryFull => "workspace_registry_full",
            Self::IdAlreadyRegistered => "workspace_id_already_registered",
            Self::NameAlreadyRegistered => "workspace_name_already_registered",
            Self::RootAlreadyRegistered => "workspace_root_already_registered",
            Self::WorkspaceNotFound => "workspace_not_found",
            Self::StaleRevision { .. } => "stale_workspace_revision",
            Self::RevisionOverflow => "workspace_revision_overflow",
        }
    }
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

impl Error for WorkspaceError {}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: WorkspaceLimits = WorkspaceLimits::new(2, 16, 12, 64);

    fn request(id: &str, name: &str, root: &str, intent: SessionIntent) -> RegisterWorkspace {
        RegisterWorkspace {
            id: WorkspaceId::parse(id, LIMITS.max_id_bytes).unwrap(),
            name: WorkspaceName::parse(name, LIMITS.max_name_bytes).unwrap(),
            canonical_root: CanonicalRoot::from_adapter(root, LIMITS.max_root_bytes).unwrap(),
            session_intent: intent,
        }
    }

    #[test]
    fn records_explicit_create_and_adopt_intent_with_monotonic_revisions() {
        let mut registry = WorkspaceRegistry::new(LIMITS).unwrap();
        let created = registry
            .register(
                0,
                request("w-1", "alpha", "/srv/alpha", SessionIntent::Create),
            )
            .unwrap();
        let adopted = registry
            .register(1, request("w-2", "beta", "/srv/beta", SessionIntent::Adopt))
            .unwrap();
        assert_eq!(created.registered_revision, 1);
        assert_eq!(adopted.registered_revision, 2);
        assert_eq!(registry.revision(), 2);
        assert_eq!(registry.snapshot().workspaces, [created, adopted]);
    }

    #[test]
    fn snapshot_is_immutable_and_sorted_by_stable_id() {
        let mut registry = WorkspaceRegistry::new(LIMITS).unwrap();
        let old = registry.snapshot();
        registry
            .register(0, request("z", "zed", "/z", SessionIntent::Create))
            .unwrap();
        registry
            .register(1, request("a", "aye", "/a", SessionIntent::Create))
            .unwrap();
        assert!(old.workspaces.is_empty());
        let current = registry.snapshot();
        assert_eq!(current.revision, 2);
        assert_eq!(current.workspaces[0].id.as_str(), "a");
        assert_eq!(current.workspaces[1].id.as_str(), "z");
    }

    #[test]
    fn enforces_unique_id_name_and_canonical_root_without_mutation() {
        let mut registry = WorkspaceRegistry::new(LIMITS).unwrap();
        registry
            .register(0, request("one", "alpha", "/a", SessionIntent::Create))
            .unwrap();
        let cases = [
            (
                request("one", "beta", "/b", SessionIntent::Create),
                WorkspaceError::IdAlreadyRegistered,
            ),
            (
                request("two", "alpha", "/b", SessionIntent::Create),
                WorkspaceError::NameAlreadyRegistered,
            ),
            (
                request("two", "beta", "/a", SessionIntent::Create),
                WorkspaceError::RootAlreadyRegistered,
            ),
        ];
        for (candidate, expected) in cases {
            assert_eq!(registry.register(1, candidate), Err(expected));
            assert_eq!(registry.revision(), 1);
            assert_eq!(registry.snapshot().workspaces.len(), 1);
        }
    }

    #[test]
    fn bounds_registry_and_releases_uniqueness_keys_on_unregister() {
        let limits = WorkspaceLimits::new(1, 16, 12, 64);
        let mut registry = WorkspaceRegistry::new(limits).unwrap();
        let first = request("one", "alpha", "/a", SessionIntent::Create);
        registry.register(0, first.clone()).unwrap();
        assert_eq!(
            registry.register(1, request("two", "beta", "/b", SessionIntent::Create)),
            Err(WorkspaceError::RegistryFull)
        );
        let removed = registry.unregister(1, &first.id).unwrap();
        assert_eq!(removed.name.as_str(), "alpha");
        assert!(registry.snapshot().workspaces.is_empty());
        registry.register(2, first).unwrap();
    }

    #[test]
    fn stale_and_missing_mutations_are_non_destructive() {
        let mut registry = WorkspaceRegistry::new(LIMITS).unwrap();
        let workspace = registry
            .register(0, request("one", "alpha", "/a", SessionIntent::Create))
            .unwrap();
        assert_eq!(
            registry.unregister(0, &workspace.id),
            Err(WorkspaceError::StaleRevision { current: 1 })
        );
        let missing = WorkspaceId::parse("missing", LIMITS.max_id_bytes).unwrap();
        assert_eq!(
            registry.unregister(1, &missing),
            Err(WorkspaceError::WorkspaceNotFound)
        );
        assert_eq!(registry.revision(), 1);
        assert_eq!(registry.get(&workspace.id), Some(workspace));
    }

    #[test]
    fn validates_safe_session_names_and_stable_ids() {
        for valid in ["a", "Build-7", "team_one"] {
            assert_eq!(WorkspaceName::parse(valid, 12).unwrap().as_str(), valid);
        }
        for invalid in ["", "-leading", "has space", "a/b", "é", "a.b", "x\n"] {
            assert_eq!(
                WorkspaceName::parse(invalid, 32),
                Err(WorkspaceError::InvalidName)
            );
        }
        for invalid in ["", "id space", "../id", "ümlaut"] {
            assert_eq!(
                WorkspaceId::parse(invalid, 32),
                Err(WorkspaceError::InvalidId)
            );
        }
        assert_eq!(
            WorkspaceName::parse("toolong", 3),
            Err(WorkspaceError::NameTooLong)
        );
        assert_eq!(
            WorkspaceId::parse("toolong", 3),
            Err(WorkspaceError::IdTooLong)
        );
    }

    #[test]
    fn accepts_only_bounded_absolute_canonical_root_input() {
        assert_eq!(CanonicalRoot::from_adapter("/", 64).unwrap().as_str(), "/");
        assert_eq!(
            CanonicalRoot::from_adapter("/srv/設計", 64)
                .unwrap()
                .as_str(),
            "/srv/設計"
        );
        for invalid in ["", "relative", "/a/", "/a//b", "/a/./b", "/a/../b", "/a\nb"] {
            assert_eq!(
                CanonicalRoot::from_adapter(invalid, 64),
                Err(WorkspaceError::InvalidCanonicalRoot)
            );
        }
        assert_eq!(
            CanonicalRoot::from_adapter("/long", 3),
            Err(WorkspaceError::RootTooLong)
        );
    }

    #[test]
    fn error_codes_are_stable_and_do_not_embed_host_input() {
        assert_eq!(
            WorkspaceError::RootAlreadyRegistered.code(),
            "workspace_root_already_registered"
        );
        assert_eq!(
            WorkspaceError::StaleRevision { current: 99 }.to_string(),
            "stale_workspace_revision"
        );
    }

    #[test]
    fn overflow_and_forged_over_limit_values_fail_before_mutation() {
        let mut registry = WorkspaceRegistry::new(LIMITS).unwrap();
        let forged = RegisterWorkspace {
            id: WorkspaceId("far-too-long-for-limit".to_owned()),
            name: WorkspaceName("valid".to_owned()),
            canonical_root: CanonicalRoot("/valid".to_owned()),
            session_intent: SessionIntent::Create,
        };
        assert_eq!(registry.register(0, forged), Err(WorkspaceError::IdTooLong));
        assert_eq!(registry.revision(), 0);

        registry.revision = RegistryRevision::MAX;
        assert_eq!(
            registry.register(
                RegistryRevision::MAX,
                request("one", "alpha", "/a", SessionIntent::Create)
            ),
            Err(WorkspaceError::RevisionOverflow)
        );
        assert!(registry.snapshot().workspaces.is_empty());
    }
}
