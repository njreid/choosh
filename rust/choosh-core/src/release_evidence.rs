//! Deterministic validation and encoding of release evidence metadata.

use std::collections::BTreeSet;
use std::path::{Component, Path};

const MAX_ID_BYTES: usize = 128;
const MAX_PATH_BYTES: usize = 512;
const MAX_RECORDS: usize = 256;
const REQUIRED_GATES: [&str; 5] = ["unit", "integration", "security", "sbom", "notices"];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseEvidence {
    pub release_id: String,
    pub app_version: Version,
    pub previous_app_version: Version,
    pub host_version: Version,
    pub previous_host_version: Version,
    pub targets: Vec<TargetEvidence>,
    pub gates: Vec<GateEvidence>,
    pub migration: MigrationEvidence,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TargetEvidence {
    pub target_id: String,
    pub artifact_path: String,
    pub sha256: String,
    pub signature_path: String,
    pub sbom_path: String,
    pub notices_path: String,
    pub tool_name: String,
    pub tool_version: String,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GateEvidence {
    pub gate_id: String,
    pub target_id: String,
    pub evidence_path: String,
    pub sha256: String,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationEvidence {
    pub from_schema: u32,
    pub to_schema: u32,
    pub forward_fixture: String,
    pub rollback_fixture: String,
    pub sha256: String,
}

/// Validates and canonically encodes one release evidence manifest.
///
/// The line-oriented format is deterministic and contains no ambient paths or
/// timestamps. Targets and gates are sorted before encoding.
///
/// # Errors
///
/// Rejects invalid/duplicate identities, unsafe paths, malformed hashes, missing
/// required gates, unknown gate targets, invalid tool metadata or durations,
/// non-monotonic versions, and incomplete migration evidence.
pub fn encode_canonical(evidence: &ReleaseEvidence) -> Result<String, EvidenceError> {
    validate(evidence)?;
    let mut targets = evidence.targets.clone();
    targets.sort();
    let mut gates = evidence.gates.clone();
    gates.sort();
    let mut output = String::from("choosh-release-evidence-v1\n");
    field(&mut output, "release", &evidence.release_id);
    field(&mut output, "app", &version_text(evidence.app_version));
    field(
        &mut output,
        "app_previous",
        &version_text(evidence.previous_app_version),
    );
    field(&mut output, "host", &version_text(evidence.host_version));
    field(
        &mut output,
        "host_previous",
        &version_text(evidence.previous_host_version),
    );
    for target in targets {
        output.push_str("target|");
        output.push_str(
            &[
                target.target_id,
                target.artifact_path,
                target.sha256,
                target.signature_path,
                target.sbom_path,
                target.notices_path,
                target.tool_name,
                target.tool_version,
                target.duration_ms.to_string(),
            ]
            .join("|"),
        );
        output.push('\n');
    }
    for gate in gates {
        output.push_str("gate|");
        output.push_str(
            &[
                gate.gate_id,
                gate.target_id,
                gate.evidence_path,
                gate.sha256,
                gate.duration_ms.to_string(),
            ]
            .join("|"),
        );
        output.push('\n');
    }
    output.push_str("migration|");
    output.push_str(
        &[
            evidence.migration.from_schema.to_string(),
            evidence.migration.to_schema.to_string(),
            evidence.migration.forward_fixture.clone(),
            evidence.migration.rollback_fixture.clone(),
            evidence.migration.sha256.clone(),
        ]
        .join("|"),
    );
    output.push('\n');
    Ok(output)
}

fn validate(evidence: &ReleaseEvidence) -> Result<(), EvidenceError> {
    validate_atom(&evidence.release_id)?;
    if evidence.app_version <= evidence.previous_app_version
        || evidence.host_version <= evidence.previous_host_version
    {
        return Err(EvidenceError::NonMonotonicVersion);
    }
    if evidence.targets.is_empty()
        || evidence.targets.len() > MAX_RECORDS
        || evidence.gates.len() > MAX_RECORDS
    {
        return Err(EvidenceError::RecordLimit);
    }
    let mut target_ids = BTreeSet::new();
    for target in &evidence.targets {
        validate_atom(&target.target_id)?;
        if !target_ids.insert(target.target_id.as_str()) {
            return Err(EvidenceError::DuplicateId);
        }
        for path in [
            &target.artifact_path,
            &target.signature_path,
            &target.sbom_path,
            &target.notices_path,
        ] {
            validate_path(path)?;
        }
        validate_hash(&target.sha256)?;
        validate_atom(&target.tool_name)?;
        validate_atom(&target.tool_version)?;
        if target.duration_ms == 0 {
            return Err(EvidenceError::InvalidDuration);
        }
    }
    let mut gate_keys = BTreeSet::new();
    let mut present_required = BTreeSet::new();
    for gate in &evidence.gates {
        validate_atom(&gate.gate_id)?;
        validate_atom(&gate.target_id)?;
        if !target_ids.contains(gate.target_id.as_str()) {
            return Err(EvidenceError::UnknownTarget);
        }
        if !gate_keys.insert((gate.target_id.as_str(), gate.gate_id.as_str())) {
            return Err(EvidenceError::DuplicateId);
        }
        validate_path(&gate.evidence_path)?;
        validate_hash(&gate.sha256)?;
        if gate.duration_ms == 0 {
            return Err(EvidenceError::InvalidDuration);
        }
        if REQUIRED_GATES.contains(&gate.gate_id.as_str()) {
            present_required.insert(gate.gate_id.as_str());
        }
    }
    if REQUIRED_GATES
        .iter()
        .any(|gate| !present_required.contains(gate))
    {
        return Err(EvidenceError::MissingRequiredGate);
    }
    if evidence.migration.to_schema <= evidence.migration.from_schema {
        return Err(EvidenceError::InvalidMigration);
    }
    validate_path(&evidence.migration.forward_fixture)?;
    validate_path(&evidence.migration.rollback_fixture)?;
    validate_hash(&evidence.migration.sha256)
}

fn validate_atom(value: &str) -> Result<(), EvidenceError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(EvidenceError::InvalidIdentity);
    }
    Ok(())
}

fn validate_path(value: &str) -> Result<(), EvidenceError> {
    if value.is_empty()
        || value.len() > MAX_PATH_BYTES
        || value.contains('\\')
        || value.contains(':')
        || value.contains('|')
        || value.chars().any(char::is_control)
    {
        return Err(EvidenceError::UnsafePath);
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(EvidenceError::UnsafePath);
    }
    Ok(())
}

fn validate_hash(value: &str) -> Result<(), EvidenceError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(EvidenceError::InvalidHash);
    }
    Ok(())
}

