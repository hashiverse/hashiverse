//! Trait, observability primitives, and shared batching loop for the PoW search engine.
//!
//! See [`crate::tools::pow_generator`] for the broader module overview. This file holds:
//!
//! - [`PowGenerator`] — the trait every concrete generator implements.
//! - [`JobTracker`] + [`PowJobStatus`] — the in-flight job registry surfaced via
//!   `PowGenerator::active_jobs()` so the UI can show users why an action is slow.
//! - [`generate_loop`] — the one shared batching loop both implementations use.

use crate::tools::pow_required_estimator::PowRequiredEstimator;
use crate::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
use crate::tools::tools;
use crate::tools::types::{Hash, Pow, Salt};
use log::trace;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub struct PowJobStatus {
    pub label: String,
    pub pow_min: Pow,
    pub best_pow_so_far: Pow,
}

type JobId = u64;

struct JobEntry {
    label: String,
    pow_min: Pow,
    best_pow_so_far: Pow,
}

#[derive(Default)]
pub struct JobTracker {
    next_id: JobId,
    jobs: HashMap<JobId, JobEntry>,
}

impl JobTracker {
    pub fn add(&mut self, label: &str, pow_min: Pow) -> JobId {
        let job_id = self.next_id;
        self.next_id += 1;
        self.jobs.insert(job_id, JobEntry { label: label.to_string(), pow_min, best_pow_so_far: Pow(0) });
        job_id
    }

    pub fn update(&mut self, job_id: JobId, best_pow_so_far: Pow) {
        if let Some(entry) = self.jobs.get_mut(&job_id) {
            entry.best_pow_so_far = best_pow_so_far;
        }
    }

    pub fn remove(&mut self, job_id: JobId) {
        self.jobs.remove(&job_id);
    }

    pub fn snapshot(&self) -> Vec<PowJobStatus> {
        self.jobs.values().map(|entry| PowJobStatus {
            label: entry.label.clone(),
            pow_min: entry.pow_min,
            best_pow_so_far: entry.best_pow_so_far,
        }).collect()
    }
}

/// RAII guard that registers a job on construction and removes it on drop.
/// Guarantees cleanup on normal return, `?` propagation, panic, and future cancellation.
struct TrackedJobGuard {
    tracker: Arc<Mutex<JobTracker>>,
    job_id: JobId,
}

impl TrackedJobGuard {
    fn new(tracker: Arc<Mutex<JobTracker>>, label: &str, pow_min: Pow) -> Self {
        let job_id = tracker.lock().unwrap().add(label, pow_min);
        Self { tracker, job_id }
    }

    fn update(&self, best_pow_so_far: Pow) {
        self.tracker.lock().unwrap().update(self.job_id, best_pow_so_far);
    }
}

impl Drop for TrackedJobGuard {
    fn drop(&mut self) {
        self.tracker.lock().unwrap().remove(self.job_id);
    }
}

/// A pluggable engine for searching for proof-of-work solutions.
///
/// Proof-of-work is required on every RPC packet, on peer announcements, and on report /
/// feedback submissions, so finding PoW is on the hot path for every outbound action a client
/// or server takes. `PowGenerator` abstracts over the concrete way we search for PoW so the
/// calling code stays platform-agnostic:
///
/// - [`crate::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator`]
///   uses `rayon` + `tokio::task::spawn_blocking` to pin the search across all CPU cores on
///   native targets.
/// - [`crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator`]
///   is a single-threaded fallback that works on every target, including WASM. Browser clients
///   use this (with a relaxed `pow_min`) because Web Workers do not expose `rayon` / threads
///   directly.
///
/// Implementations must also maintain the `active_jobs()` observability view — the UI surfaces
/// in-progress PoW searches to end users so they understand why an action is slow.
#[async_trait::async_trait]
pub trait PowGenerator: Send + Sync {
    /// Run up to `iteration_limit` hash attempts and return the best `(Salt, Pow, Hash)` found.
    /// Exits early if `pow >= pow_min` is achieved.
    ///
    /// `label` is a human-readable job name for observability (e.g. `"rpc:AnnounceV1"`, `"feedback"`).
    /// `data_hash` must be pre-computed via `pow_compute_data_hash` before calling.
    ///
    /// Note: this method does NOT register the job with the tracker. Use it only from inside
    /// `generate_loop` (which manages its own tracker entry across batches). Direct callers
    /// that want their single-batch search to show up in `active_jobs()` should use
    /// [`Self::generate_best_effort_tracked`] instead.
    async fn generate_best_effort(&self, label: &str, iteration_limit: usize, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)>;

    /// Loop `generate_best_effort` in batches until `pow >= pow_min` is achieved.
    async fn generate(&self, label: &str, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)>;

