/**
 * Client-side cache of user settings, fetched once at the App level and
 * passed down to all routes that render posts.
 *
 * These settings are authoritative in the Rust/WASM layer (MetaPostManager).
 * This cache avoids repeated WASM round-trips during rendering. The cache is
 * always populated synchronously: the initial values are pre-fetched at
 * top-level bootstrap (before React mounts) and passed in as `initial`. When
 * the user logs in/out, the parent fetches fresh settings before swapping
 * them in via `replace()`. When settings change in the config tab, callers
 * `invalidate()` which re-fetches and updates state. There is never a "still
 * loading" state visible during render.
 *
 * @module
 */

import { useCallback, useMemo, useState } from "react";
import type { HashiverseClientWasmProxy } from "../Hashiverse.ts";
import { FEEDBACKS_NEGATIVE } from "./Feedback.ts";

export type ContentThresholds = Record<number, number>;

const DEFAULT_CONTENT_THRESHOLDS: ContentThresholds = Object.fromEntries(
	FEEDBACKS_NEGATIVE.filter((f): f is typeof f & { warning_threshold: number } => f.warning_threshold !== undefined).map((f) => [f.feedback_type, f.warning_threshold]),
);

export interface UserSettings {
	is_logged_in: boolean;
	own_client_id: string;
	followed_client_ids: Set<string>;
	content_thresholds: ContentThresholds;
	skip_warnings_for_followed: boolean;
}

export const EMPTY_USER_SETTINGS: UserSettings = {
	is_logged_in: false,
	own_client_id: "",
	followed_client_ids: new Set(),
	content_thresholds: DEFAULT_CONTENT_THRESHOLDS,
	skip_warnings_for_followed: false,
};

export async function fetch_user_settings(hashiverse: HashiverseClientWasmProxy): Promise<UserSettings> {
	const [logged_in_raw, client_id_raw, followed_raw, thresholds_raw, skip_warnings_raw] = await Promise.all([
		hashiverse.logged_in().catch(() => false),
		hashiverse.get_client_id().catch(() => ""),
		hashiverse.get_followed_client_ids_v1().catch(() => [] as string[]),
		(hashiverse.get_content_thresholds_v1() as Promise<ContentThresholds>).catch(() => ({}) as ContentThresholds),
		hashiverse.get_skip_warnings_for_followed_v1().catch(() => false),
	]);

	const is_logged_in = logged_in_raw as boolean;
	return {
		is_logged_in,
		own_client_id: is_logged_in ? (client_id_raw as string) : "",
		followed_client_ids: new Set(followed_raw as string[]),
		content_thresholds: { ...DEFAULT_CONTENT_THRESHOLDS, ...(thresholds_raw as ContentThresholds) },
		skip_warnings_for_followed: skip_warnings_raw as boolean,
	};
}

export interface UserSettingsCache extends UserSettings {
	invalidate: () => void;
	reset: () => void;
	replace: (next: UserSettings) => void;
}

export function useUserSettingsCache(hashiverse: HashiverseClientWasmProxy, initial: UserSettings): UserSettingsCache {
	const [settings, set_settings] = useState<UserSettings>(initial);

	const reset = useCallback(() => {
		set_settings(EMPTY_USER_SETTINGS);
	}, []);

	const replace = useCallback((next: UserSettings) => {
		set_settings(next);
	}, []);

	const invalidate = useCallback(() => {
		fetch_user_settings(hashiverse).then(set_settings).catch(console.error);
	}, [hashiverse]);

	return useMemo(
		() => ({
			...settings,
			invalidate,
			reset,
			replace,
		}),
		[settings, invalidate, reset, replace],
	);
}
