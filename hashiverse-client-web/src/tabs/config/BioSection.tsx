import { Avatar, Box, Button, Group, Modal, SimpleGrid, Stack, Text, Textarea, TextInput, Tooltip } from "@mantine/core";
import { IconPhoto, IconRefresh, IconUser, IconX } from "@tabler/icons-react";
import type React from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { flush_cached_bio, useCachedBio } from "../../tools/BioCache.ts";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import { Toast } from "../../tools/Toast.ts";
import { Tools } from "../../tools/Tools.ts";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	user_settings_cache: UserSettingsCache;
}

const AVATAR_COUNT = 9;
const SELFIE_MAX_WIDTH = 256;

function random_seeds(count: number): string[] {
	return Array.from({ length: count }, () => Math.random().toString(36).slice(2));
}

const slot_style: React.CSSProperties = {
	position: "relative",
	width: 80,
	height: 80,
	borderRadius: 8,
	overflow: "hidden",
	cursor: "pointer",
	flexShrink: 0,
};

const overlay_style: React.CSSProperties = {
	position: "absolute",
	bottom: 0,
	left: 0,
	right: 0,
	background: "rgba(0,0,0,0.55)",
	padding: "3px 0",
	textAlign: "center",
};

const clear_btn_style: React.CSSProperties = {
	position: "absolute",
	top: 2,
	right: 2,
	background: "rgba(0,0,0,0.6)",
	borderRadius: "50%",
	width: 20,
	height: 20,
	display: "flex",
	alignItems: "center",
	justifyContent: "center",
	cursor: "pointer",
	zIndex: 1,
};

