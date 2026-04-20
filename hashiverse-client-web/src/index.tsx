/**
 * Main entry point for the hashiverse React SPA.
 *
 * @module
 */

import "./index.css";
import "@mantine/core/styles.css";
import { ActionIcon, createTheme, MantineProvider, Tooltip } from "@mantine/core";
import React, { useCallback, useState } from "react";
import { createRoot } from "react-dom/client";
import { HashRouter, Route, Routes } from "react-router";
import type { HashiverseClientWasmProxy } from "./Hashiverse.ts";
import { Hashiverse } from "./Hashiverse.ts";
import i18n from "./i18n/i18n.ts";
import { PrereleaseBanner } from "./PrereleaseBanner.tsx";
import { register_pwa_service_worker } from "./register_pwa_service_worker.ts";
import { ComposeDialog } from "./tabs/compose/ComposeDialog.tsx";
import { ComposeTab } from "./tabs/compose/ComposeTab.tsx";
import { ConfigTab } from "./tabs/config/ConfigTab.tsx";
import { ErrorTab } from "./tabs/ErrorTab.tsx";
import { HashtagsGuardTab } from "./tabs/hashtags/HashtagsGuardTab.tsx";
import { HomeTab } from "./tabs/home/HomeTab.tsx";
import { LoginTab } from "./tabs/login/LoginTab.tsx";
import { PeopleGuardTab } from "./tabs/people/PeopleGuardTab.tsx";
import { HashtagTimelineTab } from "./tabs/timeline/HashtagTimelineTab.tsx";
import { MeMentionedTimelineTab } from "./tabs/timeline/MeMentionedTimelineTab.tsx";
import { MeTimelineTab } from "./tabs/timeline/MeTimelineTab.tsx";
import { PostEmbedTab } from "./tabs/timeline/PostEmbedTab.tsx";
import { PostSequelTimelineTab } from "./tabs/timeline/PostSequelTimelineTab.tsx";
import { PostTimelineTab } from "./tabs/timeline/PostTimelineTab.tsx";
import { UserMentionedTimelineTab } from "./tabs/timeline/UserMentionedTimelineTab.tsx";
import { UserTimelineTab } from "./tabs/timeline/UserTimelineTab.tsx";
import { local_settings_delete, local_settings_get, local_settings_set } from "./tools/LocalSettings.ts";
import { register_bio } from "./tools/MentionStore.ts";
import { NeedsLoggedIn } from "./tools/NeedsLoggedIn.tsx";
import { ShareTargetHandler } from "./tools/ShareTargetHandler.tsx";

const theme = createTheme({
	components: {
		ActionIcon: ActionIcon.extend({
			defaultProps: {
				color: "gray",
				variant: "subtle",
			},
		}),
		Tooltip: Tooltip.extend({
			defaultProps: {
				openDelay: 500,
			},
		}),
	},
});

interface AppProps {
	initial_hashiverse: HashiverseClientWasmProxy;
}

import { LOCAL_SETTINGS_KEY_LANGUAGE, LOCAL_SETTINGS_KEY_LAST_LOGIN_KEY } from "./tools/LocalSettings.ts";
import { useUserSettingsCache } from "./tools/UserSettingsCache.ts";

