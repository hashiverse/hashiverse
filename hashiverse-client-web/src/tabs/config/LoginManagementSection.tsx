import { Text } from "@mantine/core";
import type React from "react";
import { useTranslation } from "react-i18next";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import { StoredAccountsList } from "../../tools/StoredAccountsList.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	on_login: (hashiverse: HashiverseClientWasmProxy) => void;
}

export const LoginManagementSection: React.FC<Props> = ({ hashiverse, on_login }) => {
	const { t } = useTranslation();

	return (
		<CollapsiblePanel title={t("login_management.title")}>
			<StoredAccountsList hashiverse={hashiverse} on_login={on_login} intro={<Text>{t("login_management.stored_keys_intro")}</Text>} />
		</CollapsiblePanel>
	);
};
