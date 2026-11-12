import { Modal, SimpleGrid, Stack, Text, TextInput } from "@mantine/core";
import { useDisclosure } from "@mantine/hooks";
import type React from "react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

export interface GifDialogControlManager {
	modal_command_open: () => Promise<string | null>;
}

export interface Props {
	ref_manager: React.RefObject<GifDialogControlManager | null>;
}

// GIF search is provided by KLIPY (Tenor's API was discontinued by Google on 2026-06-30).
//
// This is a deliberately public client key: any key a browser app uses is visible in its shipped bundle,
// so there is nothing to gain by hiding it in a build secret. Forks and self-hosters should register
// their own free key at https://partner.klipy.com and replace it here.
const KLIPY_API_KEY = "uSVnEivu842AeYZzMnXyqT7tN7HBB24TXoHINyuacKm2b3Tm0yCeW1japfmnhUxV";
const KLIPY_RESULTS_PER_PAGE = 24;

interface KlipyMediaFile {
	url: string;
	width: number;
	height: number;
	size: number;
}

interface KlipyMediaSize {
	gif?: KlipyMediaFile;
	webp?: KlipyMediaFile;
	jpg?: KlipyMediaFile;
}

interface KlipyItem {
	id: number;
	slug: string;
	title: string;
	type: string;
	file: {
		hd?: KlipyMediaSize;
		md?: KlipyMediaSize;
		sm?: KlipyMediaSize;
		xs?: KlipyMediaSize;
	};
}

interface KlipySearchResponse {
	result: boolean;
	data?: { data?: KlipyItem[] };
	errors?: { message?: string[] };
}

interface GifResult {
	id: number;
	title: string;
	preview_url: string;
	full_url: string;
}

// KLIPY may interleave non-GIF (e.g. ad) items; keep only GIFs that have the renditions we use.
// URLs are used exactly as returned (KLIPY's usage rules forbid altering or reconstructing them).
function klipy_items_to_gif_results(klipy_items: KlipyItem[]): GifResult[] {
	const gif_results: GifResult[] = [];
	for (const klipy_item of klipy_items) {
		if (klipy_item.type !== "gif") continue;
		const preview_url = klipy_item.file.xs?.gif?.url ?? klipy_item.file.sm?.gif?.url;
		const full_url = klipy_item.file.md?.gif?.url ?? klipy_item.file.hd?.gif?.url;
		if (!preview_url || !full_url) continue;
		gif_results.push({ id: klipy_item.id, title: klipy_item.title, preview_url, full_url });
	}
	return gif_results;
}

