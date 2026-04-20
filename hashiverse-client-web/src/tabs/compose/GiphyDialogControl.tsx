import { Modal, SimpleGrid, Stack, TextInput } from "@mantine/core";
import { useDisclosure } from "@mantine/hooks";
import type React from "react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

export interface GiphyDialogControlManager {
	modal_command_open: () => Promise<string | null>;
}

interface Props {
	ref_manager: React.RefObject<GiphyDialogControlManager | null>;
}

interface TenorGif {
	id: string;
	media: Array<{
		gif: { url: string };
		tinygif: { url: string };
	}>;
}

const TENOR_DEMO_KEY = "LIVDSRZULELA";

export const GiphyDialogControl: React.FC<Props> = ({ ref_manager }) => {
	const { t } = useTranslation();
	const [opened, { open, close }] = useDisclosure(false);
	const [query, set_query] = useState("");
	const [results, set_results] = useState<TenorGif[]>([]);
	const [loading, set_loading] = useState(false);
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
				open();
			}),
	};

	useEffect(() => {
		if (!opened) return;
		if (ref_debounce_timer.current) clearTimeout(ref_debounce_timer.current);
		if (!query.trim()) {
			set_results([]);
			return;
		}
		ref_debounce_timer.current = setTimeout(async () => {
			set_loading(true);
			try {
				const response = await fetch(`https://api.tenor.com/v1/search?key=${TENOR_DEMO_KEY}&q=${encodeURIComponent(query)}&limit=20&contentfilter=medium&media_filter=minimal`);
				const json = await response.json();
				set_results(json.results ?? []);
			} catch {
				set_results([]);
			} finally {
				set_loading(false);
			}
		}, 400);
		return () => {
			if (ref_debounce_timer.current) clearTimeout(ref_debounce_timer.current);
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
					<div style={{ maxHeight: 300, overflowY: "auto" }}>
						<SimpleGrid cols={3} spacing="xs">
							{results.map((gif) => (
								<button type="button" key={gif.id} onClick={() => on_select(gif.media[0].gif.url)} style={{ background: "none", border: "none", padding: 0, cursor: "pointer" }}>
									<img
										src={gif.media[0].tinygif.url}
										alt="GIF"
										style={{ width: "100%", borderRadius: 4 }}
										onMouseEnter={(e) => {
											(e.currentTarget as HTMLImageElement).src = gif.media[0].gif.url;
										}}
										onMouseLeave={(e) => {
											(e.currentTarget as HTMLImageElement).src = gif.media[0].tinygif.url;
										}}
									/>
								</button>
							))}
						</SimpleGrid>
					</div>
				</Stack>
			</Modal>
		</div>
	);
};
