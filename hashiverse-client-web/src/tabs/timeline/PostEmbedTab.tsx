import type React from "react";
import { useEffect, useRef, useState } from "react";
import { useParams } from "react-router";
import type { HashiverseClientWasmProxy, Post } from "../../Hashiverse.ts";
import { Spinner } from "../../tools/Spinner.tsx";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { ErrorTab } from "../ErrorTab.tsx";
import { PostPanel } from "./PostPanel.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	user_settings_cache: UserSettingsCache;
}

export const PostEmbedTab: React.FC<Props> = (props) => {
	const { post_id, bucket_location } = useParams();
	if (!post_id || !bucket_location) return <ErrorTab />;
	return <PostEmbedTabInner {...props} post_id={post_id} bucket_location={bucket_location} />;
};

const PostEmbedTabInner: React.FC<Props & { post_id: string; bucket_location: string }> = ({ hashiverse, user_settings_cache, post_id, bucket_location }) => {
	const [post, set_post] = useState<Post | null>(null);
	const container_ref = useRef<HTMLDivElement>(null);

	useEffect(() => {
		hashiverse.get_post_v1(bucket_location, post_id).then(set_post).catch(console.error);
	}, [post_id, bucket_location, hashiverse.get_post_v1]);

	useEffect(() => {
		if (!post || !container_ref.current) return;
		const observer = new ResizeObserver(() => {
			const height = container_ref.current?.scrollHeight ?? 0;
			window.parent.postMessage({ type: "hashiverse-embed-resize", height }, "*");
		});
		observer.observe(container_ref.current);
		return () => observer.disconnect();
	}, [post]);

	return (
		<div ref={container_ref}>
			{post ? (
				<PostPanel hashiverse={hashiverse} post={post} user_settings_cache={user_settings_cache} />
			) : (
				<div style={{ textAlign: "center", padding: 16 }}>
					<Spinner />
				</div>
			)}
		</div>
	);
};
