//! Small filesystem helpers shared across `choosh-hostd` modules.

use std::io;
use std::path::Path;

/// Writes `bytes` to `path` atomically: writes to a sibling temp file in
/// `path`'s own directory (so the final `rename` lands on the same
/// filesystem, making it a single atomic directory-entry swap), optionally
/// applies `mode` (Unix permission bits) to that temp file before the
/// rename, then renames it into place.
///
/// This is the "never leave a half-written file" guarantee every call site
/// in this crate that persists real state (a credential, hook config,
/// update state, a synced dotfile) independently needs: a reader of `path`
/// — including this same process crashing mid-write — either sees exactly
/// what was there before this call, or the fully-written new content,
/// never a truncated or partial write.
///
/// `path`'s parent directory must already exist; this function does not
/// create it (callers that need `create_dir_all` first already know that,
/// since some need it for other reasons too, e.g. to also decide whether a
/// write is even necessary).
///
/// `mode` is a no-op on non-Unix targets.
///
/// # Errors
///
/// Returns an `io::Error` if `path` has no parent directory, or if the
/// temp-file write, permission-set, or rename fails.
pub fn atomic_write(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory"))?;
    let tmp_path =
        parent.join(format!(".{}.tmp-{}", path.file_name().and_then(|n| n.to_str()).unwrap_or("atomic-write"), std::process::id()));

    let result = (|| {
        std::fs::write(&tmp_path, bytes)?;
        if let Some(mode) = mode {
            set_permissions(&tmp_path, mode)?;
        }
        std::fs::rename(&tmp_path, path)
    })();

    if result.is_err() {
        // Best-effort: don't leave the temp file behind on a failed write/
        // chmod/rename — the caller already gets the real error below.
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

#[cfg(unix)]
fn set_permissions(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_permissions(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_full_content_and_creates_the_final_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");

        atomic_write(&path, b"hello world", None).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
    }

    #[test]
    fn leaves_no_temp_file_behind_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");

        atomic_write(&path, b"content", None).unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "no .tmp- file should remain after a successful atomic_write");
    }

    #[test]
    fn overwrites_existing_content_completely() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        std::fs::write(&path, b"old content that is longer than new").unwrap();

        atomic_write(&path, b"new", None).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn fails_cleanly_when_the_parent_directory_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-parent").join("out.txt");

        assert!(atomic_write(&path, b"content", None).is_err());
        assert!(!path.exists());
    }

    #[test]
    #[cfg(unix)]
    fn applies_the_requested_mode_to_the_final_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.json");

        atomic_write(&path, b"secret bytes", Some(0o600)).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_concurrent_reader_never_observes_a_partial_write() {
        // Same discipline as `update.rs`'s `swap_in_is_atomic_...` test:
        // spin a reader in a tight loop while a background write replaces
        // the file's content with something much longer — every read must
        // be either the full old content or the full new content, never a
        // truncated in-between state.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let old_content = vec![b'O'; 4096];
        let new_content = vec![b'N'; 8192];
        std::fs::write(&path, &old_content).unwrap();

        let observer_path = path.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observer_stop = stop.clone();
        let observations = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observer_observations = observations.clone();
        let observer = std::thread::spawn(move || {
            while !observer_stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(bytes) = std::fs::read(&observer_path) {
                    assert!(
                        bytes == vec![b'O'; 4096] || bytes == vec![b'N'; 8192],
                        "observed a partial/unexpected file content: {} bytes",
                        bytes.len()
                    );
                    observer_observations.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });

        while observations.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        atomic_write(&path, &new_content, None).unwrap();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        observer.join().unwrap();

        assert!(observations.load(std::sync::atomic::Ordering::Relaxed) > 0, "observer never got a chance to run");
        assert_eq!(std::fs::read(&path).unwrap(), new_content);
    }
}
