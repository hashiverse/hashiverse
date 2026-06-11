use crate::tools::pow_generator::pow_generator::{JobTracker, PowGenerator};
use crate::tools::types::{Hash, Pow, Salt};
use js_sys::{Array, Object, Reflect};
use log::{info, warn};
use send_wrapper::SendWrapper;
use std::sync::{Arc, Mutex};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{MessageChannel, MessageEvent, Worker};

/// A `PowGenerator` that dispatches chunks to pre-created Web Workers.
/// The TypeScript side is responsible for spawning and initializing the workers;
/// this struct simply receives the ready `Worker` handles and exposes one slot per worker.
/// All scheduling — work-stealing refeed, early exit, tracker registration — lives in the
/// shared `run_pool` dispatcher in `hashiverse-lib`.
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
impl PowGenerator for WasmParallelPowGenerator {
    fn pool_size(&self) -> usize {
        self.workers.len()
    }

    async fn run_chunk(&self, slot: usize, chunk_iterations: usize, pow_min: Pow, data_hash: Hash) -> anyhow::Result<(Salt, Pow, Hash)> {
        if self.workers.is_empty() {
            anyhow::bail!("No pow workers available");
        }

        let worker = self.workers.get(slot).ok_or_else(|| anyhow::anyhow!("Invalid pow worker slot {}", slot))?.clone();
        let data_hash_hex = hex::encode(data_hash);

        // JsFuture / Worker are !Send; in WASM everything is single-threaded so wrap the
        // !Send call in SendWrapper to satisfy the async-trait Send bound.
        let inner = async move {
            let channel = MessageChannel::new().map_err(|e| anyhow::anyhow!("Failed to create MessageChannel: {:?}", e))?;
            let port1 = channel.port1();
            let port2 = channel.port2();

            // Three independent paths can end the Promise:
            //   - port1.onmessage          → success, resolve with the reply payload
            //   - port1.onmessageerror     → reply failed structured clone, reject
            //   - worker.onerror           → worker uncaught throw / load failure, reject
            // Without the two rejection paths the awaiter hangs forever if the worker dies
            // before it can post `{ error: ... }`.
            let promise = js_sys::Promise::new(&mut |resolve, reject| {
                let resolve_clone = resolve.clone();
                let onmessage = Closure::once_into_js(move |event: MessageEvent| {
                    resolve_clone.call1(&JsValue::NULL, &event.data()).ok();
                });
                port1.set_onmessage(Some(onmessage.unchecked_ref()));

                let reject_clone = reject.clone();
                let onmessageerror = Closure::once_into_js(move |_event: MessageEvent| {
                    reject_clone.call1(&JsValue::NULL, &JsValue::from_str("pow worker reply failed structured clone (onmessageerror)")).ok();
                });
                port1.set_onmessageerror(Some(onmessageerror.unchecked_ref()));

                let reject_clone = reject.clone();
                let onerror = Closure::once_into_js(move |event: JsValue| {
                    reject_clone.call1(&JsValue::NULL, &JsValue::from_str(&format!("pow worker error event: {:?}", event))).ok();
                });
                worker.set_onerror(Some(onerror.unchecked_ref()));
            });

            let msg = Object::new();
            Reflect::set(&msg, &JsValue::from_str("iteration_limit"), &JsValue::from_f64(chunk_iterations as f64)).ok();
            Reflect::set(&msg, &JsValue::from_str("pow_min"), &JsValue::from_f64(pow_min.0 as f64)).ok();
            Reflect::set(&msg, &JsValue::from_str("data_hash_hex"), &JsValue::from_str(&data_hash_hex)).ok();

            let transfer = Array::new();
            transfer.push(&port2);
            worker
                .post_message_with_transfer(&msg, &transfer)
                .map_err(|e| anyhow::anyhow!("Failed to post message to pow worker: {:?}", e))?;

            let result = JsFuture::from(promise).await;

            // Cleanup regardless of outcome: clearing worker.onerror matters most — the
            // worker is shared across run_chunk calls, so a leftover handler would mis-
            // route the next chunk's error. port1.close() releases the channel; port2
            // was transferred to the worker and is no longer ours to close.
            port1.set_onmessage(None);
            port1.set_onmessageerror(None);
            worker.set_onerror(None);
            port1.close();

            let response_data = result.map_err(|e| anyhow::anyhow!("Pow worker response error: {:?}", e))?;

            // The TS worker posts either `{ result: "salt:pow:hash" }` on success or
            // `{ error: "..." }` if `pow_compute_batch` threw. Surface the error end-to-end.
            if let Some(error_message) = Reflect::get(&response_data, &JsValue::from_str("error")).ok().and_then(|v| v.as_string()) {
                anyhow::bail!("Pow worker error: {}", error_message);
            }

            let result_str = Reflect::get(&response_data, &JsValue::from_str("result"))
                .ok()
                .and_then(|v| v.as_string())
                .ok_or_else(|| anyhow::anyhow!("Pow worker reply missing `result` string"))?;

            parse_batch_result(&result_str).ok_or_else(|| anyhow::anyhow!("Invalid pow_compute_batch result format: {}", result_str))
        };

        SendWrapper::new(inner).await
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
