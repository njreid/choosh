//! Offline license and provenance notice validation.

use std::collections::{BTreeMap, BTreeSet};

const REQUIRED_COMPONENTS: &[&str] = &["Zelland", "libghostty-vt", "wgpu", "glyphon"];
const REQUIRED_ASSETS: &[&str] = &["Iosevka Charon Mono", "Geomini", "Sora"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProvenanceLimits {
    pub max_records: usize,
    pub max_field_bytes: usize,
    pub max_obligations_per_record: usize,
    pub max_report_issues: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordKind {
    Component,
    Font,
    Asset,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewStatus {
    Approved,
    Unresolved,
    Forbidden,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvenanceRecord {
    pub name: String,
    pub kind: RecordKind,
    pub source: String,
    pub revision: String,
    pub spdx_license: String,
    pub obligations: Vec<String>,
    pub notice_artifact: String,
    pub status: ReviewStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvenanceManifest {
    pub records: Vec<ProvenanceRecord>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum IssueCode {
    DuplicateRecord,
    ForbiddenLicense,
    ForbiddenStatus,
    InvalidField,
    InvalidRevision,
    InvalidSpdx,
    MissingNotice,
    MissingObligation,
    MissingRequiredAsset,
    MissingRequiredComponent,
    UnresolvedStatus,
    WrongKind,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProvenanceIssue {
    pub record: String,
    pub code: IssueCode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvenanceReport {
    pub checked_records: usize,
    pub passed: bool,
    pub issues: Vec<ProvenanceIssue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProvenanceError {
    InvalidLimits,
    TooManyRecords,
    TooManyObligations,
    FieldTooLarge,
    ReportLimit,
}

/// Validates an offline provenance manifest and emits canonical sorted evidence.
///
/// # Errors
///
/// Rejects invalid bounds, oversized manifests/fields/obligations, or issue
/// output above the configured report ceiling. Validation issues are returned
/// in the report and unresolved/forbidden entries always block passage.
pub fn validate_provenance(
    manifest: &ProvenanceManifest,
    limits: ProvenanceLimits,
) -> Result<ProvenanceReport, ProvenanceError> {
    validate_limits(limits)?;
    if manifest.records.len() > limits.max_records {
        return Err(ProvenanceError::TooManyRecords);
    }
    let mut issues = Vec::new();
    let mut names = BTreeMap::new();
    for record in &manifest.records {
        validate_bounds(record, limits)?;
        if names.insert(record.name.as_str(), record.kind).is_some() {
            add_issue(
                &mut issues,
                &record.name,
                IssueCode::DuplicateRecord,
                limits,
            )?;
        }
        validate_record(record, &mut issues, limits)?;
    }
    for name in REQUIRED_COMPONENTS {
        match names.get(name) {
            None => add_issue(
                &mut issues,
                name,
                IssueCode::MissingRequiredComponent,
                limits,
            )?,
            Some(RecordKind::Component) => {}
            Some(_) => add_issue(&mut issues, name, IssueCode::WrongKind, limits)?,
        }
    }
    for name in REQUIRED_ASSETS {
        match names.get(name) {
            None => add_issue(&mut issues, name, IssueCode::MissingRequiredAsset, limits)?,
            Some(RecordKind::Font | RecordKind::Asset) => {}
            Some(RecordKind::Component) => {
                add_issue(&mut issues, name, IssueCode::WrongKind, limits)?;
            }
        }
    }
    issues.sort();
    issues.dedup();
    Ok(ProvenanceReport {
        checked_records: manifest.records.len(),
        passed: issues.is_empty(),
        issues,
    })
}

fn validate_limits(limits: ProvenanceLimits) -> Result<(), ProvenanceError> {
    if limits.max_records == 0
        || limits.max_field_bytes == 0
        || limits.max_obligations_per_record == 0
        || limits.max_report_issues == 0
    {
        Err(ProvenanceError::InvalidLimits)
    } else {
        Ok(())
    }
}

fn validate_bounds(
    record: &ProvenanceRecord,
    limits: ProvenanceLimits,
) -> Result<(), ProvenanceError> {
    if record.obligations.len() > limits.max_obligations_per_record {
        return Err(ProvenanceError::TooManyObligations);
    }
    let fields = [
        record.name.as_str(),
        record.source.as_str(),
        record.revision.as_str(),
        record.spdx_license.as_str(),
        record.notice_artifact.as_str(),
    ];
    if fields
        .iter()
        .any(|field| field.len() > limits.max_field_bytes)
        || record
            .obligations
            .iter()
            .any(|field| field.len() > limits.max_field_bytes)
    {
        Err(ProvenanceError::FieldTooLarge)
    } else {
        Ok(())
    }
}

fn validate_record(
    record: &ProvenanceRecord,
    issues: &mut Vec<ProvenanceIssue>,
    limits: ProvenanceLimits,
) -> Result<(), ProvenanceError> {
    if record.name.trim().is_empty() || record.source.trim().is_empty() {
        add_issue(issues, &record.name, IssueCode::InvalidField, limits)?;
    }
    if !valid_revision(&record.revision) {
        add_issue(issues, &record.name, IssueCode::InvalidRevision, limits)?;
    }
    if !valid_spdx(&record.spdx_license) {
        add_issue(issues, &record.name, IssueCode::InvalidSpdx, limits)?;
    }
    if forbidden_license(&record.spdx_license) {
        add_issue(issues, &record.name, IssueCode::ForbiddenLicense, limits)?;
    }
    if record.notice_artifact.trim().is_empty() {
        add_issue(issues, &record.name, IssueCode::MissingNotice, limits)?;
    }
    let obligations: BTreeSet<_> = record.obligations.iter().map(String::as_str).collect();
    if !obligations.contains("include_license") || !obligations.contains("include_notice") {
        add_issue(issues, &record.name, IssueCode::MissingObligation, limits)?;
    }
    match record.status {
        ReviewStatus::Approved => {}
        ReviewStatus::Unresolved => {
            add_issue(issues, &record.name, IssueCode::UnresolvedStatus, limits)?;
        }
        ReviewStatus::Forbidden => {
            add_issue(issues, &record.name, IssueCode::ForbiddenStatus, limits)?;
        }
    }
    Ok(())
}

fn add_issue(
    issues: &mut Vec<ProvenanceIssue>,
    record: &str,
    code: IssueCode,
    limits: ProvenanceLimits,
) -> Result<(), ProvenanceError> {
    if issues.len() == limits.max_report_issues {
        return Err(ProvenanceError::ReportLimit);
    }
    issues.push(ProvenanceIssue {
        record: record.to_owned(),
        code,
    });
    Ok(())
}

fn valid_revision(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+' | b'/')
        })
}

fn valid_spdx(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'(' | b')' | b' ')
        })
}

fn forbidden_license(value: &str) -> bool {
    value.eq_ignore_ascii_case("proprietary")
        || value.eq_ignore_ascii_case("unknown")
        || value.contains("AGPL")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: ProvenanceLimits = ProvenanceLimits {
        max_records: 10,
        max_field_bytes: 128,
        max_obligations_per_record: 4,
        max_report_issues: 32,
    };

    fn record(name: &str, kind: RecordKind) -> ProvenanceRecord {
        ProvenanceRecord {
            name: name.into(),
            kind,
            source: format!("source/{name}"),
            revision: "v1.2.3".into(),
            spdx_license: "Apache-2.0".into(),
            obligations: vec!["include_license".into(), "include_notice".into()],
            notice_artifact: format!("NOTICE/{name}.txt"),
            status: ReviewStatus::Approved,
        }
    }

    fn complete_manifest() -> ProvenanceManifest {
        let mut records = REQUIRED_COMPONENTS
            .iter()
            .map(|name| record(name, RecordKind::Component))
            .collect::<Vec<_>>();
        records.extend(
            REQUIRED_ASSETS
                .iter()
                .map(|name| record(name, RecordKind::Font)),
        );
        ProvenanceManifest { records }
    }

    #[test]
    fn required_terminal_and_font_provenance_passes_offline() {
        let report = validate_provenance(&complete_manifest(), LIMITS).unwrap();
        assert!(report.passed);
        assert_eq!(report.checked_records, 7);
        assert!(report.issues.is_empty());
    }

    #[test]
    fn missing_required_coverage_is_canonical_and_blocking() {
        let report = validate_provenance(&ProvenanceManifest { records: vec![] }, LIMITS).unwrap();
        assert!(!report.passed);
        assert_eq!(report.issues.len(), 7);
        assert_eq!(report.issues[0].record, "Geomini");
    }

    #[test]
    fn unresolved_forbidden_and_obligation_faults_block() {
        let mut manifest = complete_manifest();
        manifest.records[0].status = ReviewStatus::Unresolved;
        manifest.records[1].spdx_license = "AGPL-3.0-only".into();
        manifest.records[2].obligations.clear();
        let report = validate_provenance(&manifest, LIMITS).unwrap();
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == IssueCode::UnresolvedStatus)
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == IssueCode::ForbiddenLicense)
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == IssueCode::MissingObligation)
        );
        assert!(!report.passed);
    }

    #[test]
    fn duplicates_wrong_kinds_and_missing_notices_are_visible() {
        let mut manifest = complete_manifest();
        manifest.records[4].kind = RecordKind::Component;
        manifest.records[0].notice_artifact.clear();
        manifest.records.push(manifest.records[0].clone());
        let report = validate_provenance(&manifest, LIMITS).unwrap();
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == IssueCode::DuplicateRecord)
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == IssueCode::WrongKind)
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == IssueCode::MissingNotice)
        );
    }

    #[test]
    fn bounds_fail_without_partial_reports() {
        let mut limits = LIMITS;
        limits.max_records = 1;
        assert_eq!(
            validate_provenance(&complete_manifest(), limits),
            Err(ProvenanceError::TooManyRecords)
        );
        limits = LIMITS;
        limits.max_field_bytes = 3;
        assert_eq!(
            validate_provenance(&complete_manifest(), limits),
            Err(ProvenanceError::FieldTooLarge)
        );
    }
}
