//! Deterministic host binary installation, activation, and single-rollback orchestration.
//!
//! Concrete filesystem, checksum, and health-check implementations belong in the binary
//! composition root. This module passes opaque bounded bytes and exact validated versions to
//! narrow capabilities; it never invokes a shell or constructs a command line.

const MAX_VERSION_BYTES: usize = 64;
const SHA256_BYTES: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Release {
    version: String,
    bytes: Vec<u8>,
    sha256: [u8; SHA256_BYTES],
}

impl Release {
    /// Validates a bounded release artifact.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an invalid version or empty/oversized artifact.
    pub fn new(
        version: String,
        bytes: Vec<u8>,
        sha256: [u8; SHA256_BYTES],
        max_artifact_bytes: usize,
    ) -> Result<Self, UpgradeError> {
        validate_version(&version)?;
        if bytes.is_empty() || bytes.len() > max_artifact_bytes {
            return Err(UpgradeError::InvalidArtifact);
        }
        Ok(Self {
            version,
            bytes,
            sha256,
        })
    }
}

fn validate_version(version: &str) -> Result<(), UpgradeError> {
    if version.is_empty()
        || version.len() > MAX_VERSION_BYTES
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(UpgradeError::InvalidVersion);
    }
    Ok(())
}

pub trait ArtifactStore {
    type Staged;
    type Previous;
    type Error;

    /// Writes an inert staging artifact.
    ///
    /// # Errors
    ///
    /// Returns an adapter-specific staging failure.
    fn stage(&mut self, version: &str, bytes: &[u8]) -> Result<Self::Staged, Self::Error>;
    /// Atomically replaces the active artifact and returns its rollback handle.
    ///
    /// # Errors
    ///
    /// Returns an adapter-specific activation failure.
    fn activate(&mut self, staged: Self::Staged) -> Result<Self::Previous, Self::Error>;
    /// Atomically restores the previous artifact.
    ///
    /// # Errors
    ///
    /// Returns an adapter-specific rollback failure.
    fn rollback(&mut self, previous: Self::Previous) -> Result<(), Self::Error>;
    /// Removes an inert staging artifact.
    ///
    /// # Errors
    ///
    /// Returns an adapter-specific cleanup failure.
    fn discard_staged(&mut self, staged: Self::Staged) -> Result<(), Self::Error>;
}

pub trait DigestVerifier<Artifact> {
    type Error;

    /// Checks the bytes represented by the staged artifact against the expected digest.
    ///
    /// # Errors
    ///
    /// Returns an adapter-specific digest-read or calculation failure.
    fn verify_sha256(
        &mut self,
        artifact: &Artifact,
        expected: &[u8; SHA256_BYTES],
    ) -> Result<bool, Self::Error>;
}

pub trait HealthCheck {
    type Error;

