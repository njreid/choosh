//! Hardened, shell-free Git status planning and parsing.

use std::path::{Path, PathBuf};

/// A command description that an outer process-launch capability may execute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitCommandPlan {
    program: &'static str,
    arguments: Vec<&'static str>,
    environment: Vec<(&'static str, &'static str)>,
    current_dir: PathBuf,
    clear_environment: bool,
}

impl GitCommandPlan {
    /// Builds the fixed, non-interactive porcelain-status invocation.
    #[must_use]
    pub fn status(repository: &Path) -> Self {
        Self {
            program: "git",
            arguments: vec![
                "--no-pager",
                "--no-optional-locks",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "diff.external=",
                "-c",
                "diff.trustExitCode=false",
                "-c",
                "pager.status=false",
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--renames",
                "--no-ahead-behind",
            ],
            environment: vec![
                ("LC_ALL", "C"),
                ("LANG", "C"),
                ("GIT_TERMINAL_PROMPT", "0"),
                ("GIT_PAGER", "cat"),
                ("GIT_EXTERNAL_DIFF", ""),
            ],
            current_dir: repository.to_path_buf(),
            clear_environment: true,
        }
    }

    /// Program to execute directly, without a shell.
    #[must_use]
    pub const fn program(&self) -> &'static str {
        self.program
    }

    /// Fixed argument vector.
    #[must_use]
    pub fn arguments(&self) -> &[&'static str] {
        &self.arguments
    }

    /// Environment overrides required by the invocation.
    ///
    /// The executor should clear inherited environment before applying these values.
    #[must_use]
    pub fn environment(&self) -> &[(&'static str, &'static str)] {
        &self.environment
    }

    /// Registered repository root used as the process working directory.
    #[must_use]
    pub fn current_dir(&self) -> &Path {
        &self.current_dir
    }

    /// Whether the executor must clear inherited environment first.
    #[must_use]
    pub const fn clear_environment(&self) -> bool {
        self.clear_environment
    }
}

/// Resource limits applied before and during status parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusLimits {
    /// Maximum bytes accepted from Git.
    pub max_bytes: usize,
    /// Maximum changed entries accepted.
    pub max_entries: usize,
    /// Maximum bytes accepted in one path.
    pub max_path_bytes: usize,
}

/// A porcelain status code for one side of an entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChangeKind {
    Unmodified,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    UpdatedButUnmerged,
    Untracked,
    Ignored,
}

/// One immutable entry from a complete status snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusEntry {
    staged: ChangeKind,
    unstaged: ChangeKind,
    old_path: Option<Vec<u8>>,
    new_path: Vec<u8>,
}

impl StatusEntry {
    #[must_use]
    pub const fn staged(&self) -> ChangeKind {
        self.staged
    }

    #[must_use]
    pub const fn unstaged(&self) -> ChangeKind {
        self.unstaged
    }

    #[must_use]
    pub fn old_path(&self) -> Option<&[u8]> {
        self.old_path.as_deref()
    }

    #[must_use]
    pub fn new_path(&self) -> &[u8] {
        &self.new_path
    }
}

/// A complete parsed status result. Partial results are never exposed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusSnapshot {
    entries: Vec<StatusEntry>,
}

impl StatusSnapshot {
    #[must_use]
    pub fn entries(&self) -> &[StatusEntry] {
        &self.entries
    }
}

/// Deterministic status rejection reasons.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusParseError {
    OutputTooLarge,
    TooManyEntries,
    PathTooLong,
    TruncatedRecord,
    MalformedRecord,
    UnsupportedStatus,
    UnsafePath,
}

/// Parses `git status --porcelain=v1 -z` output without decoding path bytes.
///
/// # Errors
///
/// Returns a bounded [`StatusParseError`] for oversized, truncated, malformed,
/// unsupported, or unsafe input. No partial snapshot is returned.
pub fn parse_status(
    input: &[u8],
    limits: StatusLimits,
) -> Result<StatusSnapshot, StatusParseError> {
    if input.len() > limits.max_bytes {
        return Err(StatusParseError::OutputTooLarge);
    }

    let mut cursor = 0;
    let mut entries = Vec::new();
    while cursor < input.len() {
        if entries.len() == limits.max_entries {
            return Err(StatusParseError::TooManyEntries);
        }
        let record = next_field(input, &mut cursor)?;
        if record.len() < 4 || record[2] != b' ' {
            return Err(StatusParseError::MalformedRecord);
        }
        let staged = parse_kind(record[0])?;
        let unstaged = parse_kind(record[1])?;
        if (matches!(staged, ChangeKind::Untracked | ChangeKind::Ignored)
            || matches!(unstaged, ChangeKind::Untracked | ChangeKind::Ignored))
            && staged != unstaged
        {
            return Err(StatusParseError::MalformedRecord);
        }
        let new_path = &record[3..];
        validate_path(new_path, limits.max_path_bytes)?;

        let old_path = if matches!(staged, ChangeKind::Renamed | ChangeKind::Copied)
            || matches!(unstaged, ChangeKind::Renamed | ChangeKind::Copied)
        {
            let old = next_field(input, &mut cursor)?;
            validate_path(old, limits.max_path_bytes)?;
            Some(old.to_vec())
        } else {
            None
        };
        entries.push(StatusEntry {
            staged,
            unstaged,
            old_path,
            new_path: new_path.to_vec(),
        });
    }
    Ok(StatusSnapshot { entries })
}

