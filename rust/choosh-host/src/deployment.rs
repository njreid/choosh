//! Host-owned immutable deployment transaction.
//!
//! Android supplies only a validated version, digest, and bounded artifact
//! bytes. Release paths, atomic publication, service-manager invocation, and
//! private-socket health are injected host capabilities and cannot be selected
//! by the authenticated caller.

use std::fmt;

use crate::service_manager::ServiceManager;

const MAX_VERSION_BYTES: usize = 64;

/// Result of comparing a verified GitHub release with the host's active version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateDecision {
    UpToDate,
    Install,
}

/// Compares canonical `MAJOR.MINOR.PATCH` versions without accepting paths or shell text.
///
/// # Errors
///
/// Returns [`DeploymentError::InvalidVersion`] when either side is not exactly
/// three dot-separated decimal components, with an optional `v` prefix.
pub fn update_decision(installed: &str, latest: &str) -> Result<UpdateDecision, DeploymentError> {
    fn parse(value: &str) -> Option<[u64; 3]> {
        let mut parts = value.strip_prefix('v').unwrap_or(value).split('.');
        let parsed = [
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        ];
        parts.next().is_none().then_some(parsed)
    }
    let installed = parse(installed).ok_or(DeploymentError::InvalidVersion)?;
    let latest = parse(latest).ok_or(DeploymentError::InvalidVersion)?;
    Ok(if latest > installed {
        UpdateDecision::Install
    } else {
        UpdateDecision::UpToDate
    })
}

