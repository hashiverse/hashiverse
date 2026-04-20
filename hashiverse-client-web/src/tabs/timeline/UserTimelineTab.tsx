import { type DataModulesSettings, type FinderPatternInnerSettings, type FinderPatternOuterSettings, type ImageSettings, ReactQRCode } from "@lglab/react-qr-code";
import { Button, Group, Modal, Stack, Text } from "@mantine/core";
import { useDisclosure } from "@mantine/hooks";
import type React from "react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "react-router";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import logo from "../../media/logo.webp";
import { useCachedBio } from "../../tools/BioCache.ts";
import { DomainSwitcherBanner } from "../../tools/DomainSwitcherBanner.tsx";
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

export const UserTimelineTab: React.FC<Props> = (props) => {
	const { client_id_hex } = useParams();
	if (!client_id_hex) return <ErrorTab />;
	return <UserTimelineTabInner {...props} client_id_hex={client_id_hex} />;
};

const UserTimelineTabInner: React.FC<Props & { client_id_hex: string }> = ({ hashiverse, user_settings_cache, client_id_hex }) => {
	const navigate = useNavigate();
	const { t } = useTranslation();
	const bio = useCachedBio(hashiverse, client_id_hex);
	const [is_me, is_me_set] = useState<boolean | null>(null);
	const [is_followed, is_followed_set] = useState(false);
	const [qr_opened, { open: qr_open, close: qr_close }] = useDisclosure(false);
	const [qr_png_data_url, set_qr_png_data_url] = useState<string | null>(null);
	const qr_offscreen_ref = useRef<HTMLDivElement>(null);

	useEffect(() => {
		const viewing_self = user_settings_cache.own_client_id === client_id_hex;
		is_me_set(viewing_self);
		is_followed_set(user_settings_cache.followed_client_ids.has(client_id_hex));
	}, [client_id_hex, user_settings_cache.own_client_id, user_settings_cache.followed_client_ids]);

	// Render the QR offscreen and convert to PNG once it's in the DOM
	useEffect(() => {
		// Wait a tick for the offscreen ReactQRCode to render its SVG
		const timeout = setTimeout(async () => {
			const svg_element = qr_offscreen_ref.current?.querySelector("svg");
			if (!svg_element) return;

			// Inline any <image> hrefs as base64 data URLs so the logo embeds in the PNG
			const svg_clone = svg_element.cloneNode(true) as SVGSVGElement;
			const image_elements = svg_clone.querySelectorAll("image");
			await Promise.all(
				Array.from(image_elements).map(async (image_el) => {
					const href = image_el.getAttribute("href") || image_el.getAttributeNS("http://www.w3.org/1999/xlink", "href");
					if (!href || href.startsWith("data:")) return;
					try {
						const response = await fetch(href);
						const blob = await response.blob();
						const data_url = await new Promise<string>((resolve) => {
							const reader = new FileReader();
							reader.onloadend = () => resolve(reader.result as string);
							reader.readAsDataURL(blob);
						});
						image_el.setAttribute("href", data_url);
					} catch {}
				}),
			);

			const svg_data = new XMLSerializer().serializeToString(svg_clone);
			const svg_blob = new Blob([svg_data], {
				type: "image/svg+xml;charset=utf-8",
			});
			const svg_url = URL.createObjectURL(svg_blob);

			const canvas = document.createElement("canvas");
			const render_size = 512;
			canvas.width = render_size;
			canvas.height = render_size;
			const ctx = canvas.getContext("2d");
			if (!ctx) {
				URL.revokeObjectURL(svg_url);
				return;
			}

			const img = new Image();
			img.onload = () => {
				ctx.drawImage(img, 0, 0, render_size, render_size);
				URL.revokeObjectURL(svg_url);
				set_qr_png_data_url(canvas.toDataURL("image/png"));
			};
			img.onerror = () => URL.revokeObjectURL(svg_url);
			img.src = svg_url;
		}, 200);

		return () => clearTimeout(timeout);
	}, []);

	const user_page_url = `${window.location.origin}/#/user/${client_id_hex}`;

	const copy_url_to_clipboard = useCallback(() => {
		const share_text = t("user.follow_me_at", { url: user_page_url });
		navigator.clipboard.writeText(share_text).catch(console.error);
	}, [user_page_url, t]);

	const copy_qr_to_clipboard = useCallback(async () => {
		if (!qr_png_data_url) return;
		const response = await fetch(qr_png_data_url);
		const blob = await response.blob();
		navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]).catch(console.error);
	}, [qr_png_data_url]);

	const can_share = !!navigator.share;

	const share_url = useCallback(async () => {
		navigator
			.share({
				title: t("user.share_profile"),
				text: t("user.follow_me_at", { url: user_page_url }),
			})
			.catch(console.error);
	}, [user_page_url, t]);

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

	const dataModulesSettings: DataModulesSettings = {
		color: "#88bbff",
		style: "circle",
	};
	const finderPatternOuterSettings: FinderPatternOuterSettings = {
		color: "#88bbff",
		style: "outpoint-sm",
	};
	const finderPatternInnerSettings: FinderPatternInnerSettings = {
		color: "#88bbff",
		style: "heart",
	};
	const imageSettings: ImageSettings = {
		src: logo,
		width: 90,
		height: 90,
		excavate: false,
	};

	const header =
		is_me === null ? undefined : is_me ? (
			<Banner
				image={
					qr_png_data_url ? (
						<button type="button" onClick={qr_open} style={{ background: "none", border: "none", padding: 0, cursor: "pointer", width: "100%" }}>
							<img src={qr_png_data_url} alt="QR code" style={{ width: "100%" }} />
						</button>
					) : undefined
				}
				heading={
					<Text size="xl" fw={700}>
						{t("banner.me")}
					</Text>
				}
				detail={
					<>
						<Text>{t("banner.me_detail")}</Text>
						<Button onClick={() => navigate(`/user_mentioned/${client_id_hex}`)}>{t("banner.me_mentioned")}</Button>
					</>
				}
			/>
		) : (
			<Banner
				image={<UserImageControl client_id={client_id_hex} selfie={bio?.selfie} avatar={bio?.avatar} radius="100%" style={{ width: "100%", height: "auto", aspectRatio: "1" }} />}
				heading={<UserNameControl size="xl" fw={700} client_id={client_id_hex} nickname={bio?.nickname} />}
				detail={
					<>
						<Text>{bio?.status}</Text>
						<Group gap="xs">
							<Button onClick={() => navigate(`/user_mentioned/${client_id_hex}`)}>{t("banner.user_mentioned")}</Button>
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
			{!user_settings_cache.is_logged_in && <DomainSwitcherBanner hash_path={`#/user/${client_id_hex}`} />}
			<TimelineControl
				hashiverse={hashiverse}
				user_settings_cache={user_settings_cache}
				timeline_key={`user:${client_id_hex}`}
				get_more={() => hashiverse.single_timeline_get_more_user_v1(client_id_hex)}
				reset={() => hashiverse.single_timeline_reset()}
				header={header}
			/>

			{/* Offscreen QR renderer — hidden, used only to generate the PNG */}
			<div ref={qr_offscreen_ref} style={{ position: "absolute", left: "-9999px", top: "-9999px" }}>
				<ReactQRCode
					value={user_page_url}
					size={512}
					background={"#222"}
					dataModulesSettings={dataModulesSettings}
					finderPatternOuterSettings={finderPatternOuterSettings}
					finderPatternInnerSettings={finderPatternInnerSettings}
					level="H"
					marginSize={1}
					imageSettings={imageSettings}
				/>
			</div>

			<Modal opened={qr_opened} onClose={qr_close} title={t("user.share_profile")} size="sm" centered>
				<Stack align="center" gap="md">
					{qr_png_data_url && <img src={qr_png_data_url} alt="QR" style={{ width: "100%", maxWidth: 300 }} />}
					<Group justify="center" gap="sm">
						<Button variant="default" onClick={copy_qr_to_clipboard}>
							{t("user.copy_qr")}
						</Button>
						<Button variant="default" onClick={copy_url_to_clipboard}>
							{t("user.copy_url")}
						</Button>
						{can_share && (
							<Button variant="default" onClick={share_url}>
								{t("user.share_url")}
							</Button>
						)}
					</Group>
				</Stack>
			</Modal>
		</div>
	);
};
