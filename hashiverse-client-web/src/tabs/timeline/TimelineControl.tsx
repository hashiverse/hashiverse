import { Button, Skeleton, Stack } from "@mantine/core";
import type React from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Virtuoso } from "react-virtuoso";
import type { HashiverseClientWasmProxy, Post, SingleTimelineGetMoreV1Response } from "../../Hashiverse.ts";
import { Spinner } from "../../tools/Spinner.tsx";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { PostPanel } from "./PostPanel.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	user_settings_cache: UserSettingsCache;
	timeline_key: string;
	get_more: () => Promise<SingleTimelineGetMoreV1Response>;
	reset: () => Promise<void>;
	header?: React.ReactNode;
	blur_images?: boolean;
}

export const TimelineControl: React.FC<Props> = ({ hashiverse, user_settings_cache, timeline_key, get_more, reset, header, blur_images }) => {
	const [scroller, scroller_set] = useState<HTMLElement | null>(null);

	useEffect(() => {
		if (!scroller) return;
		const handler = (e: WheelEvent) => {
			if (scroller.contains(e.target as Node)) return;
			const delta = e.deltaMode === 1 ? e.deltaY * 40 : e.deltaMode === 2 ? e.deltaY * window.innerHeight : e.deltaY;
			scroller.scrollBy({ top: delta });
		};
		document.addEventListener("wheel", handler, { passive: true });
		return () => document.removeEventListener("wheel", handler);
	}, [scroller]);

	const [posts, posts_set] = useState<Post[]>(() => []);
	const [loading, loading_set] = useState(false);
	const [oldest_processed_time_millis, oldest_processed_time_millis_set] = useState<number | undefined>(undefined);

	const generation_ref = useRef(0);
	const loading_ref = useRef(false);
	const more_available_ref = useRef(true);

	// This is the standard "latest ref"	attern to avoid stale closures in effects.
	const get_more_ref = useRef(get_more);
	get_more_ref.current = get_more;
	const reset_ref = useRef(reset);
	reset_ref.current = reset;

	// When timeline_key changes, reset everything and do the initial fetch
	// biome-ignore lint/correctness/useExhaustiveDependencies: timeline_key is an intentional trigger for this effect
	useEffect(() => {
		const current_generation = ++generation_ref.current;

		posts_set([]);
		loading_set(false);
		loading_ref.current = false;
		more_available_ref.current = true;
		oldest_processed_time_millis_set(undefined);

		(async () => {
			await reset_ref.current();
			if (generation_ref.current !== current_generation) return;

			loading_set(true);
			loading_ref.current = true;
			try {
				const response = await get_more_ref.current();
				if (generation_ref.current !== current_generation) return;

				more_available_ref.current = response.posts.length > 0;
				oldest_processed_time_millis_set(response.oldest_processed_time_millis);
				response.posts.sort((a, b) => b.time_millis - a.time_millis);
				posts_set(response.posts);
			} finally {
				if (generation_ref.current === current_generation) {
					loading_set(false);
					loading_ref.current = false;
				}
			}
		})();
	}, [timeline_key]);

	// Core fetch — appends whatever the next page returns. Callers decide whether to call it.
	const fetch_more_posts = useCallback(async () => {
		if (loading_ref.current) return;

		const current_generation = generation_ref.current;
		loading_set(true);
		loading_ref.current = true;
		try {
			const response = await get_more_ref.current();
			if (generation_ref.current !== current_generation) return;

			// more_available_ref exists only to stop Virtuoso's endReached from firing
			// repeatedly when the list bottom is visible. Timelines have no true end —
			// the manual button below bypasses this flag and always fetches.
			more_available_ref.current = response.posts.length > 0;
			oldest_processed_time_millis_set(response.oldest_processed_time_millis);
			response.posts.sort((a, b) => b.time_millis - a.time_millis);
			posts_set((existing_posts) => [...existing_posts, ...response.posts]);
		} finally {
			if (generation_ref.current === current_generation) {
				loading_set(false);
				loading_ref.current = false;
			}
		}
	}, []);

	// Virtuoso endReached: gated so it doesn't loop forever when the bottom is on-screen.
	const virtuoso_end_reached = useCallback(() => {
		if (!more_available_ref.current) return;
		void fetch_more_posts();
	}, [fetch_more_posts]);

	const { t } = useTranslation();

	const Header = useMemo(() => {
		if (!header) return undefined;
		const HeaderComponent: React.FC = () => <>{header}</>;
		return HeaderComponent;
	}, [header]);

	const Footer = useMemo(() => {
		const FooterComponent: React.FC = () => (
			<Stack p="5px">
				{loading ? (
					<Stack gap="0" style={{ width: "100%" }}>
						<Spinner size={40} />
						<Skeleton height={8} mt={12} />
						<Skeleton height={8} mt={6} />
						<Skeleton height={8} mt={6} width="70%" />
					</Stack>
				) : (
					<Button disabled={loading} onClick={fetch_more_posts}>
						{oldest_processed_time_millis !== undefined
							? t("timeline.load_more_before", {
									date: new Date(oldest_processed_time_millis).toLocaleString(),
								})
							: t("timeline.load_more")}
					</Button>
				)}
			</Stack>
		);

		return FooterComponent;
	}, [loading, fetch_more_posts, oldest_processed_time_millis, t]);

	const virtuoso_components = useMemo(() => ({ Header, Footer }), [Header, Footer]);

	const item_content = useCallback(
		(_index: number, post: Post) => {
			return <PostPanel hashiverse={hashiverse} post={post} blur_images={blur_images} user_settings_cache={user_settings_cache} />;
		},
		[hashiverse, blur_images, user_settings_cache],
	);

	return (
		<Virtuoso
			style={{ height: "100%" }}
			data={posts}
			computeItemKey={(_index, post: Post) => post.post_id}
			endReached={virtuoso_end_reached}
			increaseViewportBy={50}
			components={virtuoso_components}
			itemContent={item_content}
			scrollerRef={(el) => scroller_set(el instanceof HTMLElement ? el : null)}
		/>
	);
};
