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
		try {
			const result = pow_compute_batch(iteration_limit, pow_min, data_hash_hex);
			reply_port.postMessage({ result });
		} catch (error) {
			reply_port.postMessage({ error: error instanceof Error ? error.message : String(error) });
		}
	};

	self.postMessage({ type: "ready" });
})();
