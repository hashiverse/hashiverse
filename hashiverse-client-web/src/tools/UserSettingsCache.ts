/**
 * Client-side cache of user settings, fetched once at the App level and
 * passed down to all routes that render posts.
 *
 * These settings are authoritative in the Rust/WASM layer (MetaPostManager).
 * This cache avoids repeated WASM round-trips during rendering.  When the
 * user changes settings in the config tab, the App re-fetches via
 * `on_settings_changed`.
 *
 * @module
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import type { HashiverseClientWasmProxy } from "../Hashiverse.ts";
import { FEEDBACKS_NEGATIVE } from "./Feedback.ts";

export type ContentThresholds = Record<number, number>;

const DEFAULT_CONTENT_THRESHOLDS: ContentThresholds = Object.fromEntries(
	FEEDBACKS_NEGATIVE.filter((f): f is typeof f & { warning_threshold: number } => f.warning_threshold !== undefined).map((f) => [f.feedback_type, f.warning_threshold]),
);

export interface UserSettingsCache {
	is_logged_in: boolean;
	own_client_id: string;
	followed_client_ids: Set<string>;
	content_thresholds: ContentThresholds;
	skip_warnings_for_followed: boolean;
	invalidate: () => void;
	reset: () => void;
}

export function useUserSettingsCache(hashiverse: HashiverseClientWasmProxy): UserSettingsCache {
	const [is_logged_in, is_logged_in_set] = useState(false);
	const [own_client_id, own_client_id_set] = useState("");
	const [followed_client_ids, followed_client_ids_set] = useState<Set<string>>(() => new Set());
	const [content_thresholds, content_thresholds_set] = useState<ContentThresholds>(DEFAULT_CONTENT_THRESHOLDS);
	const [skip_warnings_for_followed, skip_warnings_for_followed_set] = useState(false);
	const [settings_generation, setSettings_generation] = useState(0);

	const reset = useCallback(() => {
		is_logged_in_set(false);
		own_client_id_set("");
		followed_client_ids_set(new Set());
		content_thresholds_set(DEFAULT_CONTENT_THRESHOLDS);
		skip_warnings_for_followed_set(false);
	}, []);

	// Reset to defaults when the hashiverse instance changes (login/logout)
	useEffect(() => {
		reset();
	}, [reset]);

	// Fetch actual values from WASM
	useEffect(() => {
		hashiverse
			.logged_in()
			.then((v) => {
				const logged_in = v as boolean;
				is_logged_in_set(logged_in);
				if (logged_in) {
					hashiverse
						.get_client_id()
						.then((id) => own_client_id_set(id as string))
						.catch(console.error);
				} else {
					own_client_id_set("");
				}
			})
			.catch(console.error);
		hashiverse
			.get_followed_client_ids_v1()
			.then((ids) => followed_client_ids_set(new Set(ids)))
			.catch(console.error);
		(hashiverse.get_content_thresholds_v1() as Promise<ContentThresholds>).then((stored) => content_thresholds_set((prev) => ({ ...prev, ...stored }))).catch(console.error);
		hashiverse.get_skip_warnings_for_followed_v1().then(skip_warnings_for_followed_set).catch(console.error);
	}, [hashiverse, settings_generation]);

	const invalidate = useCallback(() => {
		setSettings_generation((g) => g + 1);
	}, []);

	return useMemo(
		() => ({
			is_logged_in,
			own_client_id,
			followed_client_ids,
			content_thresholds,
			skip_warnings_for_followed,
			invalidate,
			reset,
		}),
		[is_logged_in, own_client_id, followed_client_ids, content_thresholds, skip_warnings_for_followed, invalidate, reset],
	);
}
