import DOMPurify from "dompurify";

DOMPurify.addHook("uponSanitizeElement", (node: Node, data) => {
	const tag_name = data.tagName?.toLowerCase();
	if ("iframe" !== tag_name) return;

	const fail_and_exit = (reason?: string) => {
		if (false && reason) console.log("fail_and_exit", reason);
		node.parentNode?.removeChild(node);
	};

	const element: Element = node as Element;
	if (!element) return fail_and_exit("not an element");

	const src = element.getAttribute("src") || "";
	const src_lower = src.toLowerCase();
	if (!src_lower.startsWith("https://www.youtube-nocookie.com/embed/")) return fail_and_exit("url problem");
});

// Links in posts point off-site: open them in a new window so Hashiverse stays open behind them.
// Set here rather than allowed through (target is not in ADD_ATTR), so an author can never choose some other target or drop the rel.
DOMPurify.addHook("afterSanitizeAttributes", (node: Element) => {
	if ("A" !== node.tagName) return;
	const href = node.getAttribute("href") || "";
	if (!/^https?:\/\//i.test(href)) return;
	node.setAttribute("target", "_blank");
	node.setAttribute("rel", "noopener noreferrer nofollow");
});

const purify_config = {
	ADD_TAGS: ["iframe", "hashtag", "mention", "reply", "repost", "sequel", "urlpreview", "colgroup", "col"],
	ADD_ATTR: [
		"hashtag",
		"post_id",
		"bucket_location",
		"client_id",
		"post_header_hex",
		"url",
		"title",
		"description",
		"image_url",
		"domain",
		"data-latex",
		"data-type",
		"colspan",
		"rowspan",
		"colwidth",
	],
};

function sanitize(html: string) {
	return DOMPurify.sanitize(html, purify_config);
}

export { sanitize };
