/**
 * Client-side LRU cache for Bio data, purely for rendering performance.
 *
 * The authoritative bios are stored in BUCKET_BIO on the Rust/WASM side.
 * Every PostPanel, mention popup, etc. needs the author's bio to render,
 * and crossing the WASM bridge for each one is expensive.  This cache
 * keeps up to 512 recently-seen bios in JS memory with a 60-second TTL,
 * so most renders hit the cache instead of making a WASM round-trip.
 *
 * When the local user updates their own bio, call `flush_cached_bio()`
 * to invalidate the stale JS entry so the next read re-fetches from WASM.
 *
 * @module
 */

import { LRUCache } from "lru-cache";
import { useEffect, useState } from "react";
import type { Bio, HashiverseClientWasmProxy } from "../Hashiverse.ts";
import { register_bio } from "./MentionStore.ts";
import { Tools } from "./Tools.ts";

const cache = new LRUCache<string, Bio>({ max: 512, ttl: 60 * 1000 });
const inflight = new Map<string, Promise<Bio>>();

/** Return a cached bio if one exists in the JS-side LRU, without hitting WASM. */
function get_cached_bio_sync(client_id: string): Bio | null {
	return cache.get(client_id) ?? null;
}

/** Invalidate a JS-side cache entry (e.g. after the local user updates their bio). */
export function flush_cached_bio(client_id: string) {
	if (!client_id) return;
	console.log(`Flushing cached bio for ${client_id}`);
	cache.delete(client_id);
}

/** Fetch a bio, returning a cached copy if available or fetching from WASM and caching the result. */
function get_cached_bio(hashiverse: HashiverseClientWasmProxy, client_id: string): Promise<Bio> {
	const hit = cache.get(client_id);
	if (hit) return Promise.resolve(hit);

	const existing = inflight.get(client_id);
	if (existing) return existing;

	const p = hashiverse.get_bio(client_id).then((bio) => {
		cache.set(client_id, bio);
		inflight.delete(client_id);
		register_bio(client_id, bio);
		return bio;
	});

	inflight.set(client_id, p);

	return p;
}

/** Format a user ID and optional nickname for display in mentions. */
function format_mention_name(client_id: string, nickname?: string | null): string {
	const short_id = `${client_id.substring(0, 8)}...`;
	return nickname ? `${nickname} (${short_id})` : short_id;
}

/** Populate a mention's avatar img and nickname text from the bio cache, fetching async if needed. */
export function populate_mention_bio(hashiverse: HashiverseClientWasmProxy, client_id: string, img: HTMLImageElement, nickname_element: HTMLElement | null) {
	const apply = (bio: Bio | null) => {
		img.src = bio?.selfie || Tools.create_avatar(client_id, bio?.avatar ?? null);
		if (nickname_element) nickname_element.textContent = format_mention_name(client_id, bio?.nickname);
	};

	const cached = get_cached_bio_sync(client_id);
	apply(cached);

	if (!cached && hashiverse) {
		get_cached_bio(hashiverse, client_id)
			.then(apply)
			.catch(() => {});
	}
}

/** React hook: returns a bio from the LRU cache synchronously if available, otherwise fetches async and re-renders. */
export function useCachedBio(hashiverse: HashiverseClientWasmProxy, id: string): Bio | null {
	const [bio, set_bio] = useState<Bio | null>(() => (id ? get_cached_bio_sync(id) : null));

	useEffect(() => {
		if (!id) {
			set_bio(null);
			return;
		}
		const sync = get_cached_bio_sync(id);
		if (sync) {
			set_bio(sync);
			return;
		}
		get_cached_bio(hashiverse, id)
			.then(set_bio)
			.catch(() => {});
	}, [hashiverse, id]);

	return bio;
}
