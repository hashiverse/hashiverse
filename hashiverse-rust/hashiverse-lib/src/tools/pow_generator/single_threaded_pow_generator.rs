//! Single-threaded PoW search that works on every target.
//!
//! No `spawn_blocking`, no Web Workers — `run_chunk` just runs the PoW measurement loop
//! inline. WASM clients use this directly (with a relaxed `pow_min`) before workers are
//! wired up; native callers can use it in unit tests where setting up a thread pool would
//! be overkill.
//!
//! Despite the simplicity, this generator does real PoW work — it is not a no-op stub.
//! All the orchestration (work-stealing, early-exit, tracker registration) lives in the
//! shared [`crate::tools::pow_generator::pow_generator::run_pool`] dispatcher.

use crate::tools::pow_generator::pow_generator;
use crate::tools::pow_generator::pow_generator::{JobTracker, PowGenerator};
use crate::tools::types::{Hash, Pow, Salt};
use std::sync::{Arc, Mutex};

pub struct SingleThreadedPowGenerator {
    tracker: Arc<Mutex<JobTracker>>,
}

impl SingleThreadedPowGenerator {
    pub fn new() -> Self {
        Self { tracker: Arc::new(Mutex::new(JobTracker::default())) }
    }
}

impl Default for SingleThreadedPowGenerator {
    fn default() -> Self { Self::new() }
}

#[async_trait::async_trait]
impl PowGenerator for SingleThreadedPowGenerator {
    fn pool_size(&self) -> usize { 1 }

    async fn run_chunk(&self, _slot: usize, chunk_iterations: usize, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
        pow_generator::run_pool_chunk(chunk_iterations, pow_min, data_hash)
    }

    fn tracker(&self) -> &Arc<Mutex<JobTracker>> {
        &self.tracker
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::pow::pow_compute_data_hash;
    use crate::tools::pow_generator::pow_generator::PowGenerator;
    use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
    use crate::tools::tools;
    use crate::tools::types::Pow;

    #[tokio::test]
    async fn single_threaded_generates_valid_pow() -> anyhow::Result<()> {
        const POW_MIN: Pow = Pow(12);
        let mut data = [0u8; 64];
        tools::random_fill_bytes(&mut data);
        let data_hash = pow_compute_data_hash(&[&data]);
        let generator = SingleThreadedPowGenerator::new();
        let (_, pow, _) = generator.generate("test", POW_MIN, data_hash).await?;
        assert!(pow >= POW_MIN);
        Ok(())
    }
}
