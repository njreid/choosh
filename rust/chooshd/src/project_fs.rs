//! Bounded, fail-closed access to files beneath a registered project root.
//!
//! Resolution checks both the lexical path and its canonical destination. A
//! prepared access records filesystem identity so a later open rejects a
//! symlink or directory swap instead of silently following it.

#![cfg(unix)]

use std::fmt;
use std::fs::{self, File};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// Limits applied before consulting the filesystem.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectPathLimits {
    pub max_bytes: usize,
    pub max_components: usize,
}

impl ProjectPathLimits {
    /// Creates nonzero relative-path limits.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectFsError::InvalidConfiguration`] for a zero limit.
    pub fn new(max_bytes: usize, max_components: usize) -> Result<Self, ProjectFsError> {
        if max_bytes == 0 || max_components == 0 {
            return Err(ProjectFsError::InvalidConfiguration);
        }
        Ok(Self {
            max_bytes,
            max_components,
        })
    }
}

/// A canonical directory identity captured when a workspace is registered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredProjectRoot {
    canonical: PathBuf,
    device: u64,
    inode: u64,
    limits: ProjectPathLimits,
}

impl RegisteredProjectRoot {
    /// Canonicalizes and records an existing, real project directory.
    ///
    /// # Errors
    ///
    /// Fails when limits are invalid, the root cannot be canonicalized, or the
    /// resulting entry is not a directory.
    pub fn register(
        root: impl AsRef<Path>,
        limits: ProjectPathLimits,
    ) -> Result<Self, ProjectFsError> {
        if limits.max_bytes == 0 || limits.max_components == 0 {
            return Err(ProjectFsError::InvalidConfiguration);
        }
        let canonical = fs::canonicalize(root.as_ref())
            .map_err(|error| io_error(ProjectFsOperation::CanonicalizeRoot, &error))?;
        let metadata = fs::symlink_metadata(&canonical)
            .map_err(|error| io_error(ProjectFsOperation::InspectRoot, &error))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(ProjectFsError::UnsafeRoot);
        }
        Ok(Self {
            canonical,
            device: metadata.dev(),
            inode: metadata.ino(),
            limits,
        })
    }

    #[must_use]
    pub fn canonical_path(&self) -> &Path {
        &self.canonical
    }

    /// Resolves a bounded relative path and captures its current identity.
    ///
    /// # Errors
    ///
    /// Rejects malformed paths, canonical escapes, changed roots, non-files,
    /// and filesystem inspection failures.
    pub fn prepare(
        &self,
        relative: impl AsRef<Path>,
    ) -> Result<PreparedProjectFile, ProjectFsError> {
        self.validate_root_identity()?;
        let relative = relative.as_ref();
        validate_relative(relative, self.limits)?;
        let lexical = self.canonical.join(relative);
        let canonical = fs::canonicalize(&lexical)
            .map_err(|error| io_error(ProjectFsOperation::CanonicalizeTarget, &error))?;
        if !canonical.starts_with(&self.canonical) || canonical == self.canonical {
            return Err(ProjectFsError::PathEscape);
        }
        let metadata = fs::symlink_metadata(&canonical)
            .map_err(|error| io_error(ProjectFsOperation::InspectTarget, &error))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(ProjectFsError::NotRegularFile);
        }
        Ok(PreparedProjectFile {
            lexical,
            canonical,
            root_device: self.device,
            root_inode: self.inode,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    /// Opens a prepared file only if the root and target identities still match.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectFsError::IdentityChanged`] when an entry was swapped
    /// after preparation, or a typed I/O error when opening/inspection fails.
    pub fn open_prepared(&self, prepared: &PreparedProjectFile) -> Result<File, ProjectFsError> {
        if prepared.root_device != self.device || prepared.root_inode != self.inode {
            return Err(ProjectFsError::WrongRoot);
        }
        self.validate_root_identity()?;
        let file = File::open(&prepared.lexical)
            .map_err(|error| io_error(ProjectFsOperation::OpenTarget, &error))?;
        let metadata = file
            .metadata()
            .map_err(|error| io_error(ProjectFsOperation::InspectOpenedTarget, &error))?;
        if !metadata.is_file()
            || metadata.dev() != prepared.device
            || metadata.ino() != prepared.inode
        {
            return Err(ProjectFsError::IdentityChanged);
        }
        let opened_path = fs::canonicalize(&prepared.lexical)
            .map_err(|error| io_error(ProjectFsOperation::CanonicalizeOpenedTarget, &error))?;
        if opened_path != prepared.canonical || !opened_path.starts_with(&self.canonical) {
            return Err(ProjectFsError::IdentityChanged);
        }
        Ok(file)
    }

    /// Prepares and opens a file in one bounded operation.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::prepare`] and
    /// [`Self::open_prepared`].
    pub fn open(&self, relative: impl AsRef<Path>) -> Result<File, ProjectFsError> {
        let prepared = self.prepare(relative)?;
        self.open_prepared(&prepared)
    }

    fn validate_root_identity(&self) -> Result<(), ProjectFsError> {
        let metadata = fs::symlink_metadata(&self.canonical)
            .map_err(|error| io_error(ProjectFsOperation::InspectRoot, &error))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.dev() != self.device
            || metadata.ino() != self.inode
        {
            return Err(ProjectFsError::IdentityChanged);
        }
        Ok(())
    }
}

/// A target resolved under one registered root at a specific point in time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedProjectFile {
    lexical: PathBuf,
    canonical: PathBuf,
    root_device: u64,
    root_inode: u64,
    device: u64,
    inode: u64,
}

