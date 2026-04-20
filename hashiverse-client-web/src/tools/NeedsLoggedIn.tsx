import type React from "react";
import { NotLoggedInPanel } from "../tabs/login/NotLoggedInPanel.tsx";
import { TabHeader } from "../tabs/TabHeader.tsx";
import type { UserSettingsCache } from "./UserSettingsCache.ts";

type NeedsLoggedInProps = {
	user_settings_cache: UserSettingsCache;
	children: React.ReactNode;
};

export function NeedsLoggedIn({ user_settings_cache, children }: NeedsLoggedInProps) {
	if (!user_settings_cache.is_logged_in)
		return (
			<div className="FullColumnChildAndParent">
				<TabHeader />
				<NotLoggedInPanel />
			</div>
		);
	return <>{children}</>;
}
