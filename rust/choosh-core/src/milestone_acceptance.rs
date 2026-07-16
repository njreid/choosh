//! Deterministic M0-M6 acceptance-scenario evidence coordination.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Milestone {
    M0,
    M1,
    M2,
    M3,
    M4,
    M5,
    M6,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ScenarioClass {
    Headless,
    Device,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scenario {
    pub scenario_id: String,
    pub milestone: Milestone,
    pub class: ScenarioClass,
    pub step_budget: u32,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ScenarioOutcome {
    Passed,
    Failed,
    DevicePending,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioEvidence {
    pub scenario_id: String,
    pub steps: u32,
    pub outcome: ScenarioOutcome,
    pub artifact_id: String,
}

pub trait ScenarioExecutor {
    fn execute(&self, scenario: &Scenario) -> ScenarioEvidence;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcceptanceLimits {
    pub max_scenarios: usize,
    pub max_steps: u32,
    pub max_text_bytes: usize,
    pub max_artifacts: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MilestoneGate {
    pub milestone: Milestone,
    pub evidence: Vec<ScenarioEvidence>,
    pub status: GateStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateStatus {
    Complete,
    Failed,
    DevicePending,
    DependencyBlocked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptanceReport {
    pub gates: Vec<MilestoneGate>,
}

const REQUIRED: [(Milestone, &str, ScenarioClass); 14] = [
    (
        Milestone::M0,
        "m0-headless-bootstrap",
        ScenarioClass::Headless,
    ),
    (Milestone::M0, "m0-device-smoke", ScenarioClass::Device),
    (
        Milestone::M1,
        "m1-workspace-reconnect",
        ScenarioClass::Headless,
    ),
    (Milestone::M1, "m1-terminal-device", ScenarioClass::Device),
    (Milestone::M2, "m2-agent-events", ScenarioClass::Headless),
    (
        Milestone::M2,
        "m2-notification-device",
        ScenarioClass::Device,
    ),
    (Milestone::M3, "m3-service-gateway", ScenarioClass::Headless),
    (Milestone::M3, "m3-pinning-device", ScenarioClass::Device),
    (Milestone::M4, "m4-edit-diff-save", ScenarioClass::Headless),
    (Milestone::M4, "m4-editor-device", ScenarioClass::Device),
    (
        Milestone::M5,
        "m5-markdown-annotations",
        ScenarioClass::Headless,
    ),
    (
        Milestone::M5,
        "m5-accessibility-device",
        ScenarioClass::Device,
    ),
    (
        Milestone::M6,
        "m6-release-security",
        ScenarioClass::Headless,
    ),
    (Milestone::M6, "m6-device-matrix", ScenarioClass::Device),
];

/// Runs the required acceptance matrix twice and computes dependency gates.
///
/// Device scenarios may return `DevicePending`; such a milestone is never
/// complete. A milestone is dependency-blocked unless every earlier milestone is
/// complete.
///
/// # Errors
///
/// Rejects invalid limits, missing/duplicate/extra required scenarios, malformed
/// IDs, step/artifact bounds, mismatched evidence, invalid pending classification,
/// and nondeterministic executor results.
pub fn coordinate<E: ScenarioExecutor>(
    scenarios: &[Scenario],
    executor: &E,
    limits: AcceptanceLimits,
) -> Result<AcceptanceReport, AcceptanceError> {
    validate_limits(limits)?;
    if scenarios.len() > limits.max_scenarios {
        return Err(AcceptanceError::ScenarioLimit);
    }
    validate_matrix(scenarios, limits)?;
    let mut evidence = BTreeMap::new();
    let mut artifacts = BTreeSet::new();
    for scenario in scenarios {
        let first = executor.execute(scenario);
        let second = executor.execute(scenario);
        validate_evidence(scenario, &first, limits)?;
        validate_evidence(scenario, &second, limits)?;
        if first != second {
            return Err(AcceptanceError::Nondeterministic);
        }
        if first.outcome == ScenarioOutcome::DevicePending
            && scenario.class != ScenarioClass::Device
        {
            return Err(AcceptanceError::InvalidPending);
        }
        if !artifacts.insert(first.artifact_id.clone()) || artifacts.len() > limits.max_artifacts {
            return Err(AcceptanceError::ArtifactLimit);
        }
        evidence.insert(scenario.scenario_id.clone(), first);
    }
    let mut gates = Vec::new();
    let mut dependencies_complete = true;
    for milestone in [
        Milestone::M0,
        Milestone::M1,
        Milestone::M2,
        Milestone::M3,
        Milestone::M4,
        Milestone::M5,
        Milestone::M6,
    ] {
        let mut milestone_evidence: Vec<_> = scenarios
            .iter()
            .filter(|s| s.milestone == milestone)
            .map(|s| evidence[&s.scenario_id].clone())
            .collect();
        milestone_evidence.sort_by(|a, b| a.scenario_id.cmp(&b.scenario_id));
        let intrinsic = if milestone_evidence
            .iter()
            .any(|e| e.outcome == ScenarioOutcome::Failed)
        {
            GateStatus::Failed
        } else if milestone_evidence
            .iter()
            .any(|e| e.outcome == ScenarioOutcome::DevicePending)
        {
            GateStatus::DevicePending
        } else {
            GateStatus::Complete
        };
        let status = if dependencies_complete {
            intrinsic
        } else {
            GateStatus::DependencyBlocked
        };
        dependencies_complete &= status == GateStatus::Complete;
        gates.push(MilestoneGate {
            milestone,
            evidence: milestone_evidence,
            status,
        });
    }
    Ok(AcceptanceReport { gates })
}

fn validate_matrix(scenarios: &[Scenario], l: AcceptanceLimits) -> Result<(), AcceptanceError> {
    let mut seen = BTreeSet::new();
    for s in scenarios {
        validate_id(&s.scenario_id, l.max_text_bytes)?;
        if s.step_budget == 0 || s.step_budget > l.max_steps {
            return Err(AcceptanceError::StepLimit);
        }
        if !seen.insert((s.milestone, s.scenario_id.as_str())) {
            return Err(AcceptanceError::DuplicateScenario);
        }
    }
    let expected: BTreeSet<_> = REQUIRED.iter().map(|(m, id, _)| (*m, *id)).collect();
    let actual: BTreeSet<_> = scenarios
        .iter()
        .map(|s| (s.milestone, s.scenario_id.as_str()))
        .collect();
    if expected != actual {
        return Err(AcceptanceError::InvalidMatrix);
    }
    for (m, id, class) in REQUIRED {
        if scenarios
            .iter()
            .find(|s| s.milestone == m && s.scenario_id == id)
            .is_none_or(|s| s.class != class)
        {
            return Err(AcceptanceError::InvalidMatrix);
        }
    }
    Ok(())
}
fn validate_evidence(
    s: &Scenario,
    e: &ScenarioEvidence,
    l: AcceptanceLimits,
) -> Result<(), AcceptanceError> {
    if e.scenario_id != s.scenario_id {
        return Err(AcceptanceError::MismatchedEvidence);
    }
    if e.steps > s.step_budget || e.steps > l.max_steps {
        return Err(AcceptanceError::StepLimit);
    }
    validate_id(&e.artifact_id, l.max_text_bytes)
}
fn validate_limits(l: AcceptanceLimits) -> Result<(), AcceptanceError> {
    if l.max_scenarios == 0 || l.max_steps == 0 || l.max_text_bytes == 0 || l.max_artifacts == 0 {
        Err(AcceptanceError::InvalidLimits)
    } else {
        Ok(())
    }
}
fn validate_id(v: &str, max: usize) -> Result<(), AcceptanceError> {
    if v.is_empty()
        || v.len() > max
        || !v
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        Err(AcceptanceError::InvalidId)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcceptanceError {
    InvalidLimits,
    ScenarioLimit,
    StepLimit,
    ArtifactLimit,
    InvalidId,
    DuplicateScenario,
    InvalidMatrix,
    MismatchedEvidence,
    InvalidPending,
    Nondeterministic,
}

#[cfg(test)]
mod tests {
    use super::*;
    const L: AcceptanceLimits = AcceptanceLimits {
        max_scenarios: 20,
        max_steps: 10,
        max_text_bytes: 64,
        max_artifacts: 20,
    };
    fn matrix() -> Vec<Scenario> {
        REQUIRED
            .into_iter()
            .map(|(milestone, id, class)| Scenario {
                scenario_id: id.into(),
                milestone,
                class,
                step_budget: 4,
            })
            .collect()
    }
    struct Exec {
        pending: Option<String>,
        failed: Option<String>,
    }
    impl ScenarioExecutor for Exec {
        fn execute(&self, s: &Scenario) -> ScenarioEvidence {
            ScenarioEvidence {
                scenario_id: s.scenario_id.clone(),
                steps: 2,
                outcome: if self.failed.as_deref() == Some(&s.scenario_id) {
                    ScenarioOutcome::Failed
                } else if self.pending.as_deref() == Some(&s.scenario_id) {
                    ScenarioOutcome::DevicePending
                } else {
                    ScenarioOutcome::Passed
                },
                artifact_id: format!("artifact-{}", s.scenario_id),
            }
        }
    }
    #[test]
    fn complete_matrix_completes_all_gates() {
        let r = coordinate(
            &matrix(),
            &Exec {
                pending: None,
                failed: None,
            },
            L,
        )
        .unwrap();
        assert!(r.gates.iter().all(|g| g.status == GateStatus::Complete));
    }
    #[test]
    fn device_pending_never_completes_and_blocks_dependencies() {
        let r = coordinate(
            &matrix(),
            &Exec {
                pending: Some("m2-notification-device".into()),
                failed: None,
            },
            L,
        )
        .unwrap();
        assert_eq!(r.gates[2].status, GateStatus::DevicePending);
        assert!(
            r.gates[3..]
                .iter()
                .all(|g| g.status == GateStatus::DependencyBlocked)
        );
    }
    #[test]
    fn failed_early_gate_blocks_later_milestones() {
        let r = coordinate(
            &matrix(),
            &Exec {
                pending: None,
                failed: Some("m0-headless-bootstrap".into()),
            },
            L,
        )
        .unwrap();
        assert_eq!(r.gates[0].status, GateStatus::Failed);
        assert!(
            r.gates[1..]
                .iter()
                .all(|g| g.status == GateStatus::DependencyBlocked)
        );
    }
    #[test]
    fn missing_extra_and_headless_pending_fail() {
        let mut missing = matrix();
        missing.pop();
        assert_eq!(
            coordinate(
                &missing,
                &Exec {
                    pending: None,
                    failed: None
                },
                L
            ),
            Err(AcceptanceError::InvalidMatrix)
        );
        let pending = Exec {
            pending: Some("m0-headless-bootstrap".into()),
            failed: None,
        };
        assert_eq!(
            coordinate(&matrix(), &pending, L),
            Err(AcceptanceError::InvalidPending)
        );
    }
    struct Alternating(std::cell::Cell<bool>);
    impl ScenarioExecutor for Alternating {
        fn execute(&self, s: &Scenario) -> ScenarioEvidence {
            let old = self.0.replace(!self.0.get());
            ScenarioEvidence {
                scenario_id: s.scenario_id.clone(),
                steps: 1,
                outcome: if old {
                    ScenarioOutcome::Passed
                } else {
                    ScenarioOutcome::Failed
                },
                artifact_id: format!("artifact-{}", s.scenario_id),
            }
        }
    }
    #[test]
    fn nondeterminism_is_rejected() {
        assert_eq!(
            coordinate(&matrix(), &Alternating(std::cell::Cell::new(false)), L),
            Err(AcceptanceError::Nondeterministic)
        );
    }
}
