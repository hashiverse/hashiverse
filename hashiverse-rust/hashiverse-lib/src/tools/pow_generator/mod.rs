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
//! [`pow_generator::run_pool`] is the one-true dispatch loop used by every
//! implementation: it keeps one 4K-iteration chunk in flight per
//! [`pow_generator::PowGenerator::pool_size`] slot via
//! [`pow_generator::PowGenerator::run_chunk`], updates the tracker as chunks complete,
//! and returns as soon as one reaches `pow >= pow_min`, or fails if the generator's
//! cancellation token fires. Every chunk also yields to the runtime so on
//! single-threaded targets other tasks still get a chance to run.

pub mod pow_generator;
pub mod native_parallel_pow_generator;
pub mod single_threaded_pow_generator;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub mod wasm_parallel_pow_generator;
