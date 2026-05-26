#![feature(try_blocks)]
pub mod wasm_transport;
pub mod wasm_bootstrap_provider;
pub mod wasm_local_settings;
pub mod wasm_client_storage;
pub mod wasm_key_locker;
pub mod wasm_parallel_pow_generator;
pub mod with_js_context;
pub mod hashiverse_client_wasm;
pub mod wasm_try;

use hashiverse_lib::tools::pow_generator::pow_generator;
use hashiverse_lib::tools::types::{Hash, Pow, Salt};
use log::{info, trace};
use std::sync::Arc;
use wasm_bindgen::prelude::*;

/// Initialise logging and panic hook. Must be called manually before using the WASM module.
/// Pass `verbose = true` for the main worker (logs confirmation), `false` for PoW sub-workers (silent).
#[wasm_bindgen]
pub fn wasm_init(verbose: bool) {

    // Set up logging
    {
        fern::Dispatch::new()
            .level(log::LevelFilter::Trace) // Default level
            .level_for("wasm_bindgen", log::LevelFilter::Warn)
            .level_for("scraper", log::LevelFilter::Warn)
            .level_for("html5ever", log::LevelFilter::Warn)
            .level_for("selectors", log::LevelFilter::Warn)
            .chain(fern::Output::call(console_log::log))
            .apply()
            .expect("Failed to initialize logging")
        ;

        if verbose {
            info!("Logging initialized");
        }
    }

    console_error_panic_hook::set_once();
    if verbose {
        trace!("WASM module panic hook set");
    }
}

/// Sync PoW batch computation for sub-workers. Each sub-worker calls this function
/// with a portion of the total iteration budget.
///
/// Returns a colon-separated string: `salt_hex:pow_u8:hash_hex`
#[wasm_bindgen]
pub fn pow_compute_batch(iteration_limit: u32, pow_min: u8, data_hash_hex: String) -> String {
    let data_hash = match hex::decode(&data_hash_hex).ok().and_then(|b| Hash::from_slice(&b).ok()) {
        Some(h) => h,
        None => return format!("{}:0:{}", hex::encode(Salt::zero()), hex::encode(Hash::zero())),
    };

    match pow_generator::run_pool_chunk(iteration_limit as usize, Pow(pow_min), data_hash) {
        Ok((salt, pow, hash)) => format!("{}:{}:{}", hex::encode(salt), pow.0, hex::encode(hash)),
        Err(_) => format!("{}:0:{}", hex::encode(Salt::zero()), hex::encode(Hash::zero())),
    }
}

/// Global storage for the WasmParallelPowGenerator singleton.
static WASM_PARALLEL_POW_GENERATOR: std::sync::OnceLock<Arc<wasm_parallel_pow_generator::WasmParallelPowGenerator>> = std::sync::OnceLock::new();

/// Initialize the parallel PoW worker pool. Call from TypeScript, passing an
/// array of ready `Worker` handles that each run `HashiversePowWorker.ts`.
#[wasm_bindgen]
pub fn init_pow_workers(workers_js: JsValue) {
    let workers_array: js_sys::Array = match workers_js.dyn_into() {
        Ok(a) => a,
        Err(_) => {
            log::warn!("init_pow_workers: expected an Array of Workers");
            return;
        }
    };

    let mut workers = Vec::new();
    for i in 0..workers_array.length() {
        let val = workers_array.get(i);
        match val.dyn_into::<web_sys::Worker>() {
            Ok(w) => workers.push(w),
            Err(_) => log::warn!("init_pow_workers: element {} is not a Worker", i),
        }
    }

    if workers.is_empty() {
        log::warn!("init_pow_workers: no valid Workers provided");
        return;
    }

    let generator = wasm_parallel_pow_generator::WasmParallelPowGenerator::from_workers(workers);
    let _ = WASM_PARALLEL_POW_GENERATOR.set(Arc::new(generator));
}

/// Get the global WasmParallelPowGenerator if initialized.
pub fn get_wasm_parallel_pow_generator() -> Option<Arc<wasm_parallel_pow_generator::WasmParallelPowGenerator>> {
    WASM_PARALLEL_POW_GENERATOR.get().cloned()
}
