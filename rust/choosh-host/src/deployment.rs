//! Host-owned immutable deployment transaction.
//!
//! Android supplies only a validated version, digest, and bounded artifact
//! bytes. Release paths, atomic publication, service-manager invocation, and
//! private-socket health are injected host capabilities and cannot be selected
//! by the authenticated caller.

use std::fmt;

use crate::service_manager::ServiceManager;

const MAX_VERSION_BYTES: usize = 64;

/// One verified release upload accepted by the host deployment boundary.
#[derive(Clone, Eq, PartialEq)]
pub struct DeploymentUpload {
    version: String,
    sha256: [u8; 32],
    bytes: Vec<u8>,
}

impl DeploymentUpload {
    /// Validates an immutable, digest-addressed artifact without accepting a
    /// host path, service-manager argument, or executable name.
    ///
    /// # Errors
    ///
    /// Returns a stable error for an invalid version or empty/oversized bytes.
    pub fn new(
        version: impl Into<String>,
        sha256: [u8; 32],
        bytes: Vec<u8>,
        max_bytes: usize,
    ) -> Result<Self, DeploymentError> {
        let version = version.into();
        if version.is_empty()
            || version.len() > MAX_VERSION_BYTES
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
        {
            return Err(DeploymentError::InvalidVersion);
        }
        if max_bytes == 0 || bytes.is_empty() || bytes.len() > max_bytes {
            return Err(DeploymentError::InvalidArtifact);
        }
        Ok(Self {
            version,
            sha256,
            bytes,
        })
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
}

impl fmt::Debug for DeploymentUpload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeploymentUpload")
            .field("version", &self.version)
            .field("sha256", &"[REDACTED]")
            .field("bytes", &"[REDACTED]")
            .finish()
    }
}

/// Host-owned immutable stage and atomic activation capability.
pub trait ImmutableDeploymentStore {
    type Staged;
    type Previous;
    type Error;

    /// Publishes an inert immutable stage below a host-configured release root.
    ///
    /// # Errors
    ///
    /// Returns the host adapter's bounded staging failure.
    fn stage_immutable(&mut self, version: &str, bytes: &[u8])
    -> Result<Self::Staged, Self::Error>;
    /// Atomically selects the already verified stage and returns the prior release.
    ///
    /// # Errors
    ///
    /// Returns the host adapter's activation failure.
    fn activate(&mut self, staged: Self::Staged) -> Result<Self::Previous, Self::Error>;
    /// Atomically restores the previous release exactly once after post-activation failure.
    ///
    /// # Errors
    ///
    /// Returns the host adapter's rollback failure.
    fn rollback(&mut self, previous: Self::Previous) -> Result<(), Self::Error>;
    /// Removes a non-active stage after a pre-activation failure.
    ///
    /// # Errors
    ///
    /// Returns the host adapter's cleanup failure.
    fn discard(&mut self, staged: Self::Staged) -> Result<(), Self::Error>;
}

/// Digest verifier operating on the host-owned staged artifact.
pub trait StagedDigestVerifier<Staged> {
    type Error;

    /// Returns whether the exact stage matches the upload's SHA-256 digest.
    ///
    /// # Errors
    ///
    /// Returns the host adapter's digest-read failure.
    fn sha256_matches(&mut self, staged: &Staged, expected: &[u8; 32])
    -> Result<bool, Self::Error>;
}

/// Fixed host service-manager activation capability.
pub trait DeploymentService {
    type Error;

    /// Activates the host-configured daemon service for the selected release.
    ///
    /// # Errors
    ///
    /// Returns the fixed service-manager adapter failure.
    fn activate_selected_release(&mut self) -> Result<(), Self::Error>;
}

/// Makes a fixed per-user service-manager adapter available to deployment without exposing
/// service paths, labels, argv, or a shell command to the transaction caller.
impl<M: ServiceManager> DeploymentService for M {
    type Error = M::Error;

    fn activate_selected_release(&mut self) -> Result<(), Self::Error> {
        ServiceManager::activate(self)
    }
}

/// Private-socket health capability bound to the selected daemon service.
pub trait DeploymentHealth {
    type Error;

    /// Checks the activated exact version without accepting a path or command.
    ///
    /// # Errors
    ///
    /// Returns the private-socket health adapter failure.
    fn healthy_version(&mut self, version: &str) -> Result<bool, Self::Error>;
}

/// Successful host-owned deployment outcomes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeploymentOutcome {
    Activated { version: String },
    RolledBack { failed_version: String },
}

