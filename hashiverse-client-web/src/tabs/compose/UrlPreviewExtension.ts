import { Node } from "@tiptap/core";
import { type NodeViewRendererProps, nodePasteRule } from "@tiptap/react";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";

const PLUGIN_NAME = "urlPreview";

// Matches a line that consists solely of an https:// URL, excluding YouTube URLs
// (those are handled by TipTap's Youtube extension which embeds them as video players).
const URL_PASTE_REGEX = /^(https?:\/\/(?!(?:www\.)?(?:youtube\.com|youtu\.be)\/)\S+)$/g;

export type PreviewAttrs = {
	url: string;
	title: string;
	description: string;
	image_url: string;
	domain: string;
};

export interface UrlPreviewOptions {
	hashiverse: HashiverseClientWasmProxy | null;
}

// Builds a self-contained card DOM element that renders correctly in any browser context.
// Uses CSS classes from index.css — used by both renderHTML (post storage) and addNodeView (editor).
// on_dismiss is only provided in the editor NodeView; it adds an X button inside the image container (or card).
function build_card_dom(attrs: PreviewAttrs, on_dismiss?: () => void): HTMLElement {
	const card = document.createElement("div");
	card.className = "plugin-urlpreview-card";

	if (attrs.title && attrs.image_url) {
		const image_container = document.createElement("div");
		image_container.className = "plugin-urlpreview-card-image-container";

		const img = document.createElement("img");
		img.src = attrs.image_url;
		img.alt = "";
		img.className = "plugin-urlpreview-card-image";
		image_container.appendChild(img);

		const domain_el = document.createElement("div");
		domain_el.className = "plugin-urlpreview-card-domain";
		domain_el.textContent = attrs.domain || attrs.url || "";
		image_container.appendChild(domain_el);

		if (on_dismiss) {
			const dismiss_button = document.createElement("button");
			dismiss_button.className = "plugin-urlpreview-dismiss";
			dismiss_button.textContent = "×";
			dismiss_button.addEventListener("click", on_dismiss);
			image_container.appendChild(dismiss_button);
		}

		card.appendChild(image_container);
	}

	if (on_dismiss && !attrs.image_url) {
		const dismiss_button = document.createElement("button");
		dismiss_button.className = "plugin-urlpreview-dismiss";
		dismiss_button.textContent = "×";
		dismiss_button.addEventListener("click", on_dismiss);
		card.appendChild(dismiss_button);
	}

	const inner = document.createElement("div");
	inner.className = "plugin-urlpreview-card-inner";

	if (!attrs.image_url) {
		const domain_el = document.createElement("div");
		domain_el.className = "plugin-urlpreview-card-domain";
		domain_el.textContent = attrs.domain || attrs.url || "";
		inner.appendChild(domain_el);
	}

	if (attrs.title) {
		const title_el = document.createElement("a");
		title_el.className = "plugin-urlpreview-card-title";
		title_el.textContent = attrs.title;
		title_el.href = attrs.url;
		title_el.target = "_blank";
		title_el.rel = "noopener noreferrer nofollow";
		inner.appendChild(title_el);

		if (attrs.description) {
			const desc_el = document.createElement("div");
			desc_el.className = "plugin-urlpreview-card-description";
			desc_el.textContent = attrs.description;
			inner.appendChild(desc_el);
		}
	} else {
		const loading_el = document.createElement("div");
		loading_el.className = "plugin-urlpreview-card-loading";
		loading_el.textContent = attrs.url ? "Loading preview…" : "Preview unavailable";
		inner.appendChild(loading_el);
	}

	card.appendChild(inner);
	return card;
}

export const UrlPreview = Node.create<UrlPreviewOptions>({
	name: PLUGIN_NAME,
	group: "block",
	atom: true,
	selectable: true,
	draggable: false,

	addOptions() {
		return {
			hashiverse: null,
		};
	},

	addAttributes() {
		return {
			url: { default: "" },
			title: { default: "" },
			description: { default: "" },
			image_url: { default: "" },
			domain: { default: "" },
		};
	},

	parseHTML() {
		return [
			{
				tag: "urlpreview",
				getAttrs: (element) => {
					const el = element as HTMLElement;
					return {
						url: el.getAttribute("url") ?? "",
						title: el.getAttribute("title") ?? "",
						description: el.getAttribute("description") ?? "",
						image_url: el.getAttribute("image_url") ?? "",
						domain: el.getAttribute("domain") ?? "",
					};
				},
			},
		];
	},

	// Outputs a <urlpreview> wrapper (for round-trip attribute preservation) that contains
	// a fully-rendered card as children (so it displays correctly in any HTML context).
	renderHTML({ node }) {
		const attrs = node.attrs as PreviewAttrs;
		const wrapper = document.createElement("urlpreview");
		if (attrs.url) wrapper.setAttribute("url", attrs.url);
		if (attrs.title) wrapper.setAttribute("title", attrs.title);
		if (attrs.description) wrapper.setAttribute("description", attrs.description);
		if (attrs.image_url) wrapper.setAttribute("image_url", attrs.image_url);
		if (attrs.domain) wrapper.setAttribute("domain", attrs.domain);
		wrapper.appendChild(build_card_dom(attrs));
		return wrapper;
	},

	addNodeView() {
		return ({ node, getPos, editor }: NodeViewRendererProps) => {
			const dom = document.createElement("div");
			dom.className = "plugin-url-preview";

			const on_dismiss = () => {
				const pos = getPos();
				if (pos === undefined) return;
				const url = node.attrs.url as string;
				editor
					.chain()
					.focus()
					.deleteRange({ from: pos, to: pos + node.nodeSize })
					.insertContentAt(pos, {
						type: "text",
						text: url,
						marks: [{ type: "link", attrs: { href: url } }],
					})
					.run();
			};

			const render = (attrs: PreviewAttrs) => {
				dom.innerHTML = "";
				dom.appendChild(build_card_dom(attrs, on_dismiss));
			};

			render(node.attrs as PreviewAttrs);

			// Trigger async fetch if we have a URL but no title yet
			if (node.attrs.url && !node.attrs.title && this.options.hashiverse) {
				this.options.hashiverse
					.fetch_url_preview_v1(node.attrs.url as string)
					.then((preview: { url: string; title: string; description: string; image_url: string }) => {
						const new_attrs: PreviewAttrs = {
							url: preview.url || (node.attrs.url as string),
							title: preview.title,
							description: preview.description,
							image_url: preview.image_url,
							domain: node.attrs.domain as string,
						};
						const pos = getPos();
						if (pos === undefined) return;
						editor.view.dispatch(editor.view.state.tr.setNodeMarkup(pos, undefined, new_attrs));
						render(new_attrs);
					})
					.catch(() => {
						const loading_el = dom.querySelector(".plugin-urlpreview-card-loading");
						if (loading_el) loading_el.textContent = "Preview unavailable";
					});
			}

			return { dom };
		};
	},

	addPasteRules() {
		return [
			nodePasteRule({
				find: URL_PASTE_REGEX,
				type: this.type,
				getAttributes: (match) => {
					const url = match[1] ?? match[0];
					let domain = "";
					try {
						domain = new URL(url).hostname;
					} catch {
						// leave domain empty
					}
					return { url, title: "", description: "", image_url: "", domain };
				},
			}),
		];
	},
});
