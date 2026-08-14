//! Root-confined filesystem access for the live working copy (`@`), per
//! `host-rpc.md`'s "Root confinement" section and `jj-integration.md`'s
//! "For `@`, `hostd` reads the real files on disk... with the same
//! root-confinement and range/bound discipline" as the pre-relay SFTP
//! path. M1 only ever reads `@` — historical-revision reads through
//! `jj-lib`'s content-addressed store are a later increment's scope (see
//! `jj_ops.rs`'s module docs for the same "reported CLI-fallback, not
//! `jj-lib`" posture this module's caller, `rpc.rs`, inherits).

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use choosh_protocol::host_rpc::{ChangeKind, TreeEntry, TreeEntryKind};

#[derive(Debug)]
pub enum FsError {
    OutOfRoot,
    NotFound,
    NotADirectory,
    NotAFile,
    Io(std::io::Error),
    BoundExceeded(String),
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfRoot => write!(f, "path resolves outside the workspace root"),
            Self::NotFound => write!(f, "path does not exist"),
            Self::NotADirectory => write!(f, "path is not a directory"),
            Self::NotAFile => write!(f, "path is not a regular file"),
            // Deliberately not embedding the OS error's message, which can
            // echo back the raw path — host-rpc.md's error model forbids
            // that leaking into a caller-facing message.
            Self::Io(_) => write!(f, "I/O error"),
            Self::BoundExceeded(reason) => write!(f, "bound exceeded: {reason}"),
        }
    }
}

impl std::error::Error for FsError {}

impl From<std::io::Error> for FsError {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::NotFound {
            Self::NotFound
        } else {
            Self::Io(error)
        }
    }
}

/// Resolves `relative` (a `/`-separated path relative to `root`, as sent
/// over RPC — never a raw OS path from the caller) against `root`,
/// canonicalizes it, and verifies the result is still under `root`'s own
/// canonical form.
///
/// # Errors
///
/// Returns [`FsError::OutOfRoot`] for `..` traversal, an absolute
/// `relative` path, or a symlink that resolves outside `root` — per
/// `host-rpc.md`, this is a hard rejection, never a silent clamp back
/// under the root.
pub fn confine(root: &Path, relative: &str) -> Result<PathBuf, FsError> {
    if relative.starts_with('/') || relative.split('/').any(|segment| segment == "..") {
        return Err(FsError::OutOfRoot);
    }
    let canonical_root = root.canonicalize()?;
    let candidate = canonical_root.join(relative);
    if candidate == canonical_root {
        // `relative` is empty (listing/reading the workspace root itself).
        // The root trivially confines itself — there is no parent to
        // check, and `candidate.parent()` below would walk one level
        // *above* the root and wrongly reject it as out-of-root.
        return Ok(canonical_root);
    }
    // The candidate itself may not exist (a caller probing a bogus path),
    // but its parent must, and must still resolve under the root — this
    // catches a symlinked ancestor directory escaping the root even when
    // the final path segment doesn't exist yet.
    let parent = candidate.parent().unwrap_or(&candidate);
    let canonical_parent = if parent.exists() { parent.canonicalize()? } else { return Err(FsError::NotFound) };
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(FsError::OutOfRoot);
    }
    let resolved = canonical_parent.join(candidate.file_name().unwrap_or_default());
    if resolved.exists() && !resolved.canonicalize()?.starts_with(&canonical_root) {
        return Err(FsError::OutOfRoot);
    }
    Ok(resolved)
}

/// One page of a single directory level (`host-rpc.md`: "Directory
/// traversal depth per `tree.list` call: one level; recursion is
/// client-driven"), sorted by name for a stable cursor. `cursor` is the
/// last-seen entry name from a prior page; entries are returned strictly
/// after it.
///
/// # Errors
///
/// See [`confine`]; also returns [`FsError::NotADirectory`] if
/// `path_prefix` resolves to a file.
pub fn list_dir(root: &Path, path_prefix: &str, cursor: Option<&str>, page_size: usize) -> Result<(Vec<TreeEntry>, Option<String>), FsError> {
    let dir = confine(root, path_prefix)?;
    if !dir.is_dir() {
        return Err(if dir.exists() { FsError::NotADirectory } else { FsError::NotFound });
    }
    let mut names: Vec<(String, bool)> = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let is_dir = entry.file_type().ok()?.is_dir();
            Some((name, is_dir))
        })
        .collect();
    names.sort_by(|a, b| a.0.cmp(&b.0));

    let start = match cursor {
        Some(after) => names.partition_point(|(name, _)| name.as_str() <= after),
        None => 0,
    };
    let page: Vec<_> = names[start..].iter().take(page_size).collect();
    let next_cursor = if start + page.len() < names.len() { page.last().map(|(name, _)| name.clone()) } else { None };

    let entries = page
        .into_iter()
        .map(|(name, is_dir)| TreeEntry {
            name: name.clone(),
            kind: if *is_dir { TreeEntryKind::Directory } else { TreeEntryKind::File },
            // M1 has no jj-conflict awareness in the direct-filesystem
            // path — see this module's top-level doc comment and
            // `jj_ops.rs`'s matching gap for `workspace.status`.
            conflicted: false,
        })
        .collect();
    Ok((entries, next_cursor))
}