/// Decodes the versioned host-update envelope. The envelope carries no paths or argv.
///
/// # Errors
///
/// Returns [`DeploymentError::InvalidVersion`] for a missing or unbounded version
/// and [`DeploymentError::InvalidArtifact`] for a malformed schema, digest, or
/// artifact, or for artifact bytes exceeding `max_bytes`.
pub fn decode_upload_envelope(
    payload: &[u8],
    max_bytes: usize,
) -> Result<DeploymentUpload, DeploymentError> {
    let value: serde_json::Value =
        serde_json::from_slice(payload).map_err(|_| DeploymentError::InvalidArtifact)?;
    let object = value.as_object().ok_or(DeploymentError::InvalidArtifact)?;
    if object
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(DeploymentError::InvalidArtifact);
    }
    let version = object
        .get("version")
        .and_then(serde_json::Value::as_str)
        .ok_or(DeploymentError::InvalidVersion)?;
    let digest = object
        .get("sha256")
        .and_then(serde_json::Value::as_str)
        .ok_or(DeploymentError::InvalidArtifact)?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DeploymentError::InvalidArtifact);
    }
    let mut sha256 = [0_u8; 32];
    for (index, pair) in digest.as_bytes().chunks_exact(2).enumerate() {
        sha256[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    let artifact = object
        .get("artifact_b64")
        .and_then(serde_json::Value::as_str)
        .ok_or(DeploymentError::InvalidArtifact)?;
    DeploymentUpload::new(
        version,
        sha256,
        decode_base64_url_unpadded(artifact, max_bytes)?,
        max_bytes,
    )
}

/// Decodes canonical unpadded base64url, rejecting anything above `max_bytes`
/// before allocating the artifact.
///
/// Padding, whitespace, the standard `+/` alphabet, and non-zero trailing bits
/// are all rejected, so exactly one encoding maps to any given artifact.
fn decode_base64_url_unpadded(input: &str, max_bytes: usize) -> Result<Vec<u8>, DeploymentError> {
    let input = input.as_bytes();
    // An unpadded group of 1 character can never carry a whole byte.
    if input.len() % 4 == 1 {
        return Err(DeploymentError::InvalidArtifact);
    }
    let decoded_len = input.len() / 4 * 3 + (input.len() % 4).saturating_sub(1);
    if decoded_len > max_bytes {
        return Err(DeploymentError::InvalidArtifact);
    }
    let mut bytes = Vec::with_capacity(decoded_len);
    for group in input.chunks(4) {
        let mut accumulator: u32 = 0;
        let mut last = 0;
        for symbol in group {
            last = base64_symbol(*symbol)?;
            accumulator = (accumulator << 6) | u32::from(last);
        }
        // A short final group encodes fewer bits than its symbols can carry. The
        // unused low bits of its last symbol MUST be zero, otherwise two distinct
        // encodings would describe the same artifact.
        let unused_bits = (4 - group.len()) * 2;
        if unused_bits > 0 && last & ((1_u8 << unused_bits) - 1) != 0 {
            return Err(DeploymentError::InvalidArtifact);
        }
        accumulator <<= (4 - group.len()) * 6;
        for shift in [16_u32, 8, 0].iter().take(group.len() - 1) {
            bytes.push(u8::try_from((accumulator >> shift) & 0xff).unwrap_or_default());
        }
    }
    Ok(bytes)
}

fn base64_symbol(value: u8) -> Result<u8, DeploymentError> {
    match value {
        b'A'..=b'Z' => Ok(value - b'A'),
        b'a'..=b'z' => Ok(value - b'a' + 26),
        b'0'..=b'9' => Ok(value - b'0' + 52),
        b'-' => Ok(62),
        b'_' => Ok(63),
        _ => Err(DeploymentError::InvalidArtifact),
    }
}

fn hex_nibble(value: u8) -> Result<u8, DeploymentError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(DeploymentError::InvalidArtifact),
    }
}

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

    #[test]
    fn update_decision_is_strict_and_monotonic() {
        assert_eq!(
            update_decision("v1.2.3", "1.2.3").unwrap(),
            UpdateDecision::UpToDate
        );
        assert_eq!(
            update_decision("1.2.3", "1.2.4").unwrap(),
            UpdateDecision::Install
        );
        assert_eq!(
            update_decision("1.3.0", "1.2.99").unwrap(),
            UpdateDecision::UpToDate
        );
        assert!(update_decision("1.2", "1.2.3").is_err());
    }

    const ZERO_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn envelope(artifact_b64: &str) -> Vec<u8> {
        format!(
            r#"{{"schema_version":1,"version":"2.0.0","sha256":"{ZERO_DIGEST}","artifact_b64":"{artifact_b64}"}}"#
        )
        .into_bytes()
    }

    #[test]
    fn upload_envelope_is_bounded_and_path_free() {
        // "AQID" is base64url for the bytes 1, 2, 3.
        let upload = decode_upload_envelope(&envelope("AQID"), 8).unwrap();
        assert_eq!(upload.version(), "2.0.0");
        assert!(
            decode_upload_envelope(
                br#"{"schema_version":1,"version":"2.0.0","sha256":"bad","artifact_b64":"AQ"}"#,
                8
            )
            .is_err()
        );
        assert!(
            decode_upload_envelope(
                format!(
                    r#"{{"schema_version":1,"version":"2.0.0;sh","sha256":"{ZERO_DIGEST}","artifact_b64":"AQ"}}"#
                )
                .as_bytes(),
                8
            )
            .is_err()
        );
        assert!(decode_upload_envelope(&envelope("AQID"), 2).is_err());
        // The pre-base64 decimal-array encoding is not accepted.
        assert!(
            decode_upload_envelope(
                format!(
                    r#"{{"schema_version":1,"version":"2.0.0","sha256":"{ZERO_DIGEST}","artifact":[1,2,3]}}"#
                )
                .as_bytes(),
                8
            )
            .is_err()
        );
    }

    #[test]
    fn artifact_base64_admits_exactly_one_canonical_unpadded_url_encoding() {
        for (encoded, expected) in [
            ("AQID", vec![1_u8, 2, 3]),
            ("AQ", vec![1]),
            ("AQI", vec![1, 2]),
            // 0xff 0xff decodes through the URL-safe `-_` alphabet only.
            ("__8", vec![0xff, 0xff]),
            ("AAAAAA", vec![0, 0, 0, 0]),
        ] {
            assert_eq!(
                decode_base64_url_unpadded(encoded, 64).unwrap(),
                expected,
                "{encoded}"
            );
        }
        for rejected in [
            "AQ==",   // padded
            "A",      // an orphan symbol carries no whole byte
            "AQIDA",  // trailing orphan symbol
            "AQ I",   // whitespace
            "//8",    // standard alphabet
            "+w",     // standard alphabet
            "AR",     // non-zero unused bits in a two-symbol group
            "AQJ",    // non-zero unused bits in a three-symbol group
            "AQID\n", // trailing newline
        ] {
            assert!(
                decode_base64_url_unpadded(rejected, 64).is_err(),
                "accepted {rejected}"
            );
        }
    }

    #[test]
    fn decodes_the_golden_envelope_emitted_by_the_android_encoder() {
        // Produced verbatim by `ai.choosh.HostDeploymentEnvelope.encode` for the
        // artifact bytes below. Keeping the exact bytes here makes a one-sided
        // change to either encoder a test failure rather than a runtime reject.
        let golden = br#"{"schema_version":1,"version":"0.2.0","sha256":"6d3a0f4c7cf38573de9632a0fddf4d9bcdee120eb8e98b3855482bdf94b28aac","artifact_b64":"AAEC_v96"}"#;
        let upload = decode_upload_envelope(golden, 64).unwrap();
        assert_eq!(upload.version(), "0.2.0");
        assert_eq!(upload.bytes, vec![0x00, 0x01, 0x02, 0xfe, 0xff, 0x7a]);
    }

    #[test]
    fn artifact_base64_rejects_oversized_input_before_allocating() {
        let encoded = "A".repeat(4_000);
        assert_eq!(
            decode_base64_url_unpadded(&encoded, 8),
            Err(DeploymentError::InvalidArtifact)
        );
    }
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
