//! Single-threaded PoW search that works on every target.
//!
//! No `rayon`, no `spawn_blocking`, no Web Workers — just `pow_generate_with_iteration_limit`
//! called serially. WASM clients use this directly (with a relaxed `pow_min`); native
//! callers can use it in unit tests where setting up a thread pool would be overkill.
//!
//! Despite the simplicity, this generator does real PoW work — it is not a no-op stub.

use crate::tools::pow::pow_generate_with_iteration_limit;
use crate::tools::pow_generator::pow_generator::{generate_loop, JobTracker, PowGenerator, PowJobStatus};
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
