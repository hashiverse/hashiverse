import { Stack } from "@mantine/core";
import type React from "react";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { TabHeader } from "../TabHeader.tsx";
import { AcknowledgementsSection } from "./AcknowledgementsSection.tsx";
import { BioSection } from "./BioSection.tsx";
import { BootstrapSection } from "./BootstrapSection.tsx";
import { ContentThresholdSection } from "./ContentThresholdSection.tsx";
import { FollowedHashtagsSection } from "./FollowedHashtagsSection.tsx";
import { FollowedUsersSection } from "./FollowedUsersSection.tsx";
import { LanguageSection } from "./LanguageSection.tsx";
import { LoginManagementSection } from "./LoginManagementSection.tsx";
import { LoginSection } from "./LoginSection.tsx";
import { LogoutSection } from "./LogoutSection.tsx";
import { StorageSection } from "./StorageSection.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	on_login: (hashiverse: HashiverseClientWasmProxy) => void;
	on_logout: () => void;
	user_settings_cache: UserSettingsCache;
}

export const ConfigTab: React.FC<Props> = ({ hashiverse, on_login, on_logout, user_settings_cache }) => {
	const logged_in = user_settings_cache.is_logged_in;

	return (
		<div className="FullColumnChildAndParent FullColumnChildScrollable">
			<TabHeader />
			<Stack gap={0}>
				<LoginSection user_settings_cache={user_settings_cache} />
				<LogoutSection on_logout={on_logout} user_settings_cache={user_settings_cache} />
				<LoginManagementSection hashiverse={hashiverse} on_login={on_login} />
				<LanguageSection />
				{logged_in && <BioSection hashiverse={hashiverse} user_settings_cache={user_settings_cache} />}
				{logged_in && <FollowedUsersSection hashiverse={hashiverse} user_settings_cache={user_settings_cache} />}
				{logged_in && <FollowedHashtagsSection hashiverse={hashiverse} on_settings_changed={user_settings_cache.invalidate} />}
				{logged_in && <ContentThresholdSection hashiverse={hashiverse} user_settings_cache={user_settings_cache} />}
				<BootstrapSection />
				<StorageSection hashiverse={hashiverse} />
				<AcknowledgementsSection />
			</Stack>
		</div>
	);
};
