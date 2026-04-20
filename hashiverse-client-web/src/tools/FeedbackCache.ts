import { LRUCache } from "lru-cache";

const submitted = new LRUCache<string, true>({ max: 512 });

export function has_submitted_feedback(post_id: string): boolean {
	return submitted.has(post_id);
}

export function mark_feedback_submitted(post_id: string): void {
	submitted.set(post_id, true);
}
