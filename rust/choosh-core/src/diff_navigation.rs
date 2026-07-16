//! Immutable, bounded Git diff line navigation.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotId(String);

impl SnapshotId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineKind {
    Context,
    Addition,
    Deletion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MappedLine {
    pub kind: LineKind,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub navigation_new_line: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NavigationHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<MappedLine>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NavigationLimits {
    pub max_hunks: usize,
    pub max_lines: usize,
    pub max_line_number: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaceholderReason {
    Binary,
    Unsupported,
    Oversized,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffNavigationContent {
    Text(Vec<NavigationHunk>),
    Placeholder(PlaceholderReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffNavigation {
    snapshot_id: SnapshotId,
    content: DiffNavigationContent,
    displayed_lines: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplaySelection {
    pub start: usize,
    pub end_exclusive: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentSide {
    Current,
    Historical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentPosition {
    pub side: DocumentSide,
    pub line: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NavigationSelection {
    pub start: DocumentPosition,
    pub end: DocumentPosition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NavigationError {
    InvalidLimits,
    TooManyHunks,
    TooManyLines,
    InvalidLineNumber,
    InvalidMapping,
    StaleSnapshot,
    Placeholder,
    SelectionOutOfBounds,
    MixedSides,
}

impl DiffNavigation {
    /// Validates and freezes a textual diff navigation model.
    ///
    /// # Errors
    ///
    /// Rejects invalid limits, oversized models, inconsistent old/new numbering,
    /// and invalid deletion navigation targets.
    pub fn text(
        snapshot_id: SnapshotId,
        hunks: Vec<NavigationHunk>,
        limits: NavigationLimits,
    ) -> Result<Self, NavigationError> {
        validate_limits(limits)?;
        if hunks.len() > limits.max_hunks {
            return Err(NavigationError::TooManyHunks);
        }
        let mut displayed_lines = 0_usize;
        for hunk in &hunks {
            validate_hunk(hunk, limits)?;
            displayed_lines = displayed_lines
                .checked_add(hunk.lines.len())
                .ok_or(NavigationError::TooManyLines)?;
            if displayed_lines > limits.max_lines {
                return Err(NavigationError::TooManyLines);
            }
        }
        Ok(Self {
            snapshot_id,
            content: DiffNavigationContent::Text(hunks),
            displayed_lines,
        })
    }

    /// Creates a non-navigable metadata-only diff page.
    ///
    /// # Errors
    ///
    /// Rejects invalid limits.
    pub fn placeholder(
        snapshot_id: SnapshotId,
        reason: PlaceholderReason,
        limits: NavigationLimits,
    ) -> Result<Self, NavigationError> {
        validate_limits(limits)?;
        Ok(Self {
            snapshot_id,
            content: DiffNavigationContent::Placeholder(reason),
            displayed_lines: 0,
        })
    }

    #[must_use]
    pub const fn displayed_lines(&self) -> usize {
        self.displayed_lines
    }

    /// Resolves one unified displayed-line offset to its exact document side.
    ///
    /// # Errors
    ///
    /// Rejects stale snapshots, placeholders, or an out-of-bounds display line.
    pub fn navigate(
        &self,
        current_snapshot: &SnapshotId,
        displayed_line: usize,
    ) -> Result<DocumentPosition, NavigationError> {
        if current_snapshot != &self.snapshot_id {
            return Err(NavigationError::StaleSnapshot);
        }
        let DiffNavigationContent::Text(hunks) = &self.content else {
            return Err(NavigationError::Placeholder);
        };
        let line =
            flattened_line(hunks, displayed_line).ok_or(NavigationError::SelectionOutOfBounds)?;
        Ok(line_position(line))
    }

    /// Maps an inclusive/exclusive unified selection into one document-side range.
    ///
    /// # Errors
    ///
    /// Rejects stale snapshots, empty/out-of-bounds selections, placeholders, or
    /// selections spanning current and historical document sides.
    pub fn navigate_selection(
        &self,
        current_snapshot: &SnapshotId,
        selection: DisplaySelection,
    ) -> Result<NavigationSelection, NavigationError> {
        if selection.start >= selection.end_exclusive
            || selection.end_exclusive > self.displayed_lines
        {
            return Err(NavigationError::SelectionOutOfBounds);
        }
        let start = self.navigate(current_snapshot, selection.start)?;
        let end = self.navigate(current_snapshot, selection.end_exclusive - 1)?;
        if start.side != end.side {
            return Err(NavigationError::MixedSides);
        }
        Ok(NavigationSelection { start, end })
    }
}

fn validate_limits(limits: NavigationLimits) -> Result<(), NavigationError> {
    if limits.max_hunks == 0 || limits.max_lines == 0 || limits.max_line_number == 0 {
        Err(NavigationError::InvalidLimits)
    } else {
        Ok(())
    }
}

fn validate_hunk(hunk: &NavigationHunk, limits: NavigationLimits) -> Result<(), NavigationError> {
    if hunk.old_start == 0
        || hunk.new_start == 0
        || hunk.old_start > limits.max_line_number
        || hunk.new_start > limits.max_line_number
    {
        return Err(NavigationError::InvalidLineNumber);
    }
    let mut old = hunk.old_start;
    let mut new = hunk.new_start;
    let mut old_count = 0;
    let mut new_count = 0;
    for line in &hunk.lines {
        if line.navigation_new_line == 0 || line.navigation_new_line > limits.max_line_number {
            return Err(NavigationError::InvalidLineNumber);
        }
        let expected = match line.kind {
            LineKind::Context => (Some(old), Some(new)),
            LineKind::Addition => (None, Some(new)),
            LineKind::Deletion => (Some(old), None),
        };
        if (line.old_line, line.new_line) != expected {
            return Err(NavigationError::InvalidMapping);
        }
        if line.old_line.is_some() {
            old = old
                .checked_add(1)
                .ok_or(NavigationError::InvalidLineNumber)?;
            old_count += 1;
        }
        if line.new_line.is_some() {
            new = new
                .checked_add(1)
                .ok_or(NavigationError::InvalidLineNumber)?;
            new_count += 1;
        }
    }
    if old_count != hunk.old_count || new_count != hunk.new_count {
        Err(NavigationError::InvalidMapping)
    } else {
        Ok(())
    }
}

fn flattened_line(hunks: &[NavigationHunk], mut index: usize) -> Option<&MappedLine> {
    for hunk in hunks {
        if index < hunk.lines.len() {
            return hunk.lines.get(index);
        }
        index -= hunk.lines.len();
    }
    None
}

fn line_position(line: &MappedLine) -> DocumentPosition {
    match line.kind {
        LineKind::Context | LineKind::Addition => DocumentPosition {
            side: DocumentSide::Current,
            line: line.new_line.expect("validated mapping"),
        },
        LineKind::Deletion => DocumentPosition {
            side: DocumentSide::Historical,
            line: line.old_line.expect("validated mapping"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: NavigationLimits = NavigationLimits {
        max_hunks: 2,
        max_lines: 8,
        max_line_number: 100,
    };

    fn model() -> DiffNavigation {
        DiffNavigation::text(
            SnapshotId::new("snapshot-1"),
            vec![NavigationHunk {
                old_start: 4,
                old_count: 3,
                new_start: 4,
                new_count: 3,
                lines: vec![
                    MappedLine {
                        kind: LineKind::Context,
                        old_line: Some(4),
                        new_line: Some(4),
                        navigation_new_line: 4,
                    },
                    MappedLine {
                        kind: LineKind::Deletion,
                        old_line: Some(5),
                        new_line: None,
                        navigation_new_line: 5,
                    },
                    MappedLine {
                        kind: LineKind::Addition,
                        old_line: None,
                        new_line: Some(5),
                        navigation_new_line: 5,
                    },
                    MappedLine {
                        kind: LineKind::Context,
                        old_line: Some(6),
                        new_line: Some(6),
                        navigation_new_line: 6,
                    },
                ],
            }],
            LIMITS,
        )
        .unwrap()
    }

    #[test]
    fn unified_lines_resolve_exact_current_or_historical_side() {
        let model = model();
        let snapshot = SnapshotId::new("snapshot-1");
        assert_eq!(
            model.navigate(&snapshot, 0).unwrap(),
            DocumentPosition {
                side: DocumentSide::Current,
                line: 4
            }
        );
        assert_eq!(
            model.navigate(&snapshot, 1).unwrap(),
            DocumentPosition {
                side: DocumentSide::Historical,
                line: 5
            }
        );
        assert_eq!(
            model.navigate(&snapshot, 2).unwrap(),
            DocumentPosition {
                side: DocumentSide::Current,
                line: 5
            }
        );
    }

    #[test]
    fn selection_maps_only_when_one_document_side_owns_it() {
        let model = model();
        let snapshot = SnapshotId::new("snapshot-1");
        assert_eq!(
            model
                .navigate_selection(
                    &snapshot,
                    DisplaySelection {
                        start: 2,
                        end_exclusive: 4
                    }
                )
                .unwrap(),
            NavigationSelection {
                start: DocumentPosition {
                    side: DocumentSide::Current,
                    line: 5
                },
                end: DocumentPosition {
                    side: DocumentSide::Current,
                    line: 6
                }
            }
        );
        assert_eq!(
            model.navigate_selection(
                &snapshot,
                DisplaySelection {
                    start: 1,
                    end_exclusive: 3
                }
            ),
            Err(NavigationError::MixedSides)
        );
    }

    #[test]
    fn stale_and_placeholder_navigation_fail_closed() {
        assert_eq!(
            model().navigate(&SnapshotId::new("newer"), 0),
            Err(NavigationError::StaleSnapshot)
        );
        let placeholder =
            DiffNavigation::placeholder(SnapshotId::new("s"), PlaceholderReason::Binary, LIMITS)
                .unwrap();
        assert_eq!(
            placeholder.navigate(&SnapshotId::new("s"), 0),
            Err(NavigationError::Placeholder)
        );
    }

    #[test]
    fn invalid_numbering_and_counts_are_rejected() {
        let invalid = NavigationHunk {
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 1,
            lines: vec![MappedLine {
                kind: LineKind::Addition,
                old_line: Some(1),
                new_line: Some(1),
                navigation_new_line: 1,
            }],
        };
        assert_eq!(
            DiffNavigation::text(SnapshotId::new("s"), vec![invalid], LIMITS),
            Err(NavigationError::InvalidMapping)
        );
    }

    #[test]
    fn hunk_line_and_selection_bounds_are_enforced() {
        let mut tight = LIMITS;
        tight.max_lines = 3;
        let DiffNavigationContent::Text(hunks) = model().content else {
            unreachable!()
        };
        assert_eq!(
            DiffNavigation::text(SnapshotId::new("s"), hunks, tight),
            Err(NavigationError::TooManyLines)
        );
        assert_eq!(
            model().navigate_selection(
                &SnapshotId::new("snapshot-1"),
                DisplaySelection {
                    start: 4,
                    end_exclusive: 4
                }
            ),
            Err(NavigationError::SelectionOutOfBounds)
        );
    }
}
