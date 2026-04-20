import type React from "react";
import { useTranslation } from "react-i18next";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { NotLoggedInPanel } from "../login/NotLoggedInPanel.tsx";
import { TabHeader } from "../TabHeader.tsx";
import { PeopleTab } from "./PeopleTab.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	user_settings_cache: UserSettingsCache;
}

export const PeopleGuardTab: React.FC<Props> = ({ hashiverse, user_settings_cache }) => {
	const { t } = useTranslation();

	if (user_settings_cache.is_logged_in) {
		return <PeopleTab hashiverse={hashiverse} user_settings_cache={user_settings_cache} />;
	}

	return (
		<div className="FullColumnChildAndParent">
			<TabHeader />
			<NotLoggedInPanel message={t("banner.people_not_logged_in")} />
		</div>
	);
};
