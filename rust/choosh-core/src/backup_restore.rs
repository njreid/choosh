//! Bounded backup/restore migration evidence for non-secret durable identities.

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackupLimits {
    pub max_records: usize,
    pub max_identity_bytes: usize,
    pub max_manifest_bytes: usize,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InventoryRecord {
    Host {
        host_id: String,
        revision: u64,
    },
    Workspace {
        host_id: String,
        workspace_id: String,
        revision: u64,
    },
    Pin {
        workspace_id: String,
        item_id: String,
        revision: u64,
    },
    Annotation {
        workspace_id: String,
        document_id: String,
        annotation_id: String,
        revision: u64,
    },
    Recovery {
        workspace_id: String,
        recovery_id: String,
        revision: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupManifest {
    pub app_version: Version,
    pub schema_version: u32,
    pub encrypted_backup_attested: bool,
    pub records: Vec<InventoryRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RestoreEvidence {
    pub backup: BackupManifest,
    pub first_upgrade_version: Version,
    pub first_upgrade_schema: u32,
    pub restored_after_first: Vec<InventoryRecord>,
    pub second_upgrade_version: Version,
    pub second_upgrade_schema: u32,
    pub restored_after_second: Vec<InventoryRecord>,
}

/// Validates a two-upgrade restore and returns a canonical non-secret manifest.
///
/// The encryption flag is an attestation input only; this module implements no
/// encryption or storage operations.
///
/// # Errors
///
/// Rejects missing encryption attestation, invalid/non-monotonic versions or
/// schemas, malformed/duplicate identities, record/manifest limits, and missing,
/// extra, or revision-corrupt restored state at either upgrade.
pub fn validate_restore(
    evidence: &RestoreEvidence,
    limits: BackupLimits,
) -> Result<String, RestoreError> {
    validate_limits(limits)?;
    if !evidence.backup.encrypted_backup_attested {
        return Err(RestoreError::EncryptionNotAttested);
    }
    if evidence.first_upgrade_version <= evidence.backup.app_version
        || evidence.second_upgrade_version <= evidence.first_upgrade_version
    {
        return Err(RestoreError::NonMonotonicVersion);
    }
    if evidence.first_upgrade_schema < evidence.backup.schema_version
        || evidence.second_upgrade_schema < evidence.first_upgrade_schema
    {
        return Err(RestoreError::SchemaRegression);
    }
    let expected = canonical(&evidence.backup.records, limits)?;
    compare(
        &expected,
        &canonical(&evidence.restored_after_first, limits)?,
    )?;
    compare(
        &expected,
        &canonical(&evidence.restored_after_second, limits)?,
    )?;
    encode_manifest(&evidence.backup, &expected, limits)
}

fn canonical(
    records: &[InventoryRecord],
    limits: BackupLimits,
) -> Result<Vec<InventoryRecord>, RestoreError> {
    if records.len() > limits.max_records {
        return Err(RestoreError::RecordLimit);
    }
    let mut output = records.to_vec();
    output.sort();
    let mut keys = BTreeSet::new();
    for record in &output {
        let (kind, identity, revision) = record_key(record);
        if revision == 0 {
            return Err(RestoreError::CorruptIdentity);
        }
        for value in &identity {
            validate_identity(value, limits.max_identity_bytes)?;
        }
        if !keys.insert((kind, identity)) {
            return Err(RestoreError::DuplicateIdentity);
        }
    }
    Ok(output)
}

fn record_key(record: &InventoryRecord) -> (u8, Vec<&str>, u64) {
    match record {
        InventoryRecord::Host { host_id, revision } => (0, vec![host_id], *revision),
        InventoryRecord::Workspace {
            host_id,
            workspace_id,
            revision,
        } => (1, vec![host_id, workspace_id], *revision),
        InventoryRecord::Pin {
            workspace_id,
            item_id,
            revision,
        } => (2, vec![workspace_id, item_id], *revision),
        InventoryRecord::Annotation {
            workspace_id,
            document_id,
            annotation_id,
            revision,
        } => (3, vec![workspace_id, document_id, annotation_id], *revision),
        InventoryRecord::Recovery {
            workspace_id,
            recovery_id,
            revision,
        } => (4, vec![workspace_id, recovery_id], *revision),
    }
}

fn compare(expected: &[InventoryRecord], actual: &[InventoryRecord]) -> Result<(), RestoreError> {
    let expected_keys: BTreeSet<_> = expected.iter().map(record_identity).collect();
    let actual_keys: BTreeSet<_> = actual.iter().map(record_identity).collect();
    if !expected_keys.is_subset(&actual_keys) {
        return Err(RestoreError::MissingIdentity);
    }
    if !actual_keys.is_subset(&expected_keys) {
        return Err(RestoreError::ExtraIdentity);
    }
    if expected != actual {
        return Err(RestoreError::CorruptIdentity);
    }
    Ok(())
}

fn record_identity(record: &InventoryRecord) -> (u8, Vec<&str>) {
    let (kind, identity, _) = record_key(record);
    (kind, identity)
}

fn encode_manifest(
    backup: &BackupManifest,
    records: &[InventoryRecord],
    limits: BackupLimits,
) -> Result<String, RestoreError> {
    let mut output = format!(
        "choosh-backup-v1\napp|{}.{}.{}\nschema|{}\nencrypted|attested\n",
        backup.app_version.major,
        backup.app_version.minor,
        backup.app_version.patch,
        backup.schema_version
    );
    for record in records {
        let (kind, ids, revision) = record_key(record);
        output.push_str("record|");
        output.push_str(&kind.to_string());
        for id in ids {
            output.push('|');
            output.push_str(id);
        }
        output.push('|');
        output.push_str(&revision.to_string());
        output.push('\n');
        if output.len() > limits.max_manifest_bytes {
            return Err(RestoreError::ManifestLimit);
        }
    }
    Ok(output)
}

fn validate_identity(value: &str, max: usize) -> Result<(), RestoreError> {
    if value.is_empty()
        || value.len() > max
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        Err(RestoreError::InvalidIdentity)
    } else {
        Ok(())
    }
}
fn validate_limits(l: BackupLimits) -> Result<(), RestoreError> {
    if l.max_records == 0 || l.max_identity_bytes == 0 || l.max_manifest_bytes == 0 {
        Err(RestoreError::InvalidLimits)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestoreError {
    InvalidLimits,
    EncryptionNotAttested,
    NonMonotonicVersion,
    SchemaRegression,
    RecordLimit,
    ManifestLimit,
    InvalidIdentity,
    DuplicateIdentity,
    MissingIdentity,
    ExtraIdentity,
    CorruptIdentity,
}

#[cfg(test)]
mod tests {
    use super::*;
    const L: BackupLimits = BackupLimits {
        max_records: 16,
        max_identity_bytes: 64,
        max_manifest_bytes: 2048,
    };
    fn records() -> Vec<InventoryRecord> {
        vec![
            InventoryRecord::Host {
                host_id: "host".into(),
                revision: 1,
            },
            InventoryRecord::Workspace {
                host_id: "host".into(),
                workspace_id: "work".into(),
                revision: 2,
            },
            InventoryRecord::Pin {
                workspace_id: "work".into(),
                item_id: "terminal".into(),
                revision: 3,
            },
            InventoryRecord::Annotation {
                workspace_id: "work".into(),
                document_id: "readme.md".into(),
                annotation_id: "note".into(),
                revision: 4,
            },
            InventoryRecord::Recovery {
                workspace_id: "work".into(),
                recovery_id: "draft".into(),
                revision: 5,
            },
        ]
    }
    fn evidence() -> RestoreEvidence {
        let records = records();
        RestoreEvidence {
            backup: BackupManifest {
                app_version: Version {
                    major: 1,
                    minor: 0,
                    patch: 0,
                },
                schema_version: 1,
                encrypted_backup_attested: true,
                records: records.clone(),
            },
            first_upgrade_version: Version {
                major: 2,
                minor: 0,
                patch: 0,
            },
            first_upgrade_schema: 2,
            restored_after_first: records.clone(),
            second_upgrade_version: Version {
                major: 3,
                minor: 0,
                patch: 0,
            },
            second_upgrade_schema: 3,
            restored_after_second: records,
        }
    }
    #[test]
    fn two_upgrade_restore_is_canonical() {
        let mut e = evidence();
        e.backup.records.reverse();
        e.restored_after_first.reverse();
        let manifest = validate_restore(&e, L).unwrap();
        assert!(manifest.starts_with("choosh-backup-v1\n"));
        assert_eq!(manifest.matches("record|").count(), 5);
    }
    #[test]
    fn encryption_and_version_schema_guards_fail() {
        let mut e = evidence();
        e.backup.encrypted_backup_attested = false;
        assert_eq!(
            validate_restore(&e, L),
            Err(RestoreError::EncryptionNotAttested)
        );
        e = evidence();
        e.second_upgrade_version = e.first_upgrade_version;
        assert_eq!(
            validate_restore(&e, L),
            Err(RestoreError::NonMonotonicVersion)
        );
        e = evidence();
        e.second_upgrade_schema = 1;
        assert_eq!(validate_restore(&e, L), Err(RestoreError::SchemaRegression));
    }
    #[test]
    fn missing_extra_and_corrupt_records_are_distinct() {
        let mut e = evidence();
        e.restored_after_first.pop();
        assert_eq!(validate_restore(&e, L), Err(RestoreError::MissingIdentity));
        e = evidence();
        e.restored_after_second.push(InventoryRecord::Host {
            host_id: "extra".into(),
            revision: 1,
        });
        assert_eq!(validate_restore(&e, L), Err(RestoreError::ExtraIdentity));
        e = evidence();
        if let InventoryRecord::Host { revision, .. } = &mut e.restored_after_first[0] {
            *revision = 9;
        }
        assert_eq!(validate_restore(&e, L), Err(RestoreError::CorruptIdentity));
    }
    #[test]
    fn duplicate_path_secret_and_zero_revision_fail() {
        let mut e = evidence();
        e.backup.records.push(e.backup.records[0].clone());
        assert_eq!(
            validate_restore(&e, L),
            Err(RestoreError::DuplicateIdentity)
        );
        e = evidence();
        if let InventoryRecord::Host { host_id, .. } = &mut e.backup.records[0] {
            *host_id = "/secret/token".into();
        }
        assert_eq!(validate_restore(&e, L), Err(RestoreError::InvalidIdentity));
        e = evidence();
        if let InventoryRecord::Host { revision, .. } = &mut e.backup.records[0] {
            *revision = 0;
        }
        assert_eq!(validate_restore(&e, L), Err(RestoreError::CorruptIdentity));
    }
}
