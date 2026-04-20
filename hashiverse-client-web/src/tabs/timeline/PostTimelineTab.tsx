import { Text } from "@mantine/core";
import type React from "react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "react-router";
import type { HashiverseClientWasmProxy, Post } from "../../Hashiverse.ts";
import banner_post from "../../media/banner_post.svg";
import { Spinner } from "../../tools/Spinner.tsx";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { ErrorTab } from "../ErrorTab.tsx";
import { TabHeader } from "../TabHeader.tsx";
import { Banner } from "./Banner.tsx";
import { PostPanel } from "./PostPanel.tsx";
import { TimelineControl } from "./TimelineControl.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	user_settings_cache: UserSettingsCache;
}

export const PostTimelineTab: React.FC<Props> = (props) => {
	const { post_id, bucket_location } = useParams();
	if (!post_id || !bucket_location) return <ErrorTab />;
	return <PostTimelineTabInner {...props} post_id={post_id} bucket_location={bucket_location} />;
};

const PostTimelineTabInner: React.FC<Props & { post_id: string; bucket_location: string }> = ({ hashiverse, user_settings_cache, post_id, bucket_location }) => {
	const { t } = useTranslation();
	const [post, set_post] = useState<Post | null>(null);

	useEffect(() => {
		hashiverse.get_post_v1(bucket_location, post_id).then(set_post).catch(console.error);
	}, [post_id, bucket_location, hashiverse.get_post_v1]);

	const header = (
		<>
			<Banner
				image={<img src={banner_post} alt="post" />}
				heading={
					<Text size="xl" fw={700}>
						{t("banner.replies_to_post")}
					</Text>
				}
				detail={<Text>{t("banner.replies_detail")}</Text>}
			/>
			{post ? (
				<PostPanel hashiverse={hashiverse} post={post} user_settings_cache={user_settings_cache} />
			) : (
				<div style={{ display: "flex", justifyContent: "center", padding: 16 }}>
					<Spinner />
				</div>
			)}
		</>
	);

	return (
		<div className="FullColumnChildAndParent">
			<TabHeader />
			<TimelineControl
				hashiverse={hashiverse}
				user_settings_cache={user_settings_cache}
				timeline_key={`replies:${post_id}`}
				get_more={() => hashiverse.single_timeline_get_more_reply_to_post_v1(post_id)}
				reset={() => hashiverse.single_timeline_reset()}
				header={header}
			/>
		</div>
	);
};
