import { type CommandProps, mergeAttributes, Node } from "@tiptap/core";
import type { Node as PMNode } from "@tiptap/pm/model";
import { Plugin, TextSelection, type Transaction } from "@tiptap/pm/state";
import type { EditorView } from "@tiptap/pm/view";
import { type NodeViewRendererProps, nodePasteRule } from "@tiptap/react";

const PLUGIN_NAME = "Hashtag";

// Helps with intellisense
declare module "@tiptap/core" {
	interface Commands<ReturnType> {
		hashtag: {
			insertHashtag: () => ReturnType;
		};
	}
}

export const Hashtag = Node.create({
	name: PLUGIN_NAME,
	marks: "",
	group: "inline",
	inline: true,
	content: "text*",
	draggable: true,
	selectable: true,

	addAttributes() {
		return {
			hashtag: {
				default: null,
			},
		};
	},

	parseHTML() {
		return [
			{
				tag: "Hashtag",
				contentElement: ".plugin-hashtag-right",
			},
		];
	},

	renderHTML({ HTMLAttributes, node }) {
		return ["Hashtag", mergeAttributes(HTMLAttributes, { hashtag: node.textContent || "" }), ["span", { class: "plugin-hashtag-left" }, "#"], ["span", { class: "plugin-hashtag-right" }, 0]];
	},

	addNodeView() {
		return ({ node }: NodeViewRendererProps) => {
			const dom = document.createElement("span");
			dom.className = "plugin-nowrap";

			const label = document.createElement("span");
			label.contentEditable = "false";
			label.className = "plugin-hashtag-left";
			label.innerHTML = "#";

			// Create a container for the content
			const content = document.createElement("span");
			content.className = "plugin-hashtag-right";
			content.innerHTML = node.attrs.hashtag;

			dom.append(label, content);

			return {
				dom,
				contentDOM: content,
			};
		};
	},

	addCommands() {
		return {
			insertHashtag:
				() =>
				({ tr, state, dispatch }: CommandProps) => {
					if (dispatch) {
						tr.insert(tr.selection.from, state.schema.node(PLUGIN_NAME));
					}

					return true;
				},
		};
	},

	addPasteRules() {
		return [
			nodePasteRule({
				find: /#(\w+)/g,
				type: this.type,
				getAttributes: (match) => ({
					tag: match[1],
				}),
				getContent: (match) => {
					console.log("HashtagExtension.addPasteRules.getContent", match);
					return [
						{
							type: "text",
							text: match.tag,
						},
					];
				},
			}),
		];
	},

	addProseMirrorPlugins() {
		return [
			new Plugin({
				props: {
					handleTextInput(view: EditorView, _from: number, _to: number, text: string) {
						const { state, dispatch } = view;
						const { $from } = state.selection;
						const node_current: PMNode = $from.parent;

						// Break out of the current hashtag because of a non alphanumeric
						const is_alphanumeric = /^[\p{L}\p{N}]+$/u.test(text);
						const is_in_node_hashtag = node_current.type.name === PLUGIN_NAME;
						const is_in_empty_node = node_current.textContent.trim().length === 0;
						const is_creation_request = text === "#";

						// What are the cases where we let tiptap handle the keyboard event as normal?
						if (!is_in_node_hashtag && !is_creation_request) return false;
						if (is_in_node_hashtag && is_alphanumeric) return false;

						// Creating from scratch outside an existing node
						if (!is_in_node_hashtag && is_creation_request) {
							let tr: Transaction = state.tr;
							tr = tr.insert($from.pos, state.schema.node(PLUGIN_NAME));
							tr = tr.setSelection(TextSelection.create(tr.doc, $from.pos + 1));
							dispatch(tr);
							return true; // Suppress key
						}

						// If we are breaking out of an empty node, just kill it, insert a hash and potentially another hash node
						if (is_in_node_hashtag && is_in_empty_node) {
							let tr: Transaction = state.tr;
							tr = tr.replaceWith($from.before(), $from.after(), state.schema.text("#"));

							if (is_creation_request) {
								tr = tr.insert($from.before() + 1, state.schema.node(PLUGIN_NAME));
								tr = tr.setSelection(TextSelection.create(tr.doc, $from.before() + 2));
								dispatch(tr);
								return true; // Suppress key
							} else {
								// !is_creation_request
								dispatch(tr);
								return false; // Don't suppress key
							}
						}

						// If we are breaking out of a full hash node, append the key after the hash node and select it!
						if (is_in_node_hashtag && !is_in_empty_node) {
							if (is_creation_request) {
								let tr: Transaction = state.tr;
								tr = tr.insert($from.after(), state.schema.node(PLUGIN_NAME));
								tr = tr.setSelection(TextSelection.create(tr.doc, $from.after() + 1));
								dispatch(tr);
								return true; // Suppress key
							} else {
								// !is_creation_request
								let tr: Transaction = state.tr;
								tr = tr.insertText(text, $from.after());
								tr = tr.setSelection(TextSelection.create(tr.doc, $from.after() + 1));
								dispatch(tr);
							}
							return true; // Suppress key
						}

						return false;
					},
				},
			}),
		];
	},
});
