//! Trait, observability primitives, and shared work-stealing dispatcher for the PoW search engine.
//!
//! See [`crate::tools::pow_generator`] for the broader module overview. This file holds:
//!
//! - [`PowGenerator`] — the trait every concrete generator implements. Backends only have to
//!   provide [`PowGenerator::pool_size`] and [`PowGenerator::run_chunk`]; the shared
//!   dispatcher does the rest.
//! - [`JobTracker`] + [`PowJobStatus`] — the in-flight job registry surfaced via
//!   `PowGenerator::active_jobs()` so the UI can show users why an action is slow.
//! - [`run_pool`] — the one shared work-stealing dispatcher both [`PowGenerator::generate`]
//!   and [`PowGenerator::generate_best_effort`] use. Tracks the job, refeeds each pool slot
//!   independently as it completes, and returns the instant any chunk meets `pow_min`
//!   (discarding any in-flight chunks).

use crate::tools::pow::pow_measure_from_data_hash;
use crate::tools::pow_required_estimator::PowRequiredEstimator;
use crate::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
use crate::tools::types::{Hash, Pow, Salt};
use futures::stream::{FuturesUnordered, StreamExt};
use log::trace;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
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
///   runs chunks via `tokio::task::spawn_blocking`, one per CPU.
/// - [`crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator`]
///   runs chunks serially. Works on every target including WASM and is used as the fallback in
///   tests and the in-browser default before workers are wired up.
/// - `WasmParallelPowGenerator` (in `hashiverse-client-wasm`) dispatches chunks to pre-spawned
///   Web Workers, one per worker.
///
/// Backends only have to answer two questions — how many chunks may run in parallel
/// ([`Self::pool_size`]) and how to run one chunk on a given slot ([`Self::run_chunk`]) — and
/// the shared [`run_pool`] dispatcher handles the rest: work-stealing refeed of fast slots,
/// early exit the moment `pow_min` is met, and per-chunk cooperative yield. This is what
/// keeps heterogeneous CPUs (Apple Silicon P+E, Intel hybrid, ARM big.LITTLE) from
/// collapsing to slow-core throughput.
///
/// Implementations must also maintain the `active_jobs()` observability view — the UI
/// surfaces in-progress PoW searches to end users so they understand why an action is slow.
#[async_trait::async_trait]
pub trait PowGenerator: Send + Sync {
    /// How many `run_chunk` calls the dispatcher may have in flight concurrently.
    /// `WasmParallelPowGenerator` returns its `workers.len()`; the native backend returns
    /// the CPU count; the single-threaded backend returns 1.
    fn pool_size(&self) -> usize;

    /// Run `chunk_iterations` PoW attempts on one parallel slot. `slot` is an opaque index
    /// in `0..pool_size()` — the wasm backend uses it to address `self.workers[slot]`; the
    /// native and single-threaded backends ignore it. May short-circuit inside the chunk
    /// when `pow_min` is reached (`pow_compute_batch` already does this in
    /// `hashiverse-client-wasm/src/lib.rs`).
    async fn run_chunk(&self, slot: usize, chunk_iterations: usize, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)>;

    /// Accessor for the impl's `JobTracker`. Used by the default `generate` /
    /// `generate_best_effort` impls to register the in-flight job, and by `active_jobs()`
    /// to snapshot it for the UI.
    fn tracker(&self) -> &Arc<Mutex<JobTracker>>;

    /// Snapshot of all concurrently in-flight tracked jobs.
    fn active_jobs(&self) -> Vec<PowJobStatus> {
        self.tracker().lock().unwrap().snapshot()
    }

    /// Run up to `iteration_limit` attempts via the work-stealing pool. Registers the job
    /// in the tracker at entry. Returns the moment any chunk produces `pow >= pow_min`
    /// (discarding chunks still in flight); otherwise returns the best result found
    /// within `iteration_limit`.
    async fn generate_best_effort(&self, label: &str, iteration_limit: usize, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
        run_pool(self, label, Some(iteration_limit), pow_min, data_hash).await
    }

    /// Run the pool unbounded until `pow >= pow_min` is found. Registers the job in the
    /// tracker at entry.
    async fn generate(&self, label: &str, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
        run_pool(self, label, None, pow_min, data_hash).await
    }
}

/// Per-chunk grain size for the dispatcher. Smaller = less wasted work on early-exit and
/// finer cooperative yield, but more per-chunk dispatch overhead. With ~7 chained hashes per
/// attempt, 4K iterations is a few tens to a couple hundred ms per chunk on commodity
/// hardware — small enough that a slow-core chunk finishing after a winner is found wastes
/// at most one chunk-time of background CPU per pool slot.
const CHUNK_ITERATIONS: usize = 4 * 1024;

