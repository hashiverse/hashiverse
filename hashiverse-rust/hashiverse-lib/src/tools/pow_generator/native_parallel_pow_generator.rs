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

pub struct NativeParallelPowGenerator {
    tracker: Arc<Mutex<JobTracker>>,
}

impl NativeParallelPowGenerator {
    pub fn new() -> Self {
        Self { tracker: Arc::new(Mutex::new(JobTracker::default())) }
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
}

#[cfg(test)]
mod tests {
    use crate::tools::pow::pow_compute_data_hash;
    use crate::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator;
    use crate::tools::pow_generator::pow_generator::PowGenerator;
    use crate::tools::tools;
    use crate::tools::types::Pow;

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
}
