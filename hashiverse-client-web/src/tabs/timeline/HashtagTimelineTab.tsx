import { Button, Group, Text } from "@mantine/core";
import type React from "react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "react-router";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import banner_hashtag from "../../media/banner_hashtag.svg";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { ErrorTab } from "../ErrorTab.tsx";
import { TabHeader } from "../TabHeader.tsx";
import { Banner } from "./Banner.tsx";
import { TimelineControl } from "./TimelineControl.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	user_settings_cache: UserSettingsCache;
}

export const HashtagTimelineTab: React.FC<Props> = (props) => {
	const { hashtag } = useParams();
	if (!hashtag) return <ErrorTab />;
	return <HashtagTimelineTabInner {...props} hashtag={hashtag} />;
};

const HashtagTimelineTabInner: React.FC<Props & { hashtag: string }> = ({ hashiverse, user_settings_cache, hashtag }) => {
	const { t } = useTranslation();
	const [is_followed, is_followed_set] = useState(false);

	useEffect(() => {
		hashiverse
			.get_followed_hashtags_v1()
			.then((tags) => is_followed_set(tags.includes(hashtag)))
			.catch(console.error);
	}, [hashtag, hashiverse.get_followed_hashtags_v1]);

	const toggle_follow = () => {
		const new_state = !is_followed;
		is_followed_set(new_state);
		hashiverse.set_followed_hashtag_v1(hashtag, new_state).catch((e) => {
			is_followed_set(!new_state);
			console.error(e);
		});
	};

	const header = (
		<Banner
			image={<img src={banner_hashtag} alt="hashtag" />}
			heading={
				<Text size="xl" fw={700}>
					#{hashtag}
				</Text>
			}
			detail={
				<Group gap="xs">
					<Text>{t("banner.hashtag.disclaimer")}</Text>
					{user_settings_cache.is_logged_in && (
						<Button variant={is_followed ? "filled" : "outline"} onClick={toggle_follow}>
							{is_followed ? t("user.unfollow") : t("user.follow")}
						</Button>
					)}
				</Group>
			}
		/>
	);

	return (
		<div className="FullColumnChildAndParent">
			<TabHeader />
			<TimelineControl
				hashiverse={hashiverse}
				user_settings_cache={user_settings_cache}
				timeline_key={`hashtag:${hashtag}`}
				get_more={() => hashiverse.single_timeline_get_more_hashtag_v1(hashtag)}
				reset={() => hashiverse.single_timeline_reset()}
				header={header}
				blur_images
			/>
		</div>
	);
};
