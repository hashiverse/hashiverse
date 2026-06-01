import { ActionIcon, Button, Group, Stack, Text } from "@mantine/core";
import { IconX } from "@tabler/icons-react";
import type React from "react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { useCachedBio } from "../../tools/BioCache.ts";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import { Toast } from "../../tools/Toast.ts";
import { UserImageControl } from "../../tools/UserImageControl.tsx";
import { UserNameControl } from "../../tools/UserNameControl.tsx";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";

interface FollowedUserRowProps {
	id: string;
	hashiverse: HashiverseClientWasmProxy;
	on_remove: (id: string) => void;
}

const FollowedUserRow: React.FC<FollowedUserRowProps> = ({ id, hashiverse, on_remove }) => {
	const bio = useCachedBio(hashiverse, id);
	return (
		<Group justify="space-between" align="center">
			<Group gap="xs">
				<UserImageControl client_id={id} selfie={bio?.selfie} avatar={bio?.avatar} size="sm" />
				<UserNameControl client_id={id} nickname={bio?.nickname} size="sm" />
			</Group>
			<ActionIcon variant="subtle" color="red" onClick={() => on_remove(id)}>
				<IconX size={14} />
			</ActionIcon>
		</Group>
	);
};

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	user_settings_cache: UserSettingsCache;
}

export const FollowedUsersSection: React.FC<Props> = ({ hashiverse, user_settings_cache }) => {
	const { t } = useTranslation();
	const initial_ids = [...user_settings_cache.followed_client_ids];
	const [saved_ids, saved_ids_set] = useState<string[]>(initial_ids);
	const [followed_ids, followed_ids_set] = useState<string[]>(initial_ids);

	// Re-seed local state when the cache updates
	useEffect(() => {
		const ids = [...user_settings_cache.followed_client_ids];
		saved_ids_set(ids);
		followed_ids_set(ids);
	}, [user_settings_cache.followed_client_ids]);

	const apply = () => {
		hashiverse
			.set_followed_client_ids_v1(followed_ids)
			.then(() => {
				saved_ids_set(followed_ids);
				user_settings_cache.invalidate();
				Toast.success(t("toast.followed_users_saved"));
			})
			.catch((e) => {
				console.error(e);
				Toast.error(t("toast.error_generic"));
			});
	};

	const reset = () => followed_ids_set(saved_ids);

	const on_remove = (id: string) => followed_ids_set((ids) => ids.filter((i) => i !== id));

	return (
		<CollapsiblePanel title={t("followed.title")}>
			<Stack>
				{followed_ids.length === 0 ? <Text>{t("followed.empty")}</Text> : followed_ids.map((id) => <FollowedUserRow key={id} id={id} hashiverse={hashiverse} on_remove={on_remove} />)}
				<Group gap="xs">
					<Button onClick={apply}>{t("followed.apply")}</Button>
					<Button variant="default" onClick={reset}>
						{t("followed.reset")}
					</Button>
				</Group>
			</Stack>
		</CollapsiblePanel>
	);
};
