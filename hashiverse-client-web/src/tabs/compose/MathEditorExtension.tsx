import { Extension, InputRule, mergeAttributes, Node } from "@tiptap/core";
import type { Node as ProseMirrorNode } from "@tiptap/pm/model";
import { NodeSelection, Plugin } from "@tiptap/pm/state";
import type { EditorView } from "@tiptap/pm/view";
import type { NodeViewRendererProps } from "@tiptap/react";
import { NodeViewWrapper, ReactNodeViewRenderer } from "@tiptap/react";
import type React from "react";
import { useLayoutEffect, useRef } from "react";

export const DEFAULT_LATEX_INLINE = "y = ax^2 + bx + c";
export const DEFAULT_LATEX_BLOCK =
	"" +
	"\\begin{aligned}" +
	"  \\nabla \\cdot \\mathbf{E} &= \\frac{\\rho}{\\epsilon_0}, \\quad \\\\" +
	"  \\nabla \\cdot \\mathbf{B} &= 0, \\quad \\\\" +
	"  \\nabla \\times \\mathbf{E} &= -\\frac{\\partial \\mathbf{B}}{\\partial t}, \\quad \\\\" +
	"  \\nabla \\times \\mathbf{B} &= \\mu_0\\mathbf{J} + \\mu_0\\epsilon_0\\frac{\\partial \\mathbf{E}}{\\partial t}" +
	"\\end{aligned}";

const MATH_NODE_NAMES = new Set(["inlineMath", "blockMath"]);
function replace_with_editor(view: EditorView, node: ProseMirrorNode, node_pos: number, cursor_at_start: boolean): boolean {
	const editor_type_name = node.type.name === "blockMath" ? "blockMathEditor" : "inlineMathEditor";
	const editor_type = view.state.schema.nodes[editor_type_name];
	if (!editor_type) return false;
	view.dispatch(view.state.tr.replaceWith(node_pos, node_pos + node.nodeSize, editor_type.create({ latex: node.attrs.latex, cursor_at_start })));
	return true;
}

// Handles click-to-edit and arrow-key navigation into math nodes.
export const MathNodeNavigation = Extension.create({
	name: "mathNodeNavigation",

	addKeyboardShortcuts() {
		return {
			ArrowRight: () => {
				const { selection } = this.editor.state;
				if (!selection.empty) return false;
				const node_after = selection.$from.nodeAfter;
				if (!node_after || !MATH_NODE_NAMES.has(node_after.type.name)) return false;
				return replace_with_editor(this.editor.view, node_after, selection.$from.pos, true);
			},
			ArrowLeft: () => {
				const { selection } = this.editor.state;
				if (!selection.empty) return false;
				const node_before = selection.$from.nodeBefore;
				if (!node_before || !MATH_NODE_NAMES.has(node_before.type.name)) return false;
				return replace_with_editor(this.editor.view, node_before, selection.$from.pos - node_before.nodeSize, false);
			},
		};
	},

	addProseMirrorPlugins() {
		return [
			new Plugin({
				props: {
					// handleClickOn returning true prevents ProseMirror from focusing itself after the click
					handleClickOn(view, _pos, node, node_pos, _event, direct) {
						if (!direct || !MATH_NODE_NAMES.has(node.type.name)) return false;
						return replace_with_editor(view, node, node_pos, true);
					},
				},
				// appendTransaction catches NodeSelections created by keyboard navigation (e.g. ArrowDown into blockMath)
				appendTransaction(transactions, old_state, new_state) {
					if (new_state.selection.eq(old_state.selection)) return null;
					// Don't auto-open editor on paste — pasted math nodes should stay as render nodes
					if (transactions.some((tr) => tr.getMeta("paste"))) return null;
					const { selection } = new_state;
					if (!(selection instanceof NodeSelection)) return null;
					const { node, from } = selection;
					if (!MATH_NODE_NAMES.has(node.type.name)) return null;
					const cursor_at_start = old_state.selection.from <= from;
					const editor_type_name = node.type.name === "blockMath" ? "blockMathEditor" : "inlineMathEditor";
					const editor_type = new_state.schema.nodes[editor_type_name];
					if (!editor_type) return null;
					return new_state.tr.replaceWith(from, from + node.nodeSize, editor_type.create({ latex: node.attrs.latex, cursor_at_start }));
				},
			}),
		];
	},
});

