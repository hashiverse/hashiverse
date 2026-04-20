import { Select } from "@mantine/core";
import type React from "react";
import { useTranslation } from "react-i18next";
import i18n, { SUPPORTED_LANGUAGES } from "../../i18n/i18n.ts";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import { LOCAL_SETTINGS_KEY_LANGUAGE, local_settings_set } from "../../tools/LocalSettings.ts";

export const LanguageSection: React.FC = () => {
	const { t } = useTranslation();

	const on_change = (value: string | null) => {
		if (!value) return;
		i18n.changeLanguage(value);
		local_settings_set(LOCAL_SETTINGS_KEY_LANGUAGE, value).catch(() => {});
	};

	return (
		<CollapsiblePanel title={t("language.title")}>
			<Select label={t("language.label")} data={SUPPORTED_LANGUAGES} value={i18n.language} onChange={on_change} allowDeselect={false} />
		</CollapsiblePanel>
	);
};