export const GifDialogControl: React.FC<Props> = ({ ref_manager }) => {
	const { t } = useTranslation();
	const [opened, { open, close }] = useDisclosure(false);
	const [query, set_query] = useState("");
	const [results, set_results] = useState<GifResult[]>([]);
	const [loading, set_loading] = useState(false);
	const [search_error, set_search_error] = useState<string | null>(null);
	const [searched_query, set_searched_query] = useState("");
	const ref_resolve = useRef<((value: string | null) => void) | null>(null);
	const ref_debounce_timer = useRef<ReturnType<typeof setTimeout> | null>(null);

	ref_manager.current = {
		modal_command_open: () =>
			new Promise((resolve, reject) => {
				if (ref_resolve.current) {
					reject("already open");
					return;
				}
				ref_resolve.current = resolve;
				set_query("");
				set_results([]);
				set_search_error(null);
				set_searched_query("");
				set_loading(false);
				open();
			}),
	};

	useEffect(() => {
		if (!opened) return;
		if (ref_debounce_timer.current) clearTimeout(ref_debounce_timer.current);
		const trimmed_query = query.trim();
		if (!trimmed_query) {
			set_results([]);
			set_search_error(null);
			set_searched_query("");
			set_loading(false);
			return;
		}
		// One controller per (query, dialog session): the cleanup below aborts it whenever the query changes or the
		// dialog closes, so a slow response for an old query or a previous session can never overwrite current state.
		const abort_controller = new AbortController();
		ref_debounce_timer.current = setTimeout(async () => {
			set_loading(true);
			try {
				const search_url = `https://api.klipy.com/api/v1/${KLIPY_API_KEY}/gifs/search?q=${encodeURIComponent(trimmed_query)}&per_page=${KLIPY_RESULTS_PER_PAGE}`;
				const response = await fetch(search_url, { signal: abort_controller.signal });
				const klipy_search_response = (await response.json().catch(() => null)) as KlipySearchResponse | null;
				if (abort_controller.signal.aborted) return;
				if (!response.ok || !klipy_search_response?.result) {
					const provider_message = klipy_search_response?.errors?.message?.join(" ") || `HTTP ${response.status}`;
					throw new Error(provider_message);
				}
				set_results(klipy_items_to_gif_results(klipy_search_response.data?.data ?? []));
				set_search_error(null);
			} catch (error) {
				if (abort_controller.signal.aborted) return;
				console.error("GIF search failed", error);
				set_results([]);
				set_search_error(error instanceof Error ? error.message : String(error));
			} finally {
				if (!abort_controller.signal.aborted) {
					set_searched_query(trimmed_query);
					set_loading(false);
				}
			}
		}, 400);
		return () => {
			if (ref_debounce_timer.current) clearTimeout(ref_debounce_timer.current);
			abort_controller.abort();
		};
	}, [query, opened]);

	const on_select = (gif_url: string) => {
		ref_resolve.current?.(gif_url);
		ref_resolve.current = null;
		close();
	};

	const on_close = () => {
		ref_resolve.current?.(null);
		ref_resolve.current = null;
		close();
	};

	const show_no_results = !loading && !search_error && searched_query !== "" && searched_query === query.trim() && results.length === 0;

	return (
		// Stop React portal event bubbling — clicks inside this modal would otherwise propagate
		// up through the React tree to ComposeEditor's onClick and steal focus from the editor.
		// biome-ignore lint/a11y/noStaticElementInteractions: event-bubbling barrier, not interactive
		// biome-ignore lint/a11y/useKeyWithClickEvents: event-bubbling barrier, not interactive
		<div onClick={(e) => e.stopPropagation()} onMouseDown={(e) => e.stopPropagation()}>
			<Modal
				opened={opened}
				onClose={on_close}
				title={t("compose.insert_giphy")}
				zIndex={400}
				onKeyDown={(e) => {
					if (e.key === "Escape") e.stopPropagation();
				}}
			>
				<Stack>
					<TextInput
						data-autofocus
						placeholder={t("compose.giphy_search_placeholder")}
						value={query}
						onChange={(e) => set_query(e.currentTarget.value)}
						onPaste={(e) => e.stopPropagation()}
						rightSection={loading ? <span style={{ fontSize: 10 }}>…</span> : null}
					/>
					{search_error && (
						<Text size="sm" c="red">
							{t("compose.gif_search_failed", { message: search_error })}
						</Text>
					)}
					{show_no_results && (
						<Text size="sm" c="dimmed">
							{t("compose.gif_no_results")}
						</Text>
					)}
					<div style={{ maxHeight: 300, overflowY: "auto" }}>
						<SimpleGrid cols={3} spacing="xs">
							{results.map((gif_result) => (
								<button type="button" key={gif_result.id} onClick={() => on_select(gif_result.full_url)} style={{ background: "none", border: "none", padding: 0, cursor: "pointer" }}>
									<img
										src={gif_result.preview_url}
										alt={gif_result.title || "GIF"}
										style={{ width: "100%", borderRadius: 4 }}
										onMouseEnter={(e) => {
											(e.currentTarget as HTMLImageElement).src = gif_result.full_url;
										}}
										onMouseLeave={(e) => {
											(e.currentTarget as HTMLImageElement).src = gif_result.preview_url;
										}}
									/>
								</button>
							))}
						</SimpleGrid>
					</div>
					<Text size="xs" c="dimmed" ta="right">
						{t("compose.gif_powered_by")}
					</Text>
				</Stack>
			</Modal>
		</div>
	);
};
