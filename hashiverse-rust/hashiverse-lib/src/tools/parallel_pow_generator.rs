//! # Parallel proof-of-work search engine
//!
//! Proof-of-work is mandatory on every outgoing RPC, every peer announcement, and every
//! piece of report/feedback — so finding a PoW solution quickly is on the hot path for
//! virtually every client and server action. This module isolates that work behind a
//! single trait, [`ParallelPowGenerator`], so the calling code doesn't care whether it's
//! running on a 32-core server or a single-threaded WASM Web Worker.
//!
//! ## Implementations
//!
//! - [`NativeParallelPowGenerator`] — rayon + `tokio::task::spawn_blocking`, saturates
//!   every CPU core on native targets.
//! - [`StubParallelPowGenerator`] — single-threaded fallback that works on every target
//!   including WASM. Browser clients run this (with a relaxed `pow_min`) because Web
//!   Workers don't expose thread pools.
//!
//! ## Observability
//!
//! [`JobTracker`] + [`PowJobStatus`] expose the set of in-flight PoW jobs and the
//! best-so-far pow for each. The web client surfaces this in the UI so that when a post
//! feels slow to send the user can see it's because PoW is still grinding.
//!
//! ## Shared loop
//!
//! [`generate_loop`] is the one-true batching loop used by both implementations: repeatedly
//! call [`ParallelPowGenerator::generate_best_effort`] in 64K-attempt batches, update the
//! tracker, and bail as soon as a batch returns `pow >= pow_min`. Every batch also yields
//! to the runtime so on single-threaded targets other tasks still get a chance to run.

use crate::tools::pow::{pow_generate_with_iteration_limit};
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

