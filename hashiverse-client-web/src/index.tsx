/**
 * Main entry point for the hashiverse React SPA.
 *
 * @module
 */

import "./index.css";
import "@mantine/core/styles.css";
import "@mantine/notifications/styles.css";
import { ActionIcon, createTheme, MantineProvider, Tooltip } from "@mantine/core";
import { Notifications } from "@mantine/notifications";
import React, { useCallback, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { HashRouter, Route, Routes, useLocation, useNavigate } from "react-router";
import type { HashiverseClientWasmProxy } from "./Hashiverse.ts";
import { Hashiverse } from "./Hashiverse.ts";
import i18n from "./i18n/i18n.ts";
import { PrereleaseBanner } from "./PrereleaseBanner.tsx";
import { register_pwa_service_worker } from "./register_pwa_service_worker.ts";
import { ComposeDialog } from "./tabs/compose/ComposeDialog.tsx";
import { ComposeTab } from "./tabs/compose/ComposeTab.tsx";
import { ConfigTab } from "./tabs/config/ConfigTab.tsx";
import { ErrorTab } from "./tabs/ErrorTab.tsx";
import { GeeksPeerStatsTab } from "./tabs/geeks/GeeksPeerStatsTab.tsx";
import { GeeksPeersTab } from "./tabs/geeks/GeeksPeersTab.tsx";
import { GeeksPowJobsTab } from "./tabs/geeks/GeeksPowJobsTab.tsx";
import { GeeksTab } from "./tabs/geeks/GeeksTab.tsx";
import { HashtagsGuardTab } from "./tabs/hashtags/HashtagsGuardTab.tsx";
import { HomeTab } from "./tabs/home/HomeTab.tsx";
import { HASHIVERSE_DOMAINS } from "./tabs/login/domains.ts";
import { LoginTab } from "./tabs/login/LoginTab.tsx";
import { PeopleGuardTab } from "./tabs/people/PeopleGuardTab.tsx";
import { ShareTab } from "./tabs/share/ShareTab.tsx";
import { HashtagTimelineTab } from "./tabs/timeline/HashtagTimelineTab.tsx";
import { MeMentionedTimelineTab } from "./tabs/timeline/MeMentionedTimelineTab.tsx";
import { MeTimelineTab } from "./tabs/timeline/MeTimelineTab.tsx";
import { PostEmbedTab } from "./tabs/timeline/PostEmbedTab.tsx";
import { PostSequelTimelineTab } from "./tabs/timeline/PostSequelTimelineTab.tsx";
import { PostTimelineTab } from "./tabs/timeline/PostTimelineTab.tsx";
import { UserMentionedTimelineTab } from "./tabs/timeline/UserMentionedTimelineTab.tsx";
import { UserTimelineTab } from "./tabs/timeline/UserTimelineTab.tsx";
import { LOCAL_SETTINGS_KEY_POST_LOGIN_RETURN, LOCAL_SETTINGS_KEY_PREFERRED_DOMAIN, local_settings_delete, local_settings_get, local_settings_set } from "./tools/LocalSettings.ts";
import { register_bio } from "./tools/MentionStore.ts";
import { NeedsLoggedIn } from "./tools/NeedsLoggedIn.tsx";
import type { UserSettingsCache } from "./tools/UserSettingsCache.ts";
import { useKeyboardInsetBottomCssVariable } from "./tools/useKeyboardInsetBottomCssVariable.ts";

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
	initial_settings: UserSettings;
}

import { LOCAL_SETTINGS_KEY_LANGUAGE, LOCAL_SETTINGS_KEY_LAST_LOGIN_KEY } from "./tools/LocalSettings.ts";
import { fetch_user_settings, type UserSettings, useUserSettingsCache } from "./tools/UserSettingsCache.ts";

interface PostLoginReturnerProps {
	user_settings_cache: UserSettingsCache;
}

// Watches for a logged-out → logged-in transition and, if a return URL was
// stashed by a route that bounced the user to /login, navigates back to it.
// Only fires on transitions, so an initial saved-key auto-login does not
// resurface stale pending shares from a previous session.
const PostLoginReturner: React.FC<PostLoginReturnerProps> = ({ user_settings_cache }) => {
	const navigate = useNavigate();
	const prev_logged_in_ref = React.useRef<boolean>(user_settings_cache.is_logged_in);

	useEffect(() => {
		const prev = prev_logged_in_ref.current;
		prev_logged_in_ref.current = user_settings_cache.is_logged_in;
		if (prev || !user_settings_cache.is_logged_in) return;

		local_settings_get(LOCAL_SETTINGS_KEY_POST_LOGIN_RETURN)
			.then((url) => {
				if (!url) return;
				local_settings_delete(LOCAL_SETTINGS_KEY_POST_LOGIN_RETURN).catch(console.error);
				navigate(url);
			})
			.catch(console.error);
	}, [user_settings_cache.is_logged_in, navigate]);

	return null;
};