type SlotFuture<'a> = Pin<Box<dyn Future<Output = (usize, usize, anyhow::Result<(Salt, Pow, Hash)>)> + Send + 'a>>;

/// Work-stealing dispatcher shared by both `generate` and `generate_best_effort`.
///
/// Algorithm:
/// 1. Register the job in the tracker (RAII so panic / cancel still cleans up).
/// 2. Initial fill: push one chunk onto each of `pool_size()` slots, clamped by
///    `iteration_cap` when bounded.
/// 3. As each chunk completes, merge its best result. If `pow_min` is met, return
///    immediately — the `FuturesUnordered` is dropped, discarding any in-flight chunks
///    (the underlying worker / blocking thread continues silently and its result is lost).
/// 4. Otherwise, update the tracker / progress estimator, cooperatively yield, and refeed
///    only the slot that just freed up. Fast slots end up processing more chunks than slow
///    ones — no idle waiting on the slowest core.
pub async fn run_pool<'a, G: PowGenerator + ?Sized>(
    generator: &'a G,
    label: &'a str,
    iteration_cap: Option<usize>,
    pow_min: Pow,
    data_hash: Hash,
) -> anyhow::Result<(Salt, Pow, Hash)> {
    let tracker = generator.tracker().clone();
    let guard = TrackedJobGuard::new(tracker, label, pow_min);

    let real_time_provider = RealTimeProvider;
    let mut estimator = PowRequiredEstimator::new(real_time_provider.current_time_millis(), label, pow_min);

    let pool_size = generator.pool_size().max(1);
    let mut remaining_iterations: Option<usize> = iteration_cap;

    // Seed `best` with one real PoW sample so the in-flight loop's strict `>` check is
    // always meaningful (no zero-init placeholder to distinguish from a real Pow(0)
    // chunk). One synchronous measurement on the dispatcher thread is cheap and
    // guarantees the returned (salt, pow) pair satisfies pow_measure(salt) == pow.
    // For iteration_cap == Some(0) callers, we still do this one seed PoW — it's the
    // minimum work that yields a self-consistent result.
    let mut best = {
        let seed_salt = Salt::random();
        let (seed_pow, seed_hash) = pow_measure_from_data_hash(&data_hash, &seed_salt)?;
        (seed_salt, seed_pow, seed_hash)
    };

    guard.update(best.1);

    // Were we lucky?
    if best.1 >= pow_min {
        return Ok(best);
    }

    let mut in_flight: FuturesUnordered<SlotFuture<'a>> = FuturesUnordered::new();

    // Kick off the threads
    for slot in 0..pool_size {
        let chunk_size = pick_next_chunk_size(&mut remaining_iterations);
        if chunk_size == 0 {
            break;
        }

        in_flight.push(Box::pin(async move {
            let chunk_result = generator.run_chunk(slot, chunk_size, pow_min, data_hash).await;
            (slot, chunk_size, chunk_result)
        }));
    }

    // Then, as each thread finishes, see if there is more work to do and repeat...
    while let Some((slot, chunk_size, chunk_result)) = in_flight.next().await {
        let chunk_best = chunk_result?;
        if chunk_best.1 > best.1 {
            best = chunk_best;
            guard.update(best.1);
        }
        if best.1 >= pow_min {
            return Ok(best);
        }
        if let Some(progress) = estimator.record_batch_and_estimate(real_time_provider.current_time_millis(), chunk_size, best.1) {
            trace!("{}", progress);
        }

        let next_chunk_size = pick_next_chunk_size(&mut remaining_iterations);
        if next_chunk_size == 0 {
            continue;
        }
        
        in_flight.push(Box::pin(async move {
            let chunk_result = generator.run_chunk(slot, next_chunk_size, pow_min, data_hash).await;
            (slot, next_chunk_size, chunk_result)
        }));
    }

    Ok(best)
}

fn pick_next_chunk_size(remaining_iterations: &mut Option<usize>) -> usize {
    match remaining_iterations {
        Some(0) => 0,
        Some(remaining) => {
            let chunk_size = (*remaining).min(CHUNK_ITERATIONS);
            *remaining -= chunk_size;
            chunk_size
        }
        None => CHUNK_ITERATIONS,
    }
}