/// Reads up to `max_range_bytes` of `path` starting at `offset` (or the
/// whole file, capped the same way, if `range` is `None`). Returns
/// `(bytes_read, total_file_size)`.
///
/// # Errors
///
/// See [`confine`]; also [`FsError::NotAFile`] if `path` is a directory,
/// and [`FsError::BoundExceeded`] if a requested range's length exceeds
/// `max_range_bytes`.
pub fn read_file_range(
    root: &Path,
    path: &str,
    range: Option<(u64, u64)>,
    max_range_bytes: u64,
) -> Result<(Vec<u8>, u64), FsError> {
    let resolved = confine(root, path)?;
    let metadata = std::fs::metadata(&resolved)?;
    if !metadata.is_file() {
        return Err(FsError::NotAFile);
    }
    let total_size = metadata.len();

    let (offset, requested_length) = range.unwrap_or((0, total_size.min(max_range_bytes)));
    if requested_length > max_range_bytes {
        return Err(FsError::BoundExceeded(format!("requested {requested_length} bytes, max is {max_range_bytes}")));
    }

    let mut file = std::fs::File::open(&resolved)?;
    file.seek(SeekFrom::Start(offset))?;
    let capped_length = requested_length.min(total_size.saturating_sub(offset));
    let mut buffer = vec![0u8; usize::try_from(capped_length).unwrap_or(usize::MAX)];
    file.read_exact(&mut buffer)?;
    Ok((buffer, total_size))
}

/// Maps `jj_ops::ChangeKind` to the shared wire type, kept as a free
/// function (rather than a `From` impl in either crate) since it's the
/// only place these two enums need to meet.
#[must_use]
pub fn to_wire_change_kind(kind: crate::jj_ops::ChangeKind) -> ChangeKind {
    match kind {
        crate::jj_ops::ChangeKind::Added => ChangeKind::Added,
        crate::jj_ops::ChangeKind::Modified => ChangeKind::Modified,
        crate::jj_ops::ChangeKind::Deleted => ChangeKind::Deleted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello world").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("b.txt"), b"nested").unwrap();
        dir
    }

    #[test]
    fn confine_rejects_dot_dot_traversal() {
        let dir = sample_root();
        assert!(matches!(confine(dir.path(), "../etc/passwd"), Err(FsError::OutOfRoot)));
    }

    #[test]
    fn confine_rejects_absolute_paths() {
        let dir = sample_root();
        assert!(matches!(confine(dir.path(), "/etc/passwd"), Err(FsError::OutOfRoot)));
    }

    #[test]
    #[cfg(unix)]
    fn confine_rejects_a_symlink_escaping_the_root() {
        let dir = sample_root();
        std::os::unix::fs::symlink("/etc", dir.path().join("escape")).unwrap();
        assert!(matches!(confine(dir.path(), "escape/passwd"), Err(FsError::OutOfRoot)));
    }

    #[test]
    fn confine_accepts_a_legitimate_nested_path() {
        let dir = sample_root();
        let resolved = confine(dir.path(), "sub/b.txt").unwrap();
        assert!(resolved.ends_with("sub/b.txt"));
    }

    #[test]
    fn list_dir_lists_one_level_sorted() {
        let dir = sample_root();
        let (entries, next_cursor) = list_dir(dir.path(), "", None, 500).unwrap();
        assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), vec!["a.txt", "sub"]);
        assert!(next_cursor.is_none());
    }

    #[test]
    fn list_dir_paginates_with_a_cursor() {
        let dir = sample_root();
        let (first_page, cursor) = list_dir(dir.path(), "", None, 1).unwrap();
        assert_eq!(first_page.len(), 1);
        let cursor = cursor.expect("more entries remain");
        let (second_page, next_cursor) = list_dir(dir.path(), "", Some(&cursor), 1).unwrap();
        assert_eq!(second_page.len(), 1);
        assert_ne!(first_page[0].name, second_page[0].name);
        assert!(next_cursor.is_none());
    }

    #[test]
    fn read_file_range_reads_whole_small_file_by_default() {
        let dir = sample_root();
        let (bytes, total) = read_file_range(dir.path(), "a.txt", None, 4 * 1024 * 1024).unwrap();
        assert_eq!(bytes, b"hello world");
        assert_eq!(total, 11);
    }

    #[test]
    fn read_file_range_honors_an_explicit_offset_and_length() {
        let dir = sample_root();
        let (bytes, total) = read_file_range(dir.path(), "a.txt", Some((6, 5)), 4 * 1024 * 1024).unwrap();
        assert_eq!(bytes, b"world");
        assert_eq!(total, 11);
    }

    #[test]
    fn read_file_range_rejects_a_range_exceeding_the_bound() {
        let dir = sample_root();
        let result = read_file_range(dir.path(), "a.txt", Some((0, 100)), 10);
        assert!(matches!(result, Err(FsError::BoundExceeded(_))));
    }

    #[test]
    fn read_file_range_rejects_a_directory() {
        let dir = sample_root();
        assert!(matches!(read_file_range(dir.path(), "sub", None, 4096), Err(FsError::NotAFile)));
    }
}
