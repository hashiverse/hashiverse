//! # Exponentially-decaying event counter
//!
//! A tiny book-keeping helper for "how busy has this been *lately*" metrics. Each
//! [`DecayingCounter`] holds a single `f64` accumulator that decays continuously
//! toward zero with a configurable time constant `τ`. Recording an event adds to
//! the accumulator; reading it decays the stored value forward to the current
//! instant first.
//!
//! For a steady arrival rate `r` (events per millisecond) the accumulator settles
//! at `r · τ`. So a counter with `τ = 1 hour` reads directly as "estimated events
//! in the last hour", `τ = 1 day` as "events in the last day", and so on — the
//! same exponentially-weighted idea behind Unix's `load_1m/5m/15m`.
//!
//! Like [`crate::tools::pow_required_estimator`] this takes [`TimeMillis`] as a
//! plain parameter rather than depending on a `TimeProvider`, which keeps it
//! trivially testable with explicit timestamps.

use crate::tools::time::{DurationMillis, TimeMillis};

/// A single exponentially-weighted decaying accumulator. See the module docs for
/// the `r · τ` steady-state interpretation.
#[derive(Debug, Clone)]
pub struct DecayingCounter {
    value: f64,
    last_update_millis: TimeMillis,
    time_constant_millis: f64,
}

impl DecayingCounter {
    /// Create an empty counter with the given decay time constant `τ`.
    pub fn new(time_constant: DurationMillis) -> Self {
        Self {
            value: 0.0,
            // TimeMillis(0) is safe as a baseline: a zero accumulator decays to
            // zero from any starting point, so the first `record` just anchors the
            // clock without spuriously decaying a real value.
            last_update_millis: TimeMillis::zero(),
            time_constant_millis: time_constant.0 as f64,
        }
    }

    /// Decay the stored value forward to `now`, then add `count` events.
    pub fn record(&mut self, now: TimeMillis, count: u64) {
        self.value = self.estimate(now) + count as f64;
        self.last_update_millis = now;
    }

    /// The decayed accumulator at `now` — an estimate of the number of events in
    /// roughly the last `τ`. Does not mutate, so repeated calls at the same `now`
    /// are stable.
    pub fn estimate(&self, now: TimeMillis) -> f64 {
        // Clamp to >= 0: a `now` earlier than the last update (clock skew, or a
        // scaled test clock stepping backwards) must not inflate the value.
        let elapsed_millis = (now.0 - self.last_update_millis.0).max(0) as f64;
        self.value * (-elapsed_millis / self.time_constant_millis).exp()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::time::{MILLIS_IN_HOUR, MILLIS_IN_MINUTE};

    #[test]
    fn zero_before_any_record() {
        let counter = DecayingCounter::new(MILLIS_IN_HOUR);
        assert_eq!(counter.estimate(TimeMillis::zero()), 0.0);
        assert_eq!(counter.estimate(TimeMillis(MILLIS_IN_HOUR.0)), 0.0);
    }

    #[test]
    fn single_record_reads_count_immediately_then_decays() {
        let mut counter = DecayingCounter::new(MILLIS_IN_HOUR);
        let t0 = TimeMillis(MILLIS_IN_HOUR.0); // non-zero so we exercise real decay maths
        counter.record(t0, 100);

        // Immediately, the estimate is the recorded count.
        assert!((counter.estimate(t0) - 100.0).abs() < 1e-9, "got {}", counter.estimate(t0));

        // After one time constant it has decayed by a factor of e.
        let after_one_tau = TimeMillis(t0.0 + MILLIS_IN_HOUR.0);
        let expected = 100.0 / std::f64::consts::E;
        assert!((counter.estimate(after_one_tau) - expected).abs() < 1e-6, "got {}", counter.estimate(after_one_tau));

        // Far in the future it approaches zero.
        let much_later = TimeMillis(t0.0 + MILLIS_IN_HOUR.0 * 100);
        assert!(counter.estimate(much_later) < 1e-6, "got {}", counter.estimate(much_later));
    }

    #[test]
    fn steady_cadence_converges_near_rate_times_tau() {
        // One event per minute into a 1-hour counter should settle near 60.
        let mut counter = DecayingCounter::new(MILLIS_IN_HOUR);
        let mut now = TimeMillis::zero();
        for _ in 0..10_000 {
            now = TimeMillis(now.0 + MILLIS_IN_MINUTE.0);
            counter.record(now, 1);
        }
        let estimate = counter.estimate(now);
        assert!((estimate - 60.0).abs() < 1.0, "expected ~60, got {}", estimate);
    }

    #[test]
    fn estimate_is_non_mutating() {
        let mut counter = DecayingCounter::new(MILLIS_IN_HOUR);
        counter.record(TimeMillis(MILLIS_IN_HOUR.0), 50);
        let later = TimeMillis(MILLIS_IN_HOUR.0 * 3);
        let first = counter.estimate(later);
        let second = counter.estimate(later);
        assert_eq!(first, second);
    }

    #[test]
    fn backwards_now_does_not_inflate() {
        let mut counter = DecayingCounter::new(MILLIS_IN_HOUR);
        let t0 = TimeMillis(MILLIS_IN_HOUR.0 * 5);
        counter.record(t0, 100);
        // A timestamp before the last update is clamped — never larger than the value at t0.
        let earlier = TimeMillis(t0.0 - MILLIS_IN_HOUR.0);
        assert!(counter.estimate(earlier) <= 100.0 + 1e-9, "got {}", counter.estimate(earlier));
        assert!((counter.estimate(earlier) - 100.0).abs() < 1e-9, "got {}", counter.estimate(earlier));
    }
}