fn next_field<'a>(input: &'a [u8], cursor: &mut usize) -> Result<&'a [u8], StatusParseError> {
    let remainder = &input[*cursor..];
    let end = remainder
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(StatusParseError::TruncatedRecord)?;
    *cursor += end + 1;
    Ok(&remainder[..end])
}

fn validate_path(path: &[u8], max_bytes: usize) -> Result<(), StatusParseError> {
    if path.is_empty() {
        Err(StatusParseError::MalformedRecord)
    } else if path.len() > max_bytes {
        Err(StatusParseError::PathTooLong)
    } else if path[0] == b'/'
        || path
            .split(|byte| *byte == b'/')
            .any(|component| component.is_empty() || component == b"." || component == b"..")
    {
        Err(StatusParseError::UnsafePath)
    } else {
        Ok(())
    }
}

fn parse_kind(value: u8) -> Result<ChangeKind, StatusParseError> {
    match value {
        b' ' => Ok(ChangeKind::Unmodified),
        b'M' => Ok(ChangeKind::Modified),
        b'A' => Ok(ChangeKind::Added),
        b'D' => Ok(ChangeKind::Deleted),
        b'R' => Ok(ChangeKind::Renamed),
        b'C' => Ok(ChangeKind::Copied),
        b'U' => Ok(ChangeKind::UpdatedButUnmerged),
        b'?' => Ok(ChangeKind::Untracked),
        b'!' => Ok(ChangeKind::Ignored),
        _ => Err(StatusParseError::UnsupportedStatus),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: StatusLimits = StatusLimits {
        max_bytes: 256,
        max_entries: 4,
        max_path_bytes: 64,
    };

    #[test]
    fn command_is_shell_free_and_hardened() {
        let plan = GitCommandPlan::status(Path::new("repo; touch escaped"));
        assert_eq!(plan.program(), "git");
        assert_eq!(plan.current_dir(), Path::new("repo; touch escaped"));
        assert!(plan.clear_environment());
        assert!(plan.arguments().contains(&"--no-pager"));
        assert!(plan.arguments().contains(&"core.hooksPath=/dev/null"));
        assert!(plan.arguments().contains(&"diff.external="));
        assert!(plan.arguments().contains(&"-z"));
        assert_eq!(
            plan.environment()
                .iter()
                .find(|(key, _)| *key == "GIT_TERMINAL_PROMPT")
                .map(|(_, value)| *value),
            Some("0")
        );
    }

    #[test]
    fn parses_hostile_paths_as_opaque_bytes() {
        let input = b"M  line\nwith spaces\xff\0?? --looks-like-option\0";
        let snapshot = parse_status(input, LIMITS).unwrap();
        assert_eq!(snapshot.entries().len(), 2);
        assert_eq!(snapshot.entries()[0].new_path(), b"line\nwith spaces\xff");
        assert_eq!(snapshot.entries()[1].new_path(), b"--looks-like-option");
        assert_eq!(snapshot.entries()[1].staged(), ChangeKind::Untracked);
    }

    #[test]
    fn preserves_porcelain_z_rename_pair_order() {
        let snapshot = parse_status(b"R  new name\0old\nname\0", LIMITS).unwrap();
        let entry = &snapshot.entries()[0];
        assert_eq!(entry.new_path(), b"new name");
        assert_eq!(entry.old_path(), Some(b"old\nname".as_slice()));
        assert_eq!(entry.staged(), ChangeKind::Renamed);
    }

    #[test]
    fn rejects_truncated_and_malformed_records() {
        assert_eq!(
            parse_status(b"M  unterminated", LIMITS),
            Err(StatusParseError::TruncatedRecord)
        );
        assert_eq!(
            parse_status(b"M?bad\0", LIMITS),
            Err(StatusParseError::MalformedRecord)
        );
        assert_eq!(
            parse_status(b"R  new\0", LIMITS),
            Err(StatusParseError::TruncatedRecord)
        );
        assert_eq!(
            parse_status(b"X  path\0", LIMITS),
            Err(StatusParseError::UnsupportedStatus)
        );
    }

    #[test]
    fn enforces_all_limits_without_partial_results() {
        let tiny_output = StatusLimits {
            max_bytes: 3,
            ..LIMITS
        };
        assert_eq!(
            parse_status(b"?? a\0", tiny_output),
            Err(StatusParseError::OutputTooLarge)
        );

        let one_entry = StatusLimits {
            max_entries: 1,
            ..LIMITS
        };
        assert_eq!(
            parse_status(b"?? a\0?? b\0", one_entry),
            Err(StatusParseError::TooManyEntries)
        );

        let short_path = StatusLimits {
            max_path_bytes: 1,
            ..LIMITS
        };
        assert_eq!(
            parse_status(b"?? ab\0", short_path),
            Err(StatusParseError::PathTooLong)
        );
    }

    #[test]
    fn rejects_non_relative_and_inconsistent_special_entries() {
        for input in [
            b"?? /absolute\0".as_slice(),
            b"?? ../escape\0",
            b"?? nested/../escape\0",
        ] {
            assert_eq!(
                parse_status(input, LIMITS),
                Err(StatusParseError::UnsafePath)
            );
        }
        assert_eq!(
            parse_status(b"?M impossible\0", LIMITS),
            Err(StatusParseError::MalformedRecord)
        );
    }
}
