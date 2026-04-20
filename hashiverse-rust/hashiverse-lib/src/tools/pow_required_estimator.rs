//! # Progress reporting and ETA estimation for long PoW searches
//!
//! A book-keeping helper for [`crate::tools::parallel_pow_generator`]: tracks how many
//! attempts have been made across repeated batches, what the best-so-far `pow` is, and
//! produces a human-readable log line including an ETA.
//!
//! Because PoW search is memoryless (geometric), past attempts give no information about
//! how many attempts remain — the *expected remaining* is always `2^pow_required` no
//! matter how long you've already been trying. The estimator uses rate of attempts per
//! second to turn that into wall-clock seconds. For a geometric distribution the standard
//! deviation equals the mean, so "30s ± 30s" ETAs are the norm; this is an honest report,
//! not a bug.
//!
//! The `best_pow_so_far` field doubles as a sanity check: after `n` attempts the
//! expected maximum leading-zero count is `log2(n) - 0.83`. A value wildly below that
//! suggests a broken RNG or hash chain.

use crate::tools::time::{DurationMillis, TimeMillis, MILLIS_IN_MILLISECOND};
use crate::tools::types::Pow;

/// Tracks progress across repeated calls to `pow_generate_with_iteration_limit`
/// and produces a log-friendly ETA estimate.
///
/// PoW search is a memoryless geometric process: the *expected remaining* attempts
/// is always `2^pow_required` regardless of how many have already been tried.
/// Consequently:
///   ETA_remaining = 2^pow_required / rate - elapsed
///   std_deviation  = 2^pow_required / rate  (equals the mean for a geometric distribution)
///
/// `best_pow_so_far` is a sanity check: after `n` iterations the expected maximum
/// leading-zero count is `log2(n) - 0.83`.  A value far below that suggests a
/// broken RNG or hash chain.
pub struct PowRequiredEstimator {
    description: String,
    pow_required: Pow,
    total_iterations: usize,
    best_pow_so_far: Pow,
    started_at_millis: TimeMillis,
}

impl PowRequiredEstimator {
    pub fn new(started_at_millis: TimeMillis, description: &str, pow_required: Pow) -> Self {
        Self {
            description: description.to_string(),
            pow_required,
            total_iterations: 0,
            best_pow_so_far: Pow(0),
            started_at_millis,
        }
    }

    fn report_large_number(num: usize) -> String {
        let num = num as f64;

        if num > 1e9 {
            return format!("{:.2}B", num / 1e9);
        }
        if num > 1e6 {
            return format!("{:.2}M", num / 1e6);
        }
        if num > 1e3 {
            return format!("{:.2}k", num / 1e3);
        }

        format!("{}", num)
    }

    /// Record the results of one batch and return a progress string suitable for logging.
    pub fn record_batch_and_estimate(&mut self, current_time_millis: TimeMillis, iterations_in_batch: usize, best_pow_in_batch: Pow) -> String {
        self.total_iterations += iterations_in_batch;
        if best_pow_in_batch > self.best_pow_so_far {
            self.best_pow_so_far = best_pow_in_batch;
        }

        let elapsed_duration_millis = current_time_millis - self.started_at_millis;

        if elapsed_duration_millis < MILLIS_IN_MILLISECOND || self.total_iterations == 0 {
            return format!(
                "{}: PoW {}/{} bits | {} | {} iters | too early to estimate",
                self.description,
                self.best_pow_so_far.0,
                self.pow_required.0,
                elapsed_duration_millis,
                Self::report_large_number(self.total_iterations)
            );
        }

        let iterations_per_second = 1000.0 * self.total_iterations as f64 / elapsed_duration_millis.0 as f64;
        // Remaining ETA: memoryless property means expected remaining = 2^pow_required
        let expected_total_iterations = (2.0f64).powi(self.pow_required.0 as i32);
        let expected_total_duration = DurationMillis((1000.0 * expected_total_iterations / iterations_per_second) as i64);
        let eta_remaining_millis = expected_total_duration - elapsed_duration_millis;
        // Std deviation of a geometric distribution equals its mean
        let eta_one_sigma = expected_total_duration;
        let progress_pct = self.total_iterations as f64 / expected_total_iterations * 100.0;

        format!(
            "{}: PoW {}/{} bits | {} | {} iters | {}/s | {:.1}% of expected | ETA ~{} \u{00b1}{}",
            self.description,
            self.best_pow_so_far.0,
            self.pow_required.0,
            elapsed_duration_millis,
            Self::report_large_number(self.total_iterations),
            Self::report_large_number(iterations_per_second as usize),
            progress_pct,
            eta_remaining_millis,
            eta_one_sigma,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_estimator() -> PowRequiredEstimator {
        PowRequiredEstimator::new(TimeMillis(0), "test", Pow(24))
    }

    #[test]
    fn record_batch_accumulates_iterations_and_tracks_best_pow() {
        let mut estimator = make_estimator();

        estimator.record_batch_and_estimate(TimeMillis(100), 1024, Pow(10));
        assert_eq!(estimator.total_iterations, 1024);
        assert_eq!(estimator.best_pow_so_far, Pow(10));

        estimator.record_batch_and_estimate(TimeMillis(200), 1024, Pow(8)); // worse — should not update best
        assert_eq!(estimator.total_iterations, 2048);
        assert_eq!(estimator.best_pow_so_far, Pow(10));

        estimator.record_batch_and_estimate(TimeMillis(300), 1024, Pow(15)); // better — should update best
        assert_eq!(estimator.total_iterations, 3072);
        assert_eq!(estimator.best_pow_so_far, Pow(15));
    }

    #[test]
    fn progress_string_contains_key_fields() {
        let mut estimator = make_estimator();
        let output = estimator.record_batch_and_estimate(TimeMillis(1000), 65536, Pow(18));

        assert!(output.contains("test"), "should include description: {}", output);
        assert!(output.contains("18/24"), "should show best/required bits: {}", output);
        assert!(output.contains("1s"), "should show elapsed time: {}", output);
        assert!(output.contains("65.54"), "should show iteration count: {}", output);
        assert!(output.contains("ETA"), "should show ETA: {}", output);
    }

    #[test]
    fn progress_string_before_any_elapsed_time() {
        let mut estimator = make_estimator();
        let output = estimator.record_batch_and_estimate(TimeMillis(0), 1024, Pow(5));
        assert!(output.contains("too early"), "{}", output);
    }
}
