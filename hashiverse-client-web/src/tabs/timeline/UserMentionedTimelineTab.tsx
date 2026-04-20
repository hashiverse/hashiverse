import { Button, Group, Text } from "@mantine/core";
import type React from "react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "react-router";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import banner_me_mentioned from "../../media/banner_me_mentioned.svg";
import { useCachedBio } from "../../tools/BioCache.ts";
import { UserImageControl } from "../../tools/UserImageControl.tsx";
import { UserNameControl } from "../../tools/UserNameControl.tsx";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { ErrorTab } from "../ErrorTab.tsx";
import { TabHeader } from "../TabHeader.tsx";
import { Banner } from "./Banner.tsx";
import { TimelineControl } from "./TimelineControl.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	user_settings_cache: UserSettingsCache;
}

export const UserMentionedTimelineTab: React.FC<Props> = (props) => {
	const { client_id_hex } = useParams();
	if (!client_id_hex) return <ErrorTab />;
	return <UserMentionedTimelineTabInner {...props} client_id_hex={client_id_hex} />;
};

const UserMentionedTimelineTabInner: React.FC<Props & { client_id_hex: string }> = ({ hashiverse, user_settings_cache, client_id_hex }) => {
	const navigate = useNavigate();
	const { t } = useTranslation();
	const bio = useCachedBio(hashiverse, client_id_hex);
	const [is_me, is_me_set] = useState<boolean | null>(null);
	const [is_followed, is_followed_set] = useState(false);

	useEffect(() => {
		const viewing_self = user_settings_cache.own_client_id === client_id_hex;
		is_me_set(viewing_self);
		is_followed_set(user_settings_cache.followed_client_ids.has(client_id_hex));
	}, [client_id_hex, user_settings_cache.own_client_id, user_settings_cache.followed_client_ids]);

	const toggle_follow = () => {
		const new_state = !is_followed;
		is_followed_set(new_state);
		hashiverse
			.set_followed_client_id_v1(client_id_hex, new_state)
			.then(() => user_settings_cache.invalidate())
			.catch((e) => {
				is_followed_set(!new_state);
				console.error(e);
			});
	};

	const header =
		is_me === null ? undefined : is_me ? (
			<Banner
				image={<img src={banner_me_mentioned} alt="me_mentioned" />}
				heading={
					<Text size="xl" fw={700}>
						{t("banner.me_mentioned")}
					</Text>
				}
				detail={<Button onClick={() => navigate(`/user/${client_id_hex}`)}>{t("banner.me")}</Button>}
			/>
		) : (
			<Banner
				image={<UserImageControl client_id={client_id_hex} selfie={bio?.selfie} avatar={bio?.avatar} radius="100%" style={{ width: "100%", height: "auto", aspectRatio: "1" }} />}
				heading={<UserNameControl size="xl" fw={700} client_id={client_id_hex} nickname={bio?.nickname} />}
				detail={
					<>
						<Text>{bio?.status}</Text>
						<Group gap="xs">
							<Button onClick={() => navigate(`/user/${client_id_hex}`)}>{t("banner.user")}</Button>
							{user_settings_cache.is_logged_in && (
								<Button variant={is_followed ? "filled" : "outline"} onClick={toggle_follow}>
									{is_followed ? t("user.unfollow") : t("user.follow")}
								</Button>
							)}
						</Group>
					</>
				}
			/>
		);

	return (
		<div className="FullColumnChildAndParent">
			<TabHeader />
			<TimelineControl
				hashiverse={hashiverse}
				user_settings_cache={user_settings_cache}
				timeline_key={`user_mentioned:${client_id_hex}`}
				get_more={() => hashiverse.single_timeline_get_more_user_mentioned_v1(client_id_hex)}
				reset={() => hashiverse.single_timeline_reset()}
				header={header}
				blur_images
			/>
		</div>
	);
};