/// A pluggable engine for searching for proof-of-work solutions in parallel.
///
/// Proof-of-work is required on every RPC packet, on peer announcements, and on report /
/// feedback submissions, so finding PoW is on the hot path for every outbound action a client
/// or server takes. `ParallelPowGenerator` abstracts over the concrete way we parallelize that
/// search so the calling code stays platform-agnostic:
///
/// - [`NativeParallelPowGenerator`] uses `rayon` + `tokio::task::spawn_blocking` to pin the
///   search across all CPU cores on native targets.
/// - [`StubParallelPowGenerator`] is a single-threaded fallback that works on every target,
///   including WASM. Browser clients use this (with a relaxed `pow_min`) because Web Workers
///   do not expose `rayon` / threads directly.
///
/// Implementations must also maintain the `active_jobs()` observability view — the UI surfaces
/// in-progress PoW searches to end users so they understand why an action is slow.
#[async_trait::async_trait]
pub trait ParallelPowGenerator: Send + Sync {
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
        let job_id = self.tracker().lock().unwrap().add(label, pow_min);
        let result = self.generate_best_effort(label, iteration_limit, pow_min, data_hash).await;
        self.tracker().lock().unwrap().remove(job_id);
        result
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
    generator: &(dyn ParallelPowGenerator + '_),
    tracker: &Arc<Mutex<JobTracker>>,
    label: &str,
    pow_min: Pow,
    data_hash: Hash,
) -> anyhow::Result<(Salt, Pow, Hash)> {
    const BATCH_SIZE: usize = 64 * 1024;
    let real_time_provider = RealTimeProvider::default();
    let mut estimator = PowRequiredEstimator::new(real_time_provider.current_time_millis(), label, pow_min);
    let job_id = tracker.lock().unwrap().add(label, pow_min);
    loop {
        let result = generator.generate_best_effort(label, BATCH_SIZE, pow_min, data_hash).await?;
        if result.1 >= pow_min {
            tracker.lock().unwrap().remove(job_id);
            return Ok(result);
        }
        tracker.lock().unwrap().update(job_id, result.1);
        let progress = estimator.record_batch_and_estimate(real_time_provider.current_time_millis(), BATCH_SIZE, result.1);
        trace!("{}", progress);
        tools::yield_now().await;
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// StubParallelPowGenerator — single-threaded, works on all platforms
// ──────────────────────────────────────────────────────────────────────────────

pub struct StubParallelPowGenerator {
    tracker: Arc<Mutex<JobTracker>>,
}

impl StubParallelPowGenerator {
    pub fn new() -> Self {
        Self { tracker: Arc::new(Mutex::new(JobTracker::default())) }
    }
}

impl Default for StubParallelPowGenerator {
    fn default() -> Self { Self::new() }
}

#[async_trait::async_trait]
impl ParallelPowGenerator for StubParallelPowGenerator {
    async fn generate_best_effort(&self, _label: &str, iteration_limit: usize, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
        pow_generate_with_iteration_limit(iteration_limit, pow_min, &data_hash).await
    }

    async fn generate(&self, label: &str, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
        generate_loop(self, &self.tracker, label, pow_min, data_hash).await
    }

    fn active_jobs(&self) -> Vec<PowJobStatus> {
        self.tracker.lock().unwrap().snapshot()
    }

    fn tracker(&self) -> &Arc<Mutex<JobTracker>> {
        &self.tracker
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// NativeParallelPowGenerator — rayon + spawn_blocking, non-WASM only
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
pub struct NativeParallelPowGenerator {
    tracker: Arc<Mutex<JobTracker>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeParallelPowGenerator {
    pub fn new() -> Self {
        Self { tracker: Arc::new(Mutex::new(JobTracker::default())) }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for NativeParallelPowGenerator {
    fn default() -> Self { Self::new() }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl ParallelPowGenerator for NativeParallelPowGenerator {
    async fn generate_best_effort(&self, _label: &str, iteration_limit: usize, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
        let num_threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        let per_thread = (iteration_limit / num_threads).max(1);
        let result = tokio::task::spawn_blocking(move || {
            use rayon::prelude::*;
            (0..num_threads)
                .into_par_iter()
                .map(|_| {
                    let mut best = (Salt::zero(), Pow(0), Hash::zero());
                    for _ in 0..per_thread {
                        let salt = Salt::random();
                        if let Ok((pow, hash)) = crate::tools::pow::pow_measure_from_data_hash(&data_hash, &salt) {
                            if pow > best.1 {
                                best = (salt, pow, hash);
                                if pow >= pow_min {
                                    break;
                                }
                            }
                        }
                    }
                    best
                })
                .reduce(
                    || (Salt::zero(), Pow(0), Hash::zero()),
                    |a, b| if b.1 > a.1 { b } else { a },
                )
        })
        .await?;
        Ok(result)
    }

    async fn generate(&self, label: &str, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
        generate_loop(self, &self.tracker, label, pow_min, data_hash).await
    }

    fn active_jobs(&self) -> Vec<PowJobStatus> {
        self.tracker.lock().unwrap().snapshot()
    }

    fn tracker(&self) -> &Arc<Mutex<JobTracker>> {
        &self.tracker
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::parallel_pow_generator::{JobTracker, ParallelPowGenerator, StubParallelPowGenerator};
    use crate::tools::pow::pow_compute_data_hash;
    use crate::tools::tools;
    use crate::tools::types::Pow;

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

    #[tokio::test]
    async fn stub_generates_valid_pow() -> anyhow::Result<()> {
        const POW_MIN: Pow = Pow(12);
        let mut data = [0u8; 64];
        tools::random_fill_bytes(&mut data);
        let data_hash = pow_compute_data_hash(&[&data]);
        let generator = StubParallelPowGenerator::new();
        let (_, pow, _) = generator.generate("test", POW_MIN, data_hash).await?;
        assert!(pow >= POW_MIN);
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn native_generates_valid_pow() -> anyhow::Result<()> {
        const POW_MIN: Pow = Pow(12);
        let mut data = [0u8; 64];
        tools::random_fill_bytes(&mut data);
        let data_hash = pow_compute_data_hash(&[&data]);
        let generator = crate::tools::parallel_pow_generator::NativeParallelPowGenerator::new();
        let (_, pow, _) = generator.generate("test", POW_MIN, data_hash).await?;
        assert!(pow >= POW_MIN);
        Ok(())
    }
}
