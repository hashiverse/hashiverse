import { ActionIcon, Button, Group, Stack, Text } from "@mantine/core";
import { IconX } from "@tabler/icons-react";
import type React from "react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import { Toast } from "../../tools/Toast.ts";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	on_settings_changed: () => void;
}

export const FollowedHashtagsSection: React.FC<Props> = ({ hashiverse, on_settings_changed }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();
	const [saved_hashtags, saved_hashtags_set] = useState<string[]>([]);
	const [hashtags, hashtags_set] = useState<string[]>([]);

	useEffect(() => {
		hashiverse
			.get_followed_hashtags_v1()
			.then((h) => {
				saved_hashtags_set(h);
				hashtags_set(h);
			})
			.catch(console.error);
	}, [hashiverse.get_followed_hashtags_v1]);

	const apply = () => {
		hashiverse
			.set_followed_hashtags_v1(hashtags)
			.then(() => {
				saved_hashtags_set(hashtags);
				on_settings_changed();
				Toast.success(t("toast.followed_hashtags_saved"));
			})
			.catch((e) => {
				console.error(e);
				Toast.error(t("toast.error_generic"));
			});
	};

	const reset = () => hashtags_set(saved_hashtags);

	return (
		<CollapsiblePanel title={t("followed_hashtags.title")}>
			<Stack>
				{hashtags.length === 0 ? (
					<Text>{t("followed_hashtags.empty")}</Text>
				) : (
					hashtags.map((tag) => (
						<Group key={tag} justify="space-between" align="center">
							<button
								type="button"
								className="plugin-nowrap plugin-hashtag-clickable"
								onClick={() => navigate(`/hashtag/${tag}`)}
								style={{ background: "none", border: "none", padding: 0, cursor: "pointer", font: "inherit", color: "inherit" }}
							>
								<span className="plugin-hashtag-left">#</span>
								<span className="plugin-hashtag-right">{tag}</span>
							</button>
							<ActionIcon variant="subtle" color="red" onClick={() => hashtags_set((h) => h.filter((t) => t !== tag))}>
								<IconX size={14} />
							</ActionIcon>
						</Group>
					))
				)}
				<Group gap="xs">
					<Button onClick={apply}>{t("followed_hashtags.apply")}</Button>
					<Button variant="default" onClick={reset}>
						{t("followed_hashtags.reset")}
					</Button>
				</Group>
			</Stack>
		</CollapsiblePanel>
	);
};
