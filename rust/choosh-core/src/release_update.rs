//! Deterministic release update and one-shot rollback authorization policy.

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledRelease {
    pub version: Version,
    pub target: String,
    pub durable_schema: u32,
    pub provenance: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedArtifact<'a> {
    pub version: Version,
    pub target: &'a str,
    pub durable_schema: u32,
    pub provenance: &'a str,
    pub artifact_bytes: &'a [u8],
    pub expected_checksum: &'a str,
    pub signature: &'a [u8],
    pub authenticated_manifest: &'a [u8],
}

/// Injected cryptographic and checksum verification boundary.
pub trait ArtifactVerifier {
    fn checksum_matches(&self, bytes: &[u8], expected: &str) -> bool;
    fn signature_valid(&self, manifest: &[u8], signature: &[u8]) -> bool;
    fn manifest_authenticated(&self, manifest: &[u8]) -> bool;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdatePlan {
    pub from: Version,
    pub to: Version,
    pub target: String,
    pub durable_schema: u32,
}

/// Validates an update without installing or writing any artifact.
///
/// # Errors
///
/// Rejects unauthenticated metadata, invalid signature/checksum, unsupported
/// target or provenance, non-monotonic versions, schema regression, and empty or
/// oversized metadata/artifacts.
pub fn verify_update<V: ArtifactVerifier>(
    installed: &InstalledRelease,
    artifact: &SignedArtifact<'_>,
    verifier: &V,
    limits: UpdateLimits,
) -> Result<UpdatePlan, UpdateError> {
    validate_limits(limits)?;
    validate_installed(installed, limits)?;
    validate_artifact(artifact, limits)?;
    if artifact.target != installed.target {
        return Err(UpdateError::UnsupportedTarget);
    }
    if artifact.provenance != installed.provenance {
        return Err(UpdateError::IncompatibleProvenance);
    }
    if artifact.version <= installed.version {
        return Err(UpdateError::NonMonotonicVersion);
    }
    if artifact.durable_schema < installed.durable_schema {
        return Err(UpdateError::SchemaRegression);
    }
    if !verifier.manifest_authenticated(artifact.authenticated_manifest) {
        return Err(UpdateError::UnauthenticatedManifest);
    }
    if !verifier.signature_valid(artifact.authenticated_manifest, artifact.signature) {
        return Err(UpdateError::InvalidSignature);
    }
    if !verifier.checksum_matches(artifact.artifact_bytes, artifact.expected_checksum) {
        return Err(UpdateError::ChecksumMismatch);
    }
    Ok(UpdatePlan {
        from: installed.version,
        to: artifact.version,
        target: installed.target.clone(),
        durable_schema: artifact.durable_schema,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UpdateLimits {
    pub max_artifact_bytes: usize,
    pub max_manifest_bytes: usize,
    pub max_signature_bytes: usize,
    pub max_metadata_bytes: usize,
}

fn validate_limits(limits: UpdateLimits) -> Result<(), UpdateError> {
    if limits.max_artifact_bytes == 0
        || limits.max_manifest_bytes == 0
        || limits.max_signature_bytes == 0
        || limits.max_metadata_bytes == 0
    {
        Err(UpdateError::InvalidLimits)
    } else {
        Ok(())
    }
}

fn validate_installed(
    installed: &InstalledRelease,
    limits: UpdateLimits,
) -> Result<(), UpdateError> {
    validate_metadata(&installed.target, limits)?;
    validate_metadata(&installed.provenance, limits)
}

fn validate_artifact(
    artifact: &SignedArtifact<'_>,
    limits: UpdateLimits,
) -> Result<(), UpdateError> {
    validate_metadata(artifact.target, limits)?;
    validate_metadata(artifact.provenance, limits)?;
    if artifact.artifact_bytes.is_empty()
        || artifact.artifact_bytes.len() > limits.max_artifact_bytes
        || artifact.authenticated_manifest.is_empty()
        || artifact.authenticated_manifest.len() > limits.max_manifest_bytes
        || artifact.signature.is_empty()
        || artifact.signature.len() > limits.max_signature_bytes
        || artifact.expected_checksum.len() != 64
        || !artifact
            .expected_checksum
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(UpdateError::InvalidArtifactMetadata);
    }
    Ok(())
}

fn validate_metadata(value: &str, limits: UpdateLimits) -> Result<(), UpdateError> {
    if value.is_empty()
        || value.len() > limits.max_metadata_bytes
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        Err(UpdateError::InvalidArtifactMetadata)
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RollbackPermit {
    installed: InstalledRelease,
    previous: InstalledRelease,
    consumed: bool,
}

impl RollbackPermit {
    /// Captures exactly one compatible rollback opportunity.
    ///
    /// # Errors
    ///
    /// Rejects mismatched targets/provenance, a non-older previous release, or a
    /// previous binary unable to preserve the installed durable schema.
    pub fn new(
        installed: InstalledRelease,
        previous: InstalledRelease,
    ) -> Result<Self, UpdateError> {
        if previous.target != installed.target {
            return Err(UpdateError::UnsupportedTarget);
        }
        if previous.provenance != installed.provenance {
            return Err(UpdateError::IncompatibleProvenance);
        }
        if previous.version >= installed.version {
            return Err(UpdateError::InvalidRollbackVersion);
        }
        if previous.durable_schema < installed.durable_schema {
            return Err(UpdateError::RollbackSchemaUnsupported);
        }
        Ok(Self {
            installed,
            previous,
            consumed: false,
        })
    }

    /// Consumes the only rollback authorization.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateError::RollbackConsumed`] after the first success.
    pub fn consume(&mut self) -> Result<RollbackPlan, UpdateError> {
        if self.consumed {
            return Err(UpdateError::RollbackConsumed);
        }
        self.consumed = true;
        Ok(RollbackPlan {
            from: self.installed.version,
            to: self.previous.version,
            preserved_schema: self.installed.durable_schema,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RollbackPlan {
    pub from: Version,
    pub to: Version,
    pub preserved_schema: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateError {
    InvalidLimits,
    InvalidArtifactMetadata,
    UnsupportedTarget,
    IncompatibleProvenance,
    NonMonotonicVersion,
    SchemaRegression,
    UnauthenticatedManifest,
    InvalidSignature,
    ChecksumMismatch,
    InvalidRollbackVersion,
    RollbackSchemaUnsupported,
    RollbackConsumed,
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const LIMITS: UpdateLimits = UpdateLimits {
        max_artifact_bytes: 32,
        max_manifest_bytes: 32,
        max_signature_bytes: 32,
        max_metadata_bytes: 32,
    };

    struct FakeVerifier {
        checksum: bool,
        signature: bool,
        manifest: bool,
    }

    impl ArtifactVerifier for FakeVerifier {
        fn checksum_matches(&self, _: &[u8], _: &str) -> bool {
            self.checksum
        }
        fn signature_valid(&self, _: &[u8], _: &[u8]) -> bool {
            self.signature
        }
        fn manifest_authenticated(&self, _: &[u8]) -> bool {
            self.manifest
        }
    }

    fn installed(version: Version, schema: u32) -> InstalledRelease {
        InstalledRelease {
            version,
            target: "linux-x86_64".into(),
            durable_schema: schema,
            provenance: "choosh-release".into(),
        }
    }

    fn artifact<'a>() -> SignedArtifact<'a> {
        SignedArtifact {
            version: Version {
                major: 2,
                minor: 0,
                patch: 0,
            },
            target: "linux-x86_64",
            durable_schema: 2,
            provenance: "choosh-release",
            artifact_bytes: b"artifact",
            expected_checksum: HASH,
            signature: b"signature",
            authenticated_manifest: b"manifest",
        }
    }

    fn verifier() -> FakeVerifier {
        FakeVerifier {
            checksum: true,
            signature: true,
            manifest: true,
        }
    }

    #[test]
    fn valid_update_returns_plan_without_install_authority() {
        let current = installed(
            Version {
                major: 1,
                minor: 0,
                patch: 0,
            },
            1,
        );
        assert_eq!(
            verify_update(&current, &artifact(), &verifier(), LIMITS)
                .unwrap()
                .to,
            Version {
                major: 2,
                minor: 0,
                patch: 0
            }
        );
    }

    #[test]
    fn verification_failures_have_stable_distinct_outcomes() {
        let current = installed(
            Version {
                major: 1,
                minor: 0,
                patch: 0,
            },
            1,
        );
        for (fake, expected) in [
            (
                FakeVerifier {
                    checksum: true,
                    signature: true,
                    manifest: false,
                },
                UpdateError::UnauthenticatedManifest,
            ),
            (
                FakeVerifier {
                    checksum: true,
                    signature: false,
                    manifest: true,
                },
                UpdateError::InvalidSignature,
            ),
            (
                FakeVerifier {
                    checksum: false,
                    signature: true,
                    manifest: true,
                },
                UpdateError::ChecksumMismatch,
            ),
        ] {
            assert_eq!(
                verify_update(&current, &artifact(), &fake, LIMITS),
                Err(expected)
            );
        }
    }

    #[test]
    fn target_provenance_version_and_schema_fail_closed() {
        let current = installed(
            Version {
                major: 1,
                minor: 0,
                patch: 0,
            },
            2,
        );
        let mut candidate = artifact();
        candidate.target = "android-arm64";
        assert_eq!(
            verify_update(&current, &candidate, &verifier(), LIMITS),
            Err(UpdateError::UnsupportedTarget)
        );
        candidate = artifact();
        candidate.provenance = "other";
        assert_eq!(
            verify_update(&current, &candidate, &verifier(), LIMITS),
            Err(UpdateError::IncompatibleProvenance)
        );
        candidate = artifact();
        candidate.version = current.version;
        assert_eq!(
            verify_update(&current, &candidate, &verifier(), LIMITS),
            Err(UpdateError::NonMonotonicVersion)
        );
        candidate = artifact();
        candidate.durable_schema = 1;
        assert_eq!(
            verify_update(&current, &candidate, &verifier(), LIMITS),
            Err(UpdateError::SchemaRegression)
        );
    }

    #[test]
    fn rollback_is_exactly_once_and_preserves_schema() {
        let current = installed(
            Version {
                major: 2,
                minor: 0,
                patch: 0,
            },
            2,
        );
        let previous = installed(
            Version {
                major: 1,
                minor: 0,
                patch: 0,
            },
            2,
        );
        let mut permit = RollbackPermit::new(current, previous).unwrap();
        assert_eq!(permit.consume().unwrap().preserved_schema, 2);
        assert_eq!(permit.consume(), Err(UpdateError::RollbackConsumed));
    }

    #[test]
    fn rollback_rejects_binary_that_cannot_read_current_schema() {
        let current = installed(
            Version {
                major: 2,
                minor: 0,
                patch: 0,
            },
            2,
        );
        let previous = installed(
            Version {
                major: 1,
                minor: 0,
                patch: 0,
            },
            1,
        );
        assert_eq!(
            RollbackPermit::new(current, previous),
            Err(UpdateError::RollbackSchemaUnsupported)
        );
    }
}