    /// Snapshot of all concurrently in-flight tracked jobs.
    fn active_jobs(&self) -> Vec<PowJobStatus>;

    /// Accessor for the impl's `JobTracker`. Exists so the default `generate_best_effort_tracked`
    /// implementation can register the job without each impl having to duplicate the wrapping.
    fn tracker(&self) -> &Arc<Mutex<JobTracker>>;

    /// `generate_best_effort` plus tracker registration for the duration of the call.
    /// Use this when a single-batch PoW is run directly (i.e. not inside `generate_loop`),
    /// otherwise the job is invisible to `active_jobs()`.
    async fn generate_best_effort_tracked(&self, label: &str, iteration_limit: usize, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
        let _guard = TrackedJobGuard::new(self.tracker().clone(), label, pow_min);
        self.generate_best_effort(label, iteration_limit, pow_min, data_hash).await
    }
}

/// Shared loop logic for `generate`: repeatedly calls `generate_best_effort` in
/// `BATCH_SIZE` batches until `pow >= pow_min`, tracking progress via `JobTracker`.
///
/// Future optimization: the current batch-and-wait approach dispatches to all N workers,
/// then waits for all N to respond before dispatching the next batch. This means fast
/// workers sit idle while the slowest worker finishes. A better design would feed workers
/// individually as they complete (work-stealing / pool-style), maintaining a shared
/// "best result so far" per job and checking pow_min after each worker result. This would
/// also allow concurrent generate() calls to have their batches truly interleaved at the
/// individual-worker level rather than at the batch level.
pub async fn generate_loop(
    generator: &(dyn PowGenerator + '_),
    tracker: &Arc<Mutex<JobTracker>>,
    label: &str,
    pow_min: Pow,
    data_hash: Hash,
) -> anyhow::Result<(Salt, Pow, Hash)> {
    const BATCH_SIZE: usize = 64 * 1024;
    let real_time_provider = RealTimeProvider::default();
    let mut estimator = PowRequiredEstimator::new(real_time_provider.current_time_millis(), label, pow_min);
    let guard = TrackedJobGuard::new(tracker.clone(), label, pow_min);
    loop {
        let result = generator.generate_best_effort(label, BATCH_SIZE, pow_min, data_hash).await?;
        if result.1 >= pow_min {
            return Ok(result);
        }
        guard.update(result.1);
        let progress = estimator.record_batch_and_estimate(real_time_provider.current_time_millis(), BATCH_SIZE, result.1);
        trace!("{}", progress);
        tools::yield_now().await;
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::pow_generator::pow_generator::{JobTracker, TrackedJobGuard};
    use crate::tools::types::Pow;
    use std::sync::{Arc, Mutex};

    #[test]
    fn job_tracker_round_trip() {
        let mut tracker = JobTracker::default();
        assert!(tracker.snapshot().is_empty());

        let job_a = tracker.add("rpc", Pow(18));
        let job_b = tracker.add("post", Pow(22));

        tracker.update(job_a, Pow(7));
        tracker.update(job_b, Pow(13));
        tracker.update(99999, Pow(255)); // unknown job_id is silently ignored

        let mut snapshot = tracker.snapshot();
        snapshot.sort_by(|a, b| a.label.cmp(&b.label));
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].label, "post");
        assert_eq!(snapshot[0].pow_min, Pow(22));
        assert_eq!(snapshot[0].best_pow_so_far, Pow(13));
        assert_eq!(snapshot[1].label, "rpc");
        assert_eq!(snapshot[1].pow_min, Pow(18));
        assert_eq!(snapshot[1].best_pow_so_far, Pow(7));

        tracker.remove(job_a);
        let remaining = tracker.snapshot();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].label, "post");

        tracker.remove(job_b);
        assert!(tracker.snapshot().is_empty());
    }

    #[test]
    fn tracked_job_guard_removes_on_drop() {
        let tracker = Arc::new(Mutex::new(JobTracker::default()));
        {
            let _guard = TrackedJobGuard::new(tracker.clone(), "rpc", Pow(18));
            assert_eq!(tracker.lock().unwrap().snapshot().len(), 1);
        }
        assert!(tracker.lock().unwrap().snapshot().is_empty());
    }

    #[test]
    fn tracked_job_guard_update_writes_through() {
        let tracker = Arc::new(Mutex::new(JobTracker::default()));
        let guard = TrackedJobGuard::new(tracker.clone(), "rpc", Pow(18));
        guard.update(Pow(42));
        let snapshot = tracker.lock().unwrap().snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].label, "rpc");
        assert_eq!(snapshot[0].pow_min, Pow(18));
        assert_eq!(snapshot[0].best_pow_so_far, Pow(42));
    }
}
