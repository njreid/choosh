//! Pure exponential-backoff-with-full-jitter calculation for devhost
//! reconnects, per `docs/specs/relay-protocol.md`'s transport requirement:
//! "MUST reconnect on any close with exponential backoff and jitter,
//! starting at 1s and capped at 60s."

use std::time::Duration;

const BASE_SECS: f64 = 1.0;
const CAP_SECS: f64 = 60.0;

/// Computes the delay before reconnect attempt number `attempt` (0-indexed:
/// `attempt == 0` is the delay before the *first* retry, after the initial
/// connection attempt already failed).
///
/// `random_unit` MUST be in `[0.0, 1.0)`; callers supply it (rather than
/// this function drawing its own randomness) so the calculation stays a
/// pure, deterministically-testable function. Full jitter: the delay is
/// drawn uniformly from `[0, min(cap, base * 2^attempt))`, so `attempt == 0`
/// always yields a delay no greater than `BASE_SECS`, matching "starting at
/// 1s", and growth is capped at `CAP_SECS` regardless of how large `attempt`
/// gets (the exponent is clamped before the `2^attempt` computation so it
/// can never overflow `f64`).
#[must_use]
pub fn compute_backoff(attempt: u32, random_unit: f64) -> Duration {
    let random_unit = random_unit.clamp(0.0, 1.0);
    let exponent = attempt.min(6); // 2^6 == 64 already exceeds CAP_SECS
    let unclamped = BASE_SECS * 2f64.powi(i32::try_from(exponent).unwrap_or(i32::MAX));
    let ceiling = unclamped.min(CAP_SECS);
    Duration::from_secs_f64(ceiling * random_unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_retry_is_bounded_by_base() {
        assert!(compute_backoff(0, 1.0) <= Duration::from_secs_f64(BASE_SECS));
        assert_eq!(compute_backoff(0, 0.0), Duration::ZERO);
    }

    #[test]
    fn growth_is_capped_at_sixty_seconds() {
        for attempt in [6, 7, 20, 1000] {
            assert_eq!(compute_backoff(attempt, 1.0), Duration::from_secs_f64(CAP_SECS));
        }
    }

    #[test]
    fn delay_grows_monotonically_with_attempt_at_fixed_jitter() {
        let mut previous = Duration::ZERO;
        for attempt in 0..8 {
            let delay = compute_backoff(attempt, 1.0);
            assert!(delay >= previous, "attempt {attempt} did not grow: {delay:?} < {previous:?}");
            previous = delay;
        }
    }

    #[test]
    fn random_unit_is_clamped_to_a_valid_range() {
        assert_eq!(compute_backoff(3, -1.0), Duration::ZERO);
        assert_eq!(compute_backoff(3, 2.0), compute_backoff(3, 1.0));
    }
}