export const BioSection: React.FC<Props> = ({ hashiverse, user_settings_cache }) => {
	const { t } = useTranslation();
	const client_id = user_settings_cache.own_client_id;
	const [nickname, set_nickname] = useState("");
	const [status, set_status] = useState("");
	const [selfie, set_selfie] = useState("");
	const [selected_avatar_seed, set_selected_avatar_seed] = useState("");

	const [avatar_modal_open, set_avatar_modal_open] = useState(false);
	const [avatar_seeds, set_avatar_seeds] = useState<string[]>(() => random_seeds(AVATAR_COUNT));
	const [temp_seed, set_temp_seed] = useState("");

	const ref_selfie_input = useRef<HTMLInputElement>(null);

	const fetched_bio = useCachedBio(hashiverse, client_id);

	useEffect(() => {
		if (!fetched_bio) return;
		if (fetched_bio.nickname) set_nickname(fetched_bio.nickname);
		if (fetched_bio.status) set_status(fetched_bio.status);
		if (fetched_bio.selfie) set_selfie(fetched_bio.selfie);
		if (fetched_bio.avatar) set_selected_avatar_seed(fetched_bio.avatar);
	}, [fetched_bio]);

	const on_selfie_upload = async (e: React.ChangeEvent<HTMLInputElement>) => {
		if (!e.target.files?.length) return;
		const file = e.target.files[0];
		const image = await Tools.crush_image(file, SELFIE_MAX_WIDTH);
		set_selfie(image ?? "");
		e.target.value = "";
	};

	const open_avatar_modal = () => {
		set_temp_seed(selected_avatar_seed);
		set_avatar_modal_open(true);
	};

	const on_roll_again = useCallback(() => {
		set_avatar_seeds(random_seeds(AVATAR_COUNT));
		set_temp_seed("");
	}, []);

	const on_avatar_select = () => {
		set_selected_avatar_seed(temp_seed);
		set_avatar_modal_open(false);
	};

	const on_submit_bio = async () => {
		try {
			await hashiverse.set_bio(nickname, status, selfie, selected_avatar_seed);

			// This is one of the few times we manually push our config out as a post.
			// Normally we rely on the once-a-month push
			await hashiverse.submit_meta_post_v1();

			flush_cached_bio(client_id);
			Toast.success(t("toast.bio_saved"));
		} catch (e) {
			console.error(e);
			Toast.error(t("toast.error_generic"));
		}
	};

	return (
		<CollapsiblePanel title={t("bio.title")}>
			<Stack>
				<TextInput label={t("bio.nickname_label")} placeholder={t("bio.nickname_placeholder")} value={nickname} onChange={(e) => set_nickname(e.currentTarget.value)} />

				<Textarea label={t("bio.status_label")} placeholder={t("bio.status_placeholder")} value={status} onChange={(e) => set_status(e.currentTarget.value)} autosize minRows={2} maxRows={5} />

				<Stack gap={4}>
					<Text fw={500}>{t("bio.profile_picture")}</Text>
					<Group align="flex-start" gap="xl">
						<Stack gap="xs" align="center">
							<Text>{t("bio.selfie")}</Text>
							<Box style={slot_style} onClick={() => ref_selfie_input.current?.click()}>
								{selfie ? (
									<Avatar src={selfie} size={80} radius={0} />
								) : (
									<Avatar size={80} radius={0}>
										<IconPhoto size={32} />
									</Avatar>
								)}
								<Box style={overlay_style}>
									<Text size="xs" c="white">
										{t("bio.upload")}
									</Text>
								</Box>
								{selfie && (
									<Box
										style={clear_btn_style}
										onClick={(e) => {
											e.stopPropagation();
											set_selfie("");
										}}
									>
										<IconX size={12} color="white" />
									</Box>
								)}
							</Box>
						</Stack>

						<Stack gap="xs" align="center">
							<Text>{t("bio.chosen_avatar")}</Text>
							<Box style={slot_style} onClick={open_avatar_modal}>
								{selected_avatar_seed && client_id ? (
									<Avatar src={Tools.create_avatar(client_id, selected_avatar_seed)} size={80} radius={0} />
								) : (
									<Avatar size={80} radius={0}>
										<IconUser size={32} />
									</Avatar>
								)}
								<Box style={overlay_style}>
									<Text size="xs" c="white">
										{t("bio.choose")}
									</Text>
								</Box>
								{selected_avatar_seed && (
									<Box
										style={clear_btn_style}
										onClick={(e) => {
											e.stopPropagation();
											set_selected_avatar_seed("");
										}}
									>
										<IconX size={12} color="white" />
									</Box>
								)}
							</Box>
						</Stack>

						<Stack gap="xs" align="center">
							<Text>{t("bio.default_avatar")}</Text>
							<Avatar src={Tools.create_avatar(client_id, null)} size={80} radius={0} />
						</Stack>
					</Group>
				</Stack>

				<input ref={ref_selfie_input} type="file" accept="image/*" onChange={on_selfie_upload} style={{ display: "none" }} />

				<Button onClick={on_submit_bio}>{t("bio.save")}</Button>
			</Stack>

			<Modal opened={avatar_modal_open} onClose={() => set_avatar_modal_open(false)} title={t("bio.choose_avatar_title")}>
				<Stack gap="md">
					<SimpleGrid cols={3} spacing="xs">
						{avatar_seeds.map((seed) => {
							const is_selected = seed === temp_seed;
							return (
								<Box
									key={seed}
									onClick={() => set_temp_seed(is_selected ? "" : seed)}
									style={{
										cursor: "pointer",
										borderRadius: 8,
										outline: is_selected ? "2px solid var(--mantine-color-blue-5)" : "2px solid transparent",
										padding: 2,
									}}
								>
									<Avatar src={Tools.create_avatar(client_id, seed)} size={80} radius="md" style={{ width: "100%" }} />
								</Box>
							);
						})}
					</SimpleGrid>
					<Group justify="space-between">
						<Tooltip label={t("bio.randomise_avatars")}>
							<Button variant="subtle" size="xs" leftSection={<IconRefresh size={14} strokeWidth={1.5} />} onClick={on_roll_again}>
								{t("bio.roll_again")}
							</Button>
						</Tooltip>
						<Group gap="xs">
							<Button variant="subtle" onClick={() => set_avatar_modal_open(false)}>
								{t("bio.cancel")}
							</Button>
							<Button onClick={on_avatar_select} disabled={!temp_seed}>
								{t("bio.select")}
							</Button>
						</Group>
					</Group>
				</Stack>
			</Modal>
		</CollapsiblePanel>
	);
};