fn version_text(version: Version) -> String {
    format!("{}.{}.{}", version.major, version.minor, version.patch)
}

fn field(output: &mut String, name: &str, value: &str) {
    output.push_str(name);
    output.push('|');
    output.push_str(value);
    output.push('\n');
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceError {
    InvalidIdentity,
    UnsafePath,
    InvalidHash,
    InvalidDuration,
    RecordLimit,
    DuplicateId,
    MissingRequiredGate,
    UnknownTarget,
    NonMonotonicVersion,
    InvalidMigration,
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn sample() -> ReleaseEvidence {
        let targets = ["host-linux", "android-arm64"]
            .into_iter()
            .map(|id| TargetEvidence {
                target_id: id.into(),
                artifact_path: format!("artifacts/{id}.bin"),
                sha256: HASH.into(),
                signature_path: format!("signatures/{id}.sig"),
                sbom_path: format!("sbom/{id}.spdx"),
                notices_path: "legal/NOTICE.txt".into(),
                tool_name: "rustc".into(),
                tool_version: "1.96.0".into(),
                duration_ms: 10,
            })
            .collect();
        let gates = REQUIRED_GATES
            .into_iter()
            .map(|gate| GateEvidence {
                gate_id: gate.into(),
                target_id: "host-linux".into(),
                evidence_path: format!("evidence/{gate}.txt"),
                sha256: HASH.into(),
                duration_ms: 5,
            })
            .collect();
        ReleaseEvidence {
            release_id: "release-1".into(),
            app_version: Version {
                major: 1,
                minor: 1,
                patch: 0,
            },
            previous_app_version: Version {
                major: 1,
                minor: 0,
                patch: 0,
            },
            host_version: Version {
                major: 2,
                minor: 0,
                patch: 1,
            },
            previous_host_version: Version {
                major: 2,
                minor: 0,
                patch: 0,
            },
            targets,
            gates,
            migration: MigrationEvidence {
                from_schema: 1,
                to_schema: 2,
                forward_fixture: "migrations/forward.bin".into(),
                rollback_fixture: "migrations/rollback.bin".into(),
                sha256: HASH.into(),
            },
        }
    }

    #[test]
    fn canonical_encoding_ignores_input_order() {
        let first = sample();
        let mut reversed = first.clone();
        reversed.targets.reverse();
        reversed.gates.reverse();
        assert_eq!(
            encode_canonical(&first).unwrap(),
            encode_canonical(&reversed).unwrap()
        );
        assert!(
            encode_canonical(&first)
                .unwrap()
                .starts_with("choosh-release-evidence-v1\n")
        );
    }

    #[test]
    fn missing_gate_and_unknown_target_fail() {
        let mut missing = sample();
        missing.gates.retain(|gate| gate.gate_id != "security");
        assert_eq!(
            encode_canonical(&missing),
            Err(EvidenceError::MissingRequiredGate)
        );
        let mut unknown = sample();
        unknown.gates[0].target_id = "other".into();
        assert_eq!(
            encode_canonical(&unknown),
            Err(EvidenceError::UnknownTarget)
        );
    }

    #[test]
    fn absolute_traversal_and_secret_shaped_fields_are_rejected() {
        for path in [
            "/tmp/artifact",
            "../secret",
            "C:\\secret",
            "evidence/a|injected",
        ] {
            let mut invalid = sample();
            invalid.targets[0].artifact_path = path.into();
            assert_eq!(encode_canonical(&invalid), Err(EvidenceError::UnsafePath));
        }
        let mut token = sample();
        token.targets[0].tool_version = "token=secret".into();
        assert_eq!(
            encode_canonical(&token),
            Err(EvidenceError::InvalidIdentity)
        );
    }

    #[test]
    fn corrupted_hash_duplicate_id_and_zero_duration_fail() {
        let mut hash = sample();
        hash.targets[0].sha256 = "not-a-hash".into();
        assert_eq!(encode_canonical(&hash), Err(EvidenceError::InvalidHash));
        let mut duplicate = sample();
        duplicate.targets.push(duplicate.targets[0].clone());
        assert_eq!(
            encode_canonical(&duplicate),
            Err(EvidenceError::DuplicateId)
        );
        let mut duration = sample();
        duration.gates[0].duration_ms = 0;
        assert_eq!(
            encode_canonical(&duration),
            Err(EvidenceError::InvalidDuration)
        );
    }

    #[test]
    fn versions_and_migration_must_move_forward() {
        let mut version = sample();
        version.app_version = version.previous_app_version;
        assert_eq!(
            encode_canonical(&version),
            Err(EvidenceError::NonMonotonicVersion)
        );
        let mut migration = sample();
        migration.migration.to_schema = migration.migration.from_schema;
        assert_eq!(
            encode_canonical(&migration),
            Err(EvidenceError::InvalidMigration)
        );
    }
}
