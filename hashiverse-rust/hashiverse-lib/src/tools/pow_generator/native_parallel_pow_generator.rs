//! Multi-core PoW search for non-WASM targets.
//!
//! Uses `rayon` inside `tokio::task::spawn_blocking` to saturate every available CPU
//! core. Each parallel worker scans its slice of the `iteration_limit` and the reducer
//! picks the highest pow found across all of them.

#![cfg(not(target_arch = "wasm32"))]

use crate::tools::pow_generator::pow_generator::{generate_loop, JobTracker, PowGenerator, PowJobStatus};
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
