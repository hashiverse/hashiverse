//! Multi-core PoW search for non-WASM targets.
//!
//! Each `run_chunk` call runs on a `tokio::task::spawn_blocking` thread; the shared
//! [`crate::tools::pow_generator::pow_generator::run_pool`] dispatcher fires
//! `pool_size()` of them concurrently and refeeds whichever slot completes first. With
//! homogeneous server cores this is roughly equivalent to the previous rayon fan-out;
//! on heterogeneous CPUs (Apple Silicon P+E, Intel hybrid) it stops fast cores from
//! idling while a slow core finishes its slice.

#![cfg(not(target_arch = "wasm32"))]

use crate::tools::pow_generator::pow_generator;
use crate::tools::pow_generator::pow_generator::{JobTracker, PowGenerator};
use crate::tools::types::{Hash, Pow, Salt};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

pub struct NativeParallelPowGenerator {
    tracker: Arc<Mutex<JobTracker>>,
    cancellation_token: Option<CancellationToken>,
}

impl NativeParallelPowGenerator {
    pub fn new() -> Self {
        Self { tracker: Arc::new(Mutex::new(JobTracker::default())), cancellation_token: None }
    }

    /// Abort every search on this generator once `cancellation_token` is cancelled; see
    /// [`PowGenerator::cancellation_token`].
    pub fn with_cancellation_token(mut self, cancellation_token: CancellationToken) -> Self {
        self.cancellation_token = Some(cancellation_token);
        self
    }
}

impl Default for NativeParallelPowGenerator {
    fn default() -> Self { Self::new() }
}

#[async_trait::async_trait]
impl PowGenerator for NativeParallelPowGenerator {
    fn pool_size(&self) -> usize {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    }

    async fn run_chunk(&self, _slot: usize, chunk_iterations: usize, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<(Salt, Pow, Hash)> {
            pow_generator::run_pool_chunk(chunk_iterations, pow_min, data_hash)
        })
        .await??;

        Ok(result)
    }

    fn tracker(&self) -> &Arc<Mutex<JobTracker>> {
        &self.tracker
    }

    fn cancellation_token(&self) -> Option<&CancellationToken> {
        self.cancellation_token.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::pow::pow_compute_data_hash;
    use crate::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator;
    use crate::tools::pow_generator::pow_generator::PowGenerator;
    use crate::tools::tools;
    use crate::tools::types::Pow;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn native_generates_valid_pow() -> anyhow::Result<()> {
        const POW_MIN: Pow = Pow(12);
        let mut data = [0u8; 64];
        tools::random_fill_bytes(&mut data);
        let data_hash = pow_compute_data_hash(&[&data]);
        let generator = NativeParallelPowGenerator::new();
        let (_, pow, _) = generator.generate("test", POW_MIN, data_hash).await?;
        assert!(pow >= POW_MIN);
        Ok(())
    }

    // Pow(255) is unreachable, so these searches only ever end through cancellation.
    const UNREACHABLE_POW: Pow = Pow(255);

    #[tokio::test]
    async fn pre_cancelled_generator_fails_immediately() {
        let cancellation_token = CancellationToken::new();
        cancellation_token.cancel();
        let generator = NativeParallelPowGenerator::new().with_cancellation_token(cancellation_token);
        let data_hash = pow_compute_data_hash(&[b"pre-cancelled"]);

        let result = tokio::time::timeout(Duration::from_secs(2), generator.generate("cancel", UNREACHABLE_POW, data_hash)).await.expect("generate did not return promptly");
        assert!(result.is_err_and(|error| error.to_string().contains("cancelled")));
        assert!(generator.active_jobs().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_mid_search_stops_generate() {
        let cancellation_token = CancellationToken::new();
        let generator = Arc::new(NativeParallelPowGenerator::new().with_cancellation_token(cancellation_token.clone()));
        let data_hash = pow_compute_data_hash(&[b"cancel-mid-search"]);

        let search = {
            let generator = generator.clone();
            tokio::spawn(async move { generator.generate("cancel", UNREACHABLE_POW, data_hash).await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!generator.active_jobs().is_empty(), "search should be running");
        cancellation_token.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), search).await.expect("generate did not stop within 2s of cancellation").unwrap();
        assert!(result.is_err_and(|error| error.to_string().contains("cancelled")));
        assert!(generator.active_jobs().is_empty(), "cancelled job must be removed from the tracker");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_mid_search_stops_generate_best_effort() {
        let cancellation_token = CancellationToken::new();
        let generator = Arc::new(NativeParallelPowGenerator::new().with_cancellation_token(cancellation_token.clone()));
        let data_hash = pow_compute_data_hash(&[b"cancel-best-effort"]);

        let search = {
            let generator = generator.clone();
            tokio::spawn(async move { generator.generate_best_effort("cancel", usize::MAX, UNREACHABLE_POW, data_hash).await })
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancellation_token.cancel();

        let result = tokio::time::timeout(Duration::from_secs(2), search).await.expect("generate_best_effort did not stop within 2s of cancellation").unwrap();
        assert!(result.is_err_and(|error| error.to_string().contains("cancelled")));
    }

    #[tokio::test]
    async fn uncancelled_token_behaves_like_no_token() -> anyhow::Result<()> {
        const POW_MIN: Pow = Pow(12);
        let generator = NativeParallelPowGenerator::new().with_cancellation_token(CancellationToken::new());
        let data_hash = pow_compute_data_hash(&[b"uncancelled"]);
        let (_, pow, _) = generator.generate("test", POW_MIN, data_hash).await?;
        assert!(pow >= POW_MIN);
        let (_, best_effort_pow, _) = generator.generate_best_effort("test", 4096, POW_MIN, data_hash).await?;
        assert!(best_effort_pow > Pow(0));
        Ok(())
    }
}
