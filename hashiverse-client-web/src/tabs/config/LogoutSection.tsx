import { Button, Stack, Text } from "@mantine/core";
import type React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";

interface Props {
	on_logout: () => void;
	user_settings_cache: UserSettingsCache;
}

export const LogoutSection: React.FC<Props> = ({ on_logout, user_settings_cache }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();

	if (!user_settings_cache.is_logged_in) return null;

	const handle_logout = () => {
		on_logout();
		navigate("/login");
	};

	return (
		<CollapsiblePanel title={t("logout.title")} defaultOpen={true}>
			<Stack>
				<Text>{t("logout.description")}</Text>
				<Button color="red" variant="light" onClick={handle_logout}>
					{t("logout.button")}
				</Button>
			</Stack>
		</CollapsiblePanel>
	);
};
