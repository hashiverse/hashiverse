use hashiverse_lib::tools::parallel_pow_generator::{generate_loop, JobTracker, ParallelPowGenerator, PowJobStatus};
use hashiverse_lib::tools::types::{Hash, Pow, Salt};
use js_sys::{Array, Object, Reflect};
use log::{info, warn};
use send_wrapper::SendWrapper;
use std::sync::{Arc, Mutex};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{MessageChannel, MessageEvent, Worker};

/// A `ParallelPowGenerator` that distributes PoW work across pre-created Web Workers.
/// The TypeScript side is responsible for spawning and initializing the workers;
/// this struct simply receives the ready `Worker` handles.
pub struct WasmParallelPowGenerator {
    tracker: Arc<Mutex<JobTracker>>,
    workers: Vec<Worker>,
}

// Safety: In WASM, everything is single-threaded. Worker handles are not Send
// in web-sys's type system, but we never actually move them across threads.
unsafe impl Send for WasmParallelPowGenerator {}
unsafe impl Sync for WasmParallelPowGenerator {}

impl WasmParallelPowGenerator {
    /// Create a new generator from pre-initialized Worker handles.
    pub fn from_workers(workers: Vec<Worker>) -> Self {
        info!("WasmParallelPowGenerator: received {} pow workers", workers.len());
        Self {
            tracker: Arc::new(Mutex::new(JobTracker::default())),
            workers,
        }
    }
}

#[async_trait::async_trait]
impl ParallelPowGenerator for WasmParallelPowGenerator {
    async fn generate_best_effort(&self, _label: &str, iteration_limit: usize, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
        if self.workers.is_empty() {
            anyhow::bail!("No pow workers available");
        }

        let num_workers = self.workers.len();
        let per_worker = (iteration_limit / num_workers).max(1);
        let data_hash_hex = hex::encode(data_hash);

        // We need SendWrapper because JsFuture and Worker are !Send, but the
        // trait requires Send futures. In WASM everything is single-threaded.
        let inner = async {
            // Dispatch work to all workers using a MessageChannel per worker.
            // Each call gets its own isolated ports, so concurrent generate_best_effort
            // calls don't clobber each other's onmessage handlers.
            let mut response_futures = Vec::with_capacity(num_workers);
            for worker in &self.workers {
                let channel = MessageChannel::new()
                    .map_err(|e| anyhow::anyhow!("Failed to create MessageChannel: {:?}", e))?;

                let port1 = channel.port1();
                let port2 = channel.port2();

                // Listen for the response on port1
                let promise = js_sys::Promise::new(&mut |resolve, _reject| {
                    let resolve_clone = resolve.clone();
                    let onmessage = Closure::once_into_js(move |event: MessageEvent| {
                        resolve_clone.call1(&JsValue::NULL, &event.data()).ok();
                    });
                    port1.set_onmessage(Some(onmessage.unchecked_ref()));
                });

                // Build the request message
                let msg = Object::new();
                Reflect::set(&msg, &JsValue::from_str("iteration_limit"), &JsValue::from_f64(per_worker as f64)).ok();
                Reflect::set(&msg, &JsValue::from_str("pow_min"), &JsValue::from_f64(pow_min.0 as f64)).ok();
                Reflect::set(&msg, &JsValue::from_str("data_hash_hex"), &JsValue::from_str(&data_hash_hex)).ok();

                // Transfer port2 to the worker so it can reply on it
                let transfer = Array::new();
                transfer.push(&port2);
                worker.post_message_with_transfer(&msg, &transfer)
                    .map_err(|e| anyhow::anyhow!("Failed to post message to pow worker: {:?}", e))?;

                response_futures.push(JsFuture::from(promise));
            }

            // Await all responses and pick the best result
            let mut best = (Salt::zero(), Pow(0), Hash::zero());
            for future in response_futures {
                let response_data = future.await
                    .map_err(|e| anyhow::anyhow!("Pow worker response error: {:?}", e))?;

                if let Some(result_str) = Reflect::get(&response_data, &JsValue::from_str("result"))
                    .ok()
                    .and_then(|v| v.as_string())
                {
                    if let Some(parsed) = parse_batch_result(&result_str) {
                        if parsed.1 > best.1 {
                            best = parsed;
                        }
                    }
                }
            }

            Ok::<_, anyhow::Error>(best)
        };

        SendWrapper::new(inner).await
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

/// Parse the `salt_hex:pow_u8:hash_hex` result string from `pow_compute_batch`.
fn parse_batch_result(result: &str) -> Option<(Salt, Pow, Hash)> {
    let parts: Vec<&str> = result.splitn(3, ':').collect();
    if parts.len() != 3 {
        warn!("Invalid pow_compute_batch result format: {}", result);
        return None;
    }

    let salt_bytes = hex::decode(parts[0]).ok()?;
    let pow_val: u8 = parts[1].parse().ok()?;
    let hash_bytes = hex::decode(parts[2]).ok()?;

    let salt = Salt::from_slice(&salt_bytes).ok()?;
    let hash = Hash::from_slice(&hash_bytes).ok()?;

    Some((salt, Pow(pow_val), hash))
}