function MathEditorView({ node, getPos, editor }: NodeViewRendererProps) {
	const ref_textarea = useRef<HTMLTextAreaElement>(null);
	const ref_has_focused = useRef(false);
	const ref_focus_timer = useRef<ReturnType<typeof setTimeout> | null>(null);
	const ref_accept = useRef<(focus_after?: "before" | "after") => void>(() => {});
	const ref_accepted = useRef(false);
	const is_block = node.type.name === "blockMathEditor";
	const cursor_at_start = node.attrs.cursor_at_start as boolean;
	const select_all = node.attrs.select_all as boolean;

	useLayoutEffect(() => {
		const textarea = ref_textarea.current;
		if (!textarea) return;
		const focus_and_position = () => {
			textarea.focus();
			ref_has_focused.current = true;
			if (select_all) {
				textarea.select();
			} else {
				const len = textarea.value.length;
				const cursor_pos = cursor_at_start ? 0 : len;
				textarea.setSelectionRange(cursor_pos, cursor_pos);
			}
		};
		focus_and_position();
		// Defer a second call so it fires after ProseMirror finishes its keyboard
		// event handling, which would otherwise re-steal focus.
		ref_focus_timer.current = setTimeout(focus_and_position, 0);
		return () => {
			if (ref_focus_timer.current) clearTimeout(ref_focus_timer.current);
		};
	}, [cursor_at_start, select_all]);

	const replace_with_math = (latex: string, pos: number): void => {
		if (is_block) {
			const target_type = editor.schema.nodes.blockMath;
			if (!target_type) return;
			const math_node = target_type.create({ latex });
			const after_pos = pos + math_node.nodeSize;
			const is_last_node = after_pos >= editor.state.doc.content.size;
			const chain = editor.chain().command(({ tr }) => {
				tr.replaceWith(pos, pos + node.nodeSize, math_node);
				return true;
			});
			if (is_last_node) chain.insertContentAt(after_pos, { type: "paragraph" });
			chain.run();
		} else {
			const target_type = editor.schema.nodes.inlineMath;
			if (!target_type) return;
			editor.view.dispatch(editor.state.tr.replaceWith(pos, pos + node.nodeSize, target_type.create({ latex })));
		}
	};

	const accept = (focus_after?: "before" | "after") => {
		if (ref_accepted.current) return;
		ref_accepted.current = true;
		const pos = getPos();
		if (pos === undefined) return;
		const latex = ref_textarea.current?.value ?? (node.attrs.latex as string);
		replace_with_math(latex, pos);
		if (focus_after === "before") {
			editor.commands.focus(pos - 1);
		} else if (focus_after === "after") {
			const node_size = is_block ? editor.schema.nodes.blockMath.create({ latex }).nodeSize : editor.schema.nodes.inlineMath.create({ latex }).nodeSize;
			editor.commands.focus(pos + node_size);
		}
	};
	ref_accept.current = accept;

	const textarea_style: React.CSSProperties = {
		fontFamily: "monospace",
		fontSize: "0.9em",
		padding: "2px 6px",
		border: "1px solid var(--mantine-color-default-border)",
		borderRadius: 4,
		minWidth: 160,
		minHeight: 60,
		resize: "vertical",
		width: "100%",
		background: "var(--mantine-color-default)",
		color: "var(--mantine-color-text)",
		display: "block",
	};

	return (
		<NodeViewWrapper as={is_block ? "div" : "span"} style={is_block ? { padding: "4px 0" } : { display: "inline-block" }}>
			<textarea
				ref={ref_textarea}
				defaultValue={node.attrs.latex as string}
				style={textarea_style}
				onMouseDown={(e) => e.stopPropagation()}
				onBlur={() => {
					if (!ref_has_focused.current) return;
					// Cancel any pending re-focus — user is intentionally leaving the editor.
					if (ref_focus_timer.current) {
						clearTimeout(ref_focus_timer.current);
						ref_focus_timer.current = null;
					}
					accept();
				}}
				onKeyDown={(e) => {
					if (e.key === "Escape") {
						e.preventDefault();
						e.stopPropagation();
						accept(cursor_at_start ? "after" : "before");
					}
					const ta = ref_textarea.current;
					if (!ta) return;
					const { selectionStart: ss, selectionEnd: se, value } = ta;
					const no_sel = ss === se;
					if (e.key === "ArrowLeft" && no_sel && ss === 0) {
						e.preventDefault();
						accept("before");
					}
					if (e.key === "ArrowUp" && no_sel && value.lastIndexOf("\n", ss - 1) === -1) {
						e.preventDefault();
						accept("before");
					}
					if (e.key === "ArrowRight" && no_sel && ss === value.length) {
						e.preventDefault();
						accept("after");
					}
					if (e.key === "ArrowDown" && no_sel && value.indexOf("\n", ss) === -1) {
						e.preventDefault();
						accept("after");
					}
				}}
			/>
		</NodeViewWrapper>
	);
}

const math_editor_attributes = {
	latex: { default: "" },
	cursor_at_start: { default: false },
	select_all: { default: false },
};

export const InlineMathEditor = Node.create({
	name: "inlineMathEditor",
	group: "inline",
	inline: true,
	atom: true,

	addAttributes() {
		return math_editor_attributes;
	},

	parseHTML() {
		return [{ tag: 'span[data-type="inline-math-editor"]' }];
	},

	renderHTML({ HTMLAttributes }) {
		return ["span", mergeAttributes(HTMLAttributes, { "data-type": "inline-math-editor" })];
	},

	addNodeView() {
		return ReactNodeViewRenderer(MathEditorView);
	},
});

export const BlockMathEditor = Node.create({
	name: "blockMathEditor",
	group: "block",
	atom: true,

	addAttributes() {
		return math_editor_attributes;
	},

	parseHTML() {
		return [{ tag: 'div[data-type="block-math-editor"]' }];
	},

	renderHTML({ HTMLAttributes }) {
		return ["div", mergeAttributes(HTMLAttributes, { "data-type": "block-math-editor" })];
	},

	addNodeView() {
		return ReactNodeViewRenderer(MathEditorView);
	},
});

// Typing $$$x inserts a block math editor; $$x (not preceded by $) inserts inline.
// The triggering character is consumed; the math editor opens ready to type.
export const MathInputRules = Extension.create({
	name: "mathInputRules",

	addInputRules() {
		return [
			new InputRule({
				find: /\$\$\$([^\s$])$/,
				handler: ({ range, match, commands }) => {
					commands.insertContentAt(range, {
						type: "blockMathEditor",
						attrs: {
							latex: match[1],
							select_all: false,
							cursor_at_start: false,
						},
					});
				},
			}),
			new InputRule({
				find: /(?<!\$)\$\$([^\s$])$/,
				handler: ({ range, match, commands }) => {
					commands.insertContentAt(range, {
						type: "inlineMathEditor",
						attrs: {
							latex: match[1],
							select_all: false,
							cursor_at_start: false,
						},
					});
				},
			}),
		];
	},
});
