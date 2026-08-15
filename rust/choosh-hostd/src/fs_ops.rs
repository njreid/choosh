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
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub enum FsError {
    OutOfRoot,
    NotFound,
    NotADirectory,
    NotAFile,
    Io(std::io::Error),
    BoundExceeded(String),
    /// A `workspace.file.write` body containing a null byte — the same
    /// heuristic real `git`/`jj` use to decide a file is binary (see
    /// `jj_ops.rs`'s diff tests), per `editor-protocol.md`'s "V1 MUST reject
    /// binary files" limit.
    BinaryContent,
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
            Self::BinaryContent => write!(f, "content is binary (contains a null byte); binary files are rejected for editing"),
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
/// `(bytes_read, total_file_size, revision)`, where `revision` is
/// [`content_revision`] over the file's *entire* current bytes (see that
/// function's docs for why a ranged read still hashes the whole file).
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
) -> Result<(Vec<u8>, u64, String), FsError> {
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
    // Hash the whole file (leaving the cursor at EOF) *before* seeking to
    // `offset` for the actual ranged read below — a partial read's revision
    // must still detect a concurrent write to a part of the file the client
    // never read (editor-protocol.md's conflict model is "the file changed
    // on disk", full stop, not "the part I looked at changed").
    let revision = hash_whole_file(&mut file)?;
    file.seek(SeekFrom::Start(offset))?;
    let capped_length = requested_length.min(total_size.saturating_sub(offset));
    let mut buffer = vec![0u8; usize::try_from(capped_length).unwrap_or(usize::MAX)];
    file.read_exact(&mut buffer)?;
    Ok((buffer, total_size, revision))
}

/// Writes `new_content` in full to `path`, replacing its current bytes —
/// the only mutating file RPC (`workspace.file.write`, `jj-integration.md`).
/// `base_revision` MUST match [`content_revision`] of the file's *current*
/// on-disk bytes or the write is rejected as [`WriteOutcome::Stale`] (not an
/// `Err` — a stale write is an expected, typed outcome carrying the current
/// state back to the caller per `jj-integration.md`, not a failure of this
/// function). No `jj` invocation happens here: per `jj-integration.md`,
/// "jj snapshots the new working-copy state automatically" on its own next
/// invocation — writing the bytes to `@`'s checkout is the entire effect
/// this RPC needs to produce.
///
/// # Errors
///
/// See [`confine`]; [`FsError::NotFound`] if `path` doesn't already exist
/// (a write, in this milestone, always follows a prior
/// `workspace.file.read` of an existing file — see `rpc.rs`'s module docs);
/// [`FsError::NotAFile`] if it is a directory; [`FsError::BoundExceeded`] if
/// `new_content` exceeds `max_content_bytes`; [`FsError::BinaryContent`] if
/// `new_content` contains a null byte.
pub fn write_file(
    root: &Path,
    path: &str,
    base_revision: &str,
    new_content: &[u8],
    max_content_bytes: u64,
) -> Result<WriteOutcome, FsError> {
    let resolved = confine(root, path)?;
    let metadata = std::fs::metadata(&resolved)?;
    if !metadata.is_file() {
        return Err(FsError::NotAFile);
    }

    let content_len = u64::try_from(new_content.len()).unwrap_or(u64::MAX);
    if content_len > max_content_bytes {
        return Err(FsError::BoundExceeded(format!("content is {content_len} bytes, max is {max_content_bytes}")));
    }
    if new_content.contains(&0u8) {
        return Err(FsError::BinaryContent);
    }

    // Read (not stream-hash) the current content: on a match this is the
    // common case and we're about to overwrite it anyway, and on a
    // mismatch the caller needs these exact bytes back per
    // `WriteOutcome::Stale`'s contract, so there is no cheaper path that
    // still meets it.
    let current_content = std::fs::read(&resolved)?;
    let current_revision = content_revision(&current_content);
    if current_revision != base_revision {
        return Ok(WriteOutcome::Stale { current_revision, current_content });
    }

    // Raw bytes through, unmodified — per editor-protocol.md: "Encoding and
    // line endings MUST round-trip byte-identical."
    std::fs::write(&resolved, new_content)?;
    let new_revision = content_revision(new_content);
    Ok(WriteOutcome::Written { new_revision })
}

/// [`write_file`]'s result: a stale `base_revision` is a normal, expected
/// outcome (not an `Err`) that carries the file's actual current state back
/// to the caller, per `jj-integration.md`'s `revision_stale` contract.
#[derive(Debug, Eq, PartialEq)]
pub enum WriteOutcome {
    Written { new_revision: String },
    Stale { current_revision: String, current_content: Vec<u8> },
}