    /// Checks the newly activated exact version without invoking a shell.
    ///
    /// # Errors
    ///
    /// Returns an adapter-specific health transport or protocol failure.
    fn healthy(&mut self, version: &str) -> Result<bool, Self::Error>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpgradeOutcome {
    CompatibleNoOp { version: String },
    Activated { version: String },
    RolledBack { failed_version: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpgradeFailure<StoreError, DigestError, HealthError> {
    Stage(StoreError),
    Digest(DigestError),
    DigestMismatch,
    DiscardAfterDigestFailure(StoreError),
    Activate(StoreError),
    Health(HealthError),
    Unhealthy,
    Rollback(StoreError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpgradeError {
    InvalidVersion,
    InvalidArtifact,
}

pub struct UpgradeCoordinator<S, D, H> {
    store: S,
    digest: D,
    health: H,
}

pub type CoordinatorFailure<S, D, H> = UpgradeFailure<
    <S as ArtifactStore>::Error,
    <D as DigestVerifier<<S as ArtifactStore>::Staged>>::Error,
    <H as HealthCheck>::Error,
>;

impl<S, D, H> UpgradeCoordinator<S, D, H>
where
    S: ArtifactStore,
    D: DigestVerifier<S::Staged>,
    H: HealthCheck,
{
    #[must_use]
    pub const fn new(store: S, digest: D, health: H) -> Self {
        Self {
            store,
            digest,
            health,
        }
    }

    /// Stages, verifies, atomically activates, health-checks, and if needed rolls back once.
    ///
    /// A release matching `current_version` performs no capability calls. Digest failure always
    /// discards staging before returning. After activation, any failed or unhealthy check attempts
    /// exactly one rollback; rollback failure is terminal and is never retried implicitly.
    ///
    /// # Errors
    ///
    /// Returns a stable phase-specific failure containing the adapter error when applicable.
    pub fn install(
        &mut self,
        current_version: &str,
        release: &Release,
    ) -> Result<UpgradeOutcome, CoordinatorFailure<S, D, H>> {
        if current_version == release.version {
            return Ok(UpgradeOutcome::CompatibleNoOp {
                version: release.version.clone(),
            });
        }

        let staged = self
            .store
            .stage(&release.version, &release.bytes)
            .map_err(UpgradeFailure::Stage)?;
        match self.digest.verify_sha256(&staged, &release.sha256) {
            Ok(true) => {}
            Ok(false) => {
                self.store
                    .discard_staged(staged)
                    .map_err(UpgradeFailure::DiscardAfterDigestFailure)?;
                return Err(UpgradeFailure::DigestMismatch);
            }
            Err(error) => {
                self.store
                    .discard_staged(staged)
                    .map_err(UpgradeFailure::DiscardAfterDigestFailure)?;
                return Err(UpgradeFailure::Digest(error));
            }
        }

        let previous = self
            .store
            .activate(staged)
            .map_err(UpgradeFailure::Activate)?;
        match self.health.healthy(&release.version) {
            Ok(true) => Ok(UpgradeOutcome::Activated {
                version: release.version.clone(),
            }),
            Ok(false) => {
                self.store
                    .rollback(previous)
                    .map_err(UpgradeFailure::Rollback)?;
                Ok(UpgradeOutcome::RolledBack {
                    failed_version: release.version.clone(),
                })
            }
            Err(error) => {
                self.store
                    .rollback(previous)
                    .map_err(UpgradeFailure::Rollback)?;
                Err(UpgradeFailure::Health(error))
            }
        }
    }

    #[must_use]
    pub fn into_parts(self) -> (S, D, H) {
        (self.store, self.digest, self.health)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct FakeStore {
        calls: Vec<&'static str>,
        fail_rollback: bool,
    }

    impl ArtifactStore for FakeStore {
        type Staged = u8;
        type Previous = u8;
        type Error = &'static str;

        fn stage(&mut self, _version: &str, _bytes: &[u8]) -> Result<u8, Self::Error> {
            self.calls.push("stage");
            Ok(1)
        }

        fn activate(&mut self, _staged: u8) -> Result<u8, Self::Error> {
            self.calls.push("activate");
            Ok(0)
        }

        fn rollback(&mut self, _previous: u8) -> Result<(), Self::Error> {
            self.calls.push("rollback");
            if self.fail_rollback {
                Err("rollback")
            } else {
                Ok(())
            }
        }

        fn discard_staged(&mut self, _staged: u8) -> Result<(), Self::Error> {
            self.calls.push("discard");
            Ok(())
        }
    }

    struct FakeDigest(bool);

    impl DigestVerifier<u8> for FakeDigest {
        type Error = &'static str;

        fn verify_sha256(
            &mut self,
            _artifact: &u8,
            _expected: &[u8; SHA256_BYTES],
        ) -> Result<bool, Self::Error> {
            Ok(self.0)
        }
    }

    struct FakeHealth(Result<bool, &'static str>);

    impl HealthCheck for FakeHealth {
        type Error = &'static str;

        fn healthy(&mut self, _version: &str) -> Result<bool, Self::Error> {
            self.0
        }
    }

    fn release() -> Release {
        Release::new("1.2.3".into(), vec![1, 2, 3], [7; SHA256_BYTES], 16).unwrap()
    }

    #[test]
    fn compatible_version_is_a_capability_free_no_op() {
        let mut coordinator =
            UpgradeCoordinator::new(FakeStore::default(), FakeDigest(true), FakeHealth(Ok(true)));
        assert_eq!(
            coordinator.install("1.2.3", &release()).unwrap(),
            UpgradeOutcome::CompatibleNoOp {
                version: "1.2.3".into()
            }
        );
        assert!(coordinator.into_parts().0.calls.is_empty());
    }

    #[test]
    fn healthy_release_stages_verifies_and_activates() {
        let mut coordinator =
            UpgradeCoordinator::new(FakeStore::default(), FakeDigest(true), FakeHealth(Ok(true)));
        assert_eq!(
            coordinator.install("1.2.2", &release()).unwrap(),
            UpgradeOutcome::Activated {
                version: "1.2.3".into()
            }
        );
        assert_eq!(coordinator.into_parts().0.calls, ["stage", "activate"]);
    }

    #[test]
    fn digest_mismatch_discards_without_activation() {
        let mut coordinator = UpgradeCoordinator::new(
            FakeStore::default(),
            FakeDigest(false),
            FakeHealth(Ok(true)),
        );
        assert_eq!(
            coordinator.install("1.2.2", &release()),
            Err(UpgradeFailure::DigestMismatch)
        );
        assert_eq!(coordinator.into_parts().0.calls, ["stage", "discard"]);
    }

    #[test]
    fn unhealthy_activation_rolls_back_exactly_once() {
        let mut coordinator = UpgradeCoordinator::new(
            FakeStore::default(),
            FakeDigest(true),
            FakeHealth(Ok(false)),
        );
        assert_eq!(
            coordinator.install("1.2.2", &release()).unwrap(),
            UpgradeOutcome::RolledBack {
                failed_version: "1.2.3".into()
            }
        );
        assert_eq!(
            coordinator.into_parts().0.calls,
            ["stage", "activate", "rollback"]
        );
    }

    #[test]
    fn rollback_failure_is_terminal_and_not_retried() {
        let store = FakeStore {
            fail_rollback: true,
            ..FakeStore::default()
        };
        let mut coordinator =
            UpgradeCoordinator::new(store, FakeDigest(true), FakeHealth(Ok(false)));
        assert_eq!(
            coordinator.install("1.2.2", &release()),
            Err(UpgradeFailure::Rollback("rollback"))
        );
        assert_eq!(
            coordinator.into_parts().0.calls,
            ["stage", "activate", "rollback"]
        );
    }

    #[test]
    fn invalid_release_is_rejected_before_orchestration() {
        assert_eq!(
            Release::new("bad version".into(), vec![1], [0; SHA256_BYTES], 4),
            Err(UpgradeError::InvalidVersion)
        );
        assert_eq!(
            Release::new("1.0.0".into(), vec![1, 2], [0; SHA256_BYTES], 1),
            Err(UpgradeError::InvalidArtifact)
        );
    }
}