const App: React.FC<AppProps> = ({ initial_hashiverse }) => {
	const [hashiverse, set_hashiverse] = useState<HashiverseClientWasmProxy>(initial_hashiverse);
	const user_settings_cache = useUserSettingsCache(hashiverse);

	const on_login = useCallback((new_hv: HashiverseClientWasmProxy) => {
		new_hv
			.get_client_id()
			.then((id) => local_settings_set(LOCAL_SETTINGS_KEY_LAST_LOGIN_KEY, id as string))
			.catch(() => {});
		set_hashiverse((prev) => {
			prev.dispose().catch(() => {});
			return new_hv;
		});
	}, []);

	const on_logout = useCallback(() => {
		user_settings_cache.reset();
		local_settings_delete(LOCAL_SETTINGS_KEY_LAST_LOGIN_KEY).catch(() => {});
		Hashiverse.create_from_keyphrase("").then((new_hv) => {
			set_hashiverse((prev) => {
				prev.dispose().catch(() => {});
				return new_hv;
			});
		});
	}, [user_settings_cache]);

	return (
		<div className="App FullColumnChildAndParent">
			<HashRouter>
				<ComposeDialog hashiverse={hashiverse} user_settings_cache={user_settings_cache} />
				<ShareTargetHandler />
				<Routes>
					<Route path="/" element={<HomeTab hashiverse={hashiverse} />} />
					<Route
						path="/compose"
						element={
							<NeedsLoggedIn user_settings_cache={user_settings_cache}>
								<ComposeTab hashiverse={hashiverse} />
							</NeedsLoggedIn>
						}
					/>
					<Route path="/people" element={<PeopleGuardTab hashiverse={hashiverse} user_settings_cache={user_settings_cache} />} />
					<Route path="/hashtags" element={<HashtagsGuardTab hashiverse={hashiverse} user_settings_cache={user_settings_cache} />} />
					<Route
						path="/me"
						element={
							<NeedsLoggedIn user_settings_cache={user_settings_cache}>
								<MeTimelineTab user_settings_cache={user_settings_cache} />
							</NeedsLoggedIn>
						}
					/>
					<Route
						path="/me_mentioned"
						element={
							<NeedsLoggedIn user_settings_cache={user_settings_cache}>
								<MeMentionedTimelineTab user_settings_cache={user_settings_cache} />
							</NeedsLoggedIn>
						}
					/>
					<Route path="/user/:client_id_hex" element={<UserTimelineTab hashiverse={hashiverse} user_settings_cache={user_settings_cache} />} />
					<Route path="/user_mentioned/:client_id_hex" element={<UserMentionedTimelineTab hashiverse={hashiverse} user_settings_cache={user_settings_cache} />} />
					<Route path="/hashtag/:hashtag" element={<HashtagTimelineTab hashiverse={hashiverse} user_settings_cache={user_settings_cache} />} />
					<Route path="/post/:post_id/:bucket_location" element={<PostTimelineTab hashiverse={hashiverse} user_settings_cache={user_settings_cache} />} />
					<Route path="/post_sequels/:post_id/:bucket_location" element={<PostSequelTimelineTab hashiverse={hashiverse} user_settings_cache={user_settings_cache} />} />
					<Route path="/post_embed/:post_id/:bucket_location" element={<PostEmbedTab hashiverse={hashiverse} user_settings_cache={user_settings_cache} />} />
					<Route path="/login" element={<LoginTab hashiverse={hashiverse} on_login={on_login} on_logout={on_logout} user_settings_cache={user_settings_cache} />} />
					<Route path="/config" element={<ConfigTab hashiverse={hashiverse} on_login={on_login} on_logout={on_logout} user_settings_cache={user_settings_cache} />} />
					<Route path="*" element={<ErrorTab />} />
				</Routes>
			</HashRouter>
		</div>
	);
};

const stored_language = await local_settings_get(LOCAL_SETTINGS_KEY_LANGUAGE);
if (stored_language && stored_language !== i18n.language) {
	await i18n.changeLanguage(stored_language);
}

console.log(
	"%c H %c A %c S %c H %c I %c V %c E %c R %c S %c E %c https://github.com/hashiverse/hashiverse",
	"background:#e03131;color:#fff;font-weight:bold;font-size:16px;padding:2px 0",
	"background:#f76707;color:#fff;font-weight:bold;font-size:16px;padding:2px 0",
	"background:#f59f00;color:#fff;font-weight:bold;font-size:16px;padding:2px 0",
	"background:#2f9e44;color:#fff;font-weight:bold;font-size:16px;padding:2px 0",
	"background:#1971c2;color:#fff;font-weight:bold;font-size:16px;padding:2px 0",
	"background:#364fc7;color:#fff;font-weight:bold;font-size:16px;padding:2px 0",
	"background:#7048e8;color:#fff;font-weight:bold;font-size:16px;padding:2px 0",
	"background:#9c36b5;color:#fff;font-weight:bold;font-size:16px;padding:2px 0",
	"background:#e03131;color:#fff;font-weight:bold;font-size:16px;padding:2px 0",
	"background:#f76707;color:#fff;font-weight:bold;font-size:16px;padding:2px 0",
	"background:#ffffff;color:#004;font-weight:bold;font-size:12px;padding:2px 0",
);
console.log("Creating Hashiverse");
const saved_key = await local_settings_get(LOCAL_SETTINGS_KEY_LAST_LOGIN_KEY);
let hashiverse: HashiverseClientWasmProxy;
if (saved_key) {
	try {
		hashiverse = await Hashiverse.create_from_stored_key(saved_key);
		console.log("Auto-logged in from saved key");
	} catch {
		local_settings_delete(LOCAL_SETTINGS_KEY_LAST_LOGIN_KEY).catch(() => {});
		hashiverse = await Hashiverse.create_from_keyphrase("");
		console.log("Auto-login failed, starting anonymous");
	}
} else {
	hashiverse = await Hashiverse.create_from_keyphrase("");
	console.log("Starting as guest");
}

hashiverse
	.get_all_bios()
	.then((bios) => {
		for (const bio of bios) {
			register_bio(bio.client_id, bio);
		}
	})
	.catch(() => {});

const root = document.getElementById("root");
if (!root) throw new Error("Missing #root element");

createRoot(root).render(
	<React.StrictMode>
		<MantineProvider defaultColorScheme="dark" theme={theme}>
			<PrereleaseBanner />
			<App initial_hashiverse={hashiverse} />
		</MantineProvider>
	</React.StrictMode>,
);

register_pwa_service_worker();
