/**
 * Dedicated Web Worker that owns the {@link HashiverseClientWasm} instance.
 *
 * The main thread never loads WASM directly — it communicates with this
 * worker via `MessageChannel` RPC (see {@link HashiverseWorkerRPC} for the
 * message types).  On startup the worker initialises WASM, spawns a pool
 * of {@link HashiversePowWorker} sub-workers for proof-of-work, then
 * signals readiness to the main thread.
 *
 * Every method call on {@link HashiverseClientWasmProxy} in the main thread arrives
 * here as a `{ type: "call", method, args }` message, is dispatched to
 * the WASM client, and the result (or error) is posted back.
 *
 * @module
 */

import { HashiverseClientWasm, init_pow_workers, wasm_init } from "../../../hashiverse-rust/hashiverse-client-wasm/pkg";
import type { RPCRequest, RPCResponse } from "../tools/HashiverseWorkerRPC.ts";
import { Tools } from "../tools/Tools.ts";

let client: HashiverseClientWasm | null = null;

function assertClient(): HashiverseClientWasm {
	if (!client) {
		throw new Error("HashiverseClientWasm not initialized. Call init first.");
	}
	return client;
}

function isExposedMethodName(name: string) {
	// Prevent calling lifecycle internals from UI
	return name !== "constructor" && name !== "free" && name !== "dispose";
}

function postOk(port: MessagePort, result: unknown) {
	const res: RPCResponse = { ok: true, result };
	port.postMessage(res);
}

function postErr(port: MessagePort, err: unknown) {
	const e = err instanceof Error ? err : new Error(String(err));
	const res: RPCResponse = {
		ok: false,
		error: { name: e.name, message: e.message, stack: e.stack },
	};
	port.postMessage(res);
}

const onMessage = async (event: MessageEvent<RPCRequest>) => {
	const msg = event.data;
	const port = event.ports[0];
	if (!port) {
		console.error("HashiverseWorker: received message without a reply port, ignoring:", msg);
		return;
	}

	try {
		if (msg.type === "create_from_keyphrase") {
			client = await HashiverseClientWasm.create_from_keyphrase(msg.keyPhrase);
			postOk(port, true);
			return;
		}

		if (msg.type === "create_from_stored_key") {
			client = await HashiverseClientWasm.create_from_stored_key(msg.keyPublic);
			postOk(port, true);
			return;
		}

		if (msg.type === "dispose") {
			if (client) {
				client.free();
				client = null;
			}
			postOk(port, true);
			return;
		}

		if (msg.type === "call") {
			const c = assertClient();

			if (!isExposedMethodName(msg.method)) {
				postErr(port, new Error(`Method not exposed: ${msg.method}`));
				return;
			}

			const fn = (c as unknown as Record<string, unknown>)[msg.method];
			if (typeof fn !== "function") {
				postErr(port, new Error(`No such method on HashiverseClientWasm: ${msg.method}`));
				return;
			}

			const result = await fn.apply(c, msg.args);
			postOk(port, result);
			return;
		}
	} catch (err) {
		postErr(port, err);
	}
};

// Spawn pow sub-workers and hand them to WASM
function initPowWorkers(): Promise<void> {
	return new Promise((resolve) => {
		// How many PoW workers shall we allow?
		const IS_PRODUCTION = Tools.is_release_build();
		const GET_WORKER_COUNT = (MAX_WORKERS: number) => Math.min(MAX_WORKERS, Math.max((navigator.hardwareConcurrency || 1) - 2, 1));
		const MAX_WORKERS_DEV = 4;
		const MAX_WORKERS_PROD = 16;
		const numWorkers = IS_PRODUCTION ? GET_WORKER_COUNT(MAX_WORKERS_PROD) : GET_WORKER_COUNT(MAX_WORKERS_DEV);

		const workers: Worker[] = [];
		let readyCount = 0;

		for (let i = 0; i < numWorkers; i++) {
			const worker = new Worker(new URL("./HashiversePowWorker.ts", import.meta.url), { type: "module", name: "HashiversePowWorker" });
			worker.onmessage = (e) => {
				if (e.data?.type === "ready") {
					readyCount++;
					if (readyCount === numWorkers) {
						init_pow_workers(workers);
						console.log(`Hashiverse: ${numWorkers} PoW workers initialized`);
						resolve();
					}
				}
			};
			workers.push(worker);
		}
	});
}

(async () => {
	wasm_init(true);

	try {
		await initPowWorkers();
	} catch (e) {
		console.warn("Failed to init PoW workers, falling back to single-threaded:", e);
	}

	self.onmessage = onMessage;
	self.postMessage({ type: "ready" });
})();
