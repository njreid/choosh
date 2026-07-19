//! Root-confined, bounded SFTP request boundary.

use std::future::Future;

use choosh_core::path::{RelativePath, RelativePathError, RelativePathLimits};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SftpLimits {
    pub paths: RelativePathLimits,
    pub max_read_bytes: usize,
    pub max_write_bytes: usize,
}

impl SftpLimits {
    /// Creates positive request limits.
    ///
    /// # Errors
    ///
    /// Returns `invalid_limits` when any bound is zero.
    pub const fn new(
        paths: RelativePathLimits,
        max_read_bytes: usize,
        max_write_bytes: usize,
    ) -> Result<Self, SftpError> {
        if paths.max_bytes == 0
            || paths.max_components == 0
            || paths.max_component_bytes == 0
            || max_read_bytes == 0
            || max_write_bytes == 0
        {
            Err(SftpError::InvalidLimits)
        } else {
            Ok(Self {
                paths,
                max_read_bytes,
                max_write_bytes,
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SftpError {
    InvalidLimits,
    InvalidPath(RelativePathError),
    ReadLimitExceeded,
    WriteLimitExceeded,
    Transport,
}

impl SftpError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidLimits => "invalid_limits",
            Self::InvalidPath(_) => "invalid_path",
            Self::ReadLimitExceeded => "read_limit_exceeded",
            Self::WriteLimitExceeded => "write_limit_exceeded",
            Self::Transport => "transport_error",
        }
    }
}

/// Storage capability already bound to one canonical remote root.
///
/// Implementations must resolve the supplied lexical identity beneath that
/// root without following a symlink outside it. No method accepts a root or an
/// unvalidated string, preventing a caller from selecting a different root.
pub trait RootedSftpTransport: Send + Sync {
    type Error: Send;

    fn read<'a>(
        &'a self,
        path: &'a RelativePath,
        max_bytes: usize,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send + 'a;

    fn write_atomic<'a>(
        &'a self,
        path: &'a RelativePath,
        contents: &'a [u8],
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a;
}

pub struct ConfinedSftp<T> {
    transport: T,
    limits: SftpLimits,
}

impl<T: RootedSftpTransport> ConfinedSftp<T> {
    #[must_use]
    pub const fn new(transport: T, limits: SftpLimits) -> Self {
        Self { transport, limits }
    }

    /// Reads one validated relative path within both configured and requested bounds.
    ///
    /// # Errors
    ///
    /// Fails before transport access for unsafe paths or invalid request limits,
    /// and rejects a transport response that violates the negotiated bound.
    pub async fn read(&self, path: &str, max_bytes: usize) -> Result<Vec<u8>, SftpError> {
        if max_bytes == 0 || max_bytes > self.limits.max_read_bytes {
            return Err(SftpError::ReadLimitExceeded);
        }
        let path = RelativePath::parse(path, self.limits.paths).map_err(SftpError::InvalidPath)?;
        let contents = self
            .transport
            .read(&path, max_bytes)
            .await
            .map_err(|_| SftpError::Transport)?;
        if contents.len() > max_bytes {
            Err(SftpError::ReadLimitExceeded)
        } else {
            Ok(contents)
        }
    }

    /// Atomically writes one validated relative path within the configured bound.
    ///
    /// # Errors
    ///
    /// Fails before transport access for unsafe paths or oversized contents.
    pub async fn write_atomic(&self, path: &str, contents: &[u8]) -> Result<(), SftpError> {
        if contents.len() > self.limits.max_write_bytes {
            return Err(SftpError::WriteLimitExceeded);
        }
        let path = RelativePath::parse(path, self.limits.paths).map_err(SftpError::InvalidPath)?;
        self.transport
            .write_atomic(&path, contents)
            .await
            .map_err(|_| SftpError::Transport)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeTransport {
        files: Mutex<BTreeMap<String, Vec<u8>>>,
        calls: Mutex<Vec<String>>,
        ignore_read_limit: bool,
    }

    impl RootedSftpTransport for FakeTransport {
        type Error = ();

        async fn read(
            &self,
            path: &RelativePath,
            max_bytes: usize,
        ) -> Result<Vec<u8>, Self::Error> {
            self.calls.lock().unwrap().push(format!("read:{path}"));
            let mut value = self
                .files
                .lock()
                .unwrap()
                .get(path.as_str())
                .cloned()
                .ok_or(())?;
            if !self.ignore_read_limit {
                value.truncate(max_bytes);
            }
            Ok(value)
        }

        async fn write_atomic(
            &self,
            path: &RelativePath,
            contents: &[u8],
        ) -> Result<(), Self::Error> {
            self.calls.lock().unwrap().push(format!("write:{path}"));
            self.files
                .lock()
                .unwrap()
                .insert(path.as_str().to_owned(), contents.to_vec());
            Ok(())
        }
    }

    fn limits() -> SftpLimits {
        SftpLimits::new(RelativePathLimits::new(64, 4, 24), 8, 8).unwrap()
    }

    #[tokio::test]
    async fn bounded_relative_write_then_read_is_deterministic() {
        let adapter = ConfinedSftp::new(FakeTransport::default(), limits());
        adapter
            .write_atomic("src/main.rs", b"12345678")
            .await
            .unwrap();
        assert_eq!(adapter.read("src/main.rs", 8).await.unwrap(), b"12345678");
        assert_eq!(
            *adapter.transport.calls.lock().unwrap(),
            ["write:src/main.rs", "read:src/main.rs"]
        );
    }

    #[tokio::test]
    async fn unsafe_paths_and_oversized_requests_never_reach_transport() {
        let adapter = ConfinedSftp::new(FakeTransport::default(), limits());
        for path in [
            "",
            "/etc/passwd",
            "../secret",
            "a/./b",
            "a//b",
            "C:/secret",
            "a\\b",
        ] {
            assert!(matches!(
                adapter.read(path, 1).await,
                Err(SftpError::InvalidPath(_))
            ));
            assert!(matches!(
                adapter.write_atomic(path, b"x").await,
                Err(SftpError::InvalidPath(_))
            ));
        }
        assert_eq!(
            adapter.read("safe", 0).await,
            Err(SftpError::ReadLimitExceeded)
        );
        assert_eq!(
            adapter.read("safe", 9).await,
            Err(SftpError::ReadLimitExceeded)
        );
        assert_eq!(
            adapter.write_atomic("safe", b"123456789").await,
            Err(SftpError::WriteLimitExceeded)
        );
        assert!(adapter.transport.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn oversized_transport_response_fails_closed() {
        let transport = FakeTransport {
            ignore_read_limit: true,
            ..FakeTransport::default()
        };
        transport
            .files
            .lock()
            .unwrap()
            .insert("large".into(), b"123456789".to_vec());
        let adapter = ConfinedSftp::new(transport, limits());
        assert_eq!(
            adapter.read("large", 8).await,
            Err(SftpError::ReadLimitExceeded)
        );
    }
}
