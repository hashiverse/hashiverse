import { Node } from "@tiptap/core";
import type { NodeViewRendererProps } from "@tiptap/react";
import i18n from "i18next";

export const Sequel = Node.create({
	name: "sequel",
	group: "block",
	atom: true,
	selectable: true,
	draggable: false,

	addAttributes() {
		return {
			post_id: { default: null },
			bucket_location: { default: null },
			client_id: { default: null },
			post_header_hex: {
				default: "",
				renderHTML: () => ({}), // never rendered as a DOM attribute (too large)
			},
		};
	},

	parseHTML() {
		return [
			{
				tag: "sequel",
				getAttrs: (element) => {
					const el = element as HTMLElement;
					return {
						post_id: el.getAttribute("post_id"),
						bucket_location: el.getAttribute("bucket_location"),
						client_id: el.getAttribute("client_id"),
						post_header_hex: el.getAttribute("post_header_hex") || "",
					};
				},
			},
		];
	},

	renderHTML({ node }) {
		const el = document.createElement("sequel");
		if (node.attrs.post_id) el.setAttribute("post_id", node.attrs.post_id);
		if (node.attrs.bucket_location) el.setAttribute("bucket_location", node.attrs.bucket_location);
		if (node.attrs.client_id) el.setAttribute("client_id", node.attrs.client_id);
		if (node.attrs.post_header_hex) el.setAttribute("post_header_hex", node.attrs.post_header_hex);
		return el;
	},

	addNodeView() {
		return ({ node }: NodeViewRendererProps) => {
			const dom = document.createElement("div");
			dom.className = "plugin-sequel";
			if (node.attrs.post_id) dom.dataset.postId = node.attrs.post_id;
			if (node.attrs.bucket_location) dom.dataset.bucketLocation = node.attrs.bucket_location;
			if (node.attrs.client_id) dom.dataset.userId = node.attrs.client_id;
			dom.textContent = i18n.t("post.sequel_to_previous");
			return { dom };
		};
	},
});
