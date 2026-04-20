import { Button, Stack, Text } from "@mantine/core";
import type React from "react";
import { useTranslation } from "react-i18next";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import banner_login from "../../media/banner_login.svg";
import { DomainSwitcherBanner } from "../../tools/DomainSwitcherBanner.tsx";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { TabHeader } from "../TabHeader.tsx";
import { Banner } from "../timeline/Banner.tsx";
import { KeyphraseSection } from "./KeyphraseSection.tsx";
import { PasskeySection } from "./PasskeySection.tsx";
import { QuestionsSection } from "./QuestionsSection.tsx";
import { StoredAccountsSection } from "./StoredAccountsSection.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	on_login: (hashiverse: HashiverseClientWasmProxy) => void;
	on_logout: () => void;
	user_settings_cache: UserSettingsCache;
}

export const LoginTab: React.FC<Props> = ({ hashiverse, on_login, on_logout, user_settings_cache }) => {
	const { t } = useTranslation();
	const logged_in = user_settings_cache.is_logged_in;

	return (
		<div className="FullColumnChildAndParent">
			<TabHeader />
			<div className="FullColumnChildScrollable">
				<Banner
					image={<img src={banner_login} alt="login" />}
					heading={
						<Text size="xl" fw={700}>
							{t("login.banner_heading")}
						</Text>
					}
					detail={<Text>{t("login.banner_detail")}</Text>}
				/>

				{!logged_in && <DomainSwitcherBanner hash_path="#/login" />}

				{logged_in ? (
					<Stack p="md" gap="md">
						<Text>{t("login.already_logged_in")}</Text>
						<Button variant="default" onClick={on_logout}>
							{t("login.logout")}
						</Button>
					</Stack>
				) : (
					<Stack gap={0}>
						<StoredAccountsSection hashiverse={hashiverse} on_login={on_login} />
						<PasskeySection on_login={on_login} />
						<KeyphraseSection on_login={on_login} />
						<QuestionsSection on_login={on_login} />
					</Stack>
				)}
			</div>
		</div>
	);
};
