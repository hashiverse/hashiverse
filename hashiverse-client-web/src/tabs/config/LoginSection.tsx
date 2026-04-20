import { Button, Stack, Text } from "@mantine/core";
import type React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";

interface Props {
	user_settings_cache: UserSettingsCache;
}

export const LoginSection: React.FC<Props> = ({ user_settings_cache }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();

	if (user_settings_cache.is_logged_in) return null;

	return (
		<CollapsiblePanel title={t("not_logged_in.log_in")} defaultOpen={true}>
			<Stack>
				<Text>{t("not_logged_in.message")}</Text>
				<Button color="blue" variant="light" onClick={() => navigate("/login")}>
					{t("not_logged_in.log_in")}
				</Button>
			</Stack>
		</CollapsiblePanel>
	);
};
