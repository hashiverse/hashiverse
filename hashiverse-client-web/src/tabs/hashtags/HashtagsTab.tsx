import { Anchor, Text } from "@mantine/core";
import type React from "react";
import { useMemo } from "react";
import { Trans, useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import banner_followed_hashtags from "../../media/banner_followed_hashtags.svg";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { TabHeader } from "../TabHeader.tsx";
import { Banner } from "../timeline/Banner.tsx";
import { TimelineControl } from "../timeline/TimelineControl.tsx";
import { TrendingHashtagsGrid } from "./TrendingHashtagsGrid.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	user_settings_cache: UserSettingsCache;
}

export const HashtagsTab: React.FC<Props> = ({ hashiverse, user_settings_cache }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();

	const header = useMemo(
		() => (
			<>
				<TrendingHashtagsGrid hashiverse={hashiverse} />
				<Banner
					image={<img src={banner_followed_hashtags} alt="hashtags" />}
					heading={
						<Text size="xl" fw={700}>
							{t("banner.followed_hashtags")}
						</Text>
					}
					detail={
						<Text>
							<Trans
								i18nKey="banner.hashtags_detail"
								components={{
									settings: <Anchor onClick={() => navigate("/config")} style={{ cursor: "pointer" }} />,
								}}
							/>
						</Text>
					}
				/>
			</>
		),
		[hashiverse, t, navigate],
	);

	return (
		<div className="FullColumnChildAndParent">
			<TabHeader />
			<TimelineControl
				hashiverse={hashiverse}
				user_settings_cache={user_settings_cache}
				timeline_key="followed_hashtags"
				get_more={() => hashiverse.multiple_timeline_get_more_followed_hashtags()}
				reset={() => hashiverse.multiple_timeline_reset()}
				header={header}
			/>
		</div>
	);
};
