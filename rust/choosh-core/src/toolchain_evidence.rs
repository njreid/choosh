//! Canonical validation of supplied mobile and Rust toolchain compatibility evidence.

use std::collections::{BTreeMap, BTreeSet};
const MAX_TEXT: usize = 128;
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Component {
    AndroidSdk,
    Agp,
    Kotlin,
    Compose,
    Sora,
    NativeBridge,
    RustAndroidTarget,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lane {
    Stable,
    PreviewCompatibility,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComponentRecord {
    pub component: Component,
    pub version: String,
    pub lane: Lane,
    pub tested_together: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinSdkEvidence {
    pub selected: u16,
    pub tested_devices: BTreeSet<u16>,
    pub rationale: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Exception {
    pub component: Component,
    pub reason: String,
    pub expires_epoch_day: u32,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Evidence {
    pub records: Vec<ComponentRecord>,
    pub min_sdk: MinSdkEvidence,
    pub exceptions: Vec<Exception>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalReport {
    pub stable: BTreeMap<Component, String>,
    pub preview: BTreeMap<Component, String>,
    pub min_sdk: u16,
    pub exceptions: BTreeMap<Component, (String, u32)>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvidenceError {
    MissingComponent,
    DuplicateComponentLane,
    DynamicVersion,
    InvalidText,
    UntestedStableCombination,
    InvalidMinSdkEvidence,
    InvalidException,
    ExceptionExpired,
}

/// Validates and canonicalizes supplied evidence without consulting ambient tooling or time.
/// `today_epoch_day` is explicit so exception expiry is deterministic. Preview records are reported
/// separately and never satisfy stable requirements.
/// # Errors
/// Returns a typed missing, duplicate, dynamic, bounds, testing, min-SDK, or exception error.
pub fn validate(
    evidence: Evidence,
    today_epoch_day: u32,
) -> Result<CanonicalReport, EvidenceError> {
    let mut stable = BTreeMap::new();
    let mut preview = BTreeMap::new();
    for record in evidence.records {
        validate_text(&record.version)?;
        if dynamic(&record.version) {
            return Err(EvidenceError::DynamicVersion);
        }
        let map = match record.lane {
            Lane::Stable => {
                if !record.tested_together {
                    return Err(EvidenceError::UntestedStableCombination);
                }
                &mut stable
            }
            Lane::PreviewCompatibility => &mut preview,
        };
        if map.insert(record.component, record.version).is_some() {
            return Err(EvidenceError::DuplicateComponentLane);
        }
    }
    let required = [
        Component::AndroidSdk,
        Component::Agp,
        Component::Kotlin,
        Component::Compose,
        Component::Sora,
        Component::NativeBridge,
        Component::RustAndroidTarget,
    ];
    if required.into_iter().any(|c| !stable.contains_key(&c)) {
        return Err(EvidenceError::MissingComponent);
    }
    if evidence.min_sdk.selected == 0
        || evidence.min_sdk.tested_devices.is_empty()
        || !evidence
            .min_sdk
            .tested_devices
            .contains(&evidence.min_sdk.selected)
    {
        return Err(EvidenceError::InvalidMinSdkEvidence);
    }
    validate_text(&evidence.min_sdk.rationale)?;
    let mut exceptions = BTreeMap::new();
    for exception in evidence.exceptions {
        validate_text(&exception.reason).map_err(|_| EvidenceError::InvalidException)?;
        if exception.expires_epoch_day <= today_epoch_day {
            return Err(EvidenceError::ExceptionExpired);
        }
        if exceptions
            .insert(
                exception.component,
                (exception.reason, exception.expires_epoch_day),
            )
            .is_some()
        {
            return Err(EvidenceError::InvalidException);
        }
    }
    Ok(CanonicalReport {
        stable,
        preview,
        min_sdk: evidence.min_sdk.selected,
        exceptions,
    })
}
fn validate_text(value: &str) -> Result<(), EvidenceError> {
    if value.is_empty() || value.len() > MAX_TEXT || value.chars().any(char::is_control) {
        Err(EvidenceError::InvalidText)
    } else {
        Ok(())
    }
}
fn dynamic(version: &str) -> bool {
    version.contains('+')
        || version.contains('*')
        || version.eq_ignore_ascii_case("latest")
        || version.ends_with("-SNAPSHOT")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn evidence() -> Evidence {
        Evidence {
            records: [
                Component::RustAndroidTarget,
                Component::Compose,
                Component::AndroidSdk,
                Component::NativeBridge,
                Component::Kotlin,
                Component::Sora,
                Component::Agp,
            ]
            .into_iter()
            .map(|component| ComponentRecord {
                component,
                version: "1.2.3".into(),
                lane: Lane::Stable,
                tested_together: true,
            })
            .collect(),
            min_sdk: MinSdkEvidence {
                selected: 26,
                tested_devices: [26, 35].into(),
                rationale: "Native and security API floor".into(),
            },
            exceptions: vec![],
        }
    }
    #[test]
    fn shuffled_stable_matrix_canonicalizes_by_component() {
        let report = validate(evidence(), 100).unwrap();
        assert_eq!(report.stable.len(), 7);
        assert_eq!(report.min_sdk, 26);
    }
    #[test]
    fn preview_lane_is_reported_but_cannot_replace_stable() {
        let mut e = evidence();
        e.records.retain(|r| r.component != Component::Sora);
        e.records.push(ComponentRecord {
            component: Component::Sora,
            version: "2.0.0-beta1".into(),
            lane: Lane::PreviewCompatibility,
            tested_together: true,
        });
        assert_eq!(validate(e, 100), Err(EvidenceError::MissingComponent));
    }
    #[test]
    fn dynamic_and_untested_versions_fail_closed() {
        let mut e = evidence();
        e.records[0].version = "latest".into();
        assert_eq!(validate(e, 100), Err(EvidenceError::DynamicVersion));
        let mut e = evidence();
        e.records[0].tested_together = false;
        assert_eq!(
            validate(e, 100),
            Err(EvidenceError::UntestedStableCombination)
        );
    }
    #[test]
    fn min_sdk_requires_selected_device_evidence() {
        let mut e = evidence();
        e.min_sdk.tested_devices = [35].into();
        assert_eq!(validate(e, 100), Err(EvidenceError::InvalidMinSdkEvidence));
    }
    #[test]
    fn exceptions_are_explicit_bounded_and_time_boxed() {
        let mut e = evidence();
        e.exceptions.push(Exception {
            component: Component::Sora,
            reason: "Awaiting stable fix".into(),
            expires_epoch_day: 101,
        });
        assert!(validate(e.clone(), 100).is_ok());
        assert_eq!(validate(e, 101), Err(EvidenceError::ExceptionExpired));
    }
}