impl PreparedProjectFile {
    #[must_use]
    pub fn canonical_path(&self) -> &Path {
        &self.canonical
    }
}

fn validate_relative(path: &Path, limits: ProjectPathLimits) -> Result<(), ProjectFsError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(ProjectFsError::InvalidRelativePath);
    }
    let bytes = path.as_os_str().as_bytes();
    if bytes.len() > limits.max_bytes || bytes.contains(&0) {
        return Err(ProjectFsError::PathLimitExceeded);
    }
    let mut count = 0_usize;
    for component in path.components() {
        match component {
            Component::Normal(_) => count += 1,
            _ => return Err(ProjectFsError::InvalidRelativePath),
        }
        if count > limits.max_components {
            return Err(ProjectFsError::PathLimitExceeded);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectFsOperation {
    CanonicalizeRoot,
    InspectRoot,
    CanonicalizeTarget,
    InspectTarget,
    OpenTarget,
    InspectOpenedTarget,
    CanonicalizeOpenedTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectFsError {
    InvalidConfiguration,
    InvalidRelativePath,
    PathLimitExceeded,
    PathEscape,
    UnsafeRoot,
    NotRegularFile,
    WrongRoot,
    IdentityChanged,
    Io {
        operation: ProjectFsOperation,
        kind: io::ErrorKind,
    },
}

fn io_error(operation: ProjectFsOperation, error: &io::Error) -> ProjectFsError {
    ProjectFsError::Io {
        operation,
        kind: error.kind(),
    }
}

impl fmt::Display for ProjectFsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration => formatter.write_str("invalid project path limits"),
            Self::InvalidRelativePath => formatter.write_str("invalid relative project path"),
            Self::PathLimitExceeded => formatter.write_str("project path limit exceeded"),
            Self::PathEscape => formatter.write_str("project path escapes registered root"),
            Self::UnsafeRoot => formatter.write_str("project root is not a real directory"),
            Self::NotRegularFile => formatter.write_str("project target is not a regular file"),
            Self::WrongRoot => formatter.write_str("prepared file belongs to another root"),
            Self::IdentityChanged => formatter.write_str("project filesystem identity changed"),
            Self::Io { operation, kind } => {
                write!(
                    formatter,
                    "project filesystem {operation:?} failed: {kind:?}"
                )
            }
        }
    }
}

impl std::error::Error for ProjectFsError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        base: PathBuf,
        root: PathBuf,
        outside: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let base = std::env::temp_dir()
                .join(format!("choosh-project-fs-{}-{serial}", std::process::id()));
            let root = base.join("root");
            let outside = base.join("outside");
            fs::create_dir_all(root.join("src")).expect("create root");
            fs::create_dir_all(&outside).expect("create outside");
            fs::write(root.join("src/main.rs"), b"inside").expect("write inside");
            fs::write(outside.join("secret"), b"outside").expect("write outside");
            fs::write(outside.join("main.rs"), b"outside").expect("write swapped target");
            Self {
                base,
                root,
                outside,
            }
        }

        fn registered(&self) -> RegisteredProjectRoot {
            RegisteredProjectRoot::register(
                &self.root,
                ProjectPathLimits::new(128, 8).expect("limits"),
            )
            .expect("register")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn opens_regular_file_beneath_registered_root() {
        let fixture = Fixture::new();
        let root = fixture.registered();
        let mut file = root.open("src/main.rs").expect("open");
        let mut contents = String::new();
        file.read_to_string(&mut contents).expect("read");
        assert_eq!(contents, "inside");
    }

    #[test]
    fn rejects_lexical_and_bounded_input_violations() {
        let fixture = Fixture::new();
        let root = fixture.registered();
        assert_eq!(
            root.prepare("../outside/secret"),
            Err(ProjectFsError::InvalidRelativePath)
        );
        assert_eq!(
            root.prepare("one/two/three/four/five/six/seven/eight/nine"),
            Err(ProjectFsError::PathLimitExceeded)
        );
    }

    #[test]
    fn rejects_symlink_that_canonically_escapes_root() {
        let fixture = Fixture::new();
        symlink(&fixture.outside, fixture.root.join("escape")).expect("symlink");
        assert_eq!(
            fixture.registered().prepare("escape/secret"),
            Err(ProjectFsError::PathEscape)
        );
    }

    #[test]
    fn rejects_symlink_swap_between_prepare_and_open() {
        let fixture = Fixture::new();
        let root = fixture.registered();
        let link = fixture.root.join("current");
        symlink("src", &link).expect("initial symlink");
        let prepared = root.prepare("current/main.rs").expect("prepare");
        fs::remove_file(&link).expect("remove initial symlink");
        symlink(&fixture.outside, &link).expect("replacement symlink");
        assert!(matches!(
            root.open_prepared(&prepared),
            Err(ProjectFsError::IdentityChanged)
        ));
    }

    #[test]
    fn prepared_file_cannot_cross_registered_roots() {
        let first = Fixture::new();
        let second = Fixture::new();
        let prepared = first.registered().prepare("src/main.rs").expect("prepare");
        assert!(matches!(
            second.registered().open_prepared(&prepared),
            Err(ProjectFsError::WrongRoot)
        ));
    }
}
