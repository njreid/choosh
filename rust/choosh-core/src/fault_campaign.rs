//! Deterministic bounded fault-campaign coordination and regression evidence.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CampaignLimits {
    pub max_cases: usize,
    pub max_input_bytes: usize,
    pub max_steps: u32,
    pub max_detail_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FaultTarget {
    Framing,
    Path,
    Event,
    Git,
    Gateway,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultCase {
    pub case_id: String,
    pub seed: u64,
    pub target: FaultTarget,
    pub input: Vec<u8>,
    pub step_budget: u32,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum InvariantOutcome {
    Clean,
    Rejected,
    LimitEnforced,
    Crash,
    Hang,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaseEvidence {
    pub case_id: String,
    pub seed: u64,
    pub target: FaultTarget,
    pub steps: u32,
    pub outcome: InvariantOutcome,
    pub detail_code: String,
}

/// Injected target execution boundary. Implementations must obey `step_budget`.
pub trait FaultExecutor {
    fn execute(&self, case: &FaultCase) -> CaseEvidence;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CampaignReport {
    pub evidence: Vec<CaseEvidence>,
    pub invariant_failures: Vec<CaseEvidence>,
}

/// Executes canonicalized cases twice and rejects nondeterministic evidence.
///
/// # Errors
///
/// Rejects invalid limits/cases, duplicate IDs or `(target, seed)` pairs,
/// oversized inputs/details, step-budget violations, mismatched evidence
/// identity, and nondeterministic executor output.
pub fn run_campaign<E: FaultExecutor>(
    cases: &[FaultCase],
    executor: &E,
    limits: CampaignLimits,
) -> Result<CampaignReport, CampaignError> {
    validate_limits(limits)?;
    if cases.len() > limits.max_cases {
        return Err(CampaignError::CaseLimit);
    }
    let mut ordered: Vec<&FaultCase> = cases.iter().collect();
    ordered.sort_by_key(|case| (&case.case_id, case.target, case.seed));
    validate_cases(&ordered, limits)?;
    let mut evidence = Vec::with_capacity(ordered.len());
    for case in ordered {
        let first = executor.execute(case);
        let second = executor.execute(case);
        validate_evidence(case, &first, limits)?;
        validate_evidence(case, &second, limits)?;
        if first != second {
            return Err(CampaignError::NondeterministicEvidence);
        }
        evidence.push(first);
    }
    let invariant_failures = evidence
        .iter()
        .filter(|item| {
            matches!(
                item.outcome,
                InvariantOutcome::Crash | InvariantOutcome::Hang
            )
        })
        .cloned()
        .collect();
    Ok(CampaignReport {
        evidence,
        invariant_failures,
    })
}

fn validate_cases(cases: &[&FaultCase], limits: CampaignLimits) -> Result<(), CampaignError> {
    let mut ids = BTreeSet::new();
    let mut seeds = BTreeSet::new();
    for case in cases {
        validate_code(&case.case_id, limits.max_detail_bytes)?;
        if !ids.insert(case.case_id.as_str()) || !seeds.insert((case.target, case.seed)) {
            return Err(CampaignError::DuplicateCase);
        }
        if case.input.len() > limits.max_input_bytes {
            return Err(CampaignError::InputLimit);
        }
        if case.step_budget == 0 || case.step_budget > limits.max_steps {
            return Err(CampaignError::StepLimit);
        }
    }
    Ok(())
}

fn validate_evidence(
    case: &FaultCase,
    e: &CaseEvidence,
    limits: CampaignLimits,
) -> Result<(), CampaignError> {
    if e.case_id != case.case_id || e.seed != case.seed || e.target != case.target {
        return Err(CampaignError::MismatchedEvidence);
    }
    if e.steps > case.step_budget || e.steps > limits.max_steps {
        return Err(CampaignError::StepLimit);
    }
    validate_code(&e.detail_code, limits.max_detail_bytes)
}

fn validate_limits(l: CampaignLimits) -> Result<(), CampaignError> {
    if l.max_cases == 0 || l.max_input_bytes == 0 || l.max_steps == 0 || l.max_detail_bytes == 0 {
        Err(CampaignError::InvalidLimits)
    } else {
        Ok(())
    }
}
fn validate_code(v: &str, max: usize) -> Result<(), CampaignError> {
    if v.is_empty()
        || v.len() > max
        || !v
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        Err(CampaignError::InvalidCode)
    } else {
        Ok(())
    }
}

/// Canonical regression corpus keyed by stable case ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegressionCorpus {
    cases: BTreeMap<String, FaultCase>,
    limits: CampaignLimits,
}

impl RegressionCorpus {
    #[must_use]
    pub fn new(limits: CampaignLimits) -> Self {
        Self {
            cases: BTreeMap::new(),
            limits,
        }
    }

    /// Inserts a canonical failing regression case.
    ///
    /// # Errors
    ///
    /// Rejects clean/non-failing outcomes, identity mismatch, duplicates, and
    /// invalid/oversized case or evidence fields.
    pub fn insert(
        &mut self,
        case: FaultCase,
        evidence: &CaseEvidence,
    ) -> Result<(), CampaignError> {
        validate_limits(self.limits)?;
        validate_cases(&[&case], self.limits)?;
        validate_evidence(&case, evidence, self.limits)?;
        if !matches!(
            evidence.outcome,
            InvariantOutcome::Crash | InvariantOutcome::Hang
        ) {
            return Err(CampaignError::NotRegression);
        }
        if self.cases.len() >= self.limits.max_cases {
            return Err(CampaignError::CaseLimit);
        }
        if self.cases.contains_key(&case.case_id) {
            return Err(CampaignError::DuplicateCase);
        }
        self.cases.insert(case.case_id.clone(), case);
        Ok(())
    }

    #[must_use]
    pub fn cases(&self) -> Vec<&FaultCase> {
        self.cases.values().collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CampaignError {
    InvalidLimits,
    CaseLimit,
    InputLimit,
    StepLimit,
    InvalidCode,
    DuplicateCase,
    MismatchedEvidence,
    NondeterministicEvidence,
    NotRegression,
}

#[cfg(test)]
mod tests {
    use super::*;
    const L: CampaignLimits = CampaignLimits {
        max_cases: 8,
        max_input_bytes: 16,
        max_steps: 10,
        max_detail_bytes: 32,
    };
    struct Deterministic;
    impl FaultExecutor for Deterministic {
        fn execute(&self, case: &FaultCase) -> CaseEvidence {
            CaseEvidence {
                case_id: case.case_id.clone(),
                seed: case.seed,
                target: case.target,
                steps: 2,
                outcome: if case.input.first() == Some(&0xff) {
                    InvariantOutcome::Crash
                } else {
                    InvariantOutcome::Rejected
                },
                detail_code: "bounded-result".into(),
            }
        }
    }
    fn case(id: &str, seed: u64, target: FaultTarget, input: &[u8]) -> FaultCase {
        FaultCase {
            case_id: id.into(),
            seed,
            target,
            input: input.into(),
            step_budget: 4,
        }
    }

    #[test]
    fn canonical_campaign_classifies_failures() {
        let cases = [
            case("z", 2, FaultTarget::Gateway, &[1]),
            case("a", 1, FaultTarget::Framing, &[0xff]),
        ];
        let report = run_campaign(&cases, &Deterministic, L).unwrap();
        assert_eq!(report.evidence[0].case_id, "a");
        assert_eq!(report.invariant_failures.len(), 1);
    }
    #[test]
    fn duplicate_seed_and_oversized_input_fail() {
        let duplicate = [
            case("a", 1, FaultTarget::Path, &[1]),
            case("b", 1, FaultTarget::Path, &[2]),
        ];
        assert_eq!(
            run_campaign(&duplicate, &Deterministic, L),
            Err(CampaignError::DuplicateCase)
        );
        assert_eq!(
            run_campaign(
                &[case("x", 1, FaultTarget::Git, &[0; 17])],
                &Deterministic,
                L
            ),
            Err(CampaignError::InputLimit)
        );
    }
    struct Alternating(std::cell::Cell<bool>);
    impl FaultExecutor for Alternating {
        fn execute(&self, case: &FaultCase) -> CaseEvidence {
            let prior = self.0.replace(!self.0.get());
            CaseEvidence {
                case_id: case.case_id.clone(),
                seed: case.seed,
                target: case.target,
                steps: 1,
                outcome: if prior {
                    InvariantOutcome::Clean
                } else {
                    InvariantOutcome::Rejected
                },
                detail_code: "result".into(),
            }
        }
    }
    #[test]
    fn nondeterministic_evidence_is_rejected() {
        assert_eq!(
            run_campaign(
                &[case("x", 1, FaultTarget::Event, &[1])],
                &Alternating(std::cell::Cell::new(false)),
                L
            ),
            Err(CampaignError::NondeterministicEvidence)
        );
    }
    #[test]
    fn corpus_is_canonical_and_failure_only() {
        let a = case("b", 2, FaultTarget::Gateway, &[0xff]);
        let b = case("a", 1, FaultTarget::Framing, &[0xff]);
        let ea = Deterministic.execute(&a);
        let eb = Deterministic.execute(&b);
        let mut corpus = RegressionCorpus::new(L);
        corpus.insert(a, &ea).unwrap();
        corpus.insert(b, &eb).unwrap();
        assert_eq!(
            corpus
                .cases()
                .iter()
                .map(|c| c.case_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        let clean = case("c", 3, FaultTarget::Git, &[1]);
        assert_eq!(
            corpus.insert(clean.clone(), &Deterministic.execute(&clean)),
            Err(CampaignError::NotRegression)
        );
    }
}
