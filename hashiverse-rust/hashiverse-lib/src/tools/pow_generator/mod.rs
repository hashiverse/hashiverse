//! # Proof-of-work search engine
//!
//! Proof-of-work is mandatory on every outgoing RPC, every peer announcement, and every
//! piece of report/feedback — so finding a PoW solution quickly is on the hot path for
//! virtually every client and server action. This module isolates that work behind a
//! single trait, [`pow_generator::PowGenerator`], so the calling code doesn't care
//! whether it's running on a 32-core server or a single-threaded WASM Web Worker.
//!
//! ## Implementations
//!
//! - [`native_parallel_pow_generator::NativeParallelPowGenerator`] — rayon +
//!   `tokio::task::spawn_blocking`, saturates every CPU core on native targets.
//! - [`single_threaded_pow_generator::SingleThreadedPowGenerator`] — single-threaded
//!   fallback that works on every target including WASM. Browser clients run this
//!   (with a relaxed `pow_min`) because Web Workers don't expose thread pools.
//!
//! ## Observability
//!
//! [`pow_generator::JobTracker`] + [`pow_generator::PowJobStatus`] expose the set of
//! in-flight PoW jobs and the best-so-far pow for each. The web client surfaces this in
//! the UI so that when a post feels slow to send the user can see it's because PoW is
//! still grinding.
//!
//! ## Shared loop
//!
//! [`pow_generator::generate_loop`] is the one-true batching loop used by both
//! implementations: repeatedly call [`pow_generator::PowGenerator::generate_best_effort`]
//! in 64K-attempt batches, update the tracker, and bail as soon as a batch returns
//! `pow >= pow_min`. Every batch also yields to the runtime so on single-threaded targets
//! other tasks still get a chance to run.

pub mod pow_generator;
pub mod native_parallel_pow_generator;
pub mod single_threaded_pow_generator;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub mod wasm_parallel_pow_generator;
