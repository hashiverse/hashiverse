import { Button, Group, Image, Modal, Stack, TextInput } from "@mantine/core";
import { useDisclosure } from "@mantine/hooks";
import type React from "react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";

export interface YouTubeDialogControlManager {
	modal_command_open: () => Promise<string | null>;
}

interface Props {
	ref_manager: React.RefObject<YouTubeDialogControlManager | null>;
}

function getYouTubeThumbnail(url: string): string | null {
	try {
		const parsed_url = new URL(url);
		let video_id: string | null = null;
		if (parsed_url.hostname.includes("youtu.be")) {
			video_id = parsed_url.pathname.slice(1).split("?")[0];
		} else if (parsed_url.hostname.includes("youtube.com")) {
			video_id = parsed_url.searchParams.get("v");
			if (!video_id) {
				const path_match = parsed_url.pathname.match(/\/(?:shorts|embed)\/([^/?]+)/);
				if (path_match) video_id = path_match[1];
			}
		}
		return video_id ? `https://img.youtube.com/vi/${video_id}/hqdefault.jpg` : null;
	} catch {
		return null;
	}
}

export const YouTubeDialogControl: React.FC<Props> = ({ ref_manager }) => {
	const { t } = useTranslation();
	const [opened, { open, close }] = useDisclosure(false);
	const [url, set_url] = useState("");
	const ref_resolve = useRef<((value: string | null) => void) | null>(null);

	ref_manager.current = {
		modal_command_open: () =>
			new Promise((resolve, reject) => {
				if (ref_resolve.current) {
					reject("already open");
					return;
				}
				ref_resolve.current = resolve;
				set_url("");
				open();
			}),
	};

	const thumbnail = getYouTubeThumbnail(url);

	const on_insert = () => {
		ref_resolve.current?.(url);
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
				title={t("compose.insert_youtube")}
				zIndex={400}
				onKeyDown={(e) => {
					if (e.key === "Escape") e.stopPropagation();
				}}
			>
				<Stack>
					<TextInput
						data-autofocus
						label={t("compose.youtube_url_label")}
						placeholder="https://www.youtube.com/watch?v=..."
						value={url}
						onChange={(e) => set_url(e.currentTarget.value)}
						onPaste={(e) => e.stopPropagation()}
						onKeyDown={(e) => {
							if (e.key === "Enter" && thumbnail) on_insert();
						}}
					/>
					{thumbnail && <Image src={thumbnail} radius="sm" fit="cover" w={"100%"} />}
					<Group justify="flex-end">
						<Button variant="default" onClick={on_close}>
							{t("bio.cancel")}
						</Button>
						<Button disabled={!thumbnail} onClick={on_insert}>
							{t("compose.youtube_insert")}
						</Button>
					</Group>
				</Stack>
			</Modal>
		</div>
	);
};