/// Hex-encoded SHA-256 of `bytes` — the whole-file content-hash "revision"
/// token `workspace.file.read`/`workspace.file.write` use to detect a stale
/// write. `jj-integration.md` leaves the revision-identity mechanism an
/// implementation choice ("a plain change-detection token, not a jj
/// change/commit id"); a content hash of the file's current bytes is used
/// here rather than e.g. an mtime, which is coarser than most filesystems'
/// clock resolution and can alias two different writes within the same
/// tick.
#[must_use]
pub fn content_revision(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Streams `file`'s full content through SHA-256 without loading it all
/// into memory at once. Leaves `file`'s cursor at EOF — callers that also
/// need to read from an offset afterward (see [`read_file_range`]) must
/// `seek` back explicitly.
fn hash_whole_file(file: &mut std::fs::File) -> Result<String, FsError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_encode(&hasher.finalize()))
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
        let (bytes, total, revision) = read_file_range(dir.path(), "a.txt", None, 4 * 1024 * 1024).unwrap();
        assert_eq!(bytes, b"hello world");
        assert_eq!(total, 11);
        assert_eq!(revision, content_revision(b"hello world"));
    }

    #[test]
    fn read_file_range_honors_an_explicit_offset_and_length() {
        let dir = sample_root();
        let (bytes, total, revision) = read_file_range(dir.path(), "a.txt", Some((6, 5)), 4 * 1024 * 1024).unwrap();
        assert_eq!(bytes, b"world");
        assert_eq!(total, 11);
        // A ranged read's revision still reflects the WHOLE file, not just
        // the queried slice — this is what lets a partial read still detect
        // a concurrent write to a part of the file the client never read.
        assert_eq!(revision, content_revision(b"hello world"));
    }

    #[test]
    fn read_file_range_revision_changes_when_an_unread_part_of_the_file_changes() {
        let dir = sample_root();
        let (_, _, revision_before) = read_file_range(dir.path(), "a.txt", Some((0, 5)), 4 * 1024 * 1024).unwrap();
        // Modify a byte range this read never looked at.
        std::fs::write(dir.path().join("a.txt"), b"hello WORLD").unwrap();
        let (_, _, revision_after) = read_file_range(dir.path(), "a.txt", Some((0, 5)), 4 * 1024 * 1024).unwrap();
        assert_ne!(revision_before, revision_after, "revision must detect a change outside the previously-read range");
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

    #[test]
    fn content_revision_differs_for_different_content_and_matches_for_identical_content() {
        assert_ne!(content_revision(b"hello"), content_revision(b"hello!"));
        assert_eq!(content_revision(b"hello"), content_revision(b"hello"));
    }

    #[test]
    fn write_file_updates_content_and_returns_a_new_revision() {
        let dir = sample_root();
        let base_revision = content_revision(b"hello world");
        let outcome = write_file(dir.path(), "a.txt", &base_revision, b"goodbye world", 4 * 1024 * 1024).unwrap();
        let WriteOutcome::Written { new_revision } = outcome else { panic!("expected Written, got {outcome:?}") };
        assert_eq!(new_revision, content_revision(b"goodbye world"));
        assert_ne!(new_revision, base_revision);
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"goodbye world");
    }

    #[test]
    fn write_file_rejects_a_stale_base_revision_without_touching_the_file() {
        let dir = sample_root();
        let stale_revision = content_revision(b"this was never the real content");
        let outcome = write_file(dir.path(), "a.txt", &stale_revision, b"attempted overwrite", 4 * 1024 * 1024).unwrap();
        let WriteOutcome::Stale { current_revision, current_content } = outcome else {
            panic!("expected Stale, got {outcome:?}")
        };
        assert_eq!(current_revision, content_revision(b"hello world"));
        assert_eq!(current_content, b"hello world");
        // The file itself must be provably untouched by the rejected write.
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"hello world");
    }

    #[test]
    fn write_file_rejects_oversized_content() {
        let dir = sample_root();
        let base_revision = content_revision(b"hello world");
        let result = write_file(dir.path(), "a.txt", &base_revision, &[b'x'; 20], 10);
        assert!(matches!(result, Err(FsError::BoundExceeded(_))));
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"hello world");
    }

    #[test]
    fn write_file_rejects_binary_content_containing_a_null_byte() {
        let dir = sample_root();
        let base_revision = content_revision(b"hello world");
        let result = write_file(dir.path(), "a.txt", &base_revision, b"hello\0world", 4 * 1024 * 1024);
        assert!(matches!(result, Err(FsError::BinaryContent)));
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"hello world");
    }

    #[test]
    fn write_file_round_trips_mixed_line_endings_byte_identical() {
        let dir = sample_root();
        let mixed = b"line1\r\nline2\nline3\r\nline4\n";
        std::fs::write(dir.path().join("a.txt"), mixed).unwrap();
        let (read_before, _, revision) = read_file_range(dir.path(), "a.txt", None, 4 * 1024 * 1024).unwrap();
        assert_eq!(read_before, mixed);
        let outcome = write_file(dir.path(), "a.txt", &revision, mixed, 4 * 1024 * 1024).unwrap();
        assert!(matches!(outcome, WriteOutcome::Written { .. }));
        let (read_after, _, _) = read_file_range(dir.path(), "a.txt", None, 4 * 1024 * 1024).unwrap();
        assert_eq!(read_after, mixed, "mixed line endings must round-trip byte-identical through read -> write -> read");
    }

    #[test]
    fn write_file_rejects_a_path_that_does_not_yet_exist() {
        let dir = sample_root();
        let base_revision = content_revision(b"");
        let result = write_file(dir.path(), "brand_new.txt", &base_revision, b"content", 4 * 1024 * 1024);
        assert!(matches!(result, Err(FsError::NotFound)));
    }

    #[test]
    fn write_file_rejects_a_directory() {
        let dir = sample_root();
        let result = write_file(dir.path(), "sub", "irrelevant", b"content", 4 * 1024 * 1024);
        assert!(matches!(result, Err(FsError::NotAFile)));
    }
}