// Reflects the current route in document.title so browser history, tab
// labels, and bookmarks distinguish between screens. Literal pathname
// suffix — no param resolution.
const RouteTitleUpdater: React.FC = () => {
	const location = useLocation();
	useEffect(() => {
		const suffix = location.pathname === "/" ? "Home" : location.pathname.slice(1);
		document.title = `Hashiverse - ${suffix}`;
	}, [location.pathname]);
	return null;
};

const App: React.FC<AppProps> = ({ initial_hashiverse, initial_settings }) => {
	useKeyboardInsetBottomCssVariable();
	const [hashiverse, set_hashiverse] = useState<HashiverseClientWasmProxy>(initial_hashiverse);
	const user_settings_cache = useUserSettingsCache(hashiverse, initial_settings);

	const on_login = useCallback(
		async (new_hv: HashiverseClientWasmProxy) => {
			const [_, new_settings] = await Promise.all([
				new_hv
					.get_client_id()
					.then((id) => local_settings_set(LOCAL_SETTINGS_KEY_LAST_LOGIN_KEY, id as string))
					.catch(() => {}),
				fetch_user_settings(new_hv),
			]);
			set_hashiverse((prev) => {
				prev.dispose().catch(() => {});
				return new_hv;
			});
			user_settings_cache.replace(new_settings);
		},
		[user_settings_cache],
	);

	const on_logout = useCallback(async () => {
		local_settings_delete(LOCAL_SETTINGS_KEY_LAST_LOGIN_KEY).catch(() => {});
		const new_hv = await Hashiverse.create_from_keyphrase("");
		const new_settings = await fetch_user_settings(new_hv);
		set_hashiverse((prev) => {
			prev.dispose().catch(() => {});
			return new_hv;
		});
		user_settings_cache.replace(new_settings);
	}, [user_settings_cache]);

	return (
		<div className="App FullColumnChildAndParent">
			<HashRouter>
				<RouteTitleUpdater />
				<ComposeDialog hashiverse={hashiverse} user_settings_cache={user_settings_cache} />
				<PostLoginReturner user_settings_cache={user_settings_cache} />
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
					<Route path="/share" element={<ShareTab user_settings_cache={user_settings_cache} />} />
					<Route path="/config" element={<ConfigTab hashiverse={hashiverse} on_login={on_login} on_logout={on_logout} user_settings_cache={user_settings_cache} />} />
					<Route path="/geeks" element={<GeeksTab />} />
					<Route path="/geeks/peers" element={<GeeksPeersTab hashiverse={hashiverse} />} />
					<Route path="/geeks/peer-stats/:peer_id_hex" element={<GeeksPeerStatsTab hashiverse={hashiverse} />} />
					<Route path="/geeks/pow-jobs" element={<GeeksPowJobsTab hashiverse={hashiverse} />} />
					<Route path="*" element={<ErrorTab />} />
				</Routes>
			</HashRouter>
		</div>
	);
};

// On a fresh page load, check for a "preferred domain" autoredirect rule.
//
// If we just got auto-redirected here (autoredirected=1 in the search), wipe
// any preferred-domain entry on this origin so we can't bounce the user back
// somewhere on a subsequent visit, and strip the marker from the URL.
//
// Otherwise, if the user has saved a preferred domain that isn't this one,
// substitute the hostname and redirect, keeping path/search/hash intact.
async function maybe_consume_or_auto_redirect(): Promise<boolean> {
	const search_params = new URLSearchParams(window.location.search);

	if (search_params.get("autoredirected") === "1") {
		await local_settings_delete(LOCAL_SETTINGS_KEY_PREFERRED_DOMAIN).catch(console.error);
		search_params.delete("autoredirected");
		const new_search = search_params.toString();
		const new_url = `${window.location.pathname}${new_search ? `?${new_search}` : ""}${window.location.hash}`;
		history.replaceState(null, "", new_url);
		return false;
	}

	const preferred = await local_settings_get(LOCAL_SETTINGS_KEY_PREFERRED_DOMAIN);
	if (!preferred) return false;
	if (preferred === window.location.hostname) return false;
	if (!(HASHIVERSE_DOMAINS as readonly string[]).includes(preferred)) return false;

	const sep = window.location.search ? "&" : "?";
	const url = `https://${preferred}${window.location.pathname}${window.location.search}${sep}autoredirected=1${window.location.hash}`;
	window.location.replace(url);
	return true;
}

if (await maybe_consume_or_auto_redirect()) {
	await new Promise(() => {});
}

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

const initial_settings = await fetch_user_settings(hashiverse);

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
			<Notifications position="top-right" />
			<PrereleaseBanner />
			<App initial_hashiverse={hashiverse} initial_settings={initial_settings} />
		</MantineProvider>
	</React.StrictMode>,
);

register_pwa_service_worker();
