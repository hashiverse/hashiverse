/**
 * Main-thread API facade for the hashiverse WASM client.
 *
 * All WASM work runs off the main thread inside a dedicated
 * {@link HashiverseWorker}.  This module spawns that worker, waits for
 * it to initialise, and returns a {@link HashiverseClientWasmProxy} proxy whose
 * method calls are transparently forwarded over `MessageChannel` RPC.
 *
 * UI code should only interact with hashiverse through {@link HashiverseClientWasmProxy}
 * — it never touches WASM or the worker directly.
 *
 * @module
 */

import type { HashiverseClientWasm } from "../../hashiverse-rust/hashiverse-client-wasm/pkg";

export type {
	Bio,
	PeerInfoV1,
	Post,
	PowJobStatusV1,
	SingleTimelineGetMoreV1Response,
	TrendingHashtag,
	TrendingHashtagsFetchResponse,
} from "../../hashiverse-rust/hashiverse-client-wasm/pkg";

import { DeferredPromise } from "./tools/DeferredPromise.ts";
import type { RPCRequest, RPCResponse } from "./tools/HashiverseWorkerRPC.ts";

// biome-ignore lint/suspicious/noExplicitAny: required for conditional type inference in MethodKeys/WrapAsync
type AnyFn = (...args: any[]) => any;

type MethodKeys<T> = {
	[K in keyof T]: T[K] extends AnyFn ? K : never;
}[keyof T];

// biome-ignore lint/suspicious/noExplicitAny: required for conditional type inference in MethodKeys/WrapAsync
type WrapAsync<T extends AnyFn> = T extends (...args: infer A) => infer R ? (R extends Promise<any> ? T : (...args: A) => Promise<R>) : never;

/**
 * Async proxy type mirroring every public method on {@link HashiverseClientWasm},
 * with all return types wrapped in `Promise`.  Lifecycle methods (`free`,
 * `Symbol.dispose`) are excluded and replaced with a JS-friendly `dispose()`.
 *
 * Calls on this proxy are forwarded over `MessageChannel` RPC to the
 * {@link HashiverseWorker}, which invokes the real {@link HashiverseClientWasm}
 * method inside its dedicated Web Worker.
 */
export type HashiverseClientWasmProxy = {
	[K in Exclude<MethodKeys<HashiverseClientWasm>, "free" | typeof Symbol.dispose>]: WrapAsync<HashiverseClientWasm[K]>;
} & { dispose(): Promise<void> };

export class Hashiverse {
	private static async create_from_xxx(init_req: RPCRequest): Promise<HashiverseClientWasmProxy> {
		const deferred_promise = new DeferredPromise();

		console.log("Creating Hashiverse worker");
		const worker = new Worker(new URL("./workers/HashiverseWorker.ts", import.meta.url), { type: "module", name: "HashiverseWorker" });
		console.log("Created Hashiverse worker");

		// Wait for the worker to signal it's ready (one-time bootstrap via self.postMessage)
		worker.onmessage = async () => {
			worker.onmessage = null;
			await rpc<boolean>(init_req);
			deferred_promise.resolve(null);
		};

		worker.onerror = (e) => {
			console.error("Hashiverse worker error:", e.message);
		};

		function rpc<T>(request: RPCRequest): Promise<T> {
			return new Promise<T>((resolve, reject) => {
				const channel = new MessageChannel();
				channel.port1.onmessage = (event: MessageEvent<RPCResponse>) => {
					channel.port1.close();
					const response = event.data;
					if (response.ok) {
						resolve(response.result as T);
					} else {
						const err = new Error(response.error.message);
						err.name = response.error.name ?? "WorkerError";
						(err as unknown as Record<string, unknown>).stack = response.error.stack;
						reject(err);
					}
				};
				worker.postMessage(request, [channel.port2]);
			});
		}

		await deferred_promise.promise;

		const method_cache = new Map<string | symbol, unknown>();

		const api = new Proxy(
			{},
			{
				get(_target, prop) {
					const cached = method_cache.get(prop);
					if (cached !== undefined) return cached;

					let value: unknown;

					if (prop === "dispose") {
						value = async () => {
							await rpc({ type: "dispose" });
							worker.terminate();
						};
					} else if (prop === "then" || prop === "catch" || prop === "finally" || prop === "toJSON") {
						return undefined;
					} else if (typeof prop !== "string") {
						return undefined;
					} else {
						value = (...args: unknown[]) => rpc({ type: "call", method: prop, args });
					}

					method_cache.set(prop, value);
					return value;
				},
			},
		);

		return api as HashiverseClientWasmProxy;
	}

	static async create_from_keyphrase(keyPhrase: string): Promise<HashiverseClientWasmProxy> {
		return Hashiverse.create_from_xxx({
			type: "create_from_keyphrase",
			keyPhrase,
		});
	}

	static async create_from_stored_key(keyPublic: string): Promise<HashiverseClientWasmProxy> {
		return Hashiverse.create_from_xxx({
			type: "create_from_stored_key",
			keyPublic,
		});
	}
}
