import type React from "react";
import { useEffect } from "react";
import { useNavigate } from "react-router";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { TabHeader } from "../TabHeader.tsx";

interface Props {
	user_settings_cache: UserSettingsCache;
}

export const MeTimelineTab: React.FC<Props> = ({ user_settings_cache }) => {
	const navigate = useNavigate();

	useEffect(() => {
		if (user_settings_cache.own_client_id) {
			navigate(`/user/${user_settings_cache.own_client_id}`, { replace: true });
		}
	}, [user_settings_cache.own_client_id, navigate]);

	// We return this so there is no flicker of the toolbar
	return (
		<div className="FullColumnChildAndParent">
			<TabHeader />
		</div>
	);
};
