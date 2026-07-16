//! Renderer-independent deterministic terminal conformance fixtures.

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureId(String);

impl FixtureId {
    /// Creates a bounded fixture ID validated later by the runner.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConformanceLimits {
    pub max_rows: usize,
    pub max_columns: usize,
    pub max_chunks: usize,
    pub max_input_bytes: usize,
    pub max_cells: usize,
    pub max_damage: usize,
    pub max_fixture_id_bytes: usize,
    pub repeat_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModeState {
    Disabled,
    Enabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpectedModes {
    pub alternate_screen: ModeState,
    pub application_cursor: ModeState,
    pub mouse_reporting: ModeState,
    pub bracketed_paste: ModeState,
}

impl Default for ExpectedModes {
    fn default() -> Self {
        Self {
            alternate_screen: ModeState::Disabled,
            application_cursor: ModeState::Disabled,
            mouse_reporting: ModeState::Disabled,
            bracketed_paste: ModeState::Disabled,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DamageRegion {
    pub row: usize,
    pub start_column: usize,
    pub end_column: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSnapshot {
    pub rows: usize,
    pub columns: usize,
    pub cells: Vec<char>,
    pub cursor_row: usize,
    pub cursor_column: usize,
    pub modes: ExpectedModes,
    pub damage: Vec<DamageRegion>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceFixture {
    pub id: FixtureId,
    pub rows: usize,
    pub columns: usize,
    pub input_chunks: Vec<Vec<u8>>,
    pub expected: TerminalSnapshot,
}

pub trait TerminalExecutor {
    type Error;

    /// Resets the executor to a blank terminal of the requested dimensions.
    ///
    /// # Errors
    ///
    /// Returns an executor-specific reset failure.
    fn reset(&mut self, rows: usize, columns: usize) -> Result<(), Self::Error>;

    /// Feeds one exact fixture chunk without changing its boundaries.
    ///
    /// # Errors
    ///
    /// Returns an executor-specific input failure.
    fn feed(&mut self, chunk: &[u8]) -> Result<(), Self::Error>;

    /// Returns an immutable renderer-independent terminal snapshot.
    ///
    /// # Errors
    ///
    /// Returns an executor-specific snapshot failure.
    fn snapshot(&mut self) -> Result<TerminalSnapshot, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mismatch {
    Grid,
    Cursor,
    Modes,
    Damage,
    Nondeterministic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConformanceEvidence {
    pub fixture_id: FixtureId,
    pub repeats: usize,
    pub passed: bool,
    pub mismatches: Vec<Mismatch>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConformanceError<ExecutorError> {
    InvalidLimits,
    InvalidFixture,
    InputLimit,
    SnapshotLimit,
    Executor(ExecutorError),
}

/// Runs one fixture repeatedly and emits canonical semantic evidence.
///
/// # Errors
///
/// Rejects invalid dimensions/IDs/bounds, oversized input or snapshots, and
/// wraps executor failures. Semantic mismatches are evidence, not runner errors.
pub fn run_fixture<E: TerminalExecutor>(
    executor: &mut E,
    fixture: &ConformanceFixture,
    limits: ConformanceLimits,
) -> Result<ConformanceEvidence, ConformanceError<E::Error>> {
    validate_limits(limits).map_err(|()| ConformanceError::InvalidLimits)?;
    validate_fixture(fixture, limits).map_err(|error| match error {
        FixtureValidation::Invalid => ConformanceError::InvalidFixture,
        FixtureValidation::Input => ConformanceError::InputLimit,
        FixtureValidation::Snapshot => ConformanceError::SnapshotLimit,
    })?;
    let mut first = None;
    let mut mismatches = Vec::new();
    for _ in 0..limits.repeat_count {
        executor
            .reset(fixture.rows, fixture.columns)
            .map_err(ConformanceError::Executor)?;
        for chunk in &fixture.input_chunks {
            executor.feed(chunk).map_err(ConformanceError::Executor)?;
        }
        let snapshot = executor.snapshot().map_err(ConformanceError::Executor)?;
        validate_snapshot(&snapshot, limits).map_err(|_| ConformanceError::SnapshotLimit)?;
        if let Some(baseline) = &first {
            if baseline != &snapshot {
                push_unique(&mut mismatches, Mismatch::Nondeterministic);
            }
        } else {
            first = Some(snapshot.clone());
        }
        compare(&snapshot, &fixture.expected, &mut mismatches);
    }
    Ok(ConformanceEvidence {
        fixture_id: fixture.id.clone(),
        repeats: limits.repeat_count,
        passed: mismatches.is_empty(),
        mismatches,
    })
}

fn compare(actual: &TerminalSnapshot, expected: &TerminalSnapshot, mismatches: &mut Vec<Mismatch>) {
    if actual.rows != expected.rows
        || actual.columns != expected.columns
        || actual.cells != expected.cells
    {
        push_unique(mismatches, Mismatch::Grid);
    }
    if actual.cursor_row != expected.cursor_row || actual.cursor_column != expected.cursor_column {
        push_unique(mismatches, Mismatch::Cursor);
    }
    if actual.modes != expected.modes {
        push_unique(mismatches, Mismatch::Modes);
    }
    if actual.damage != expected.damage {
        push_unique(mismatches, Mismatch::Damage);
    }
}

fn push_unique(values: &mut Vec<Mismatch>, value: Mismatch) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[derive(Clone, Copy)]
enum FixtureValidation {
    Invalid,
    Input,
    Snapshot,
}

fn validate_limits(limits: ConformanceLimits) -> Result<(), ()> {
    if limits.max_rows == 0
        || limits.max_columns == 0
        || limits.max_chunks == 0
        || limits.max_input_bytes == 0
        || limits.max_cells == 0
        || limits.max_damage == 0
        || limits.max_fixture_id_bytes == 0
        || limits.repeat_count == 0
    {
        Err(())
    } else {
        Ok(())
    }
}

fn validate_fixture(
    fixture: &ConformanceFixture,
    limits: ConformanceLimits,
) -> Result<(), FixtureValidation> {
    if fixture.id.0.is_empty()
        || fixture.id.0.len() > limits.max_fixture_id_bytes
        || fixture.rows == 0
        || fixture.columns == 0
        || fixture.rows > limits.max_rows
        || fixture.columns > limits.max_columns
        || fixture.input_chunks.len() > limits.max_chunks
        || fixture.expected.rows != fixture.rows
        || fixture.expected.columns != fixture.columns
    {
        return Err(FixtureValidation::Invalid);
    }
    let bytes = fixture
        .input_chunks
        .iter()
        .try_fold(0_usize, |sum, chunk| {
            sum.checked_add(chunk.len()).ok_or(FixtureValidation::Input)
        })?;
    if bytes > limits.max_input_bytes {
        return Err(FixtureValidation::Input);
    }
    validate_snapshot(&fixture.expected, limits)
}

fn validate_snapshot(
    snapshot: &TerminalSnapshot,
    limits: ConformanceLimits,
) -> Result<(), FixtureValidation> {
    let cells = snapshot
        .rows
        .checked_mul(snapshot.columns)
        .ok_or(FixtureValidation::Snapshot)?;
    if snapshot.rows == 0
        || snapshot.columns == 0
        || snapshot.rows > limits.max_rows
        || snapshot.columns > limits.max_columns
        || cells > limits.max_cells
        || snapshot.cells.len() != cells
        || snapshot.cursor_row >= snapshot.rows
        || snapshot.cursor_column >= snapshot.columns
        || snapshot.damage.len() > limits.max_damage
        || snapshot.damage.iter().any(|damage| {
            damage.row >= snapshot.rows
                || damage.start_column >= damage.end_column
                || damage.end_column > snapshot.columns
        })
    {
        Err(FixtureValidation::Snapshot)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FakeExecutor {
        snapshots: Vec<TerminalSnapshot>,
        run: usize,
        chunks: Vec<Vec<u8>>,
    }

    impl TerminalExecutor for FakeExecutor {
        type Error = &'static str;

        fn reset(&mut self, _: usize, _: usize) -> Result<(), Self::Error> {
            self.chunks.clear();
            Ok(())
        }

        fn feed(&mut self, chunk: &[u8]) -> Result<(), Self::Error> {
            self.chunks.push(chunk.to_vec());
            Ok(())
        }

        fn snapshot(&mut self) -> Result<TerminalSnapshot, Self::Error> {
            let snapshot = self.snapshots[self.run.min(self.snapshots.len() - 1)].clone();
            self.run += 1;
            Ok(snapshot)
        }
    }

    const LIMITS: ConformanceLimits = ConformanceLimits {
        max_rows: 4,
        max_columns: 8,
        max_chunks: 4,
        max_input_bytes: 32,
        max_cells: 32,
        max_damage: 4,
        max_fixture_id_bytes: 32,
        repeat_count: 2,
    };

    fn snapshot(text: &str) -> TerminalSnapshot {
        TerminalSnapshot {
            rows: 1,
            columns: 4,
            cells: text.chars().collect(),
            cursor_row: 0,
            cursor_column: 3,
            modes: ExpectedModes {
                application_cursor: ModeState::Enabled,
                ..ExpectedModes::default()
            },
            damage: vec![DamageRegion {
                row: 0,
                start_column: 0,
                end_column: 4,
            }],
        }
    }

    fn fixture() -> ConformanceFixture {
        ConformanceFixture {
            id: FixtureId::new("zellij-agent-cursor"),
            rows: 1,
            columns: 4,
            input_chunks: vec![b"ab".to_vec(), b"c ".to_vec()],
            expected: snapshot("abc "),
        }
    }

    #[test]
    fn representative_chunked_fixture_passes_repeatedly() {
        let expected = snapshot("abc ");
        let mut executor = FakeExecutor {
            snapshots: vec![expected],
            run: 0,
            chunks: vec![],
        };
        let evidence = run_fixture(&mut executor, &fixture(), LIMITS).unwrap();
        assert!(evidence.passed);
        assert_eq!(evidence.repeats, 2);
        assert_eq!(executor.chunks, [b"ab".to_vec(), b"c ".to_vec()]);
    }

    #[test]
    fn canonical_mismatches_are_deduplicated() {
        let mut wrong = snapshot("bad ");
        wrong.cursor_column = 1;
        wrong.modes.mouse_reporting = ModeState::Enabled;
        wrong.damage.clear();
        let mut executor = FakeExecutor {
            snapshots: vec![wrong],
            run: 0,
            chunks: vec![],
        };
        let evidence = run_fixture(&mut executor, &fixture(), LIMITS).unwrap();
        assert_eq!(
            evidence.mismatches,
            [
                Mismatch::Grid,
                Mismatch::Cursor,
                Mismatch::Modes,
                Mismatch::Damage
            ]
        );
    }

    #[test]
    fn repeat_difference_is_explicit_nondeterminism() {
        let mut changed = snapshot("abc ");
        changed.cursor_column = 2;
        let mut executor = FakeExecutor {
            snapshots: vec![snapshot("abc "), changed],
            run: 0,
            chunks: vec![],
        };
        let evidence = run_fixture(&mut executor, &fixture(), LIMITS).unwrap();
        assert!(evidence.mismatches.contains(&Mismatch::Nondeterministic));
        assert!(evidence.mismatches.contains(&Mismatch::Cursor));
    }

    #[test]
    fn input_and_snapshot_bounds_fail_before_partial_evidence() {
        let mut oversized = fixture();
        oversized.input_chunks = vec![vec![0; 33]];
        let mut executor = FakeExecutor {
            snapshots: vec![snapshot("abc ")],
            run: 0,
            chunks: vec![],
        };
        assert_eq!(
            run_fixture(&mut executor, &oversized, LIMITS),
            Err(ConformanceError::InputLimit)
        );

        let mut invalid = fixture();
        invalid.expected.cells.pop();
        assert_eq!(
            run_fixture(&mut executor, &invalid, LIMITS),
            Err(ConformanceError::SnapshotLimit)
        );
    }

    #[test]
    fn fixture_identity_and_damage_are_validated() {
        let mut invalid = fixture();
        invalid.id = FixtureId::new("");
        let mut executor = FakeExecutor {
            snapshots: vec![snapshot("abc ")],
            run: 0,
            chunks: vec![],
        };
        assert_eq!(
            run_fixture(&mut executor, &invalid, LIMITS),
            Err(ConformanceError::InvalidFixture)
        );

        invalid = fixture();
        invalid.expected.damage[0].end_column = 5;
        assert_eq!(
            run_fixture(&mut executor, &invalid, LIMITS),
            Err(ConformanceError::SnapshotLimit)
        );
    }
}
