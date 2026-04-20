import { LRUCache } from "lru-cache";
import type { Bio } from "../Hashiverse.ts";

// Tier 1: every bio we have ever fetched, keyed by client_id.
// Unbounded — bios are small and genuinely useful for search.
const known_bios = new Map<string, Bio>();

// Tier 2: raw client_ids we have encountered (posts, timelines).
// Bounded LRU so it doesn't grow forever.
const seen_client_ids = new LRUCache<string, true>({ max: 2048 });

export function register_bio(client_id: string, bio: Bio): void {
	seen_client_ids.delete(client_id);
	known_bios.set(client_id, bio);
}

export function register_client_id(client_id: string): void {
	if (!known_bios.has(client_id)) {
		seen_client_ids.set(client_id, true);
	}
}

export function search_mentions(query: string, max_results = 32): Bio[] {
	const q = query.toLowerCase();
	const results: Bio[] = [];
	const seen_in_results = new Set<string>();

	// Tier 1: known bios
	// — nickname: substring match anywhere
	// — client_id: prefix match only
	for (const bio of known_bios.values()) {
		if (bio.nickname?.toLowerCase().includes(q) || bio.client_id.toLowerCase().startsWith(q)) {
			results.push(bio);
			seen_in_results.add(bio.client_id);
			if (results.length >= max_results) return results;
		}
	}

	// Tier 2: recency cache — client_id prefix match only
	for (const client_id of seen_client_ids.keys()) {
		if (seen_in_results.has(client_id)) continue;
		if (client_id.toLowerCase().startsWith(q)) {
			results.push({
				client_id,
				nickname: "",
				status: "",
				selfie: "",
				avatar: "",
			});
			if (results.length >= max_results) return results;
		}
	}

	return results;
}
