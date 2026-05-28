import { Button, Radio, Stack, Text, TextInput } from "@mantine/core";
import type React from "react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import { LOCAL_SETTINGS_KEY_BOOTSTRAP, local_settings_delete, local_settings_get, local_settings_set } from "../../tools/LocalSettings.ts";
import { Toast } from "../../tools/Toast.ts";

type BootstrapMode = "production" | "local" | "manual";

function mode_from_value(value: string | null): BootstrapMode {
	if (!value || value === "production") return "production";
	if (value === "local") return "local";
	return "manual";
}

export const BootstrapSection: React.FC = () => {
	const { t } = useTranslation();
	const [mode, set_mode] = useState<BootstrapMode>("production");
	const [manual_addresses, set_manual_addresses] = useState("");

	useEffect(() => {
		local_settings_get(LOCAL_SETTINGS_KEY_BOOTSTRAP).then((value) => {
			set_mode(mode_from_value(value));
			if (value && value !== "production" && value !== "local") {
				set_manual_addresses(value);
			}
		});
	}, []);

	const handle_save = async () => {
		try {
			if (mode === "production") {
				await local_settings_delete(LOCAL_SETTINGS_KEY_BOOTSTRAP);
			} else if (mode === "local") {
				await local_settings_set(LOCAL_SETTINGS_KEY_BOOTSTRAP, "local");
			} else {
				await local_settings_set(LOCAL_SETTINGS_KEY_BOOTSTRAP, manual_addresses.trim());
			}
			Toast.success(t("toast.bootstrap_saved"));
		} catch (e) {
			console.error(e);
			Toast.error(t("toast.error_generic"));
		}
	};

	return (
		<CollapsiblePanel title={t("bootstrap.title")}>
			<Stack>
				<Text size="sm" c="dimmed">
					{t("bootstrap.description")}
				</Text>
				<Radio.Group value={mode} onChange={(value) => set_mode(value as BootstrapMode)}>
					<Stack gap="xs">
						<Radio value="production" label={t("bootstrap.production")} />
						<Radio value="local" label={t("bootstrap.local")} />
						<Radio value="manual" label={t("bootstrap.manual")} />
					</Stack>
				</Radio.Group>
				{mode === "manual" && <TextInput placeholder={t("bootstrap.manual_placeholder")} value={manual_addresses} onChange={(e) => set_manual_addresses(e.currentTarget.value)} />}
				<Button onClick={handle_save}>{t("bootstrap.save")}</Button>
			</Stack>
		</CollapsiblePanel>
	);
};
