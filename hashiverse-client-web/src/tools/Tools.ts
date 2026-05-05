import { adventurer as AvatarCollection } from "@dicebear/collection";
import { createAvatar } from "@dicebear/core";
import { blobToURL, fromBlob } from "image-resize-compress";
import type { NavigateFunction } from "react-router";

export class Tools {
	public static is_release_build(): boolean {
		return import.meta.env.PROD;
	}

	private static readonly DEFAULT_YELLOW_SKIN_COLORS = ["FFCC4D", "FFD635", "F9DC2A", "FCD34D"];

	public static create_avatar(id: string, avatar: string | undefined | null) {
		const has_chosen_avatar = !!avatar;
		const avatar_icon = createAvatar(AvatarCollection, {
			seed: has_chosen_avatar ? (avatar as string) : id,
			size: 128,
			...(has_chosen_avatar ? {} : { skinColor: Tools.DEFAULT_YELLOW_SKIN_COLORS }),
		});

		return avatar_icon.toDataUri();
	}

	public static trim_start(str: string, char: string) {
		if (!str) return str;

		while (str.startsWith(char)) {
			str = str.substring(1);
		}
		return str;
	}

	public static play_ui_sound(sound: string) {
		const audio = new Audio(sound);
		audio.volume = 0.5;
		audio.play().then(() => {});
	}

	public static navigate_to_user(navigate: NavigateFunction, id: string) {
		navigate(`/user/${encodeURIComponent(id)}`);
	}

	public static navigate_to_hashtag(navigate: NavigateFunction, hashtag: string) {
		navigate(`/hashtag/${encodeURIComponent(hashtag)}`);
	}

	public static navigate_to_post(navigate: NavigateFunction, post_id: string, bucket_location: string) {
		navigate(`/post/${encodeURIComponent(post_id)}/${encodeURIComponent(bucket_location)}`);
	}

	public static navigate_to_post_sequels(navigate: NavigateFunction, post_id: string, bucket_location: string) {
		navigate(`/post_sequels/${encodeURIComponent(post_id)}/${encodeURIComponent(bucket_location)}`);
	}

	public static async crush_image(img: Blob | File, max_width: number): Promise<string> {
		const image_webp = await fromBlob(img, 0.8, max_width, "auto", "webp");
		const image_webp_base64 = (await blobToURL(image_webp)) as string;
		const image_jpeg = await fromBlob(img, 0.8, max_width, "auto", "jpeg");
		const image_jpeg_base64 = (await blobToURL(image_jpeg)) as string;

		return image_webp_base64.length <= image_jpeg_base64.length ? image_webp_base64 : image_jpeg_base64;
	}
}
