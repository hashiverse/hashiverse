import { type CommandProps, mergeAttributes, Node } from "@tiptap/core";
import { Plugin, TextSelection, type Transaction } from "@tiptap/pm/state";
import type { EditorView } from "@tiptap/pm/view";
import { type NodeViewRendererProps, nodePasteRule } from "@tiptap/react";
import type { RefObject } from "react";
import type { HashiverseClientWasmProxy } from "../../Hashiverse.ts";
import { populate_mention_bio } from "../../tools/BioCache.ts";
import type { UserSearchDialogControlManager } from "./UserSearchDialogControl.tsx";

const PLUGIN_NAME = "Mention";

// Helps with intellisense
declare module "@tiptap/core" {
	interface Commands<ReturnType> {
		mention: {
			insertMention: () => ReturnType;
		};
	}
}

export interface MentionOptions {
	ref_user_search_dialog_control_manager: RefObject<UserSearchDialogControlManager | null>;
	hashiverse: HashiverseClientWasmProxy | null;
}

export const Mention = Node.create<MentionOptions>({
	name: PLUGIN_NAME,
	marks: "",
	group: "inline",
	inline: true,
	atom: true,
	content: "",
	draggable: true,
	selectable: true,

	addAttributes() {
		return {
			client_id: {
				default: null,
			},
		};
	},

	parseHTML() {
		return [
			{
				tag: "mention",
			},
		];
	},

	renderHTML({ HTMLAttributes }) {
		return ["mention", mergeAttributes(HTMLAttributes)];
	},

	addNodeView() {
		return ({ node }: NodeViewRendererProps) => {
			const client_id = node.attrs.client_id as string;

			const dom = document.createElement("span");
			dom.className = "plugin-nowrap";
			dom.style.display = "inline-block";

			const img = document.createElement("img");
			img.contentEditable = "false";
			img.style.cssText = "width:22px;height:22px;border-radius:50%;object-fit:cover;vertical-align:text-top;";

			const content = document.createElement("span");
			content.contentEditable = "false";
			content.className = "plugin-mention-right";

			const label = document.createElement("span");
			label.contentEditable = "false";
			label.className = "plugin-mention-left";
			label.appendChild(img);

			dom.append(label, content);

			if (this.options.hashiverse) populate_mention_bio(this.options.hashiverse, client_id, img, content);

			return { dom };
		};
	},

	addCommands() {
		return {
			insertMention:
				() =>
				({ editor, view }: CommandProps) => {
					// Launch our async function
					(async () => {
						editor.setEditable(false);

						try {
							console.log("this.options", this.options);
							const bio = await this.options.ref_user_search_dialog_control_manager.current?.modal_command_open();
							console.log("Dialog returned", bio);

							if (bio) {
								const { state, dispatch } = view;
								const { $from } = state.selection;

								const attrs = {
									client_id: bio.client_id,
								};

								let tr: Transaction = state.tr;
								tr = tr.insert($from.pos, state.schema.node(PLUGIN_NAME, attrs));
								tr = tr.insertText(" ", $from.pos + 1);
								tr = tr.setSelection(TextSelection.create(tr.doc, $from.pos + 2));
								dispatch(tr);
							}
						} finally {
							//
							editor.setEditable(true);
							editor.chain().focus().run();
						}
					})();

					return true;
				},
		};
	},

	addPasteRules() {
		return [
			nodePasteRule({
				find: /@(\w+)/g,
				type: this.type,
				getAttributes: (match) => ({
					tag: match[1],
				}),
				getContent: (match) => {
					console.log("MentionExtension.addPasteRules.getContent", match);
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
		const editor = this.editor;

		return [
			new Plugin({
				props: {
					handleTextInput(_view: EditorView, _from: number, _to: number, text: string) {
						// Are we adding a node?
						if (text === "@") {
							editor.chain().focus().insertMention().run();
							return true;
						}

						return false;
					},
				},
			}),
		];
	},
});
