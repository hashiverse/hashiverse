import type React from "react";
import { useEffect, useRef } from "react";
import { useLocation, useNavigate } from "react-router";
import { redirect_to_login_with_return } from "../../tools/PostLoginRedirect.ts";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { open_compose } from "../compose/ComposeDialogStore.ts";

function is_youtube_url(url: string): boolean {
	try {
		const hostname = new URL(url).hostname;
		return hostname.includes("youtube.com") || hostname.includes("youtu.be");
	} catch {
		return false;
	}
}

interface Props {
	user_settings_cache: UserSettingsCache;
}

export const ShareTab: React.FC<Props> = ({ user_settings_cache }) => {
	const location = useLocation();
	const navigate = useNavigate();
	const fired = useRef(false);

	useEffect(() => {
		if (fired.current) return;
		fired.current = true;

		const params = new URLSearchParams(location.search);
		const text = params.get("text") ?? "";
		const url = params.get("url") ?? "";
		const has_file = params.get("has_file") === "true";

		if (!user_settings_cache.is_logged_in) {
			redirect_to_login_with_return(navigate, location.pathname + location.search, { replace: true });
			return;
		}

		navigate("/", { replace: true });

		(async () => {
			let share_image_blob: Blob | undefined;
			if (has_file) {
				const share_cache = await caches.open("hashiverse-share-v1");
				const resp = await share_cache.match("/share-incoming-file");
				if (resp) {
					share_image_blob = await resp.blob();
					await share_cache.delete("/share-incoming-file");
				}
			}

			open_compose({
				initial_html: text ? `<p>${text}</p>` : "",
				share_image_blob,
				share_youtube_url: url && is_youtube_url(url) ? url : undefined,
				share_url: url && !is_youtube_url(url) ? url : undefined,
			});
		})();
	}, [user_settings_cache.is_logged_in, location.search, location.pathname, navigate]);

	return null;
};
