//! Deterministic explorer projection over bounded, canonical identities.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Section {
    Agents,
    Services,
    ChangedFiles,
    ProjectTree,
}
pub const SECTION_ORDER: [Section; 4] = [
    Section::Agents,
    Section::Services,
    Section::ChangedFiles,
    Section::ProjectTree,
];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ExplorerId {
    Agent(String),
    Service(String),
    ChangedPath(String),
    TreePath(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerEntry {
    pub id: ExplorerId,
    pub available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerSection {
    pub section: Section,
    pub entries: Vec<ExplorerEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerProjection {
    pub sections: Vec<ExplorerSection>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportedTarget {
    pub id: String,
    pub available: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportedChange {
    pub path: String,
    pub status: GitStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExplorerError {
    CapacityExceeded,
    InvalidIdentity,
    RootEscape,
    ConflictingStatus,
}

/// Builds fixed-order sections from shuffled, untrusted observations.
///
/// Canonical identities, rather than list positions, determine deduplication and sorting. Duplicate
/// changes must agree on immutable status. Missing retained targets may be supplied explicitly with
/// `available: false`; they are never replaced by another identity.
///
/// # Errors
/// Returns validation, root-escape, conflicting-status, or total-capacity errors.
pub fn project(
    agents: impl IntoIterator<Item = ReportedTarget>,
    services: impl IntoIterator<Item = ReportedTarget>,
    changes: impl IntoIterator<Item = ReportedChange>,
    tree_paths: impl IntoIterator<Item = String>,
    capacity: usize,
) -> Result<ExplorerProjection, ExplorerError> {
    let agents = targets(agents, ExplorerId::Agent)?;
    let services = targets(services, ExplorerId::Service)?;
    let mut status_by_path = BTreeMap::new();
    for change in changes {
        let path = canonical_path(&change.path)?;
        if status_by_path
            .insert(path.clone(), change.status)
            .is_some_and(|old| old != change.status)
        {
            return Err(ExplorerError::ConflictingStatus);
        }
    }
    let tree: BTreeSet<_> = tree_paths
        .into_iter()
        .map(|path| canonical_path(&path))
        .collect::<Result<_, _>>()?;
    let changed_entries = status_by_path
        .into_keys()
        .map(|path| ExplorerEntry {
            id: ExplorerId::ChangedPath(path),
            available: true,
        })
        .collect::<Vec<_>>();
    let tree_entries = tree
        .into_iter()
        .map(|path| ExplorerEntry {
            id: ExplorerId::TreePath(path),
            available: true,
        })
        .collect::<Vec<_>>();
    let total = agents.len() + services.len() + changed_entries.len() + tree_entries.len();
    if capacity == 0 || total > capacity {
        return Err(ExplorerError::CapacityExceeded);
    }
    Ok(ExplorerProjection {
        sections: vec![
            ExplorerSection {
                section: Section::Agents,
                entries: agents,
            },
            ExplorerSection {
                section: Section::Services,
                entries: services,
            },
            ExplorerSection {
                section: Section::ChangedFiles,
                entries: changed_entries,
            },
            ExplorerSection {
                section: Section::ProjectTree,
                entries: tree_entries,
            },
        ],
    })
}

fn targets(
    values: impl IntoIterator<Item = ReportedTarget>,
    make: fn(String) -> ExplorerId,
) -> Result<Vec<ExplorerEntry>, ExplorerError> {
    let mut targets = BTreeMap::new();
    for value in values {
        validate_id(&value.id)?;
        targets
            .entry(value.id)
            .and_modify(|available| *available |= value.available)
            .or_insert(value.available);
    }
    Ok(targets
        .into_iter()
        .map(|(id, available)| ExplorerEntry {
            id: make(id),
            available,
        })
        .collect())
}

fn validate_id(value: &str) -> Result<(), ExplorerError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(ExplorerError::InvalidIdentity)
    } else {
        Ok(())
    }
}

fn canonical_path(value: &str) -> Result<String, ExplorerError> {
    validate_id(value)?;
    if value.starts_with('/')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ExplorerError::RootEscape);
    }
    Ok(value.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(id: &str, available: bool) -> ReportedTarget {
        ReportedTarget {
            id: id.into(),
            available,
        }
    }
    fn change(path: &str, status: GitStatus) -> ReportedChange {
        ReportedChange {
            path: path.into(),
            status,
        }
    }

    #[test]
    fn shuffled_inputs_have_fixed_sections_and_sorted_canonical_ids() {
        let result = project(
            [target("z", true), target("a", true)],
            [target("web", true)],
            [
                change("src/z.rs", GitStatus::Modified),
                change("src/a.rs", GitStatus::Added),
            ],
            ["src/z.rs".into(), "src/a.rs".into()],
            10,
        )
        .unwrap();
        assert_eq!(
            result
                .sections
                .iter()
                .map(|s| s.section)
                .collect::<Vec<_>>(),
            SECTION_ORDER
        );
        assert_eq!(
            result.sections[0].entries[0].id,
            ExplorerId::Agent("a".into())
        );
        assert_eq!(
            result.sections[2].entries[0].id,
            ExplorerId::ChangedPath("src/a.rs".into())
        );
    }
    #[test]
    fn duplicates_dedupe_by_identity_and_preserve_availability() {
        let result = project(
            [target("agent", false), target("agent", true)],
            [],
            [],
            [],
            1,
        )
        .unwrap();
        assert_eq!(
            result.sections[0].entries,
            [ExplorerEntry {
                id: ExplorerId::Agent("agent".into()),
                available: true
            }]
        );
    }
    #[test]
    fn unavailable_exact_target_remains_placeholder() {
        let result = project([target("missing", false)], [], [], [], 1).unwrap();
        assert!(!result.sections[0].entries[0].available);
        assert_eq!(
            result.sections[0].entries[0].id,
            ExplorerId::Agent("missing".into())
        );
    }
    #[test]
    fn root_escapes_and_conflicting_status_are_rejected() {
        assert_eq!(
            project([], [], [change("../secret", GitStatus::Modified)], [], 2),
            Err(ExplorerError::RootEscape)
        );
        assert_eq!(
            project(
                [],
                [],
                [
                    change("a", GitStatus::Added),
                    change("a", GitStatus::Modified)
                ],
                [],
                2
            ),
            Err(ExplorerError::ConflictingStatus)
        );
    }
    #[test]
    fn capacity_counts_deduplicated_entries() {
        assert_eq!(
            project([target("a", true)], [target("b", true)], [], [], 1),
            Err(ExplorerError::CapacityExceeded)
        );
    }
}
