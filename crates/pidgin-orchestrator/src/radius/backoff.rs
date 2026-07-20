//! The pure backoff / 404 state machine driving radius heartbeats.
//!
//! This is the decision core of pi's `heartbeatMachine` / `heartbeatPi`
//! (`radius.ts`) with the network and clock removed: given the current counters
//! and a heartbeat [`HeartbeatOutcome`], it returns the next [`HeartbeatDecision`]
//! — reschedule after a delay, or re-register — touching nothing external. Kept
//! separate (like pidgin-ai's OAuth `Step` machines) so tests drive every branch
//! deterministically.

/// Consecutive `404`s tolerated before re-registering (`NOT_FOUND_RETRY_THRESHOLD`).
const NOT_FOUND_RETRY_THRESHOLD: u32 = 3;
/// Backoff base delay (`HEARTBEAT_BACKOFF_BASE_MS`).
const HEARTBEAT_BACKOFF_BASE_MS: i64 = 1_000;
/// Backoff ceiling (`HEARTBEAT_BACKOFF_MAX_MS`).
const HEARTBEAT_BACKOFF_MAX_MS: i64 = 30_000;

/// The exponential (pre-jitter) delay for a failure count, mirroring pi's
/// `HEARTBEAT_BACKOFF_BASE_MS * 2 ** Math.max(0, failureCount - 1)` capped at the
/// max. Pure and total (saturating on overflow).
fn exponential_delay_ms(failure_count: u32) -> i64 {
    let exponent = failure_count.saturating_sub(1);
    let factor = 2_i64.checked_pow(exponent).unwrap_or(i64::MAX);
    let scaled = HEARTBEAT_BACKOFF_BASE_MS
        .checked_mul(factor)
        .unwrap_or(i64::MAX);
    scaled.min(HEARTBEAT_BACKOFF_MAX_MS)
}

/// The exclusive upper bound of the jitter window, mirroring pi's
/// `Math.max(250, exponentialDelay / 4)` (integer-floored division).
fn jitter_upper_bound(exponential_delay: i64) -> i64 {
    (exponential_delay / 4).max(250)
}

/// The full backoff delay for a failure count, given a `[0, 1)` random sample.
///
/// Mirrors pi's `computeBackoffDelayMs`: exponential delay plus a jittered
/// `floor(random * max(250, exp/4))`, capped at the max. `random01` is an
/// injected sample (pi's `Math.random()`), so the function stays pure and
/// testable.
pub fn compute_backoff_delay_ms(failure_count: u32, random01: f64) -> i64 {
    let exponential_delay = exponential_delay_ms(failure_count);
    let upper = jitter_upper_bound(exponential_delay);
    let jitter = (random01.clamp(0.0, 1.0) * upper as f64).floor() as i64;
    (exponential_delay + jitter).min(HEARTBEAT_BACKOFF_MAX_MS)
}

/// The outcome of a single heartbeat request, as the pure state machine sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    /// The heartbeat succeeded.
    Success,
    /// The heartbeat returned `404` — the registration is stale.
    NotFound,
    /// The heartbeat failed for any other reason (network, 5xx, ...).
    TransientError,
}

/// What the state machine decided to do after applying an outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatDecision {
    /// Schedule the next heartbeat after this delay.
    Reschedule {
        /// The delay before the next heartbeat, in milliseconds.
        delay_ms: i64,
    },
    /// The `404` threshold was reached — the caller must re-register.
    ReRegister,
}

/// The pure backoff / 404 counters for one heartbeat target (the machine, or a
/// single Pi). Mirrors the `intervalMs` + `consecutiveNotFoundCount` +
/// `transientFailureCount` triple pi keeps per target, with the decision logic
/// of `heartbeatMachine` / `heartbeatPi` factored out of the network path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatBackoff {
    /// The steady-state heartbeat interval from the last registration.
    pub interval_ms: i64,
    /// Consecutive `404`s seen since the last success.
    pub consecutive_not_found: u32,
    /// Consecutive transient failures since the last success.
    pub transient_failure: u32,
}

impl HeartbeatBackoff {
    /// A fresh backoff at `interval_ms` with zeroed counters.
    pub fn new(interval_ms: i64) -> Self {
        Self {
            interval_ms,
            consecutive_not_found: 0,
            transient_failure: 0,
        }
    }

    /// Apply a heartbeat outcome, updating the counters and returning the
    /// decision. `random01` is the jitter sample used on a transient failure.
    pub fn apply(&mut self, outcome: HeartbeatOutcome, random01: f64) -> HeartbeatDecision {
        match outcome {
            HeartbeatOutcome::Success => self.on_success(),
            HeartbeatOutcome::TransientError => self.on_transient(random01),
            HeartbeatOutcome::NotFound => self.on_not_found(),
        }
    }

