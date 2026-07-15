//! Deterministic reconnect delay policy.
//!
//! This module reads neither clocks nor randomness. Callers supply logical time
//! and a seed-derived sample for each attempt, which keeps reconnect schedules
//! reproducible in headless tests.

/// Inclusive upper bound for a normalized jitter sample.
pub const JITTER_SAMPLE_MAX: u32 = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackoffConfig {
    pub base_delay_millis: u64,
    pub max_delay_millis: u64,
    /// Maximum variation around the exponential delay, in basis points.
    /// `1_000` means ten percent.
    pub jitter_basis_points: u16,
    pub max_attempts: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackoffError {
    ZeroBaseDelay,
    MaximumBelowBase,
    JitterOutOfRange,
    ZeroMaximumAttempts,
    AttemptOutOfRange,
    JitterSampleOutOfRange,
}

impl BackoffError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ZeroBaseDelay => "backoff_zero_base_delay",
            Self::MaximumBelowBase => "backoff_maximum_below_base",
            Self::JitterOutOfRange => "backoff_jitter_out_of_range",
            Self::ZeroMaximumAttempts => "backoff_zero_maximum_attempts",
            Self::AttemptOutOfRange => "backoff_attempt_out_of_range",
            Self::JitterSampleOutOfRange => "backoff_jitter_sample_out_of_range",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryDeadline {
    pub attempt: u32,
    pub delay_millis: u64,
    pub deadline_millis: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackoffPolicy {
    config: BackoffConfig,
}

impl BackoffPolicy {
    /// Validates and constructs a reconnect policy.
    ///
    /// Jitter is restricted below 100% so every scheduled delay remains at
    /// least one millisecond and can never create a retry busy-loop.
    ///
    /// # Errors
    /// Returns a stable typed error for the first invalid configuration field.
    pub const fn new(config: BackoffConfig) -> Result<Self, BackoffError> {
        if config.base_delay_millis == 0 {
            return Err(BackoffError::ZeroBaseDelay);
        }
        if config.max_delay_millis < config.base_delay_millis {
            return Err(BackoffError::MaximumBelowBase);
        }
        if config.jitter_basis_points >= 10_000 {
            return Err(BackoffError::JitterOutOfRange);
        }
        if config.max_attempts == 0 {
            return Err(BackoffError::ZeroMaximumAttempts);
        }
        Ok(Self { config })
    }

    #[must_use]
    pub const fn config(self) -> BackoffConfig {
        self.config
    }

    /// Computes a capped delay and deadline for a one-based retry attempt.
    ///
    /// `jitter_sample` is uniformly normalized to `0..=JITTER_SAMPLE_MAX` by
    /// the caller. Zero selects the low end, half selects the nominal delay,
    /// and the maximum selects the high end. The final delay never exceeds the
    /// configured cap. A deadline that cannot be represented saturates at
    /// `u64::MAX` rather than wrapping into an immediate retry.
    ///
    /// # Errors
    /// Returns `AttemptOutOfRange` for zero or exhausted attempts and
    /// `JitterSampleOutOfRange` for a non-normalized sample.
    pub fn deadline(
        self,
        attempt: u32,
        now_millis: u64,
        jitter_sample: u32,
    ) -> Result<RetryDeadline, BackoffError> {
        if attempt == 0 || attempt > self.config.max_attempts {
            return Err(BackoffError::AttemptOutOfRange);
        }
        if jitter_sample > JITTER_SAMPLE_MAX {
            return Err(BackoffError::JitterSampleOutOfRange);
        }

        let shift = attempt.saturating_sub(1).min(127);
        let exponential = u128::from(self.config.base_delay_millis)
            .checked_shl(shift)
            .unwrap_or(u128::MAX);
        let nominal = exponential.min(u128::from(self.config.max_delay_millis));
        let spread = nominal * u128::from(self.config.jitter_basis_points) / 10_000;
        let low = nominal - spread;
        let width = spread * 2;
        let jittered = low + width * u128::from(jitter_sample) / u128::from(JITTER_SAMPLE_MAX);
        let bounded_delay = jittered
            .max(1)
            .min(u128::from(self.config.max_delay_millis));
        let delay_millis = u64::try_from(bounded_delay).unwrap_or(u64::MAX);

        Ok(RetryDeadline {
            attempt,
            delay_millis,
            deadline_millis: now_millis.saturating_add(delay_millis),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn config() -> BackoffConfig {
        BackoffConfig {
            base_delay_millis: 100,
            max_delay_millis: 1_000,
            jitter_basis_points: 1_000,
            max_attempts: 64,
        }
    }

    #[test]
    fn rejects_invalid_configuration_with_stable_codes() {
        let cases = [
            (
                BackoffConfig {
                    base_delay_millis: 0,
                    ..config()
                },
                BackoffError::ZeroBaseDelay,
                "backoff_zero_base_delay",
            ),
            (
                BackoffConfig {
                    base_delay_millis: 101,
                    max_delay_millis: 100,
                    ..config()
                },
                BackoffError::MaximumBelowBase,
                "backoff_maximum_below_base",
            ),
            (
                BackoffConfig {
                    jitter_basis_points: 10_000,
                    ..config()
                },
                BackoffError::JitterOutOfRange,
                "backoff_jitter_out_of_range",
            ),
            (
                BackoffConfig {
                    max_attempts: 0,
                    ..config()
                },
                BackoffError::ZeroMaximumAttempts,
                "backoff_zero_maximum_attempts",
            ),
        ];
        for (config, expected, code) in cases {
            let error = BackoffPolicy::new(config).unwrap_err();
            assert_eq!(error, expected);
            assert_eq!(error.code(), code);
        }
    }

    #[test]
    fn exponential_schedule_is_capped_without_overflow() {
        let policy = BackoffPolicy::new(BackoffConfig {
            jitter_basis_points: 0,
            ..config()
        })
        .unwrap();
        let expected = [100, 200, 400, 800, 1_000, 1_000];
        for (attempt, expected_delay) in (1..).zip(expected) {
            assert_eq!(
                policy.deadline(attempt, 7, 500_000).unwrap().delay_millis,
                expected_delay
            );
        }
        assert_eq!(policy.deadline(64, 7, 500_000).unwrap().delay_millis, 1_000);
    }

    #[test]
    fn deterministic_jitter_selects_low_center_and_capped_high() {
        let policy = BackoffPolicy::new(config()).unwrap();
        assert_eq!(policy.deadline(3, 1_000, 0).unwrap().delay_millis, 360);
        assert_eq!(
            policy.deadline(3, 1_000, 500_000).unwrap().delay_millis,
            400
        );
        assert_eq!(
            policy
                .deadline(3, 1_000, JITTER_SAMPLE_MAX)
                .unwrap()
                .delay_millis,
            440
        );
        assert_eq!(
            policy
                .deadline(5, 1_000, JITTER_SAMPLE_MAX)
                .unwrap()
                .delay_millis,
            1_000
        );
        assert_eq!(
            policy.deadline(3, 1_000, 123_456),
            policy.deadline(3, 1_000, 123_456)
        );
    }

    #[test]
    fn attempts_and_samples_are_bounded() {
        let policy = BackoffPolicy::new(BackoffConfig {
            max_attempts: 2,
            ..config()
        })
        .unwrap();
        assert_eq!(
            policy.deadline(0, 0, 0),
            Err(BackoffError::AttemptOutOfRange)
        );
        assert_eq!(
            policy.deadline(3, 0, 0),
            Err(BackoffError::AttemptOutOfRange)
        );
        assert_eq!(
            policy.deadline(1, 0, JITTER_SAMPLE_MAX + 1),
            Err(BackoffError::JitterSampleOutOfRange)
        );
    }

    #[test]
    fn deadlines_saturate_and_delays_never_create_busy_loop() {
        let policy = BackoffPolicy::new(BackoffConfig {
            base_delay_millis: 1,
            max_delay_millis: u64::MAX,
            jitter_basis_points: 9_999,
            max_attempts: u32::MAX,
        })
        .unwrap();
        for attempt in [1, 2, 64, 128, u32::MAX] {
            for sample in [0, 500_000, JITTER_SAMPLE_MAX] {
                let result = policy.deadline(attempt, u64::MAX - 1, sample).unwrap();
                assert!(result.delay_millis >= 1);
                assert_eq!(result.deadline_millis, u64::MAX);
            }
        }
    }

    #[test]
    fn jitter_is_monotonic_and_always_within_cap() {
        let policy = BackoffPolicy::new(config()).unwrap();
        for attempt in 1..=policy.config().max_attempts {
            let mut previous = 0;
            for sample in (0..=JITTER_SAMPLE_MAX).step_by(997) {
                let delay = policy.deadline(attempt, 0, sample).unwrap().delay_millis;
                assert!(delay >= previous);
                assert!(delay <= policy.config().max_delay_millis);
                previous = delay;
            }
        }
    }
}
