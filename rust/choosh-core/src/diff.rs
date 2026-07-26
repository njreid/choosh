//! Bounded, deterministic textual diff construction from already-fetched bytes.
//!
//! This module never invokes Git or reads a filesystem. Callers must provide
//! identity-bound versions and validated path identities.

use crate::path::RelativePath;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiffLimits {
    pub max_bytes_per_side: usize,
    pub max_lines_per_side: usize,
    pub max_cells: usize,
    pub max_work: u64,
    pub max_hunks: usize,
    pub context_lines: usize,
}

/// Immutable identity attached to one diff computation.
///
/// The host protocol addresses blobs by snapshot and entry rather than by a
/// display path. Keeping that identity beside the bytes prevents callers from
/// accidentally rendering a diff for a newer snapshot under an older entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffRequest {
    pub snapshot_id: String,
    pub entry_id: String,
    pub comparison: Comparison,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Comparison {
    Working,
    Staged,
    Combined,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentifiedDiff {
    pub request: DiffRequest,
    pub result: DiffResult,
}

/// Computes a diff while retaining the immutable request identity.
///
/// Empty identities are rejected before any bytes are inspected. The actual
/// diff remains pure and bounded; this wrapper is the seam used by protocol
/// adapters and navigation caches.
#[must_use]
pub fn compute_identified_diff(
    request: DiffRequest,
    metadata: DiffMetadata,
    old: &[u8],
    new: &[u8],
    limits: DiffLimits,
) -> Option<IdentifiedDiff> {
    if request.snapshot_id.is_empty() || request.entry_id.is_empty() {
        return None;
    }
    Some(IdentifiedDiff {
        request,
        result: compute_diff(metadata, old, new, limits),
    })
}

impl Default for DiffLimits {
    fn default() -> Self {
        Self {
            max_bytes_per_side: 2 * 1024 * 1024,
            max_lines_per_side: 100_000,
            // Maximum retained Myers frontier cells used for deterministic
            // backtracking. This bounds auxiliary memory independently of
            // the input matrix shape.
            max_cells: 4_000_000,
            max_work: 8_000_000,
            max_hunks: 10_000,
            context_lines: 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedKind {
    Submodule,
    UnsafeSymlink,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffMetadata {
    pub old_path: Option<RelativePath>,
    pub new_path: Option<RelativePath>,
    pub change: ChangeKind,
    pub old_mode: Option<u32>,
    pub new_mode: Option<u32>,
    pub unsupported: Option<UnsupportedKind>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataReason {
    Binary,
    TooLarge,
    TooManyLines,
    UnsupportedEncoding,
    MixedLineEndings,
    UnsupportedKind,
    DiffBudgetExceeded,
    TooManyHunks,
    InvalidLimits,
}

impl MetadataReason {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::TooLarge => "too_large",
            Self::TooManyLines => "too_many_lines",
            Self::UnsupportedEncoding => "unsupported_encoding",
            Self::MixedLineEndings => "mixed_line_endings",
            Self::UnsupportedKind => "unsupported_kind",
            Self::DiffBudgetExceeded => "diff_budget_exceeded",
            Self::TooManyHunks => "too_many_hunks",
            Self::InvalidLimits => "invalid_limits",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffResult {
    Text(TextDiff),
    MetadataOnly {
        metadata: DiffMetadata,
        reason: MetadataReason,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextDiff {
    pub metadata: DiffMetadata,
    pub algorithm: &'static str,
    pub old_ends_with_newline: bool,
    pub new_ends_with_newline: bool,
    pub hunks: Vec<Hunk>,
    /// Every displayed changed/context line, in hunk order. This is stored
    /// rather than reconstructed by the UI so navigation is reproducible.
    pub line_map: Vec<LineMapping>,
    pub work_used: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_end: usize,
    pub new_start: usize,
    pub new_end: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineKind {
    Context,
    Addition,
    Deletion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub has_terminator: bool,
    /// Exact terminator style. This makes a CRLF-only edit visible and lets a
    /// headless consumer reconstruct the right side byte-for-byte.
    pub terminator: LineTerminator,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineTerminator {
    None,
    Lf,
    CrLf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineMapping {
    pub kind: LineKind,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    /// Target in the new version. Deletions use the next surviving line, then
    /// the previous surviving line, then line one.
    pub navigation_new_line: usize,
}

#[derive(Clone, Copy)]
struct SourceLine<'a> {
    text: &'a str,
    terminator: LineTerminator,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Ending {
    None,
    Lf,
    CrLf,
    Mixed,
}

#[derive(Clone, Copy)]
enum Op<'a> {
    Equal(SourceLine<'a>),
    Delete(SourceLine<'a>),
    Add(SourceLine<'a>),
}

/// Computes a stable line diff. Resource failures return metadata only and
/// never expose a partial hunk list.
#[must_use]
pub fn compute_diff(
    metadata: DiffMetadata,
    old: &[u8],
    new: &[u8],
    limits: DiffLimits,
) -> DiffResult {
    let fail = |reason| DiffResult::MetadataOnly {
        metadata: metadata.clone(),
        reason,
    };
    if limits.max_bytes_per_side == 0
        || limits.max_lines_per_side == 0
        || limits.max_cells == 0
        || limits.max_work == 0
        || limits.max_hunks == 0
    {
        return fail(MetadataReason::InvalidLimits);
    }
    if metadata.unsupported.is_some() {
        return fail(MetadataReason::UnsupportedKind);
    }
    if old.len() > limits.max_bytes_per_side || new.len() > limits.max_bytes_per_side {
        return fail(MetadataReason::TooLarge);
    }
    if old.contains(&0) || new.contains(&0) {
        return fail(MetadataReason::Binary);
    }
    let (Ok(old_text), Ok(new_text)) = (str::from_utf8(old), str::from_utf8(new)) else {
        return fail(MetadataReason::UnsupportedEncoding);
    };
    let (old_lines, old_ending) = split_lines(old_text);
    let (new_lines, new_ending) = split_lines(new_text);
    if old_ending == Ending::Mixed || new_ending == Ending::Mixed {
        return fail(MetadataReason::MixedLineEndings);
    }
    if old_lines.len() > limits.max_lines_per_side || new_lines.len() > limits.max_lines_per_side {
        return fail(MetadataReason::TooManyLines);
    }
    let Some((ops, work)) = bounded_myers(&old_lines, &new_lines, limits) else {
        return fail(MetadataReason::DiffBudgetExceeded);
    };
    let hunks = build_hunks(&ops, limits.context_lines);
    if hunks.len() > limits.max_hunks {
        return fail(MetadataReason::TooManyHunks);
    }
    let line_map = hunks.iter().flat_map(mappings_for_hunk).collect();
    DiffResult::Text(TextDiff {
        metadata,
        algorithm: "bounded-myers-v1",
        old_ends_with_newline: old.last() == Some(&b'\n'),
        new_ends_with_newline: new.last() == Some(&b'\n'),
        hunks,
        line_map,
        work_used: work,
    })
}

/// Computes a shortest edit script with Myers' O((N + M)D) frontier algorithm.
///
/// We retain the successive frontiers solely to reconstruct a deterministic
/// script. `max_cells` caps that retained trace; `max_work` caps frontier,
/// snake, and reconstruction work. The operation ordering deliberately keeps
/// the previous LCS contract's deletion-before-addition tie break.
fn bounded_myers<'a>(
    old: &[SourceLine<'a>],
    new: &[SourceLine<'a>],
    limits: DiffLimits,
) -> Option<(Vec<Op<'a>>, u64)> {
    let max_distance = old.len().checked_add(new.len())?;
    // One sentinel on each side keeps the `k ± 1` frontier lookup valid at
    // the first and outermost diagonals.
    let frontier_len = max_distance.checked_mul(2)?.checked_add(3)?;
    let offset = isize::try_from(max_distance.checked_add(1)?).ok()?;
    let mut frontier = vec![0_isize; frontier_len];
    let mut trace = Vec::<Vec<isize>>::new();
    let mut retained_cells = 0_usize;
    let mut work = 0_u64;

    for distance in 0..=max_distance {
        let distance_i = isize::try_from(distance).ok()?;
        for diagonal in (-distance_i..=distance_i).step_by(2) {
            work = work.checked_add(1)?;
            if work > limits.max_work {
                return None;
            }
            let index = usize::try_from(offset.checked_add(diagonal)?).ok()?;
            let mut x = if diagonal == -distance_i
                || (diagonal != distance_i && frontier[index + 1] > frontier[index - 1])
            {
                frontier[index + 1]
            } else {
                frontier[index - 1].checked_add(1)?
            };
            let mut y = x.checked_sub(diagonal)?;
            while usize::try_from(x).ok().is_some_and(|i| i < old.len())
                && usize::try_from(y).ok().is_some_and(|i| i < new.len())
                && lines_equal(old[usize::try_from(x).ok()?], new[usize::try_from(y).ok()?])
            {
                x = x.checked_add(1)?;
                y = y.checked_add(1)?;
                work = work.checked_add(1)?;
                if work > limits.max_work {
                    return None;
                }
            }
            frontier[index] = x;
            if usize::try_from(x).ok() == Some(old.len())
                && usize::try_from(y).ok() == Some(new.len())
            {
                retained_cells = retained_cells.checked_add(frontier_len)?;
                if retained_cells > limits.max_cells {
                    return None;
                }
                trace.push(frontier.clone());
                return backtrack_myers(old, new, &trace, work, offset, limits.max_work);
            }
        }
        retained_cells = retained_cells.checked_add(frontier_len)?;
        if retained_cells > limits.max_cells {
            return None;
        }
        trace.push(frontier.clone());
    }
    None
}

fn backtrack_myers<'a>(
    old: &[SourceLine<'a>],
    new: &[SourceLine<'a>],
    trace: &[Vec<isize>],
    mut work: u64,
    offset: isize,
    max_work: u64,
) -> Option<(Vec<Op<'a>>, u64)> {
    let mut x = isize::try_from(old.len()).ok()?;
    let mut y = isize::try_from(new.len()).ok()?;
    let mut ops = Vec::with_capacity(old.len().checked_add(new.len())?);
    for distance in (1..trace.len()).rev() {
        let distance_i = isize::try_from(distance).ok()?;
        let previous = &trace[distance - 1];
        let diagonal = x.checked_sub(y)?;
        let index = usize::try_from(offset.checked_add(diagonal)?).ok()?;
        let previous_diagonal = if diagonal == -distance_i
            || (diagonal != distance_i && previous[index + 1] > previous[index - 1])
        {
            diagonal.checked_add(1)?
        } else {
            diagonal.checked_sub(1)?
        };
        let previous_index = usize::try_from(offset.checked_add(previous_diagonal)?).ok()?;
        let previous_x = previous[previous_index];
        let previous_y = previous_x.checked_sub(previous_diagonal)?;
        while x > previous_x && y > previous_y {
            x = x.checked_sub(1)?;
            y = y.checked_sub(1)?;
            ops.push(Op::Equal(old[usize::try_from(x).ok()?]));
            work = work.checked_add(1)?;
            if work > max_work {
                return None;
            }
        }
        if x == previous_x {
            y = y.checked_sub(1)?;
            ops.push(Op::Add(new[usize::try_from(y).ok()?]));
        } else {
            x = x.checked_sub(1)?;
            ops.push(Op::Delete(old[usize::try_from(x).ok()?]));
        }
        work = work.checked_add(1)?;
        if work > max_work {
            return None;
        }
    }
    while x > 0 && y > 0 {
        x = x.checked_sub(1)?;
        y = y.checked_sub(1)?;
        ops.push(Op::Equal(old[usize::try_from(x).ok()?]));
        work = work.checked_add(1)?;
        if work > max_work {
            return None;
        }
    }
    while x > 0 {
        x = x.checked_sub(1)?;
        ops.push(Op::Delete(old[usize::try_from(x).ok()?]));
    }
    while y > 0 {
        y = y.checked_sub(1)?;
        ops.push(Op::Add(new[usize::try_from(y).ok()?]));
    }
    ops.reverse();
    Some((ops, work))
}

fn split_lines(text: &str) -> (Vec<SourceLine<'_>>, Ending) {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut ending = Ending::None;
    for (index, byte) in text.bytes().enumerate() {
        if byte != b'\n' {
            continue;
        }
        let crlf = index > start && text.as_bytes()[index - 1] == b'\r';
        let found = if crlf { Ending::CrLf } else { Ending::Lf };
        ending = match (ending, found) {
            (Ending::None, value) => value,
            (a, b) if a == b => a,
            _ => Ending::Mixed,
        };
        let end = if crlf { index - 1 } else { index };
        lines.push(SourceLine {
            text: &text[start..end],
            terminator: if crlf {
                LineTerminator::CrLf
            } else {
                LineTerminator::Lf
            },
        });
        start = index + 1;
    }
    if start < text.len() {
        lines.push(SourceLine {
            text: &text[start..],
            terminator: LineTerminator::None,
        });
    }
    (lines, ending)
}

fn lines_equal(left: SourceLine<'_>, right: SourceLine<'_>) -> bool {
    left.text == right.text && left.terminator == right.terminator
}

fn build_hunks(ops: &[Op<'_>], context: usize) -> Vec<Hunk> {
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(i, op)| (!matches!(op, Op::Equal(_))).then_some(i))
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut start = changed[0].saturating_sub(context);
    let mut end = changed[0].saturating_add(context + 1).min(ops.len());
    for &index in &changed[1..] {
        let next_start = index.saturating_sub(context);
        let next_end = index.saturating_add(context + 1).min(ops.len());
        if next_start <= end {
            end = end.max(next_end);
        } else {
            ranges.push((start, end));
            start = next_start;
            end = next_end;
        }
    }
    ranges.push((start, end));
    ranges
        .into_iter()
        .map(|(start, end)| {
            let (mut old_no, mut new_no) = (1, 1);
            for op in &ops[..start] {
                advance(*op, &mut old_no, &mut new_no);
            }
            let (old_start, new_start) = (old_no - 1, new_no - 1);
            let mut lines = Vec::new();
            for op in &ops[start..end] {
                let (kind, source, old_line, new_line) = match *op {
                    Op::Equal(line) => (LineKind::Context, line, Some(old_no), Some(new_no)),
                    Op::Delete(line) => (LineKind::Deletion, line, Some(old_no), None),
                    Op::Add(line) => (LineKind::Addition, line, None, Some(new_no)),
                };
                lines.push(DiffLine {
                    kind,
                    text: source.text.to_owned(),
                    old_line,
                    new_line,
                    has_terminator: source.terminator != LineTerminator::None,
                    terminator: source.terminator,
                });
                advance(*op, &mut old_no, &mut new_no);
            }
            Hunk {
                old_start,
                old_end: old_no - 1,
                new_start,
                new_end: new_no - 1,
                lines,
            }
        })
        .collect()
}

fn advance(op: Op<'_>, old: &mut usize, new: &mut usize) {
    match op {
        Op::Equal(_) => {
            *old += 1;
            *new += 1;
        }
        Op::Delete(_) => *old += 1,
        Op::Add(_) => *new += 1,
    }
}

fn mappings_for_hunk(hunk: &Hunk) -> Vec<LineMapping> {
    hunk.lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let navigation_new_line = line.new_line.unwrap_or_else(|| {
                hunk.lines[index + 1..]
                    .iter()
                    .find_map(|candidate| candidate.new_line)
                    .or_else(|| {
                        hunk.lines[..index]
                            .iter()
                            .rev()
                            .find_map(|candidate| candidate.new_line)
                    })
                    .unwrap_or(1)
            });
            LineMapping {
                kind: line.kind,
                old_line: line.old_line,
                new_line: line.new_line,
                navigation_new_line,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;
    fn meta(change: ChangeKind) -> DiffMetadata {
        DiffMetadata {
            old_path: None,
            new_path: None,
            change,
            old_mode: None,
            new_mode: None,
            unsupported: None,
        }
    }

    #[test]
    fn identified_diff_retains_snapshot_entry_and_comparison() {
        let request = DiffRequest {
            snapshot_id: "snap-1".into(),
            entry_id: "entry-1".into(),
            comparison: Comparison::Working,
        };
        let identified = compute_identified_diff(
            request.clone(),
            meta(ChangeKind::Modified),
            b"old\n",
            b"new\n",
            DiffLimits::default(),
        )
        .expect("non-empty identity");
        assert_eq!(identified.request, request);
        assert!(matches!(identified.result, DiffResult::Text(_)));
    }

    #[test]
    fn identified_diff_rejects_empty_identity_without_reading_bytes() {
        let request = DiffRequest {
            snapshot_id: String::new(),
            entry_id: "entry-1".into(),
            comparison: Comparison::Combined,
        };
        assert!(
            compute_identified_diff(
                request,
                meta(ChangeKind::Modified),
                &[0],
                &[0],
                DiffLimits::default(),
            )
            .is_none()
        );
    }
    fn text(old: &[u8], new: &[u8]) -> TextDiff {
        match compute_diff(meta(ChangeKind::Modified), old, new, DiffLimits::default()) {
            DiffResult::Text(v) => v,
            other @ DiffResult::MetadataOnly { .. } => panic!("{other:?}"),
        }
    }

    #[test]
    fn addition_deletion_and_replace_are_stable() {
        let d = text(b"a\nb\n", b"a\nc\nd\n");
        let kinds: Vec<_> = d.hunks[0].lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Context,
                LineKind::Deletion,
                LineKind::Addition,
                LineKind::Addition
            ]
        );
        assert_eq!(d.hunks[0].old_start..d.hunks[0].old_end, 0..2);
        assert_eq!(d.hunks[0].new_start..d.hunks[0].new_end, 0..3);
        assert_eq!(d, text(b"a\nb\n", b"a\nc\nd\n"));
    }
    #[test]
    fn untracked_and_deleted_versions_work() {
        assert!(matches!(
            compute_diff(
                meta(ChangeKind::Added),
                b"",
                b"new\n",
                DiffLimits::default()
            ),
            DiffResult::Text(_)
        ));
        let d = text(b"old\n", b"");
        assert_eq!(d.line_map[0].navigation_new_line, 1);
    }
    #[test]
    fn preserves_missing_final_newline() {
        let d = text(b"old", b"new\n");
        assert!(!d.old_ends_with_newline && d.new_ends_with_newline);
        assert!(!d.hunks[0].lines[0].has_terminator);
        let newline_only = text(b"same", b"same\n");
        assert_eq!(newline_only.hunks[0].lines.len(), 2);
    }
    #[test]
    fn preserves_and_reports_crlf_only_changes() {
        let d = text(b"same\r\n", b"same\n");
        assert_eq!(
            d.hunks[0]
                .lines
                .iter()
                .map(|line| line.kind)
                .collect::<Vec<_>>(),
            vec![LineKind::Deletion, LineKind::Addition]
        );
        assert_eq!(d.hunks[0].lines[0].terminator, LineTerminator::CrLf);
        assert_eq!(d.hunks[0].lines[1].terminator, LineTerminator::Lf);
    }
    #[test]
    fn rejects_binary_encoding_and_limits() {
        assert!(matches!(
            compute_diff(
                meta(ChangeKind::Modified),
                b"a\0",
                b"",
                DiffLimits::default()
            ),
            DiffResult::MetadataOnly {
                reason: MetadataReason::Binary,
                ..
            }
        ));
        assert!(matches!(
            compute_diff(
                meta(ChangeKind::Modified),
                &[0xff],
                b"",
                DiffLimits::default()
            ),
            DiffResult::MetadataOnly {
                reason: MetadataReason::UnsupportedEncoding,
                ..
            }
        ));
        let limits = DiffLimits {
            max_bytes_per_side: 1,
            ..DiffLimits::default()
        };
        assert!(matches!(
            compute_diff(meta(ChangeKind::Modified), b"ab", b"", limits),
            DiffResult::MetadataOnly {
                reason: MetadataReason::TooLarge,
                ..
            }
        ));
        let limits = DiffLimits {
            max_lines_per_side: 1,
            ..DiffLimits::default()
        };
        assert!(matches!(
            compute_diff(meta(ChangeKind::Modified), b"a\nb\n", b"", limits),
            DiffResult::MetadataOnly {
                reason: MetadataReason::TooManyLines,
                ..
            }
        ));
    }
    #[test]
    fn explicit_work_budget_is_enforced() {
        let limits = DiffLimits {
            max_work: 3,
            ..DiffLimits::default()
        };
        assert!(matches!(
            compute_diff(meta(ChangeKind::Modified), b"a\nb\n", b"c\nd\n", limits),
            DiffResult::MetadataOnly {
                reason: MetadataReason::DiffBudgetExceeded,
                ..
            }
        ));
    }
    #[test]
    fn large_unchanged_input_does_not_allocate_a_quadratic_matrix() {
        // This is deliberately beyond the previous 4,000,000-cell LCS
        // matrix ceiling. Myers follows the one matching diagonal instead.
        let mut input = String::new();
        for line in 0..10_000 {
            writeln!(input, "line-{line}").unwrap();
        }
        let diff = text(input.as_bytes(), input.as_bytes());
        assert_eq!(diff.algorithm, "bounded-myers-v1");
        assert!(diff.hunks.is_empty());
        assert!(diff.work_used <= 20_001);
    }
    #[test]
    fn retained_frontier_budget_fails_without_a_partial_diff() {
        let limits = DiffLimits {
            max_cells: 10,
            ..DiffLimits::default()
        };
        assert!(matches!(
            compute_diff(meta(ChangeKind::Modified), b"a\n", b"b\n", limits),
            DiffResult::MetadataOnly {
                reason: MetadataReason::DiffBudgetExceeded,
                ..
            }
        ));
    }
    #[test]
    fn deletion_navigation_prefers_next_then_previous() {
        let d = text(b"a\nb\nc\n", b"a\nc\n");
        let deletion = d
            .line_map
            .iter()
            .find(|m| m.kind == LineKind::Deletion)
            .unwrap();
        assert_eq!(deletion.old_line, Some(2));
        assert_eq!(deletion.navigation_new_line, 2);
    }
    #[test]
    fn metadata_classification_short_circuits_content() {
        let mut m = meta(ChangeKind::Modified);
        m.unsupported = Some(UnsupportedKind::Submodule);
        assert!(matches!(
            compute_diff(m, b"text", b"text", DiffLimits::default()),
            DiffResult::MetadataOnly {
                reason: MetadataReason::UnsupportedKind,
                ..
            }
        ));
    }

    fn append_source(output: &mut Vec<u8>, line: &SourceLine<'_>) {
        output.extend_from_slice(line.text.as_bytes());
        match line.terminator {
            LineTerminator::None => {}
            LineTerminator::Lf => output.push(b'\n'),
            LineTerminator::CrLf => output.extend_from_slice(b"\r\n"),
        }
    }

    fn append_diff_line(output: &mut Vec<u8>, line: &DiffLine) {
        output.extend_from_slice(line.text.as_bytes());
        match line.terminator {
            LineTerminator::None => {}
            LineTerminator::Lf => output.push(b'\n'),
            LineTerminator::CrLf => output.extend_from_slice(b"\r\n"),
        }
    }

    fn apply_hunks(old: &[u8], diff: &TextDiff) -> Vec<u8> {
        let (old_lines, _) = split_lines(std::str::from_utf8(old).unwrap());
        let mut old_cursor = 0;
        let mut reconstructed = Vec::new();
        for hunk in &diff.hunks {
            for line in &old_lines[old_cursor..hunk.old_start] {
                append_source(&mut reconstructed, line);
            }
            old_cursor = hunk.old_start;
            for line in &hunk.lines {
                match line.kind {
                    LineKind::Context => {
                        append_diff_line(&mut reconstructed, line);
                        old_cursor += 1;
                    }
                    LineKind::Deletion => old_cursor += 1,
                    LineKind::Addition => append_diff_line(&mut reconstructed, line),
                }
            }
            assert_eq!(old_cursor, hunk.old_end);
        }
        for line in &old_lines[old_cursor..] {
            append_source(&mut reconstructed, line);
        }
        reconstructed
    }

    #[test]
    fn applying_emitted_hunks_reconstructs_exact_right_side() {
        for (old, new) in [
            (&b""[..], &b"created\n"[..]),
            (&b"deleted\n"[..], &b""[..]),
            (&b"a\nb\nc\n"[..], &b"a\nchanged\nc\n"[..]),
            (&b"same"[..], &b"same\n"[..]),
            (
                "alpha\n雪\nomega\n".as_bytes(),
                "zero\nalpha\n雪だるま\nomega".as_bytes(),
            ),
            (&b"one\r\ntwo\r\n"[..], &b"one\r\nchanged\r\n"[..]),
            (&b"same\r\n"[..], &b"same\n"[..]),
        ] {
            let diff = text(old, new);
            assert_eq!(apply_hunks(old, &diff), new);
        }
    }
}
