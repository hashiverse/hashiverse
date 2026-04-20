import { ActionIcon, Box, Button, Group, Menu, Modal, Stack, Text, Tooltip } from "@mantine/core";
import { useDisclosure } from "@mantine/hooks";
import { IconArrowForward, IconChevronDown, IconCode, IconCopy, IconDots, IconHtml, IconLink, IconPhoto, IconShare } from "@tabler/icons-react";
import html2canvas from "html2canvas";
import katex from "katex";
import React, { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import type { HashiverseClientWasmProxy, Post } from "../../Hashiverse.ts";
import sound_interaction_negative from "../../media/interaction_negative.wav";
import sound_interaction_positive from "../../media/interaction_positive.wav";
import { populate_mention_bio, useCachedBio } from "../../tools/BioCache.ts";
import { FEEDBACK_TYPE_COMMENT, FEEDBACK_TYPE_CSAM, FEEDBACK_TYPE_REPOST, FEEDBACK_TYPE_SEQUEL, FEEDBACKS_NEGATIVE, FEEDBACKS_POSITIVE, FEEDBACKS_WITH_ACTIONS } from "../../tools/Feedback.ts";
import { has_submitted_feedback, mark_feedback_submitted } from "../../tools/FeedbackCache.ts";
import { register_client_id } from "../../tools/MentionStore.ts";
import { sanitize } from "../../tools/PostPurifier.ts";
import { RelativeTimeAgo } from "../../tools/RelativeTimeAgo.tsx";
import { Tools } from "../../tools/Tools.ts";
import { UserImageControl } from "../../tools/UserImageControl.tsx";
import { UserNameControl } from "../../tools/UserNameControl.tsx";
import type { UserSettingsCache } from "../../tools/UserSettingsCache.ts";
import { open_compose } from "../compose/ComposeDialogStore.ts";
import { ContentWarningOverlay } from "./ContentWarningOverlay.tsx";

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	post: Post;
	blur_images?: boolean;
	user_settings_cache: UserSettingsCache;
}

