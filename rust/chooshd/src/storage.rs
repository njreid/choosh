//! Atomic storage for the daemon's opaque, versioned state bytes.
//!
//! The caller injects the complete state path and a deterministic temporary
//! suffix. The containing directory is expected to have already passed the
//! daemon's private-directory checks.

#![cfg(unix)]

use std::fmt;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

const PRIVATE_FILE_MODE: u32 = 0o600;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateStorage {
    path: PathBuf,
    max_bytes: usize,
}

impl StateStorage {
    /// Constructs storage for an absolute, normalized state-file path.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidConfiguration`] when the byte limit is
    /// zero or the path is not absolute, normalized, and file-like.
    pub fn new(path: impl Into<PathBuf>, max_bytes: usize) -> Result<Self, StorageError> {
        let path = path.into();
        if max_bytes == 0
            || !path.is_absolute()
            || path.file_name().is_none()
            || path.parent().is_none()
            || path
                .components()
                .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        {
            return Err(StorageError::InvalidConfiguration);
        }
        Ok(Self { path, max_bytes })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the complete opaque state, or returns `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the existing entry is unsafe, exceeds the
    /// configured bound, changes while opening, or cannot be inspected/read.
    pub fn read(&self) -> Result<Option<Vec<u8>>, StorageError> {
        let expected = match fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(StorageOperation::Inspect, &error)),
        };
        validate_existing(&expected)?;
        let expected_len =
            usize::try_from(expected.len()).map_err(|_| StorageError::LimitExceeded)?;
        if expected_len > self.max_bytes {
            return Err(StorageError::LimitExceeded);
        }

        let file =
            File::open(&self.path).map_err(|error| io_error(StorageOperation::Open, &error))?;
        let opened = file
            .metadata()
            .map_err(|error| io_error(StorageOperation::Inspect, &error))?;
        if !opened.is_file() || opened.dev() != expected.dev() || opened.ino() != expected.ino() {
            return Err(StorageError::UnsafeExistingFile);
        }

        let bound = u64::try_from(self.max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let mut bytes = Vec::with_capacity(expected_len);
        file.take(bound)
            .read_to_end(&mut bytes)
            .map_err(|error| io_error(StorageOperation::Read, &error))?;
        if bytes.len() > self.max_bytes {
            return Err(StorageError::LimitExceeded);
        }
        Ok(Some(bytes))
    }

    /// Atomically replaces the state after fully syncing a private sibling temp.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the payload exceeds the bound, the suffix is
    /// unsafe, an existing target is unsafe, or an atomic-write step fails.
    pub fn replace(&self, bytes: &[u8], suffix: &str) -> Result<(), StorageError> {
        if bytes.len() > self.max_bytes {
            return Err(StorageError::LimitExceeded);
        }
        validate_suffix(suffix)?;
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) => validate_existing(&metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(StorageOperation::Inspect, &error)),
        }

        let file_name = self
            .path
            .file_name()
            .ok_or(StorageError::InvalidConfiguration)?
            .to_string_lossy();
        let temp_path = self
            .path
            .with_file_name(format!(".{file_name}.tmp-{suffix}"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(PRIVATE_FILE_MODE);
        let mut file = options
            .open(&temp_path)
            .map_err(|error| io_error(StorageOperation::CreateTemporary, &error))?;
        let mut cleanup = TemporaryFile::new(temp_path.clone());

        file.write_all(bytes)
            .map_err(|error| io_error(StorageOperation::Write, &error))?;
        file.sync_all()
            .map_err(|error| io_error(StorageOperation::SyncFile, &error))?;
        fs::set_permissions(&temp_path, Permissions::from_mode(PRIVATE_FILE_MODE))
            .map_err(|error| io_error(StorageOperation::SetPermissions, &error))?;
        drop(file);
        fs::rename(&temp_path, &self.path)
            .map_err(|error| io_error(StorageOperation::Rename, &error))?;
        cleanup.disarm();
        let parent = self
            .path
            .parent()
            .ok_or(StorageError::InvalidConfiguration)?;
        sync_directory(parent)?;
        Ok(())
    }
}

fn validate_existing(metadata: &fs::Metadata) -> Result<(), StorageError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(StorageError::UnsafeExistingFile);
    }
    if metadata.mode() & 0o777 != PRIVATE_FILE_MODE {
        return Err(StorageError::UnsafeExistingFile);
    }
    Ok(())
}

fn validate_suffix(suffix: &str) -> Result<(), StorageError> {
    if suffix.is_empty()
        || suffix.len() > 128
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(StorageError::InvalidTemporarySuffix);
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), StorageError> {
    match File::open(path).and_then(|directory| directory.sync_all()) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(io_error(StorageOperation::SyncDirectory, &error)),
    }
}

struct TemporaryFile {
    path: PathBuf,
    armed: bool,
}

impl TemporaryFile {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageOperation {
    Inspect,
    Open,
    Read,
    CreateTemporary,
    Write,
    SyncFile,
    SetPermissions,
    Rename,
    SyncDirectory,
}

#[derive(Debug)]
pub enum StorageError {
    InvalidConfiguration,
    InvalidTemporarySuffix,
    LimitExceeded,
    UnsafeExistingFile,
    Io {
        operation: StorageOperation,
        kind: io::ErrorKind,
    },
}

fn io_error(operation: StorageOperation, error: &io::Error) -> StorageError {
    StorageError::Io {
        operation,
        kind: error.kind(),
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration => {
                formatter.write_str("invalid state storage configuration")
            }
            Self::InvalidTemporarySuffix => formatter.write_str("invalid temporary-file suffix"),
            Self::LimitExceeded => formatter.write_str("state byte limit exceeded"),
            Self::UnsafeExistingFile => formatter.write_str("unsafe existing state file"),
            Self::Io { operation, kind } => {
                write!(formatter, "state storage {operation:?} failed: {kind:?}")
            }
        }
    }
}

impl std::error::Error for StorageError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "choosh-storage-test-{}-{serial}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn state(&self, max: usize) -> StateStorage {
            StateStorage::new(self.0.join("state.bin"), max).unwrap()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn round_trip_and_atomic_replacement() {
        let directory = TestDirectory::new();
        let storage = directory.state(16);
        assert_eq!(storage.read().unwrap(), None);
        storage.replace(b"old", "first").unwrap();
        storage.replace(b"new-state", "second").unwrap();
        assert_eq!(storage.read().unwrap(), Some(b"new-state".to_vec()));
        assert_eq!(
            fs::symlink_metadata(storage.path()).unwrap().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn enforces_read_and_write_limits() {
        let directory = TestDirectory::new();
        let storage = directory.state(3);
        assert!(matches!(
            storage.replace(b"four", "write"),
            Err(StorageError::LimitExceeded)
        ));
        fs::write(storage.path(), b"four").unwrap();
        fs::set_permissions(storage.path(), Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(storage.read(), Err(StorageError::LimitExceeded)));
    }

    #[test]
    fn rejects_symlink_and_non_private_target() {
        let directory = TestDirectory::new();
        let storage = directory.state(16);
        let destination = directory.0.join("elsewhere");
        fs::write(&destination, b"secret").unwrap();
        symlink(&destination, storage.path()).unwrap();
        assert!(matches!(
            storage.read(),
            Err(StorageError::UnsafeExistingFile)
        ));
        assert!(matches!(
            storage.replace(b"x", "link"),
            Err(StorageError::UnsafeExistingFile)
        ));
        assert_eq!(fs::read(destination).unwrap(), b"secret");
    }

    #[test]
    fn preserves_preexisting_temp_and_old_state_on_collision() {
        let directory = TestDirectory::new();
        let storage = directory.state(16);
        storage.replace(b"old", "initial").unwrap();
        let temporary = directory.0.join(".state.bin.tmp-collision");
        fs::write(&temporary, b"partial").unwrap();
        assert!(matches!(
            storage.replace(b"new", "collision"),
            Err(StorageError::Io {
                operation: StorageOperation::CreateTemporary,
                ..
            })
        ));
        assert_eq!(fs::read(temporary).unwrap(), b"partial");
        assert_eq!(storage.read().unwrap(), Some(b"old".to_vec()));
    }

    #[test]
    fn invalid_suffix_cannot_escape_or_create_temp() {
        let directory = TestDirectory::new();
        let storage = directory.state(16);
        assert!(matches!(
            storage.replace(b"x", "../escape"),
            Err(StorageError::InvalidTemporarySuffix)
        ));
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 0);
    }
}
