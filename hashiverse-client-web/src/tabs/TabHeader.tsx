import { Box, Tabs } from "@mantine/core";
import { IconHash, IconMoodHeart, IconSend, IconSettings, IconUserCircle } from "@tabler/icons-react";
import type React from "react";
import { useTranslation } from "react-i18next";
import { useLocation, useNavigate } from "react-router";
import { open_compose } from "./compose/ComposeDialogStore.ts";

export const TabHeader: React.FC = () => {
	const navigate = useNavigate();
	const location = useLocation();
	const { t } = useTranslation();

	const on_change = (value: string | null) => {
		if (value === "/compose") {
			open_compose();
		} else {
			navigate(value || "");
		}
	};

	return (
		<Tabs value={location.pathname} onChange={on_change}>
			<Tabs.List>
				<Tabs.Tab p="10px" leftSection={<IconSend size={24} color="orange" />} value="/compose">
					<Box visibleFrom="sm">{t("nav.compose")}</Box>
				</Tabs.Tab>
				<Tabs.Tab p="10px" leftSection={<IconUserCircle size={16} />} value="/me">
					{t("nav.me")}
				</Tabs.Tab>
				<Tabs.Tab p="10px" leftSection={<IconMoodHeart size={16} />} value="/people">
					{t("nav.people")}
				</Tabs.Tab>
				<Tabs.Tab p="10px" leftSection={<IconHash size={16} />} value="/hashtags">
					{t("nav.hashtags")}
				</Tabs.Tab>
				<Tabs.Tab p="10px" leftSection={<IconSettings size={16} />} value="/config" ml="auto"></Tabs.Tab>
			</Tabs.List>
		</Tabs>
	);
};