    /// Reset both counters and reschedule at the steady-state interval.
    fn on_success(&mut self) -> HeartbeatDecision {
        self.consecutive_not_found = 0;
        self.transient_failure = 0;
        HeartbeatDecision::Reschedule {
            delay_ms: self.interval_ms,
        }
    }

    /// Increment the transient counter and reschedule at the backoff delay.
    fn on_transient(&mut self, random01: f64) -> HeartbeatDecision {
        self.transient_failure += 1;
        HeartbeatDecision::Reschedule {
            delay_ms: compute_backoff_delay_ms(self.transient_failure, random01),
        }
    }

    /// A `404` clears the transient counter and increments the not-found
    /// counter; below the threshold it reschedules at the interval, at or above
    /// it asks to re-register.
    fn on_not_found(&mut self) -> HeartbeatDecision {
        self.transient_failure = 0;
        self.consecutive_not_found += 1;
        if self.consecutive_not_found < NOT_FOUND_RETRY_THRESHOLD {
            HeartbeatDecision::Reschedule {
                delay_ms: self.interval_ms,
            }
        } else {
            HeartbeatDecision::ReRegister
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_delay_escalates_and_caps() {
        assert_eq!(exponential_delay_ms(1), 1_000);
        assert_eq!(exponential_delay_ms(2), 2_000);
        assert_eq!(exponential_delay_ms(3), 4_000);
        assert_eq!(exponential_delay_ms(4), 8_000);
        assert_eq!(exponential_delay_ms(5), 16_000);
        // 32_000 would exceed the ceiling.
        assert_eq!(exponential_delay_ms(6), 30_000);
        assert_eq!(exponential_delay_ms(100), 30_000);
    }

    #[test]
    fn backoff_delay_adds_floored_jitter_and_caps() {
        // Zero jitter -> exactly the exponential delay.
        assert_eq!(compute_backoff_delay_ms(1, 0.0), 1_000);
        // Jitter window for fc=1 is max(250, 1000/4)=250; near-1 sample adds ~249.
        let jittered = compute_backoff_delay_ms(1, 0.999);
        assert!((1_249..=1_250).contains(&jittered), "got {jittered}");
        // Capped at the ceiling even with maximal jitter.
        assert_eq!(compute_backoff_delay_ms(100, 1.0), 30_000);
    }

    #[test]
    fn success_resets_counters() {
        let mut backoff = HeartbeatBackoff::new(15_000);
        backoff.transient_failure = 4;
        backoff.consecutive_not_found = 2;
        let decision = backoff.apply(HeartbeatOutcome::Success, 0.0);
        assert_eq!(decision, HeartbeatDecision::Reschedule { delay_ms: 15_000 });
        assert_eq!(backoff.transient_failure, 0);
        assert_eq!(backoff.consecutive_not_found, 0);
    }

    #[test]
    fn transient_escalates() {
        let mut backoff = HeartbeatBackoff::new(15_000);
        assert_eq!(
            backoff.apply(HeartbeatOutcome::TransientError, 0.0),
            HeartbeatDecision::Reschedule { delay_ms: 1_000 }
        );
        assert_eq!(
            backoff.apply(HeartbeatOutcome::TransientError, 0.0),
            HeartbeatDecision::Reschedule { delay_ms: 2_000 }
        );
        assert_eq!(backoff.transient_failure, 2);
    }

    #[test]
    fn not_found_threshold_triggers_reregister() {
        let mut backoff = HeartbeatBackoff::new(15_000);
        // First two 404s stay below the threshold and reschedule at the interval.
        assert_eq!(
            backoff.apply(HeartbeatOutcome::NotFound, 0.0),
            HeartbeatDecision::Reschedule { delay_ms: 15_000 }
        );
        assert_eq!(
            backoff.apply(HeartbeatOutcome::NotFound, 0.0),
            HeartbeatDecision::Reschedule { delay_ms: 15_000 }
        );
        // The third reaches the threshold.
        assert_eq!(
            backoff.apply(HeartbeatOutcome::NotFound, 0.0),
            HeartbeatDecision::ReRegister
        );
        assert_eq!(backoff.consecutive_not_found, 3);
    }

    #[test]
    fn not_found_clears_transient_counter() {
        let mut backoff = HeartbeatBackoff::new(15_000);
        backoff.apply(HeartbeatOutcome::TransientError, 0.0);
        assert_eq!(backoff.transient_failure, 1);
        backoff.apply(HeartbeatOutcome::NotFound, 0.0);
        assert_eq!(backoff.transient_failure, 0);
        assert_eq!(backoff.consecutive_not_found, 1);
    }
}
