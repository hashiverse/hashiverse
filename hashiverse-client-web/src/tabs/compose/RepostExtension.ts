import { Node } from "@tiptap/core";
import type { NodeViewRendererProps } from "@tiptap/react";

export const Repost = Node.create({
	name: "repost",
	group: "block",
	atom: true, // truly indivisible: cursor cannot enter, Delete removes the whole block
	selectable: true,
	draggable: false,

	addAttributes() {
		return {
			post_id: { default: null },
			bucket_location: { default: null },
			client_id: { default: null },
			inner_html: {
				default: "",
				renderHTML: () => ({}), // never rendered as a DOM attribute
			},
		};
	},

	parseHTML() {
		return [
			{
				tag: "repost",
				getAttrs: (element) => {
					const el = element as HTMLElement;
					return {
						post_id: el.getAttribute("post_id"),
						bucket_location: el.getAttribute("bucket_location"),
						client_id: el.getAttribute("client_id"),
						inner_html: el.innerHTML,
					};
				},
			},
		];
	},

	// Return a real DOM element so the stored HTML round-trips correctly.
	renderHTML({ node }) {
		const el = document.createElement("repost");
		if (node.attrs.post_id) el.setAttribute("post_id", node.attrs.post_id);
		if (node.attrs.bucket_location) el.setAttribute("bucket_location", node.attrs.bucket_location);
		if (node.attrs.client_id) el.setAttribute("client_id", node.attrs.client_id);
		el.innerHTML = node.attrs.inner_html || "";
		return el;
	},

	addNodeView() {
		return ({ node }: NodeViewRendererProps) => {
			const dom = document.createElement("div");
			dom.className = "plugin-repost";
			if (node.attrs.post_id) dom.dataset.postId = node.attrs.post_id;
			if (node.attrs.bucket_location) dom.dataset.bucketLocation = node.attrs.bucket_location;
			if (node.attrs.client_id) dom.dataset.userId = node.attrs.client_id;
			dom.innerHTML = node.attrs.inner_html || "";
			return { dom }; // no contentDOM — ProseMirror never manages children
		};
	},
});