export const PostPanel: React.FC<Props> = React.memo(({ hashiverse, post, blur_images, user_settings_cache }) => {
	const { t } = useTranslation();
	const navigate = useNavigate();

	// *** TODO eventually find a reasonable place to put this
	register_client_id(post.client_id);

	const bio = useCachedBio(hashiverse, post.client_id);
	const [feedbacks, feedbacks_set] = useState<Uint32Array | null>(null);
	const [has_voted, set_has_voted] = useState(() => has_submitted_feedback(post.post_id));

	useEffect(() => {
		hashiverse
			.get_post_feedbacks_v1(post.bucket_location, post.post_id)
			.then(feedbacks_set)
			.catch(() => {});
	}, [post.bucket_location, post.post_id, hashiverse]);

	const is_own_post = post.client_id === user_settings_cache.own_client_id;
	const is_author_followed = user_settings_cache.followed_client_ids.has(post.client_id);
	const effective_blur_images = blur_images && !is_own_post && !(user_settings_cache.skip_warnings_for_followed && is_author_followed);

	const get_clicked_element = useCallback((e: React.MouseEvent<HTMLDivElement>, selector: string): HTMLElement | null => {
		const target = e.target as HTMLElement | null;
		if (!target) return null;

		const el = target.closest(selector) as HTMLElement | null;
		if (!el) return null;

		return el;
	}, []);

	const on_content_click = useCallback(
		(e: React.MouseEvent<HTMLDivElement>) => {
			// Unblur image on tap
			if (effective_blur_images) {
				const target = e.target as HTMLElement;
				if (target.tagName === "IMG" && !target.classList.contains("unblur-image")) {
					e.preventDefault();
					target.classList.add("unblur-image");
					return;
				}
			}

			// Check for mention
			{
				const element = get_clicked_element(e, "mention");
				if (element) {
					e.preventDefault();
					const mention_id = element.getAttribute("client_id")?.trim();
					if (mention_id) Tools.navigate_to_user(navigate, mention_id);
					return;
				}
			}

			// Check for hashtag
			{
				const element = get_clicked_element(e, "hashtag");
				if (element) {
					e.preventDefault();
					const name = element.getAttribute("hashtag")?.trim();
					if (name) Tools.navigate_to_hashtag(navigate, name);
					return;
				}
			}

			// Check for reply / repost — navigate to the quoted post
			{
				const element = get_clicked_element(e, "reply, repost");
				if (element) {
					e.preventDefault();
					const post_id = element.getAttribute("post_id")?.trim();
					const bucket_location = element.getAttribute("bucket_location")?.trim();
					if (post_id && bucket_location) Tools.navigate_to_post(navigate, post_id, bucket_location);
					return;
				}
			}

			// Check for sequel — navigate to the sequels timeline of the referenced post
			{
				const element = get_clicked_element(e, "sequel");
				if (element) {
					e.preventDefault();
					const post_id = element.getAttribute("post_id")?.trim();
					const bucket_location = element.getAttribute("bucket_location")?.trim();
					if (post_id && bucket_location) Tools.navigate_to_post_sequels(navigate, post_id, bucket_location);
					return;
				}
			}
		},
		[get_clicked_element, navigate, effective_blur_images],
	);

	const sanitized_html = useMemo(() => {
		return sanitize(post.post);
	}, [post.post]);

	const panel_ref = useRef<HTMLDivElement>(null);
	const content_ref = useRef<HTMLDivElement>(null);
	const [is_expanded, set_is_expanded] = useState(false);
	const [is_overflowing, set_is_overflowing] = useState(false);

	const POST_CONTENT_MAX_HEIGHT = 300;

	useLayoutEffect(() => {
		const el = content_ref.current;
		if (!el) return;
		el.querySelectorAll<HTMLElement>("mention[client_id]").forEach((mention) => {
			const mention_id = mention.getAttribute("client_id")?.trim();
			if (!mention_id) return;
			const left = mention.querySelector<HTMLElement>(".plugin-mention-left");
			const right = mention.querySelector<HTMLElement>(".plugin-mention-right");
			if (!left) return;

			const img = document.createElement("img");
			img.style.cssText = "width:22px;height:22px;border-radius:50%;object-fit:cover;vertical-align:text-top;";
			img.className = "unblur-image"; // This ensures that blurs are not applied to the avatars

			left.textContent = "";
			left.appendChild(img);

			populate_mention_bio(hashiverse, mention_id, img, right);
		});

		el.querySelectorAll<HTMLElement>("sequel").forEach((sequel_el) => {
			if (!sequel_el.textContent?.trim()) {
				sequel_el.textContent = t("post.sequel_to_previous");
			}
		});

		el.querySelectorAll<HTMLTableElement>("table:not(.table-scroll-wrapper > table)").forEach((table) => {
			const wrapper = document.createElement("div");
			wrapper.className = "table-scroll-wrapper";
			table.parentNode?.insertBefore(wrapper, table);
			wrapper.appendChild(table);
		});

		el.querySelectorAll<HTMLElement>('[data-type="inline-math"][data-latex], [data-type="block-math"][data-latex]').forEach((math_el) => {
			const latex = math_el.getAttribute("data-latex") ?? "";
			const display = math_el.getAttribute("data-type") === "block-math";
			try {
				katex.render(latex, math_el, {
					displayMode: display,
					throwOnError: false,
				});
			} catch {}
		});

		set_is_overflowing(el.scrollHeight > POST_CONTENT_MAX_HEIGHT);
	});

	// Why this handler exists:
	// KaTeX renders math by replacing the innerHTML of the outer element
	// (e.g. <span data-type="inline-math" data-latex="y=ax^2">) with deeply nested
	// KaTeX spans. When the user selects rendered math and copies, the browser's
	// selection anchors land *inside* those KaTeX spans. The resulting clipboard
	// text/html therefore contains only the inner KaTeX structure — the outer element
	// with data-type and data-latex never makes it onto the clipboard. Tiptap finds
	// no matching parseHTML rule and falls back to the plain-text Unicode approximation.
	// We intercept copy, detect any math in the selection, and rebuild the clipboard
	// HTML with clean <span data-type="inline-math" data-latex="..."> elements so
	// that pasting into the editor correctly recreates the math nodes.
	const on_content_copy = useCallback((e: React.ClipboardEvent<HTMLDivElement>) => {
		const selection = window.getSelection();
		if (!selection || selection.rangeCount === 0) return;

		const range = selection.getRangeAt(0);

		// Case 1: selection anchors are BOTH inside the same single math element
		// (cloneContents gives only the inner KaTeX spans, outer element absent)
		const start_el = range.startContainer instanceof Element ? range.startContainer : range.startContainer.parentElement;
		const end_el = range.endContainer instanceof Element ? range.endContainer : range.endContainer.parentElement;
		const enclosing_math = start_el?.closest<HTMLElement>('[data-type="inline-math"],[data-type="block-math"]');
		const end_enclosing_math = end_el?.closest<HTMLElement>('[data-type="inline-math"],[data-type="block-math"]');
		if (enclosing_math && enclosing_math === end_enclosing_math) {
			const latex = enclosing_math.getAttribute("data-latex") ?? "";
			const is_block = enclosing_math.getAttribute("data-type") === "block-math";
			const tag = is_block ? "div" : "span";
			e.clipboardData.setData("text/html", `<${tag} data-type="${is_block ? "block-math" : "inline-math"}" data-latex="${latex.replace(/"/g, "&quot;")}"></${tag}>`);
			e.clipboardData.setData("text/plain", is_block ? `$$${latex}$$` : `$${latex}$`);
			e.preventDefault();
			return;
		}

		// Case 2: selection spans multiple elements — clone, replace math elements
		const fragment = range.cloneContents();
		const temp = document.createElement("div");
		temp.appendChild(fragment);

		const math_els = temp.querySelectorAll<HTMLElement>('[data-type="inline-math"],[data-type="block-math"]');
		if (math_els.length === 0) return; // No math — let browser handle normally

		math_els.forEach((el) => {
			const latex = el.getAttribute("data-latex") ?? "";
			const is_block = el.getAttribute("data-type") === "block-math";
			const clean = document.createElement(is_block ? "div" : "span");
			clean.setAttribute("data-type", is_block ? "block-math" : "inline-math");
			clean.setAttribute("data-latex", latex);
			el.replaceWith(clean);
		});

		e.clipboardData.setData("text/html", temp.innerHTML);
		e.clipboardData.setData("text/plain", selection.toString());
		e.preventDefault();
	}, []);

	const on_comment_click = useCallback(() => {
		const reply_html = `<reply post_id="${post.post_id}" bucket_location="${post.bucket_location}" client_id="${post.client_id}" post_header_hex="${post.encoded_post_header_hex}">${sanitized_html}</reply>`;
		open_compose({
			initial_html: `<p></p>${reply_html}`,
			on_posted: () => hashiverse.submit_feedback_v1(post.bucket_location, post.post_id, FEEDBACK_TYPE_COMMENT),
		});
	}, [post.post_id, post.bucket_location, post.client_id, post.encoded_post_header_hex, sanitized_html, hashiverse]);

	const on_sequel_click = useCallback(() => {
		const sequel_html = `<sequel post_id="${post.post_id}" bucket_location="${post.bucket_location}" client_id="${post.client_id}" post_header_hex="${post.encoded_post_header_hex}"/>`;
		open_compose({
			initial_html: `${sequel_html}<p></p>`,
			on_posted: () => hashiverse.submit_feedback_v1(post.bucket_location, post.post_id, FEEDBACK_TYPE_SEQUEL),
		});
	}, [post.post_id, post.bucket_location, post.client_id, post.encoded_post_header_hex, hashiverse]);

	const post_url = `${window.location.origin}/#/post/${encodeURIComponent(post.post_id)}/${encodeURIComponent(post.bucket_location)}`;

	const on_copy_link = useCallback(() => {
		navigator.clipboard.writeText(post_url).catch(console.error);
	}, [post_url]);

	const on_copy_text = useCallback(() => {
		const text = content_ref.current?.innerText || "";
		navigator.clipboard.writeText(text).catch(console.error);
	}, []);

	const on_copy_html = useCallback(() => {
		// Suck all the CSS rules that apply to the post and rework them to be embeddable in a static html document
		const embed_rules: string[] = [];
		{
			const rewrite_selectors = [".PostPanelContent", ".PostPanel"];
			for (const sheet of Array.from(document.styleSheets)) {
				try {
					for (const rule of Array.from(sheet.cssRules)) {
						if (!(rule instanceof CSSStyleRule)) continue;
						if (!rewrite_selectors.some((selector) => rule.selectorText.includes(selector))) continue;
						const rewritten_selector = rewrite_selectors.reduce((selector, original) => selector.split(original).join(".hashiverse-embed"), rule.selectorText);
						let css_text = rule.style.cssText;
						if (rewrite_selectors.some((s) => rule.selectorText.startsWith(s)) && !rule.selectorText.includes(" ")) {
							css_text = css_text.replace(/background(-color)?:\s*[^;]+;?\s*/g, "");
						}
						if (!css_text.trim()) continue;
						const resolved_css = css_text.replace(/var\(--[^)]+\)/g, (match) => {
							const var_name = match.slice(4, -1).split(",")[0].trim();
							return getComputedStyle(document.documentElement).getPropertyValue(var_name).trim() || match;
						});
						embed_rules.push(`${rewritten_selector} { ${resolved_css} }`);
					}
				} catch {}
			}
		}
		const embed_css = embed_rules.join("\n");

		// NB we dont mind this external reference to katex as it is for standalone copied-and-pasted exported post html
		const has_math = sanitized_html.includes('data-type="inline-math"') || sanitized_html.includes('data-type="block-math"');
		const katex_tags = has_math
			? `<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/katex@0.16/dist/katex.min.css">\n<script defer src="https://cdn.jsdelivr.net/npm/katex@0.16/dist/katex.min.js"></script>\n<script defer>document.addEventListener('DOMContentLoaded',function(){document.querySelectorAll('[data-type="inline-math"][data-latex],[data-type="block-math"][data-latex]').forEach(function(el){katex.render(el.getAttribute('data-latex'),el,{displayMode:el.getAttribute('data-type')==='block-math',throwOnError:false})})});</script>\n`
			: "";

		const embed_html = `<!DOCTYPE html>\n${katex_tags}<style>\nbody { margin: 0; }\n.hashiverse-embed { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; color: #fff; background: #242424; }\n${embed_css}\n</style>\n<div class="hashiverse-embed">\n${sanitized_html}\n</div>`;
		navigator.clipboard.writeText(embed_html).catch(console.error);
	}, [sanitized_html]);

	const on_copy_image = useCallback(async () => {
		if (!panel_ref.current) return;
		try {
			const canvas = await html2canvas(panel_ref.current, {
				backgroundColor: "#242424",
				scale: 2,
				useCORS: true,
			});
			const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/png"));
			if (blob) {
				await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
			}
		} catch (error) {
			console.error("Failed to copy post as image:", error);
		}
	}, []);

	const embed_url = `${window.location.origin}/#/post_embed/${encodeURIComponent(post.post_id)}/${encodeURIComponent(post.bucket_location)}`;

	const on_embed = useCallback(() => {
		const iframe_id = `hashiverse-embed-${post.post_id.slice(0, 8)}`;
		const iframe_html = `<iframe id="${iframe_id}" src="${embed_url}" style="border:none;max-height:500px;overflow:auto;" width="100%" height="150"></iframe>\n<script>window.addEventListener('message',function(e){if(e.data&&e.data.type==='hashiverse-embed-resize'){var f=document.getElementById('${iframe_id}');if(f)f.height=Math.min(e.data.height,500)}});</script>`;
		navigator.clipboard.writeText(iframe_html).catch(console.error);
	}, [embed_url, post.post_id]);

	const on_share_link = useCallback(() => {
		navigator.share({ title: t("post.share_link"), url: post_url }).catch(console.error);
	}, [post_url, t]);

	const has_sequels = feedbacks ? feedbacks[FEEDBACK_TYPE_SEQUEL] > 0 : false;

	const [needs_login_opened, { open: needs_login_open, close: needs_login_close }] = useDisclosure(false);

	const guard = useCallback(
		(action: () => void) => () => {
			if (!user_settings_cache.is_logged_in) {
				needs_login_open();
				return;
			}
			action();
		},
		[user_settings_cache.is_logged_in, needs_login_open],
	);

	const [repost_opened, { open: repost_open, close: repost_close }] = useDisclosure(false);
	const [reposting, set_reposting] = useState(false);

	const [csam_opened, { open: csam_open, close: csam_close }] = useDisclosure(false);
	const [csam_submitting, set_csam_submitting] = useState(false);

	const on_csam_confirm = async () => {
		try {
			set_csam_submitting(true);
			await on_interaction_negative_click(FEEDBACK_TYPE_CSAM);
			csam_close();
		} finally {
			set_csam_submitting(false);
		}
	};

	const on_repost_confirm = async () => {
		try {
			set_reposting(true);
			const repost_html = `<repost post_id="${post.post_id}" bucket_location="${post.bucket_location}" client_id="${post.client_id}">${sanitized_html}</repost>`;
			await hashiverse.post_v1(repost_html);
			await hashiverse.submit_feedback_v1(post.bucket_location, post.post_id, FEEDBACK_TYPE_REPOST);
			Tools.play_ui_sound(sound_interaction_positive);
			repost_close();
		} finally {
			set_reposting(false);
		}
	};

	const format_count = (n: number): string => {
		if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(n >= 10_000_000_000 ? 0 : 1)}B`;
		if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n >= 10_000_000 ? 0 : 1)}M`;
		if (n >= 1_000) return `${(n / 1_000).toFixed(n >= 10_000 ? 0 : 1)}k`;
		return `${n}`;
	};

	const ActionWithCount: React.FC<{
		icon: React.ReactNode;
		count: number;
		onClick?: () => void;
		tooltip?: string;
		disabled?: boolean;
	}> = ({ icon, count, onClick, tooltip, disabled }) => (
		<Stack gap={0} align="center" style={{ cursor: onClick && !disabled ? "pointer" : "default" }}>
			<Tooltip label={tooltip} disabled={!tooltip}>
				<ActionIcon onClick={disabled ? undefined : onClick} disabled={disabled}>
					{icon}
				</ActionIcon>
			</Tooltip>
			<Text size="xs" lh={1}>
				{format_count(count)}
			</Text>
		</Stack>
	);

	const on_interaction_positive_click = async (feedback_type: number) => {
		Tools.play_ui_sound(sound_interaction_positive);
		mark_feedback_submitted(post.post_id);
		set_has_voted(true);
		await hashiverse.submit_feedback_v1(post.bucket_location, post.post_id, feedback_type);
	};

	const on_interaction_negative_click = async (feedback_type: number) => {
		Tools.play_ui_sound(sound_interaction_negative);
		mark_feedback_submitted(post.post_id);
		set_has_voted(true);
		await hashiverse.submit_feedback_v1(post.bucket_location, post.post_id, feedback_type);
	};

	return (
		<div ref={panel_ref} className="PostPanel">
			<div className="PostPanelHeader">
				<Group align="flex-start" wrap="nowrap" gap="sm">
					<UserImageControl
						client_id={post.client_id}
						selfie={bio?.selfie}
						avatar={bio?.avatar}
						radius="xl"
						size="lg"
						style={{ padding: "2px", cursor: "pointer" }}
						onClick={() => Tools.navigate_to_user(navigate, post.client_id)}
					/>

					<Box style={{ flex: 1, minWidth: 0 }}>
						<Group gap="xs" wrap="nowrap" justify="space-between">
							<UserNameControl client_id={post.client_id} nickname={bio?.nickname} tooltip={bio?.status} onClick={() => Tools.navigate_to_user(navigate, post.client_id)} />

							<Box ml="auto" style={{ flex: "0 0 auto", cursor: "pointer" }} onClick={() => Tools.navigate_to_post(navigate, post.post_id, post.bucket_location)}>
								<RelativeTimeAgo date={post.time_millis} />
							</Box>
						</Group>

						<Group gap="xs" align="flex-start">
							{FEEDBACKS_WITH_ACTIONS.map((option) => {
								const Icon = option.icon;
								const base = option.feedback_type === FEEDBACK_TYPE_COMMENT ? on_comment_click : option.feedback_type === FEEDBACK_TYPE_REPOST ? repost_open : (option.action ?? undefined);
								return (
									<ActionWithCount
										key={option.feedback_type}
										icon={<Icon size={16} />}
										count={feedbacks ? feedbacks[option.feedback_type] : 0}
										onClick={base ? guard(base) : undefined}
										tooltip={t(option.title)}
									/>
								);
							})}

							{FEEDBACKS_POSITIVE.map((option) => {
								const Icon = option.icon;
								return (
									<ActionWithCount
										key={option.feedback_type}
										icon={<Icon size={16} />}
										count={feedbacks ? feedbacks[option.feedback_type] : 0}
										onClick={guard(() => on_interaction_positive_click(option.feedback_type))}
										tooltip={t(option.title)}
										disabled={has_voted}
									/>
								);
							})}

							<Menu>
								<Menu.Target>
									<ActionIcon>
										<IconChevronDown />
									</ActionIcon>
								</Menu.Target>
								<Menu.Dropdown>
									{FEEDBACKS_NEGATIVE.map((option) => {
										const Icon = option.icon;
										const on_click = option.feedback_type === FEEDBACK_TYPE_CSAM ? guard(csam_open) : guard(() => on_interaction_negative_click(option.feedback_type));
										return (
											<Menu.Item
												key={option.feedback_type}
												leftSection={<Icon size={16} />}
												onClick={on_click}
												disabled={has_voted}
												color={option.feedback_type === FEEDBACK_TYPE_CSAM ? "red" : undefined}
											>
												{t(option.title)} <small>({feedbacks ? format_count(feedbacks[option.feedback_type]) : "-"})</small>
											</Menu.Item>
										);
									})}
								</Menu.Dropdown>
							</Menu>

							<Box style={{ flex: 1 }} />

							<Menu>
								<Menu.Target>
									<ActionIcon>
										<IconDots />
									</ActionIcon>
								</Menu.Target>
								<Menu.Dropdown>
									{is_own_post && (
										<Menu.Item leftSection={<IconArrowForward size={16} />} onClick={guard(on_sequel_click)}>
											{t("post.sequel")}
										</Menu.Item>
									)}
									{!!navigator.share && (
										<Menu.Item leftSection={<IconShare size={16} />} onClick={on_share_link}>
											{t("post.share_link")}
										</Menu.Item>
									)}
									<Menu.Item leftSection={<IconLink size={16} />} onClick={on_copy_link}>
										{t("post.copy_link")}
									</Menu.Item>
									<Menu.Item leftSection={<IconCode size={16} />} onClick={on_embed}>
										{t("post.embed")}
									</Menu.Item>
									<Menu.Item leftSection={<IconPhoto size={16} />} onClick={on_copy_image}>
										{t("post.copy_image")}
									</Menu.Item>
									<Menu.Item leftSection={<IconHtml size={16} />} onClick={on_copy_html}>
										{t("post.copy_html")}
									</Menu.Item>
									<Menu.Item leftSection={<IconCopy size={16} />} onClick={on_copy_text}>
										{t("post.copy_text")}
									</Menu.Item>
								</Menu.Dropdown>
							</Menu>
						</Group>
					</Box>
				</Group>
			</div>
			{has_sequels && (
				<button
					type="button"
					className="plugin-sequel plugin-sequel-clickable"
					onClick={() => Tools.navigate_to_post_sequels(navigate, post.post_id, post.bucket_location)}
					style={{ background: "none", border: "none", padding: 0, cursor: "pointer", font: "inherit", color: "inherit", width: "100%", textAlign: "inherit" }}
				>
					{t("post.see_sequels")}
				</button>
			)}
			<ContentWarningOverlay feedbacks={feedbacks} is_own_post={is_own_post} is_author_followed={is_author_followed} user_settings_cache={user_settings_cache}>
				<div className={`PostPanelContentWrapper${is_overflowing && !is_expanded ? " PostPanelContentWrapper--collapsed" : ""}`}>
					{
						// biome-ignore lint/a11y/noStaticElementInteractions: post content with click handlers for mentions/hashtags
						// biome-ignore lint/a11y/useKeyWithClickEvents: post content with click handlers for mentions/hashtags
						<div
							ref={content_ref}
							className={`PostPanelContent${effective_blur_images ? " blur-images" : ""}${is_overflowing && !is_expanded ? " PostPanelContent--collapsed" : ""}`}
							dangerouslySetInnerHTML={{ __html: sanitized_html }}
							onClick={on_content_click}
							onCopy={on_content_copy}
						/>
					}
					{is_overflowing && (
						<button
							type="button"
							className="PostPanelReadMore"
							onClick={() => set_is_expanded(!is_expanded)}
							style={{ background: "none", border: "none", cursor: "pointer", font: "inherit", color: "inherit", width: "100%", padding: 0 }}
						>
							{is_expanded ? t("post.show_less") : t("post.read_more")}
						</button>
					)}
				</div>
			</ContentWarningOverlay>
			{has_sequels && (
				<button
					type="button"
					className="plugin-sequel plugin-sequel-clickable"
					onClick={() => Tools.navigate_to_post_sequels(navigate, post.post_id, post.bucket_location)}
					style={{ background: "none", border: "none", padding: 0, cursor: "pointer", font: "inherit", color: "inherit", width: "100%", textAlign: "inherit" }}
				>
					{t("post.see_sequels")}
				</button>
			)}

			<Modal opened={needs_login_opened} onClose={needs_login_close} title={t("not_logged_in.needs_login_title")} size="sm" centered>
				<Text mb="md">{t("not_logged_in.needs_login_message")}</Text>
				<Group justify="flex-end">
					<Button variant="default" onClick={needs_login_close}>
						{t("bio.cancel")}
					</Button>
					<Button
						onClick={() => {
							needs_login_close();
							navigate("/login");
						}}
					>
						{t("not_logged_in.log_in")}
					</Button>
				</Group>
			</Modal>

			<Modal opened={repost_opened} onClose={repost_close} title={t("feedback.repost")} size="sm" centered>
				<Text mb="md">{t("feedback.repost_confirm")}</Text>
				<Group justify="flex-end">
					<Button variant="default" onClick={repost_close}>
						{t("bio.cancel")}
					</Button>
					<Button onClick={on_repost_confirm} loading={reposting}>
						{t("feedback.repost")}
					</Button>
				</Group>
			</Modal>

			<Modal opened={csam_opened} onClose={csam_close} title={t("feedback.csam_confirm_title")} size="md" centered>
				<Text mb="md">{t("feedback.csam_confirm_warning")}</Text>
				<Group justify="flex-end">
					<Button variant="default" onClick={csam_close}>
						{t("bio.cancel")}
					</Button>
					<Button color="red" onClick={on_csam_confirm} loading={csam_submitting}>
						{t("feedback.csam_confirm_submit")}
					</Button>
				</Group>
			</Modal>
		</div>
	);
});
