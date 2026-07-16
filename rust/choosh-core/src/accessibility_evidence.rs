//! Canonical validation of headless accessibility and device evidence.

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EvidenceLimits {
    pub max_records: usize,
    pub max_text_bytes: usize,
    pub max_exceptions: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DeviceClass {
    Phone,
    Tablet,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EvidenceKind {
    ScreenReaderLabels,
    ScreenReaderOrder,
    TouchTargets,
    Contrast,
    KeyboardNavigation,
    ReducedMotion,
    Rotation,
    BackgroundResume,
    LowMemoryRecovery,
    TerminalIme,
    TerminalAccessoryKeys,
    TerminalGpuRecovery,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EvidenceRecord {
    pub evidence_id: String,
    pub device: DeviceClass,
    pub kind: EvidenceKind,
    pub passed: bool,
    pub check_count: u32,
    pub artifact_id: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AcceptedException {
    pub exception_id: String,
    pub device: DeviceClass,
    pub kind: EvidenceKind,
    pub owner: String,
    pub expires_release: String,
    pub rationale: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EvidenceFailure {
    pub device: DeviceClass,
    pub kind: EvidenceKind,
    pub reason: FailureReason,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FailureReason {
    Missing,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceReport {
    pub records: Vec<EvidenceRecord>,
    pub failures: Vec<EvidenceFailure>,
    pub exceptions: Vec<AcceptedException>,
}

const REQUIRED: [EvidenceKind; 12] = [
    EvidenceKind::ScreenReaderLabels,
    EvidenceKind::ScreenReaderOrder,
    EvidenceKind::TouchTargets,
    EvidenceKind::Contrast,
    EvidenceKind::KeyboardNavigation,
    EvidenceKind::ReducedMotion,
    EvidenceKind::Rotation,
    EvidenceKind::BackgroundResume,
    EvidenceKind::LowMemoryRecovery,
    EvidenceKind::TerminalIme,
    EvidenceKind::TerminalAccessoryKeys,
    EvidenceKind::TerminalGpuRecovery,
];

/// Validates and canonically orders accessibility/device evidence.
///
/// Accepted exceptions remain separate and never remove failures.
///
/// # Errors
///
/// Rejects invalid limits, excessive or duplicate records/exceptions, malformed
/// bounded text, zero-check evidence, and exceptions for non-required cells.
pub fn validate_evidence(
    records: &[EvidenceRecord],
    exceptions: &[AcceptedException],
    limits: EvidenceLimits,
) -> Result<EvidenceReport, EvidenceError> {
    validate_limits(limits)?;
    if records.len() > limits.max_records || exceptions.len() > limits.max_exceptions {
        return Err(EvidenceError::RecordLimit);
    }
    let mut records = records.to_vec();
    records.sort();
    let mut ids = BTreeSet::new();
    let mut cells = BTreeSet::new();
    for r in &records {
        validate_atom(&r.evidence_id, limits.max_text_bytes)?;
        validate_atom(&r.artifact_id, limits.max_text_bytes)?;
        if r.check_count == 0 {
            return Err(EvidenceError::ZeroChecks);
        }
        if !ids.insert(r.evidence_id.as_str()) || !cells.insert((r.device, r.kind)) {
            return Err(EvidenceError::DuplicateEvidence);
        }
    }
    let mut failures = Vec::new();
    for device in [DeviceClass::Phone, DeviceClass::Tablet] {
        for kind in REQUIRED {
            match records
                .iter()
                .find(|record| record.device == device && record.kind == kind)
            {
                None => failures.push(EvidenceFailure {
                    device,
                    kind,
                    reason: FailureReason::Missing,
                }),
                Some(record) if !record.passed => failures.push(EvidenceFailure {
                    device,
                    kind,
                    reason: FailureReason::Failed,
                }),
                Some(_) => {}
            }
        }
    }
    let mut exceptions = exceptions.to_vec();
    exceptions.sort();
    let mut exception_ids = BTreeSet::new();
    let mut exception_cells = BTreeSet::new();
    for e in &exceptions {
        validate_atom(&e.exception_id, limits.max_text_bytes)?;
        validate_atom(&e.owner, limits.max_text_bytes)?;
        validate_atom(&e.expires_release, limits.max_text_bytes)?;
        validate_text(&e.rationale, limits.max_text_bytes)?;
        if e.rationale.is_empty()
            || !REQUIRED.contains(&e.kind)
            || !exception_ids.insert(e.exception_id.as_str())
            || !exception_cells.insert((e.device, e.kind))
        {
            return Err(EvidenceError::InvalidException);
        }
    }
    Ok(EvidenceReport {
        records,
        failures,
        exceptions,
    })
}

fn validate_limits(l: EvidenceLimits) -> Result<(), EvidenceError> {
    if l.max_records == 0 || l.max_text_bytes == 0 || l.max_exceptions == 0 {
        Err(EvidenceError::InvalidLimits)
    } else {
        Ok(())
    }
}
fn validate_atom(v: &str, max: usize) -> Result<(), EvidenceError> {
    validate_text(v, max)?;
    if v.is_empty()
        || !v
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        Err(EvidenceError::InvalidText)
    } else {
        Ok(())
    }
}
fn validate_text(v: &str, max: usize) -> Result<(), EvidenceError> {
    if v.len() > max || v.chars().any(|c| matches!(c, '\0' | '\r' | '\n')) {
        Err(EvidenceError::InvalidText)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceError {
    InvalidLimits,
    RecordLimit,
    InvalidText,
    ZeroChecks,
    DuplicateEvidence,
    InvalidException,
}

#[cfg(test)]
mod tests {
    use super::*;
    const L: EvidenceLimits = EvidenceLimits {
        max_records: 32,
        max_text_bytes: 128,
        max_exceptions: 4,
    };
    fn complete() -> Vec<EvidenceRecord> {
        [DeviceClass::Phone, DeviceClass::Tablet]
            .into_iter()
            .flat_map(|device| {
                REQUIRED.into_iter().map(move |kind| EvidenceRecord {
                    evidence_id: format!("{device:?}-{kind:?}"),
                    device,
                    kind,
                    passed: true,
                    check_count: 1,
                    artifact_id: "headless-report".into(),
                })
            })
            .collect()
    }
    #[test]
    fn complete_matrix_passes_and_is_canonical() {
        let mut records = complete();
        records.reverse();
        let report = validate_evidence(&records, &[], L).unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.records.len(), 24);
        assert!(report.records.windows(2).all(|p| p[0] <= p[1]));
    }
    #[test]
    fn missing_and_failed_cells_are_distinct() {
        let mut records = complete();
        records.retain(|r| {
            !(r.device == DeviceClass::Tablet && r.kind == EvidenceKind::TerminalGpuRecovery)
        });
        records
            .iter_mut()
            .find(|r| r.device == DeviceClass::Phone && r.kind == EvidenceKind::Contrast)
            .unwrap()
            .passed = false;
        let report = validate_evidence(&records, &[], L).unwrap();
        assert!(report.failures.contains(&EvidenceFailure {
            device: DeviceClass::Tablet,
            kind: EvidenceKind::TerminalGpuRecovery,
            reason: FailureReason::Missing
        }));
        assert!(report.failures.contains(&EvidenceFailure {
            device: DeviceClass::Phone,
            kind: EvidenceKind::Contrast,
            reason: FailureReason::Failed
        }));
    }
    #[test]
    fn exception_never_suppresses_failure() {
        let mut records = complete();
        records
            .iter_mut()
            .find(|r| r.device == DeviceClass::Phone && r.kind == EvidenceKind::ReducedMotion)
            .unwrap()
            .passed = false;
        let exception = AcceptedException {
            exception_id: "risk-1".into(),
            device: DeviceClass::Phone,
            kind: EvidenceKind::ReducedMotion,
            owner: "accessibility".into(),
            expires_release: "release-2".into(),
            rationale: "Pending platform compatibility evidence".into(),
        };
        let report = validate_evidence(&records, &[exception], L).unwrap();
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.exceptions.len(), 1);
    }
    #[test]
    fn duplicate_cells_zero_checks_and_malformed_exceptions_fail() {
        let mut records = complete();
        records.push(records[0].clone());
        assert_eq!(
            validate_evidence(&records, &[], L),
            Err(EvidenceError::DuplicateEvidence)
        );
        let mut zero = complete();
        zero[0].check_count = 0;
        assert_eq!(
            validate_evidence(&zero, &[], L),
            Err(EvidenceError::ZeroChecks)
        );
        let exception = AcceptedException {
            exception_id: "r".into(),
            device: DeviceClass::Phone,
            kind: EvidenceKind::Contrast,
            owner: "a11y".into(),
            expires_release: "r2".into(),
            rationale: String::new(),
        };
        assert_eq!(
            validate_evidence(&complete(), &[exception], L),
            Err(EvidenceError::InvalidException)
        );
    }
}
