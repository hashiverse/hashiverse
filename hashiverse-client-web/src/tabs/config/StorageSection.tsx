import { Button, Stack, Text } from "@mantine/core";
import type React from "react";
import { useTranslation } from "react-i18next";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import { local_settings_reset } from "../../tools/LocalSettings.ts";
import { Tools } from "../../tools/Tools.ts";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
}

export const StorageSection: React.FC<Props> = ({ hashiverse }) => {
	const { t } = useTranslation();

	const on_reset_click = async () => {
		await hashiverse.client_storage_reset();
	};

	const on_reset_local_settings_click = async () => {
		await local_settings_reset();
	};

	return (
		<CollapsiblePanel title={t("storage.title")} defaultOpen={!Tools.is_release_build()}>
			<Stack>
				<Text>{t("storage.description")}</Text>
				<Button onClick={on_reset_click}>{t("storage.reset")}</Button>
				<Button onClick={on_reset_local_settings_click}>{t("storage.reset_local_settings")}</Button>
			</Stack>
		</CollapsiblePanel>
	);
};
