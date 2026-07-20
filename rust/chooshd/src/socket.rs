//! Secure lifecycle for the daemon's only listening endpoint.
//!
//! The socket lives directly inside a private, injected state directory.  This
//! module intentionally has no production-path default and exposes no network
//! listener API.

#![cfg(unix)]

use std::fmt;
use std::fs::{self, DirBuilder, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Component, Path, PathBuf};

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_SOCKET_MODE: u32 = 0o600;

/// A validated, but not yet filesystem-inspected, daemon socket layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SocketPlan {
    state_dir: PathBuf,
    socket_path: PathBuf,
}

impl SocketPlan {
    /// Validate injected paths without consulting ambient state such as `HOME`.
    ///
    /// # Errors
    ///
    /// Returns [`SocketError::InvalidPath`] for non-absolute or non-normal
    /// paths, and [`SocketError::InvalidLayout`] unless the socket is an
    /// immediate child of the state directory.
    pub fn new(
        state_dir: impl Into<PathBuf>,
        socket_path: impl Into<PathBuf>,
    ) -> Result<Self, SocketError> {
        let state_dir = state_dir.into();
        let socket_path = socket_path.into();
        validate_absolute_normal_path(&state_dir, "state directory")?;
        validate_absolute_normal_path(&socket_path, "socket")?;

        if socket_path.parent() != Some(state_dir.as_path()) || socket_path.file_name().is_none() {
            return Err(SocketError::InvalidLayout(
                "socket must be an immediate child of the state directory",
            ));
        }

        Ok(Self {
            state_dir,
            socket_path,
        })
    }

    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

fn validate_absolute_normal_path(path: &Path, label: &'static str) -> Result<(), SocketError> {
    if !path.is_absolute() {
        return Err(SocketError::InvalidPath(label));
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(SocketError::InvalidPath(label));
    }
    Ok(())
}

#[derive(Debug)]
pub enum SocketError {
    InvalidPath(&'static str),
    InvalidLayout(&'static str),
    UnsafeStateDirectory(&'static str),
    ExistingSocketPath,
    Io(io::Error),
}

impl fmt::Display for SocketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(label) => write!(formatter, "invalid {label} path"),
            Self::InvalidLayout(reason) => write!(formatter, "invalid socket layout: {reason}"),
            Self::UnsafeStateDirectory(reason) => {
                write!(formatter, "unsafe state directory: {reason}")
            }
            Self::ExistingSocketPath => formatter.write_str("socket path already exists"),
            Self::Io(error) => write!(formatter, "socket filesystem operation failed: {error}"),
        }
    }
}

impl std::error::Error for SocketError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for SocketError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// An open Unix listener whose path is removed only while it is still the same
/// filesystem object created by this instance.
pub struct OwnedUnixListener {
    listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl OwnedUnixListener {
    #[must_use]
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for OwnedUnixListener {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Create or validate the private state directory and bind the Unix listener.
///
/// Any existing socket-path entry is rejected, including a socket left by a
/// previous process. Removing a stale endpoint is an explicit recovery action,
/// not something startup guesses is safe to do.
///
/// # Errors
///
/// Fails closed if the state directory is not private and real, if any entry
/// already occupies the socket path, or if directory, bind, permission, or
/// metadata operations fail.
pub fn bind(plan: &SocketPlan) -> Result<OwnedUnixListener, SocketError> {
    ensure_private_state_directory(plan.state_dir())?;

    match fs::symlink_metadata(plan.socket_path()) {
        Ok(_) => return Err(SocketError::ExistingSocketPath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let listener = UnixListener::bind(plan.socket_path())?;
    if let Err(error) = fs::set_permissions(
        plan.socket_path(),
        Permissions::from_mode(PRIVATE_SOCKET_MODE),
    ) {
        let _ = fs::remove_file(plan.socket_path());
        return Err(error.into());
    }

    let metadata = fs::symlink_metadata(plan.socket_path())?;
    if !metadata.file_type().is_socket() || metadata.mode() & 0o777 != PRIVATE_SOCKET_MODE {
        let _ = fs::remove_file(plan.socket_path());
        return Err(SocketError::UnsafeStateDirectory(
            "bound endpoint did not retain socket type and mode 0600",
        ));
    }

    Ok(OwnedUnixListener {
        listener,
        path: plan.socket_path().to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

/// Verifies an explicitly injected, already-running daemon endpoint.
///
/// The stdio relay uses this before connecting so it never follows a socket
/// plan through a public or symlinked state directory. It does not create,
/// remove, or otherwise recover any filesystem entry.
///
/// # Errors
///
/// Fails closed unless the state directory is private and real and the exact
/// planned path is a mode-`0600` Unix socket.
pub fn verify_running_endpoint(plan: &SocketPlan) -> Result<(), SocketError> {
    let state_metadata = fs::symlink_metadata(plan.state_dir())?;
    validate_state_directory(&state_metadata)?;

    let socket_metadata = fs::symlink_metadata(plan.socket_path())?;
    if !socket_metadata.file_type().is_socket()
        || socket_metadata.mode() & 0o777 != PRIVATE_SOCKET_MODE
    {
        return Err(SocketError::UnsafeStateDirectory(
            "existing endpoint is not a mode 0600 Unix socket",
        ));
    }
    Ok(())
}

fn ensure_private_state_directory(path: &Path) -> Result<(), SocketError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_state_directory(&metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            builder.mode(PRIVATE_DIRECTORY_MODE);
            builder.create(path)?;
            fs::set_permissions(path, Permissions::from_mode(PRIVATE_DIRECTORY_MODE))?;
            validate_state_directory(&fs::symlink_metadata(path)?)
        }
        Err(error) => Err(error.into()),
    }
}

fn validate_state_directory(metadata: &fs::Metadata) -> Result<(), SocketError> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(SocketError::UnsafeStateDirectory(
            "path is not a real directory",
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(SocketError::UnsafeStateDirectory(
            "group or other permissions are present",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            // Keep the socket under the repository in restricted test
            // sandboxes that prohibit AF_UNIX creation in the system temp
            // directory. The path is still fully injected and uses no HOME.
            let path = std::env::current_dir()
                .expect("current test directory")
                .join(format!(
                    "choosh-socket-test-{}-{serial}",
                    std::process::id()
                ));
            fs::create_dir(&path).expect("create test root");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn plan_requires_absolute_direct_child_paths() {
        assert!(SocketPlan::new("relative", "relative/daemon.sock").is_err());
        assert!(SocketPlan::new("/state", "/state/nested/daemon.sock").is_err());
        assert!(SocketPlan::new("/state/../state", "/state/daemon.sock").is_err());
        assert!(SocketPlan::new("/state", "/state/daemon.sock").is_ok());
    }

    #[test]
    fn bind_creates_private_directory_and_socket_then_cleans_up() {
        let root = TestDirectory::new();
        let state = root.0.join("state");
        let socket = state.join("daemon.sock");
        let plan = SocketPlan::new(&state, &socket).expect("valid plan");

        let owned = bind(&plan).expect("bind socket");
        assert_eq!(fs::metadata(&state).unwrap().mode() & 0o777, 0o700);
        let socket_metadata = fs::symlink_metadata(&socket).unwrap();
        assert!(socket_metadata.file_type().is_socket());
        assert_eq!(socket_metadata.mode() & 0o777, 0o600);
        drop(owned);
        assert!(!socket.exists());
    }

    #[test]
    fn rejects_public_or_symlinked_state_directory() {
        let root = TestDirectory::new();
        let public = root.0.join("public");
        fs::create_dir(&public).unwrap();
        fs::set_permissions(&public, Permissions::from_mode(0o750)).unwrap();
        let public_plan = SocketPlan::new(&public, public.join("daemon.sock")).unwrap();
        assert!(matches!(
            bind(&public_plan),
            Err(SocketError::UnsafeStateDirectory(_))
        ));

        let private = root.0.join("private");
        fs::create_dir(&private).unwrap();
        fs::set_permissions(&private, Permissions::from_mode(0o700)).unwrap();
        let linked = root.0.join("linked");
        symlink(&private, &linked).unwrap();
        let linked_plan = SocketPlan::new(&linked, linked.join("daemon.sock")).unwrap();
        assert!(matches!(
            bind(&linked_plan),
            Err(SocketError::UnsafeStateDirectory(_))
        ));
    }

    #[test]
    fn rejects_every_existing_socket_path_type() {
        let root = TestDirectory::new();
        let state = root.0.join("state");
        fs::create_dir(&state).unwrap();
        fs::set_permissions(&state, Permissions::from_mode(0o700)).unwrap();
        let socket = state.join("daemon.sock");
        fs::write(&socket, b"not a socket").unwrap();
        let plan = SocketPlan::new(&state, &socket).unwrap();
        assert!(matches!(bind(&plan), Err(SocketError::ExistingSocketPath)));
    }

    #[test]
    fn verifies_only_the_private_running_endpoint_at_the_injected_path() {
        let root = TestDirectory::new();
        let state = root.0.join("state");
        let socket = state.join("daemon.sock");
        let plan = SocketPlan::new(&state, &socket).unwrap();
        assert!(verify_running_endpoint(&plan).is_err());

        let owned = bind(&plan).unwrap();
        assert!(verify_running_endpoint(&plan).is_ok());
        drop(owned);

        fs::write(&socket, b"not a socket").unwrap();
        assert!(matches!(
            verify_running_endpoint(&plan),
            Err(SocketError::UnsafeStateDirectory(_))
        ));
    }

    #[test]
    fn cleanup_preserves_replacement_at_the_same_path() {
        let root = TestDirectory::new();
        let state = root.0.join("state");
        let socket = state.join("daemon.sock");
        let plan = SocketPlan::new(&state, &socket).unwrap();
        let owned = bind(&plan).unwrap();

        fs::remove_file(&socket).unwrap();
        fs::write(&socket, b"replacement").unwrap();
        drop(owned);

        assert_eq!(fs::read(&socket).unwrap(), b"replacement");
    }
}
