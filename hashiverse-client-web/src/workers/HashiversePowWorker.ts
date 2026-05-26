/**
 * Dedicated Web Worker for proof-of-work computation.
 *
 * A pool of these workers is spawned by {@link HashiverseWorker} at
 * startup and registered with the WASM runtime via `init_pow_workers()`.
 * The WASM layer dispatches PoW batches to them over `MessageChannel`,
 * keeping the main thread and the primary WASM worker unblocked during
 * the CPU-intensive hash grinding.
 *
 * @module
 */

import { pow_compute_batch, wasm_init } from "../../../hashiverse-rust/hashiverse-client-wasm/pkg";

type PowRequest = {
	iteration_limit: number;
	pow_min: number;
	data_hash_hex: string;
};

(async () => {
	wasm_init(false);

	self.onmessage = (event: MessageEvent<PowRequest>) => {
		const reply_port = event.ports[0];
		if (!reply_port) {
			console.error("HashiversePowWorker: received message without a reply port, ignoring");
			return;
		}
		const { iteration_limit, pow_min, data_hash_hex } = event.data;
		const result = pow_compute_batch(iteration_limit, pow_min, data_hash_hex);
		// Delay the reply so this worker thread idles for 5ms per chunk. The Rust dispatcher
		// only sends the next chunk in response to this reply, so the delay gives the OS
		// scheduler a window for UI repaints / other processes on weaker devices.
		setTimeout(() => {
			reply_port.postMessage({ result });
		}, 5);
	};

	self.postMessage({ type: "ready" });
})();