pub fn run_pool_chunk(chunk_iterations: usize, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
    let mut best = {
        let salt = Salt::random();
        let (pow, hash) = pow_measure_from_data_hash(&data_hash, &salt)?;
        (salt, pow, hash)
    };

    if best.1 >= pow_min {
        return Ok(best);
    }

    for _ in 1..chunk_iterations {
        let salt = Salt::random();
        let (pow, hash) = pow_measure_from_data_hash(&data_hash, &salt)?;
        if pow > best.1 {
            best = (salt, pow, hash);
            if best.1 >= pow_min {
                return Ok(best);
            }
        }
    }

    Ok(best)
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

    #[tokio::test]
    async fn run_pool_returns_consistent_sample_when_iteration_limit_is_zero() {
        use crate::tools::pow::{pow_compute_data_hash, pow_measure_from_data_hash};
        use crate::tools::pow_generator::pow_generator::PowGenerator;
        use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
        use crate::tools::types::Pow;

        // iteration_cap=0 means "no chunked work", but run_pool still does the one seed PoW
        // so the returned (salt, pow) is always self-consistent under pow_measure.
        let data_hash = pow_compute_data_hash(&[b"zero-budget"]);
        let generator = SingleThreadedPowGenerator::new();
        let (salt, achieved_pow, _) = generator.generate_best_effort("zero", 0, Pow(255), data_hash).await.unwrap();
        let (recomputed_pow, _) = pow_measure_from_data_hash(&data_hash, &salt).unwrap();
        assert_eq!(recomputed_pow, achieved_pow);
    }

    #[tokio::test]
    async fn run_pool_tracker_clears_after_completion() {
        use crate::tools::pow::pow_compute_data_hash;
        use crate::tools::pow_generator::pow_generator::PowGenerator;
        use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
        use crate::tools::types::Pow;

        let data_hash = pow_compute_data_hash(&[b"tracker-cleanup"]);
        let generator = SingleThreadedPowGenerator::new();
        // Pow(0) succeeds on the first attempt → fast.
        let _ = generator.generate("clean", Pow(0), data_hash).await.unwrap();
        assert!(generator.active_jobs().is_empty());
    }

    #[tokio::test]
    async fn run_pool_returns_as_soon_as_pow_min_is_met() {
        use crate::tools::pow::pow_compute_data_hash;
        use crate::tools::pow_generator::pow_generator::PowGenerator;
        use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
        use crate::tools::types::Pow;

        const POW_MIN: Pow = Pow(8);
        let data_hash = pow_compute_data_hash(&[b"early-exit"]);
        let generator = SingleThreadedPowGenerator::new();
        let (_, achieved_pow, _) = generator.generate("early", POW_MIN, data_hash).await.unwrap();
        assert!(achieved_pow >= POW_MIN);
    }

    /// Regression: with `pow_min = 0` and a tiny chunk, the sampled salt was being
    /// dropped in favour of the zero-init placeholder whenever the sample happened to
    /// yield Pow(0) — leaving the returned salt and pow inconsistent under `pow_measure`.
    /// Uses a different `data_hash` per trial so the bug can't hide behind a coincidental
    /// `pow_measure(Salt::zero(), data_hash) == Pow(0)` for any single fixed input.
    #[tokio::test]
    async fn run_pool_pow_min_zero_returns_consistent_salt_and_pow() {
        use crate::tools::pow::{pow_compute_data_hash, pow_measure_from_data_hash};
        use crate::tools::pow_generator::pow_generator::PowGenerator;
        use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
        use crate::tools::types::Pow;

        let generator = SingleThreadedPowGenerator::new();
        for trial in 0u32..256 {
            let data_hash = pow_compute_data_hash(&[&trial.to_le_bytes()]);
            let (salt, achieved_pow, _) = generator.generate_best_effort("regression", 1, Pow(0), data_hash).await.unwrap();
            let (recomputed_pow, _) = pow_measure_from_data_hash(&data_hash, &salt).unwrap();
            assert_eq!(recomputed_pow, achieved_pow, "trial {}: salt and pow drifted apart", trial);
        }
    }

    /// Regression: when the entire iteration budget is spent without ever beating Pow(0),
    /// run_pool used to return the zero-init placeholder (Salt::zero, Pow(0), Hash::zero)
    /// because its `chunk_best.1 > best.1` check could never accept a chunk with pow == 0.
    /// Now exercises both layers: run_chunk produces a real (salt, pow=0, hash) sample
    /// (~50% of the time for tiny chunks), and run_pool must accept it.
    #[tokio::test]
    async fn run_pool_returns_consistent_sample_when_budget_exhausted() {
        use crate::tools::pow::{pow_compute_data_hash, pow_measure_from_data_hash};
        use crate::tools::pow_generator::pow_generator::PowGenerator;
        use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
        use crate::tools::types::Pow;

        let generator = SingleThreadedPowGenerator::new();
        for trial in 0u32..256 {
            let data_hash = pow_compute_data_hash(&[b"exhaust", &trial.to_le_bytes()]);
            // Pow(255) is unreachable in a single iteration → budget always exhausts.
            let (salt, achieved_pow, _) = generator.generate_best_effort("exhaust", 1, Pow(255), data_hash).await.unwrap();
            let (recomputed_pow, _) = pow_measure_from_data_hash(&data_hash, &salt).unwrap();
            assert_eq!(recomputed_pow, achieved_pow, "trial {}: returned salt does not produce returned pow", trial);
        }
    }
}
