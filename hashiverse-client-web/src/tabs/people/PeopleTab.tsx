import { Anchor, Text } from "@mantine/core";
import type React from "react";
import { Trans, useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import banner_followed_users from "../../media/banner_followed_users.svg";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { TabHeader } from "../TabHeader.tsx";
import { Banner } from "../timeline/Banner.tsx";
import { TimelineControl } from "../timeline/TimelineControl.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	user_settings_cache: UserSettingsCache;
}

export const PeopleTab: React.FC<Props> = ({ hashiverse, user_settings_cache }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();

	const header = (
		<Banner
			image={<img src={banner_followed_users} alt="followed users" />}
			heading={
				<Text size="xl" fw={700}>
					{t("banner.followed_users")}
				</Text>
			}
			detail={
				<Text>
					<Trans
						i18nKey="banner.people_detail"
						components={{
							settings: <Anchor onClick={() => navigate("/config")} style={{ cursor: "pointer" }} />,
						}}
					/>
				</Text>
			}
		/>
	);

	return (
		<div className="FullColumnChildAndParent">
			<TabHeader />
			<TimelineControl
				hashiverse={hashiverse}
				user_settings_cache={user_settings_cache}
				timeline_key="followed_users"
				get_more={() => hashiverse.multiple_timeline_get_more_followed_users()}
				reset={() => hashiverse.multiple_timeline_reset()}
				header={header}
			/>
		</div>
	);
};
