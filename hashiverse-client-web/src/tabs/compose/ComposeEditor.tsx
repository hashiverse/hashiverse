import "@mantine/tiptap/styles.css";
import "katex/dist/katex.min.css";

import { ActionIcon, ColorSwatch, Menu, Tooltip } from "@mantine/core";
import { Link, RichTextEditor } from "@mantine/tiptap";
import {
	IconAt,
	IconBrandYoutube,
	IconColumnInsertLeft,
	IconColumnInsertRight,
	IconColumnRemove,
	IconEraser,
	IconGif,
	IconHash,
	IconHeading,
	IconHighlight,
	IconMath,
	IconMathIntegral,
	IconPalette,
	IconPhoto,
	IconRowInsertBottom,
	IconRowInsertTop,
	IconRowRemove,
	IconSend,
	IconTable,
	IconTableOff,
	IconTablePlus,
} from "@tabler/icons-react";
import type { JSONContent } from "@tiptap/core";
import { Color } from "@tiptap/extension-color";
import { Highlight } from "@tiptap/extension-highlight";
import { Image } from "@tiptap/extension-image";
import { BlockMath, InlineMath } from "@tiptap/extension-mathematics";
import SubScript from "@tiptap/extension-subscript";
import Superscript from "@tiptap/extension-superscript";
import { Table, TableCell, TableHeader, TableRow } from "@tiptap/extension-table";
import TextAlign from "@tiptap/extension-text-align";
import { TextStyle } from "@tiptap/extension-text-style";
import { Typography } from "@tiptap/extension-typography";
import { Youtube } from "@tiptap/extension-youtube";
import { useEditor } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import React, { useEffect, useImperativeHandle, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import sound_compose from "../../media/compose.wav";
import { LOCAL_SETTINGS_KEY_DRAFT_POST, local_settings_delete, local_settings_get, local_settings_set } from "../../tools/LocalSettings.ts";
import { sanitize } from "../../tools/PostPurifier.ts";
import { Spinner } from "../../tools/Spinner.tsx";
import { Toast } from "../../tools/Toast.ts";
import { Tools } from "../../tools/Tools.ts";
import { GiphyDialogControl, type GiphyDialogControlManager } from "./GiphyDialogControl.tsx";
import { Hashtag } from "./HashtagExtension.ts";
import { BlockMathEditor, DEFAULT_LATEX_BLOCK, DEFAULT_LATEX_INLINE, InlineMathEditor, MathInputRules, MathNodeNavigation } from "./MathEditorExtension.tsx";
import { Mention } from "./MentionExtension.ts";
import { Reply } from "./ReplyExtension.ts";
import { Repost } from "./RepostExtension.ts";
import { Sequel } from "./SequelExtension.ts";
import { Smilie } from "./SmilieExtension.ts";
import { UrlPreview } from "./UrlPreviewExtension.ts";
import { UserSearchDialogControl, type UserSearchDialogControlManager } from "./UserSearchDialogControl.tsx";
import { YouTubeDialogControl, type YouTubeDialogControlManager } from "./YouTubeDialogControl.tsx";

const HEADING_LEVELS = [1, 2, 3, 4, 5, 6] as const;
const TEXT_COLORS = ["#000000", "#e03131", "#f76707", "#f59f00", "#2f9e44", "#ffffff", "#1971c2", "#364fc7", "#7048e8", "#555555"];
const HIGHLIGHT_COLORS = ["#868e96", "#ff8787", "#ffa94d", "#ffd43b", "#69db7c", "#ced4da", "#4dabf7", "#748ffc", "#9775fa", "#adb5bd"];
const MAX_EMBEDDING_WIDTH = 512;

const contains_meaningful_tiptap_node = (node: JSONContent | undefined): boolean => {
	if (!node) return false;
	if (node.type === "text") return (typeof node.text === "string" ? node.text : "").trim().length > 0;
	if (node.type && new Set(["image", "youtube", "mention", "hashtag", "horizontalrule", "urlpreview", "inlinemath", "blockmath"]).has(node.type.toLowerCase())) return true;
	if (Array.isArray(node.content)) return node.content.some(contains_meaningful_tiptap_node);
	return false;
};

export interface ComposeEditorHandle {
	set_content: (html: string) => void;
	focus: () => void;
	has_meaningful_content: () => boolean;
	insert_image_blob: (blob: Blob) => Promise<void>;
	insert_youtube_url: (url: string) => void;
	insert_url_preview: (url: string) => void;
}

interface Props {
	hashiverse: HashiverseClientWasmProxy;
	restore_draft?: boolean;
	on_posted?: () => void | Promise<void>;
	on_submit_complete?: () => void;
	on_html_change?: (html_sanitized: string, html_raw: string) => void;
}

export const ComposeEditor = React.forwardRef<ComposeEditorHandle, Props>(({ hashiverse, restore_draft, on_posted, on_submit_complete, on_html_change }, ref) => {
	const { t } = useTranslation();
	const ref_file_input_image = useRef<HTMLInputElement>(null);
	const ref_user_search_dialog_control_manager = useRef<UserSearchDialogControlManager>(null);
	const ref_youtube_dialog_manager = useRef<YouTubeDialogControlManager | null>(null);
	const ref_giphy_dialog_manager = useRef<GiphyDialogControlManager | null>(null);
	const [text_color_menu_open, set_text_color_menu_open] = useState(false);
	const [highlight_menu_open, set_highlight_menu_open] = useState(false);
	const ref_do_submit = useRef<() => Promise<void>>(null);
	const ref_draft_save_timer = useRef<ReturnType<typeof setTimeout> | null>(null);
	const [submitting, set_submitting] = useState(false);

	const extensions = [
		StarterKit.configure({ link: false }),
		Hashtag,
		Mention.configure({ ref_user_search_dialog_control_manager, hashiverse }),
		Smilie,
		Image.configure({
			HTMLAttributes: { style: "width:100%;max-width:100%;" },
			allowBase64: true,
			inline: true,
		}),
		Youtube.configure({
			HTMLAttributes: { style: "width:100%;max-width:100%;" },
			origin: window.location.origin,
			inline: true,
			nocookie: true,
			width: MAX_EMBEDDING_WIDTH,
			modestBranding: true,
		}),
		Reply,
		Repost,
		Sequel,
		UrlPreview.configure({ hashiverse }),
		TextStyle,
		Typography,
		Color,
		Highlight.configure({ multicolor: true }),
		InlineMath,
		BlockMath,
		InlineMathEditor,
		BlockMathEditor,
		MathNodeNavigation,
		MathInputRules,
		Link,
		Superscript,
		SubScript,
		TextAlign.configure({ types: ["heading", "paragraph"] }),
		Table.configure({ resizable: true }),
		TableRow,
		TableHeader,
		TableCell,
	];

	const editor = useEditor({
		extensions,
		content: "",
		editable: true,
		shouldRerenderOnTransaction: true,
		onUpdate: ({ editor }) => {
			const raw = editor.getHTML();
			if (on_html_change) on_html_change(sanitize(raw), raw);
			if (ref_draft_save_timer.current) clearTimeout(ref_draft_save_timer.current);
			ref_draft_save_timer.current = setTimeout(() => {
				local_settings_set(LOCAL_SETTINGS_KEY_DRAFT_POST, raw).catch(() => {});
			}, 1000);
		},
		editorProps: {
			attributes: { dir: "auto", style: "min-height: 100%; cursor: text;" },
			handleKeyDown: (_view, event) => {
				if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
					event.preventDefault();
					ref_do_submit.current?.();
					return true;
				}
				return false;
			},
		},
	});

	useImperativeHandle(
		ref,
		() => ({
			set_content: (html: string) => {
				editor?.commands.setContent(html);
			},
			focus: () => {
				setTimeout(() => editor?.chain().focus("end").run(), 100);
			},
			has_meaningful_content: () => contains_meaningful_tiptap_node(editor?.getJSON()),
			insert_image_blob: async (blob: Blob) => {
				const src = await Tools.crush_image(blob, MAX_EMBEDDING_WIDTH);
				editor?.chain().focus().setImage({ src }).run();
			},
			insert_youtube_url: (url: string) => {
				editor?.chain().focus().setYoutubeVideo({ src: url }).run();
			},
			insert_url_preview: (url: string) => {
				let domain = "";
				try {
					domain = new URL(url).hostname;
				} catch {
					/* leave empty */
				}
				editor
					?.chain()
					.focus()
					.insertContent({
						type: "urlPreview",
						attrs: { url, title: "", description: "", image_url: "", domain },
					})
					.run();
			},
		}),
		[editor],
	);

	useEffect(() => {
		editor?.chain().focus().run();
		if (restore_draft && editor) {
			local_settings_get(LOCAL_SETTINGS_KEY_DRAFT_POST)
				.then((draft) => {
					if (draft) editor.commands.setContent(draft);
				})
				.catch(() => {});
		}
	}, [editor, restore_draft]);

	useEffect(() => {
		if (editor) editor.setEditable(!submitting);
	}, [editor, submitting]);

	const insert_image_file_input_on_changed = async (e: React.ChangeEvent<HTMLInputElement>) => {
		if (!e.target.files?.length) return;
		const file = e.target.files[0];
		const image = await Tools.crush_image(file, MAX_EMBEDDING_WIDTH);
		editor?.chain().focus().setImage({ src: image }).run();
		e.target.value = "";
	};

	const do_submit = async () => {
		if (submitting) return;
		try {
			set_submitting(true);
			if (!editor) return;
			if (!contains_meaningful_tiptap_node(editor.getJSON())) return;
			try {
				// Fast post: return once the User bucket secures its first commit; the rest propagates
				// in the background (surfaced by the busy indicator).
				await hashiverse.post_v2(editor.getHTML(), false);
			} catch (e) {
				console.error(e);
				Toast.error(t("toast.error_generic"));
				return;
			}
			await on_posted?.();
			Tools.play_ui_sound(sound_compose);
			editor.commands.clearContent();
			try {
				await local_settings_delete(LOCAL_SETTINGS_KEY_DRAFT_POST);
			} catch (e) {
				console.error(e);
			}
			on_submit_complete?.();
			Toast.success(t("toast.post_submitted"));
		} finally {
			set_submitting(false);
		}
	};
	ref_do_submit.current = do_submit;

	function InsertImageControl() {
		return (
			<RichTextEditor.Control
				onClick={(e) => {
					e.preventDefault();
					ref_file_input_image.current?.click();
				}}
				aria-label={t("compose.insert_image")}
				title={t("compose.insert_image")}
			>
				<IconPhoto strokeWidth={1.5} size={16} />
			</RichTextEditor.Control>
		);
	}

	function InsertHashtagControl() {
		return (
			<RichTextEditor.Control
				aria-label={t("compose.insert_hashtag")}
				title={t("compose.insert_hashtag")}
				onClick={(e) => {
					e.preventDefault();
					editor?.commands.insertHashtag();
				}}
			>
				<IconHash strokeWidth={1.5} size={16} />
			</RichTextEditor.Control>
		);
	}

	function InsertMentionControl() {
		return (
			<RichTextEditor.Control
				aria-label={t("compose.insert_mention")}
				title={t("compose.insert_mention")}
				onClick={(e) => {
					e.preventDefault();
					editor?.commands.insertMention();
				}}
			>
				<IconAt strokeWidth={1.5} size={16} />
			</RichTextEditor.Control>
		);
	}

	function InsertYouTubeControl() {
		return (
			<RichTextEditor.Control
				onClick={async (e) => {
					e.preventDefault();
					const url = await ref_youtube_dialog_manager.current?.modal_command_open();
					if (url) editor?.commands.setYoutubeVideo({ src: url });
				}}
				aria-label={t("compose.insert_youtube")}
				title={t("compose.insert_youtube")}
			>
				<IconBrandYoutube strokeWidth={1.5} size={16} />
			</RichTextEditor.Control>
		);
	}

	function InsertMathMenuControl() {
		return (
			<Menu shadow="md" position="bottom-start" trapFocus={false} returnFocus={false}>
				<Menu.Target>
					<RichTextEditor.Control aria-label={t("compose.insert_math")} title={t("compose.insert_math")}>
						<IconMath strokeWidth={1.5} size={16} />
					</RichTextEditor.Control>
				</Menu.Target>
				<Menu.Dropdown>
					<Menu.Item
						leftSection={<IconMath strokeWidth={1.5} size={16} />}
						onClick={() =>
							editor
								?.chain()
								.focus()
								.insertContent({
									type: "inlineMathEditor",
									attrs: { latex: DEFAULT_LATEX_INLINE, select_all: true },
								})
								.run()
						}
					>
						{t("compose.insert_math_inline")}
					</Menu.Item>
					<Menu.Item
						leftSection={<IconMathIntegral strokeWidth={1.5} size={16} />}
						onClick={() =>
							editor
								?.chain()
								.focus()
								.insertContent({
									type: "blockMathEditor",
									attrs: { latex: DEFAULT_LATEX_BLOCK, select_all: true },
								})
								.run()
						}
					>
						{t("compose.insert_math_block")}
					</Menu.Item>
				</Menu.Dropdown>
			</Menu>
		);
	}

	function InsertGiphyControl() {
		return (
			<RichTextEditor.Control
				onClick={async (e) => {
					e.preventDefault();
					const url = await ref_giphy_dialog_manager.current?.modal_command_open();
					if (url) editor?.commands.setImage({ src: url });
				}}
				aria-label={t("compose.insert_giphy")}
				title={t("compose.insert_giphy")}
			>
				<IconGif strokeWidth={1.5} size={16} />
			</RichTextEditor.Control>
		);
	}

	const rte_labels = {
		linkControlLabel: t("editor.link"),
		boldControlLabel: t("editor.bold"),
		italicControlLabel: t("editor.italic"),
		underlineControlLabel: t("editor.underline"),
		strikeControlLabel: t("editor.strikethrough"),
		clearFormattingControlLabel: t("editor.clear_formatting"),
		unlinkControlLabel: t("editor.unlink"),
		bulletListControlLabel: t("editor.bullet_list"),
		orderedListControlLabel: t("editor.ordered_list"),
		sourceCodeControlLabel: t("editor.source_code"),
		h1ControlLabel: t("editor.h1"),
		h2ControlLabel: t("editor.h2"),
		h3ControlLabel: t("editor.h3"),
		h4ControlLabel: t("editor.h4"),
		h5ControlLabel: t("editor.h5"),
		h6ControlLabel: t("editor.h6"),
		blockquoteControlLabel: t("editor.blockquote"),
		alignLeftControlLabel: t("editor.align_left"),
		alignCenterControlLabel: t("editor.align_center"),
		alignRightControlLabel: t("editor.align_right"),
		alignJustifyControlLabel: t("editor.align_justify"),
		codeControlLabel: t("editor.code"),
		codeBlockControlLabel: t("editor.code_block"),
		subscriptControlLabel: t("editor.subscript"),
		superscriptControlLabel: t("editor.superscript"),
		hrControlLabel: t("editor.hr"),
		undoControlLabel: t("editor.undo"),
		redoControlLabel: t("editor.redo"),
		linkEditorInputLabel: t("editor.link_input_label"),
		linkEditorInputPlaceholder: t("editor.link_input_placeholder"),
		linkEditorExternalLink: t("editor.link_external"),
		linkEditorInternalLink: t("editor.link_internal"),
		linkEditorSave: t("editor.link_save"),
	};

	function TextColorControl() {
		const active_color: string | undefined = editor?.getAttributes("textStyle").color;
		return (
			<Menu shadow="md" position="bottom-start" trapFocus={false} returnFocus={false} opened={text_color_menu_open} onChange={set_text_color_menu_open}>
				<Menu.Target>
					<RichTextEditor.Control
						aria-label={t("editor.text_color")}
						title={t("editor.text_color")}
						data-active={active_color ? true : undefined}
						style={active_color ? { background: active_color } : undefined}
					>
						<IconPalette strokeWidth={1.5} size={16} />
					</RichTextEditor.Control>
				</Menu.Target>
				<Menu.Dropdown>
					<div
						style={{
							display: "grid",
							gridTemplateColumns: "repeat(5, 1fr)",
							gap: 4,
							padding: 8,
						}}
					>
						{TEXT_COLORS.map((color) => (
							<ColorSwatch
								key={color}
								color={color}
								size={22}
								style={{ cursor: "pointer" }}
								onClick={() => {
									editor?.chain().focus().extendMarkRange("textStyle").setColor(color).run();
									set_text_color_menu_open(false);
								}}
							/>
						))}
					</div>
					<Menu.Divider />
					<Menu.Item
						onClick={() => {
							editor?.chain().focus().extendMarkRange("textStyle").unsetColor().run();
							set_text_color_menu_open(false);
						}}
					>
						{t("editor.remove_color")}
					</Menu.Item>
				</Menu.Dropdown>
			</Menu>
		);
	}

	function HighlightColorControl() {
		const active_highlight: string | undefined = editor?.getAttributes("highlight").color;
		return (
			<Menu shadow="md" position="bottom-start" trapFocus={false} returnFocus={false} opened={highlight_menu_open} onChange={set_highlight_menu_open}>
				<Menu.Target>
					<RichTextEditor.Control
						aria-label={t("editor.highlight")}
						title={t("editor.highlight")}
						data-active={active_highlight ? true : undefined}
						style={active_highlight ? { background: active_highlight } : undefined}
					>
						<IconHighlight strokeWidth={1.5} size={16} />
					</RichTextEditor.Control>
				</Menu.Target>
				<Menu.Dropdown>
					<div
						style={{
							display: "grid",
							gridTemplateColumns: "repeat(5, 1fr)",
							gap: 4,
							padding: 8,
						}}
					>
						{HIGHLIGHT_COLORS.map((color) => (
							<ColorSwatch
								key={color}
								color={color}
								size={22}
								style={{ cursor: "pointer" }}
								onClick={() => {
									editor?.chain().focus().extendMarkRange("highlight").setHighlight({ color }).run();
									set_highlight_menu_open(false);
								}}
							/>
						))}
					</div>
					<Menu.Divider />
					<Menu.Item
						onClick={() => {
							editor?.chain().focus().extendMarkRange("highlight").unsetHighlight().run();
							set_highlight_menu_open(false);
						}}
					>
						{t("editor.remove_highlight")}
					</Menu.Item>
				</Menu.Dropdown>
			</Menu>
		);
	}

	function InsertTableMenuControl() {
		const in_table = editor?.isActive("table") ?? false;
		return (
			<Menu shadow="md" position="bottom-start" trapFocus={false} returnFocus={false}>
				<Menu.Target>
					<RichTextEditor.Control aria-label={t("editor.table")} title={t("editor.table")} data-active={in_table || undefined}>
						<IconTable strokeWidth={1.5} size={16} />
					</RichTextEditor.Control>
				</Menu.Target>
				<Menu.Dropdown>
					{!in_table && (
						<Menu.Item leftSection={<IconTablePlus strokeWidth={1.5} size={16} />} onClick={() => editor?.chain().focus().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run()}>
							{t("editor.table_insert")}
						</Menu.Item>
					)}
					{in_table && (
						<>
							<Menu.Item leftSection={<IconColumnInsertLeft strokeWidth={1.5} size={16} />} onClick={() => editor?.chain().focus().addColumnBefore().run()}>
								{t("editor.table_add_col_before")}
							</Menu.Item>
							<Menu.Item leftSection={<IconColumnInsertRight strokeWidth={1.5} size={16} />} onClick={() => editor?.chain().focus().addColumnAfter().run()}>
								{t("editor.table_add_col_after")}
							</Menu.Item>
							<Menu.Item leftSection={<IconColumnRemove strokeWidth={1.5} size={16} />} onClick={() => editor?.chain().focus().deleteColumn().run()}>
								{t("editor.table_delete_col")}
							</Menu.Item>
							<Menu.Divider />
							<Menu.Item leftSection={<IconRowInsertTop strokeWidth={1.5} size={16} />} onClick={() => editor?.chain().focus().addRowBefore().run()}>
								{t("editor.table_add_row_before")}
							</Menu.Item>
							<Menu.Item leftSection={<IconRowInsertBottom strokeWidth={1.5} size={16} />} onClick={() => editor?.chain().focus().addRowAfter().run()}>
								{t("editor.table_add_row_after")}
							</Menu.Item>
							<Menu.Item leftSection={<IconRowRemove strokeWidth={1.5} size={16} />} onClick={() => editor?.chain().focus().deleteRow().run()}>
								{t("editor.table_delete_row")}
							</Menu.Item>
							<Menu.Divider />
							<Menu.Item leftSection={<IconTableOff strokeWidth={1.5} size={16} />} color="red" onClick={() => editor?.chain().focus().deleteTable().run()}>
								{t("editor.table_delete")}
							</Menu.Item>
						</>
					)}
				</Menu.Dropdown>
			</Menu>
		);
	}

	function HeadingMenuControl() {
		const active_level = HEADING_LEVELS.find((level) => editor?.isActive("heading", { level }));
		return (
			<Menu shadow="md" position="bottom-start" trapFocus={false} returnFocus={false}>
				<Menu.Target>
					<RichTextEditor.Control aria-label={t("editor.heading")} title={t("editor.heading")} data-active={active_level !== undefined || undefined}>
						<IconHeading strokeWidth={1.5} size={16} />
					</RichTextEditor.Control>
				</Menu.Target>
				<Menu.Dropdown>
					<RichTextEditor.ControlsGroup>
						<RichTextEditor.H1 />
						<RichTextEditor.H2 />
						<RichTextEditor.H3 />
						<RichTextEditor.H4 />
						<RichTextEditor.H5 />
						<RichTextEditor.H6 />
					</RichTextEditor.ControlsGroup>
				</Menu.Dropdown>
			</Menu>
		);
	}

	const toolbar = (
		<RichTextEditor.Toolbar
			sticky
			stickyOffset="var(--docs-header-height)"
			style={{
				display: "flex",
				flexDirection: "row",
				justifyContent: "space-between",
				alignItems: "center",
			}}
		>
			<RichTextEditor.ControlsGroup>
				<InsertHashtagControl />
				<InsertMentionControl />
				<InsertImageControl />
				<InsertYouTubeControl />
				<InsertGiphyControl />
			</RichTextEditor.ControlsGroup>

			<RichTextEditor.ControlsGroup>
				<RichTextEditor.Bold />
				<RichTextEditor.Italic />
				<RichTextEditor.Underline />
				<RichTextEditor.Strikethrough />
				<RichTextEditor.Code />
			</RichTextEditor.ControlsGroup>

			<RichTextEditor.ControlsGroup>
				<RichTextEditor.Link />
				<RichTextEditor.Subscript />
				<RichTextEditor.Superscript />
				<RichTextEditor.BulletList />
				<RichTextEditor.OrderedList />
			</RichTextEditor.ControlsGroup>

			<RichTextEditor.ControlsGroup>
				{HeadingMenuControl()}
				<RichTextEditor.CodeBlock />
				<RichTextEditor.Blockquote />
				{InsertMathMenuControl()}
				{InsertTableMenuControl()}
			</RichTextEditor.ControlsGroup>

			<RichTextEditor.ControlsGroup>
				<RichTextEditor.AlignLeft />
				<RichTextEditor.AlignCenter />
				<RichTextEditor.AlignJustify />
				<RichTextEditor.AlignRight />
				<RichTextEditor.Hr />
			</RichTextEditor.ControlsGroup>

			<RichTextEditor.ControlsGroup>
				{TextColorControl()}
				{HighlightColorControl()}
				<RichTextEditor.Control
					onClick={() => editor?.chain().focus().clearNodes().unsetAllMarks().run()}
					aria-label={t("editor.clear_formatting")}
					title={t("editor.clear_formatting")}
					disabled={editor?.state.selection.empty ?? true}
				>
					<IconEraser strokeWidth={1.5} size={16} />
				</RichTextEditor.Control>
				<RichTextEditor.Undo />
				<RichTextEditor.Redo />
			</RichTextEditor.ControlsGroup>
		</RichTextEditor.Toolbar>
	);

	const submit_button = submitting ? (
		<Spinner size={64} />
	) : (
		<Tooltip label={t("compose.send")}>
			<ActionIcon
				onClick={(e) => {
					e.preventDefault();
					do_submit();
				}}
				variant="gradient"
				gradient={{ from: "blue", to: "cyan", deg: 90 }}
				size="64"
				radius="32"
				aria-label={t("compose.send")}
			>
				<IconSend style={{ width: "60%", height: "60%" }} stroke={1.5} />
			</ActionIcon>
		</Tooltip>
	);

	const file_input = <input ref={ref_file_input_image} type="file" accept="image/*" onChange={insert_image_file_input_on_changed} style={{ display: "none" }} />;

	return (
		<div className="FullColumnChildAndParent" style={{ position: "relative" }}>
			<UserSearchDialogControl ref_user_search_dialog_control_manager={ref_user_search_dialog_control_manager} hashiverse={hashiverse} />
			<YouTubeDialogControl ref_manager={ref_youtube_dialog_manager} />
			<GiphyDialogControl ref_manager={ref_giphy_dialog_manager} />
			<RichTextEditor editor={editor} variant="subtle" className="FullColumnChildAndParent" labels={rte_labels}>
				{toolbar}
				<RichTextEditor.Content className="FullColumnChild" />
			</RichTextEditor>
			<div
				style={{
					position: "absolute",
					bottom: "calc(30px + var(--keyboard-inset-bottom, 0px))",
					right: "30px",
					zIndex: 10,
				}}
			>
				{submit_button}
			</div>
			{file_input}
		</div>
	);
});
