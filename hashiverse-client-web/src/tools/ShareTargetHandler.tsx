import type React from "react";
import { useEffect } from "react";
import { useLocation, useNavigate } from "react-router";
import { open_compose } from "../tabs/compose/ComposeDialogStore.ts";

function is_youtube_url(url: string): boolean {
	try {
		const hostname = new URL(url).hostname;
		return hostname.includes("youtube.com") || hostname.includes("youtu.be");
	} catch {
		return false;
	}
}

export const ShareTargetHandler: React.FC = () => {
	const location = useLocation();
	const navigate = useNavigate();

	useEffect(() => {
		const params = new URLSearchParams(location.search);
		if (!params.get("share")) return;

		const text = params.get("text") ?? "";
		const url = params.get("url") ?? "";
		const has_file = params.get("has_file") === "true";

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
	}, [location.search, navigate]);

	return null;
};
