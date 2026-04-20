import type React from "react";
import { useTranslation } from "react-i18next";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { CollapsiblePanel } from "../../tools/CollapsiblePanel.tsx";
import { StoredAccountsList } from "../../tools/StoredAccountsList.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	on_login: (hashiverse: HashiverseClientWasmProxy) => void;
}

export const StoredAccountsSection: React.FC<Props> = ({ hashiverse, on_login }) => {
	const { t } = useTranslation();

	return (
		<CollapsiblePanel title={t("login.stored_accounts_title")} defaultOpen={true}>
			<StoredAccountsList hashiverse={hashiverse} on_login={on_login} />
		</CollapsiblePanel>
	);
};
