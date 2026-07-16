//! Scheduled, generation-safe readiness probing for one immutable loopback destination.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbePolicy {
    pub max_attempts: u16,
    pub interval_ms: u64,
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadinessStatus {
    Starting,
    Running,
    Unknown,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeDiagnostic {
    Refused,
    TimedOut,
    Transport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeRequest {
    pub generation: u64,
    pub attempt: u16,
    pub host: &'static str,
    pub port: u16,
    pub timeout_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeSchedule {
    pub after_ms: u64,
    pub request: ProbeRequest,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeResult {
    Ready,
    NotReady(ProbeDiagnostic),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadinessError {
    InvalidPolicy,
    InvalidPort,
    StaleGeneration,
    StaleAttempt,
    NotProbing,
    AttemptExhausted,
}

#[derive(Debug)]
pub struct ReadinessProbe {
    port: u16,
    policy: ProbePolicy,
    generation: u64,
    next_attempt: u16,
    pending: Option<u16>,
    status: ReadinessStatus,
    diagnostic: Option<ProbeDiagnostic>,
}

impl ReadinessProbe {
    /// Creates a probe for exactly `127.0.0.1:port`.
    /// # Errors
    /// Returns an error for zero port or unusable bounded policy.
    pub fn new(port: u16, policy: ProbePolicy) -> Result<Self, ReadinessError> {
        if port == 0 {
            return Err(ReadinessError::InvalidPort);
        }
        if policy.max_attempts == 0
            || policy.max_attempts > 1024
            || policy.interval_ms == 0
            || policy.timeout_ms == 0
        {
            return Err(ReadinessError::InvalidPolicy);
        }
        Ok(Self {
            port,
            policy,
            generation: 0,
            next_attempt: 0,
            pending: None,
            status: ReadinessStatus::Cancelled,
            diagnostic: None,
        })
    }

    /// Starts a new process generation and returns its immediate first probe.
    /// # Errors
    /// Returns generation exhaustion if no next generation exists.
    pub fn start(&mut self) -> Result<ProbeSchedule, ReadinessError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(ReadinessError::AttemptExhausted)?;
        self.status = ReadinessStatus::Starting;
        self.next_attempt = 1;
        self.diagnostic = None;
        self.issue(0)
    }

    /// Marks a retained service unknown and schedules a fresh probe in the same generation.
    /// # Errors
    /// Returns an error if cancelled/failed or an attempt is already pending/exhausted.
    pub fn reconnect(&mut self) -> Result<ProbeSchedule, ReadinessError> {
        if matches!(
            self.status,
            ReadinessStatus::Cancelled | ReadinessStatus::Failed
        ) {
            return Err(ReadinessError::NotProbing);
        }
        self.status = ReadinessStatus::Unknown;
        self.next_attempt = 1;
        self.issue(0)
    }

    /// Applies one exact pending probe result and optionally schedules a retry.
    /// # Errors
    /// Rejects stale generations, stale attempts, and results while not probing.
    pub fn complete(
        &mut self,
        request: ProbeRequest,
        result: ProbeResult,
    ) -> Result<Option<ProbeSchedule>, ReadinessError> {
        if request.generation != self.generation {
            return Err(ReadinessError::StaleGeneration);
        }
        if self.pending != Some(request.attempt) {
            return Err(ReadinessError::StaleAttempt);
        }
        if !matches!(
            self.status,
            ReadinessStatus::Starting | ReadinessStatus::Unknown
        ) {
            return Err(ReadinessError::NotProbing);
        }
        self.pending = None;
        match result {
            ProbeResult::Ready => {
                self.status = ReadinessStatus::Running;
                self.diagnostic = None;
                Ok(None)
            }
            ProbeResult::NotReady(diagnostic) => {
                self.diagnostic = Some(diagnostic);
                if self.next_attempt > self.policy.max_attempts {
                    self.status = ReadinessStatus::Failed;
                    Ok(None)
                } else {
                    self.issue(self.policy.interval_ms).map(Some)
                }
            }
        }
    }

    pub fn cancel(&mut self) {
        self.pending = None;
        self.status = ReadinessStatus::Cancelled;
    }
    #[must_use]
    pub const fn status(&self) -> ReadinessStatus {
        self.status
    }
    #[must_use]
    pub const fn diagnostic(&self) -> Option<ProbeDiagnostic> {
        self.diagnostic
    }

    fn issue(&mut self, after_ms: u64) -> Result<ProbeSchedule, ReadinessError> {
        if self.pending.is_some() {
            return Err(ReadinessError::NotProbing);
        }
        if self.next_attempt == 0 || self.next_attempt > self.policy.max_attempts {
            return Err(ReadinessError::AttemptExhausted);
        }
        let attempt = self.next_attempt;
        self.next_attempt = attempt.saturating_add(1);
        self.pending = Some(attempt);
        Ok(ProbeSchedule {
            after_ms,
            request: ProbeRequest {
                generation: self.generation,
                attempt,
                host: "127.0.0.1",
                port: self.port,
                timeout_ms: self.policy.timeout_ms,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn probe() -> ReadinessProbe {
        ReadinessProbe::new(
            3000,
            ProbePolicy {
                max_attempts: 2,
                interval_ms: 50,
                timeout_ms: 10,
            },
        )
        .unwrap()
    }
    #[test]
    fn request_is_exact_bounded_loopback_destination() {
        let mut p = probe();
        let s = p.start().unwrap();
        assert_eq!(
            (s.request.host, s.request.port, s.request.timeout_ms),
            ("127.0.0.1", 3000, 10)
        );
    }
    #[test]
    fn retry_then_ready_transitions_running() {
        let mut p = probe();
        let first = p.start().unwrap();
        let second = p
            .complete(
                first.request,
                ProbeResult::NotReady(ProbeDiagnostic::Refused),
            )
            .unwrap()
            .unwrap();
        assert_eq!(second.after_ms, 50);
        assert_eq!(p.complete(second.request, ProbeResult::Ready), Ok(None));
        assert_eq!(p.status(), ReadinessStatus::Running);
    }
    #[test]
    fn exhaustion_is_failed_with_stable_diagnostic() {
        let mut p = probe();
        let a = p.start().unwrap();
        let b = p
            .complete(a.request, ProbeResult::NotReady(ProbeDiagnostic::TimedOut))
            .unwrap()
            .unwrap();
        assert_eq!(
            p.complete(b.request, ProbeResult::NotReady(ProbeDiagnostic::TimedOut)),
            Ok(None)
        );
        assert_eq!(p.status(), ReadinessStatus::Failed);
        assert_eq!(p.diagnostic(), Some(ProbeDiagnostic::TimedOut));
    }
    #[test]
    fn cancellation_and_generation_reject_stale_completion() {
        let mut p = probe();
        let old = p.start().unwrap();
        p.cancel();
        let fresh = p.start().unwrap();
        assert_eq!(
            p.complete(old.request, ProbeResult::Ready),
            Err(ReadinessError::StaleGeneration)
        );
        assert_eq!(p.complete(fresh.request, ProbeResult::Ready), Ok(None));
    }
    #[test]
    fn reconnect_unknown_probes_same_generation() {
        let mut p = probe();
        let first = p.start().unwrap();
        p.complete(first.request, ProbeResult::Ready).unwrap();
        let reconnect = p.reconnect().unwrap();
        assert_eq!(reconnect.request.generation, first.request.generation);
        assert_eq!(p.status(), ReadinessStatus::Unknown);
    }
}