/// Stable deployment phase failures with no artifact bytes or host paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeploymentFailure<StoreError, DigestError, ServiceError, HealthError> {
    Stage(StoreError),
    Digest(DigestError),
    DigestMismatch,
    Discard(StoreError),
    Activate(StoreError),
    Service(ServiceError),
    Health(HealthError),
    Unhealthy,
    Rollback(StoreError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeploymentError {
    InvalidVersion,
    InvalidArtifact,
}

/// Coordinates the one-way immutable deployment transaction.
pub struct HostDeployment<S, D, A, H> {
    store: S,
    digest: D,
    service: A,
    health: H,
}

/// Alias for the four-capability deployment failure shape.
pub type HostDeploymentFailure<S, D, A, H> = DeploymentFailure<
    <S as ImmutableDeploymentStore>::Error,
    <D as StagedDigestVerifier<<S as ImmutableDeploymentStore>::Staged>>::Error,
    <A as DeploymentService>::Error,
    <H as DeploymentHealth>::Error,
>;

impl<S, D, A, H> HostDeployment<S, D, A, H>
where
    S: ImmutableDeploymentStore,
    D: StagedDigestVerifier<S::Staged>,
    A: DeploymentService,
    H: DeploymentHealth,
{
    #[must_use]
    pub const fn new(store: S, digest: D, service: A, health: H) -> Self {
        Self {
            store,
            digest,
            service,
            health,
        }
    }

    /// Stages, verifies, atomically activates, starts, and health-checks one release.
    ///
    /// A digest failure discards only the inert stage. Any failure after
    /// activation rolls back exactly once; no retry or caller-selected command
    /// is possible through this boundary.
    ///
    /// # Errors
    ///
    /// Returns a phase-specific, content-free host adapter failure.
    pub fn deploy(
        &mut self,
        upload: &DeploymentUpload,
    ) -> Result<DeploymentOutcome, HostDeploymentFailure<S, D, A, H>> {
        let staged = self
            .store
            .stage_immutable(&upload.version, &upload.bytes)
            .map_err(DeploymentFailure::Stage)?;
        match self.digest.sha256_matches(&staged, &upload.sha256) {
            Ok(true) => {}
            Ok(false) => {
                self.store
                    .discard(staged)
                    .map_err(DeploymentFailure::Discard)?;
                return Err(DeploymentFailure::DigestMismatch);
            }
            Err(error) => {
                self.store
                    .discard(staged)
                    .map_err(DeploymentFailure::Discard)?;
                return Err(DeploymentFailure::Digest(error));
            }
        }
        let previous = self
            .store
            .activate(staged)
            .map_err(DeploymentFailure::Activate)?;
        if let Err(error) = self.service.activate_selected_release() {
            self.store
                .rollback(previous)
                .map_err(DeploymentFailure::Rollback)?;
            return Err(DeploymentFailure::Service(error));
        }
        match self.health.healthy_version(&upload.version) {
            Ok(true) => Ok(DeploymentOutcome::Activated {
                version: upload.version.clone(),
            }),
            Ok(false) => {
                self.store
                    .rollback(previous)
                    .map_err(DeploymentFailure::Rollback)?;
                Ok(DeploymentOutcome::RolledBack {
                    failed_version: upload.version.clone(),
                })
            }
            Err(error) => {
                self.store
                    .rollback(previous)
                    .map_err(DeploymentFailure::Rollback)?;
                Err(DeploymentFailure::Health(error))
            }
        }
    }

    #[must_use]
    pub fn into_parts(self) -> (S, D, A, H) {
        (self.store, self.digest, self.service, self.health)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service_manager::{
        ProcessOutcome, ServiceInvocation, ServiceProcessRunner, SystemdUserManager,
    };
    use std::ffi::OsString;

    #[derive(Default)]
    struct Store {
        calls: Vec<&'static str>,
    }

    impl ImmutableDeploymentStore for Store {
        type Staged = u8;
        type Previous = u8;
        type Error = &'static str;
        fn stage_immutable(&mut self, _: &str, _: &[u8]) -> Result<u8, Self::Error> {
            self.calls.push("stage");
            Ok(1)
        }
        fn activate(&mut self, _: u8) -> Result<u8, Self::Error> {
            self.calls.push("activate");
            Ok(2)
        }
        fn rollback(&mut self, _: u8) -> Result<(), Self::Error> {
            self.calls.push("rollback");
            Ok(())
        }
        fn discard(&mut self, _: u8) -> Result<(), Self::Error> {
            self.calls.push("discard");
            Ok(())
        }
    }

    struct Digest(bool);
    impl StagedDigestVerifier<u8> for Digest {
        type Error = &'static str;
        fn sha256_matches(&mut self, _: &u8, _: &[u8; 32]) -> Result<bool, Self::Error> {
            Ok(self.0)
        }
    }

    struct Service(Result<(), &'static str>);
    impl DeploymentService for Service {
        type Error = &'static str;
        fn activate_selected_release(&mut self) -> Result<(), Self::Error> {
            self.0
        }
    }

    struct Health(Result<bool, &'static str>);
    impl DeploymentHealth for Health {
        type Error = &'static str;
        fn healthy_version(&mut self, version: &str) -> Result<bool, Self::Error> {
            assert_eq!(version, "2.0.0");
            self.0
        }
    }

    fn upload() -> DeploymentUpload {
        DeploymentUpload::new("2.0.0", [7; 32], b"artifact".to_vec(), 32).unwrap()
    }

    #[derive(Default)]
    struct Process {
        calls: Vec<ServiceInvocation>,
    }

    impl ServiceProcessRunner for Process {
        type Error = &'static str;

        fn run(&mut self, invocation: ServiceInvocation) -> Result<ProcessOutcome, Self::Error> {
            self.calls.push(invocation);
            Ok(ProcessOutcome::Success)
        }
    }

    #[test]
    fn verified_immutable_stage_activates_the_host_service_then_health_checks() {
        let mut deployment = HostDeployment::new(
            Store::default(),
            Digest(true),
            Service(Ok(())),
            Health(Ok(true)),
        );
        assert_eq!(
            deployment.deploy(&upload()),
            Ok(DeploymentOutcome::Activated {
                version: "2.0.0".into()
            })
        );
        assert_eq!(deployment.into_parts().0.calls, ["stage", "activate"]);
    }

    #[test]
    fn deployment_composes_only_fixed_systemd_user_activation_before_private_health() {
        let mut deployment = HostDeployment::new(
            Store::default(),
            Digest(true),
            SystemdUserManager::new(Process::default()),
            Health(Ok(true)),
        );

        assert!(matches!(
            deployment.deploy(&upload()),
            Ok(DeploymentOutcome::Activated { .. })
        ));
        let (store, _, manager, _) = deployment.into_parts();
        assert_eq!(store.calls, ["stage", "activate"]);
        let runner = manager.into_inner();
        assert_eq!(runner.calls.len(), 2);
        assert_eq!(runner.calls[0].program(), "systemctl");
        assert_eq!(
            runner.calls[0].arguments(),
            &[OsString::from("--user"), OsString::from("daemon-reload")]
        );
        assert_eq!(
            runner.calls[1].arguments(),
            &[
                OsString::from("--user"),
                OsString::from("enable"),
                OsString::from("--now"),
                OsString::from("chooshd.service"),
            ]
        );
    }

    #[test]
    fn digest_mismatch_discards_before_service_or_health() {
        let mut deployment = HostDeployment::new(
            Store::default(),
            Digest(false),
            Service(Ok(())),
            Health(Ok(true)),
        );
        assert_eq!(
            deployment.deploy(&upload()),
            Err(DeploymentFailure::DigestMismatch)
        );
        assert_eq!(deployment.into_parts().0.calls, ["stage", "discard"]);
    }

    #[test]
    fn service_or_health_failure_rolls_back_once() {
        let mut service_failure = HostDeployment::new(
            Store::default(),
            Digest(true),
            Service(Err("service")),
            Health(Ok(true)),
        );
        assert_eq!(
            service_failure.deploy(&upload()),
            Err(DeploymentFailure::Service("service"))
        );
        assert_eq!(
            service_failure.into_parts().0.calls,
            ["stage", "activate", "rollback"]
        );

        let mut unhealthy = HostDeployment::new(
            Store::default(),
            Digest(true),
            Service(Ok(())),
            Health(Ok(false)),
        );
        assert_eq!(
            unhealthy.deploy(&upload()),
            Ok(DeploymentOutcome::RolledBack {
                failed_version: "2.0.0".into()
            })
        );
        assert_eq!(
            unhealthy.into_parts().0.calls,
            ["stage", "activate", "rollback"]
        );
    }

    #[test]
    fn caller_cannot_construct_paths_or_shell_like_versions() {
        assert_eq!(
            DeploymentUpload::new("2.0.0;sh", [0; 32], vec![1], 4),
            Err(DeploymentError::InvalidVersion)
        );
        assert_eq!(
            DeploymentUpload::new("2.0.0", [0; 32], Vec::new(), 4),
            Err(DeploymentError::InvalidArtifact)
        );
        assert!(!format!("{:?}", upload()).contains("artifact"));
    }
}
