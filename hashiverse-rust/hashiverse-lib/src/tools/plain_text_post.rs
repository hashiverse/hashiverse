//! # Plain-text → hashiverse HTML conversion
//!
//! Hashiverse posts are stored and transmitted as a constrained subset of HTML (so that
//! rich posts from the web client, API clients, and plain-text API clients are all the
//! same format on the wire). This module provides the one-way convenience path for
//! callers that have nothing but a string of text — mainly the Python client, plain-text
//! API integrations, and quick CLI posts.
//!
//! The output is the same HTML shape produced by the Tiptap editor in the web client:
//! HTML-escaped body, `#hashtag` tokens rewritten as `<hashtag>` elements, `@<64-hex-id>`
//! mentions rewritten as `<mention>` elements, and literal newlines turned into `<br>`.
//! `submit_post()` then parses the result into the canonical on-wire representation.

/// Converts a plain-text post into well-formed HTML that `submit_post()` can parse.
///
/// - HTML-escapes `<`, `>`, `&`, `"` in the input to prevent injection
/// - Converts `#hashtag` patterns into `<hashtag hashtag="...">` elements
/// - Converts `@<64-hex-char-id>` patterns into `<mention client_id="...">` elements
/// - Converts newlines into `<br>` tags
pub fn convert_text_to_hashiverse_html(text: &str) -> String {
    let escaped = html_escape(text);
    let chars: Vec<char> = escaped.chars().collect();
    let len = chars.len();
    let mut output = String::with_capacity(escaped.len() * 2);
    let mut i = 0;

    while i < len {
        match chars[i] {
            '#' => {
                let start = i + 1;
                let mut end = start;
                while end < len && chars[end].is_alphanumeric() {
                    end += 1;
                }
                if end > start {
                    let hashtag_text: String = chars[start..end].iter().collect();
                    output.push_str(&convert_text_to_hashiverse_html_x_hashtag(&hashtag_text));
                    i = end;
                } else {
                    output.push('#');
                    i += 1;
                }
            }
            '@' => {
                let start = i + 1;
                let mut end = start;
                while end < len && end - start < 64 && is_hex_char(chars[end]) {
                    end += 1;
                }
                let hex_len = end - start;
                // Must be exactly 64 hex chars, and the next char (if any) must NOT be hex
                // to avoid matching a prefix of a longer hex string
                if hex_len == 64 && (end >= len || !is_hex_char(chars[end])) {
                    let hex_string: String = chars[start..end].iter().collect();
                    output.push_str(&convert_text_to_hashiverse_html_x_mention(&hex_string));
                    i = end;
                } else {
                    output.push('@');
                    i += 1;
                }
            }
            '\n' => {
                output.push_str("<br>");
                i += 1;
            }
            '\r' => {
                // Skip carriage returns — \r\n is handled by skipping \r and emitting <br> on \n
                i += 1;
            }
            c => {
                output.push(c);
                i += 1;
            }
        }
    }

    output
}

/// Render a hashtag as the canonical hashiverse element.
///
/// Accepts either `"rust"` or `"#rust"` — a single leading `#` is stripped
/// before validation. If the remaining text is empty or contains any
/// non-alphanumeric character, this function returns the original `hashtag`
/// parameter **untouched** (an identity no-op) so malformed input never
/// produces malformed HTML. Otherwise it emits the canonical element with a
/// lower-cased `hashtag` attribute (used for indexing) and a case-preserving
/// visible span.
pub fn convert_text_to_hashiverse_html_x_hashtag(hashtag: &str) -> String {
    let stripped = hashtag.strip_prefix('#').unwrap_or(hashtag);
    if stripped.is_empty() || !stripped.chars().all(char::is_alphanumeric) {
        return hashtag.to_string();
    }
    let stripped_lower = stripped.to_lowercase();
    format!(
        "<hashtag hashtag=\"{}\"><span class=\"plugin-hashtag-left\">#</span><span class=\"plugin-hashtag-right\">{}</span></hashtag>",
        stripped_lower, stripped,
    )
}

/// Render a 64-hex client_id as a `<mention>` element. Caller is responsible
/// for validating the hex length; we accept any string.
pub fn convert_text_to_hashiverse_html_x_mention(client_id: &str) -> String {
    format!("<mention client_id=\"{}\"></mention>", client_id)
}

/// Render the canonical URL preview card. Same shape as the web client's
/// `build_card_dom` (hashiverse-client-web/src/tabs/compose/UrlPreviewExtension.ts):
///
/// - With `image_url`: a `card-image-container` wraps the `<img>` and the
///   domain label, sibling to a `card-inner` column holding the title link
///   (and optional description).
/// - Without `image_url`: no image container; the domain label moves *inside*
///   `card-inner`, above the title link.
///
/// The description div is omitted entirely when `description` is empty.
/// Domain is derived from `url` internally. All field values are HTML-escaped.
pub fn convert_text_to_hashiverse_html_x_url_preview(
    title: &str,
    description: &str,
    image_url: &str,
    url: &str,
) -> String {
    let domain = extract_host_or_url(url);
    let mut out = String::with_capacity(512);
    out.push_str("<div class=\"plugin-urlpreview-card\">");
    if !image_url.is_empty() {
        out.push_str("<div class=\"plugin-urlpreview-card-image-container\">");
        out.push_str(&format!(
            "<img src=\"{}\" alt=\"\" class=\"plugin-urlpreview-card-image unblur-image\">",
            html_escape(image_url),
        ));
        out.push_str(&format!(
            "<div class=\"plugin-urlpreview-card-domain\">{}</div>",
            html_escape(domain),
        ));
        out.push_str("</div>");
    }
    out.push_str("<div class=\"plugin-urlpreview-card-inner\">");
    if image_url.is_empty() {
        out.push_str(&format!(
            "<div class=\"plugin-urlpreview-card-domain\">{}</div>",
            html_escape(domain),
        ));
    }
    out.push_str(&format!(
        "<a class=\"plugin-urlpreview-card-title\" href=\"{}\" rel=\"noopener noreferrer nofollow\">{}</a>",
        html_escape(url),
        html_escape(title),
    ));
    if !description.is_empty() {
        out.push_str(&format!(
            "<div class=\"plugin-urlpreview-card-description\">{}</div>",
            html_escape(description),
        ));
    }
    out.push_str("</div>");
    out.push_str("</div>");
    out
}

fn extract_host_or_url(url: &str) -> &str {
    match url.split_once("://") {
        Some((_, after)) => after
            .split(['/', '?', '#'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(url),
        None => url,
    }
}

fn html_escape(text: &str) -> String {
    // Reserve a little more room in case we escape
    let mut escaped = String::with_capacity(11 * text.len() / 10);
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn is_hex_char(c: char) -> bool {
    c.is_ascii_hexdigit()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Hashtag tests ---

    #[test]
    fn test_hashtag_at_start() {
        let result = convert_text_to_hashiverse_html("#rust is great");
        assert!(result.contains("<hashtag hashtag=\"rust\">"));
        assert!(result.contains("<span class=\"plugin-hashtag-right\">rust</span>"));
        assert!(result.ends_with(" is great"));
    }

    #[test]
    fn test_hashtag_at_end() {
        let result = convert_text_to_hashiverse_html("hello #rust");
        assert!(result.starts_with("hello "));
        assert!(result.contains("<hashtag hashtag=\"rust\">"));
    }

    #[test]
    fn test_hashtag_in_middle() {
        let result = convert_text_to_hashiverse_html("I love #rust programming");
        assert!(result.contains("<hashtag hashtag=\"rust\">"));
        assert!(result.contains(" programming"));
    }

    #[test]
    fn test_multiple_hashtags() {
        let result = convert_text_to_hashiverse_html("#rust and #golang");
        assert!(result.contains("<hashtag hashtag=\"rust\">"));
        assert!(result.contains("<hashtag hashtag=\"golang\">"));
    }

    #[test]
    fn test_adjacent_hashtags() {
        let result = convert_text_to_hashiverse_html("#rust#golang");
        assert!(result.contains("<hashtag hashtag=\"rust\">"));
        assert!(result.contains("<hashtag hashtag=\"golang\">"));
    }

    #[test]
    fn test_hashtag_case_lowered_in_attribute() {
        let result = convert_text_to_hashiverse_html("#RuStLang");
        assert!(result.contains("hashtag=\"rustlang\""));
        // The display text preserves original case
        assert!(result.contains("<span class=\"plugin-hashtag-right\">RuStLang</span>"));
    }

    #[test]
    fn test_bare_hash_no_alphanumeric() {
        assert_eq!(convert_text_to_hashiverse_html("# alone"), "# alone");
    }

    #[test]
    fn test_hash_at_end_of_string() {
        assert_eq!(convert_text_to_hashiverse_html("test #"), "test #");
    }

    #[test]
    fn test_unicode_hashtag() {
        let result = convert_text_to_hashiverse_html("#日本語");
        assert!(result.contains("hashtag=\"日本語\""));
        assert!(result.contains("<span class=\"plugin-hashtag-right\">日本語</span>"));
    }

    #[test]
    fn test_hashtag_with_numbers() {
        let result = convert_text_to_hashiverse_html("#web3");
        assert!(result.contains("hashtag=\"web3\""));
    }

    #[test]
    fn test_hashtag_terminated_by_punctuation() {
        let result = convert_text_to_hashiverse_html("#rust, nice");
        assert!(result.contains("<hashtag hashtag=\"rust\">"));
        assert!(result.contains("</hashtag>, nice"));
    }

    // --- Mention tests ---

    #[test]
    fn test_valid_mention() {
        let hex_id = "a".repeat(64);
        let input = format!("hello @{} world", hex_id);
        let result = convert_text_to_hashiverse_html(&input);
        assert!(result.contains(&format!("<mention client_id=\"{}\"></mention>", hex_id)));
        assert!(result.starts_with("hello "));
        assert!(result.ends_with(" world"));
    }

    #[test]
    fn test_mention_mixed_case_hex() {
        let hex_id = "aAbBcCdDeEfF0011223344556677889900112233445566778899aAbBcCdDeEfF";
        assert_eq!(hex_id.len(), 64);
        let input = format!("@{}", hex_id);
        let result = convert_text_to_hashiverse_html(&input);
        assert!(result.contains(&format!("<mention client_id=\"{}\"></mention>", hex_id)));
    }

    #[test]
    fn test_mention_too_short() {
        let result = convert_text_to_hashiverse_html("@abcdef");
        assert_eq!(result, "@abcdef");
        assert!(!result.contains("<mention"));
    }

    #[test]
    fn test_mention_non_hex_after_at() {
        let result = convert_text_to_hashiverse_html("@hello");
        assert_eq!(result, "@hello");
    }

    #[test]
    fn test_bare_at() {
        assert_eq!(convert_text_to_hashiverse_html("@"), "@");
    }

    #[test]
    fn test_at_end_of_string() {
        assert_eq!(convert_text_to_hashiverse_html("test @"), "test @");
    }

    #[test]
    fn test_mention_65_hex_chars_not_matched() {
        // 65 hex chars — should NOT match as a mention (next char is also hex)
        let hex_65 = "a".repeat(65);
        let input = format!("@{}", hex_65);
        let result = convert_text_to_hashiverse_html(&input);
        assert!(!result.contains("<mention"));
    }

    #[test]
    fn test_mention_64_hex_then_non_hex() {
        let hex_id = "b".repeat(64);
        let input = format!("@{}xyz", hex_id);
        let result = convert_text_to_hashiverse_html(&input);
        assert!(result.contains(&format!("<mention client_id=\"{}\"></mention>", hex_id)));
        assert!(result.ends_with("xyz"));
    }

    // --- HTML escaping tests ---

    #[test]
    fn test_html_injection_escaped() {
        let result = convert_text_to_hashiverse_html("<script>alert(1)</script>");
        assert!(result.contains("&lt;script&gt;"));
        assert!(!result.contains("<script>"));
    }

    #[test]
    fn test_ampersand_escaped() {
        let result = convert_text_to_hashiverse_html("AT&T");
        assert_eq!(result, "AT&amp;T");
    }

    #[test]
    fn test_quotes_escaped() {
        let result = convert_text_to_hashiverse_html("he said \"hello\"");
        assert!(result.contains("&quot;"));
    }

    // --- Newline tests ---

    #[test]
    fn test_newline_to_br() {
        let result = convert_text_to_hashiverse_html("line1\nline2");
        assert_eq!(result, "line1<br>line2");
    }

    #[test]
    fn test_crlf_to_br() {
        let result = convert_text_to_hashiverse_html("line1\r\nline2");
        assert_eq!(result, "line1<br>line2");
    }

    #[test]
    fn test_bare_cr_skipped() {
        let result = convert_text_to_hashiverse_html("line1\rline2");
        assert_eq!(result, "line1line2");
    }

    // --- Combined tests ---

    #[test]
    fn test_combined_post() {
        let hex_id = "c".repeat(64);
        let input = format!("Hello #hashiverse from @{}!\nGreat to be here.", hex_id);
        let result = convert_text_to_hashiverse_html(&input);
        assert!(result.contains("<hashtag hashtag=\"hashiverse\">"));
        assert!(result.contains(&format!("<mention client_id=\"{}\"></mention>", hex_id)));
        assert!(result.contains("<br>"));
        assert!(result.contains("Great to be here."));
    }

    #[test]
    fn test_empty_string() {
        assert_eq!(convert_text_to_hashiverse_html(""), "");
    }

    #[test]
    fn test_plain_text_no_specials() {
        assert_eq!(convert_text_to_hashiverse_html("just a normal post"), "just a normal post");
    }

    // --- Round-trip test: verify scraper can parse the output the same way submit_post does ---

    #[test]
    fn test_round_trip_hashtag_extraction() {
        let result = convert_text_to_hashiverse_html("I love #Rust and #golang");
        let html = scraper::Html::parse_fragment(&result);
        let selector = scraper::Selector::parse("hashtag").unwrap();
        let hashtags: Vec<&str> = html.select(&selector)
            .filter_map(|el| el.attr("hashtag"))
            .collect();
        assert_eq!(hashtags, vec!["rust", "golang"]);
    }

    #[test]
    fn test_round_trip_mention_extraction() {
        let hex_id = "d".repeat(64);
        let result = convert_text_to_hashiverse_html(&format!("hello @{}", hex_id));
        let html = scraper::Html::parse_fragment(&result);
        let selector = scraper::Selector::parse("mention").unwrap();
        let client_ids: Vec<&str> = html.select(&selector)
            .filter_map(|el| el.attr("client_id"))
            .collect();
        assert_eq!(client_ids, vec![hex_id.as_str()]);
    }

    #[test]
    fn test_round_trip_combined() {
        let hex_id = "e".repeat(64);
        let input = format!("#hashiverse post by @{} about #Rust", hex_id);
        let result = convert_text_to_hashiverse_html(&input);
        let html = scraper::Html::parse_fragment(&result);

        let hashtag_selector = scraper::Selector::parse("hashtag").unwrap();
        let hashtags: Vec<&str> = html.select(&hashtag_selector)
            .filter_map(|el| el.attr("hashtag"))
            .collect();
        assert_eq!(hashtags, vec!["hashiverse", "rust"]);

        let mention_selector = scraper::Selector::parse("mention").unwrap();
        let client_ids: Vec<&str> = html.select(&mention_selector)
            .filter_map(|el| el.attr("client_id"))
            .collect();
        assert_eq!(client_ids, vec![hex_id.as_str()]);
    }

    // --- Fragment helpers: _x_hashtag, _x_mention, _x_url_preview ---

    #[test]
    fn test_x_hashtag_lowercases_attribute_preserves_span_text() {
        let result = convert_text_to_hashiverse_html_x_hashtag("RuStLang");
        assert!(result.contains("hashtag=\"rustlang\""));
        assert!(result.contains("<span class=\"plugin-hashtag-right\">RuStLang</span>"));
        assert!(result.contains("<span class=\"plugin-hashtag-left\">#</span>"));
    }

    #[test]
    fn test_x_hashtag_handles_unicode() {
        let result = convert_text_to_hashiverse_html_x_hashtag("日本語");
        assert!(result.contains("hashtag=\"日本語\""));
        assert!(result.contains("<span class=\"plugin-hashtag-right\">日本語</span>"));
    }

    #[test]
    fn test_x_hashtag_non_alphanumeric_returns_input_untouched() {
        // Bad input must NOT produce a <hashtag> element (would be malformed
        // HTML). The contract is "identity no-op" — return the original
        // parameter byte-for-byte.
        for bad in ["a<b", "a\"b", "a b", "a&b", "##rust", "#a<b"] {
            assert_eq!(convert_text_to_hashiverse_html_x_hashtag(bad), bad);
        }
    }

    #[test]
    fn test_x_hashtag_empty_returns_empty() {
        assert_eq!(convert_text_to_hashiverse_html_x_hashtag(""), "");
    }

    #[test]
    fn test_x_hashtag_lone_hash_returns_lone_hash() {
        // Strip the '#' → empty → fail validation → return the input.
        assert_eq!(convert_text_to_hashiverse_html_x_hashtag("#"), "#");
    }

    #[test]
    fn test_x_hashtag_strips_leading_hash_if_provided() {
        let with_hash = convert_text_to_hashiverse_html_x_hashtag("#rust");
        let without_hash = convert_text_to_hashiverse_html_x_hashtag("rust");
        assert_eq!(with_hash, without_hash);
        assert!(with_hash.contains("hashtag=\"rust\""));
        assert!(with_hash.contains("<span class=\"plugin-hashtag-right\">rust</span>"));
    }

    #[test]
    fn test_x_hashtag_no_op_does_not_emit_hashtag_element() {
        // Defensive: any caller-supplied invalid input must never end up in a
        // `<hashtag>` element.
        for bad in ["a<b", "a\"b", "a b", "", "##rust", "#a<b"] {
            let result = convert_text_to_hashiverse_html_x_hashtag(bad);
            assert!(!result.contains("<hashtag"), "unexpected element for {bad:?}: {result:?}");
            assert!(!result.contains("plugin-hashtag-left"), "unexpected span for {bad:?}: {result:?}");
        }
    }

    #[test]
    fn test_x_mention_emits_64hex_client_id() {
        let hex_id = "f".repeat(64);
        let result = convert_text_to_hashiverse_html_x_mention(&hex_id);
        assert_eq!(result, format!("<mention client_id=\"{}\"></mention>", hex_id));
    }

    #[test]
    fn test_x_url_preview_with_image_renders_image_container() {
        let result = convert_text_to_hashiverse_html_x_url_preview(
            "Title",
            "Desc",
            "https://img.example/x.png",
            "https://example.com/path",
        );
        assert!(result.starts_with("<div class=\"plugin-urlpreview-card\"><div class=\"plugin-urlpreview-card-image-container\">"));
        assert!(result.contains("<img src=\"https://img.example/x.png\" alt=\"\" class=\"plugin-urlpreview-card-image unblur-image\">"));
        assert!(result.contains("<div class=\"plugin-urlpreview-card-domain\">example.com</div>"));
        assert!(result.contains("<a class=\"plugin-urlpreview-card-title\" href=\"https://example.com/path\" rel=\"noopener noreferrer nofollow\">Title</a>"));
        assert!(result.contains("<div class=\"plugin-urlpreview-card-description\">Desc</div>"));
    }

    #[test]
    fn test_x_url_preview_without_image_moves_domain_into_inner() {
        let result = convert_text_to_hashiverse_html_x_url_preview(
            "Title",
            "Desc",
            "",
            "https://example.com/path",
        );
        assert!(!result.contains("plugin-urlpreview-card-image-container"));
        assert!(!result.contains("<img "));
        // domain div sits inside card-inner, BEFORE the title link
        let inner_pos = result.find("plugin-urlpreview-card-inner").unwrap();
        let domain_pos = result.find("plugin-urlpreview-card-domain").unwrap();
        let title_pos = result.find("plugin-urlpreview-card-title").unwrap();
        assert!(inner_pos < domain_pos && domain_pos < title_pos);
    }

    #[test]
    fn test_x_url_preview_omits_description_when_blank() {
        let result = convert_text_to_hashiverse_html_x_url_preview(
            "Title",
            "",
            "https://img.example/x.png",
            "https://example.com/",
        );
        assert!(!result.contains("plugin-urlpreview-card-description"));
    }

    #[test]
    fn test_x_url_preview_extracts_domain_from_https_url() {
        let result = convert_text_to_hashiverse_html_x_url_preview(
            "T", "", "", "https://sub.example.com:8443/path?q=1#frag",
        );
        assert!(result.contains("<div class=\"plugin-urlpreview-card-domain\">sub.example.com:8443</div>"));
    }

    #[test]
    fn test_x_url_preview_falls_back_to_full_url_when_no_scheme() {
        let result = convert_text_to_hashiverse_html_x_url_preview("T", "", "", "not-a-url");
        assert!(result.contains("<div class=\"plugin-urlpreview-card-domain\">not-a-url</div>"));
    }

    #[test]
    fn test_x_url_preview_html_escapes_attribute_and_text_values() {
        let result = convert_text_to_hashiverse_html_x_url_preview(
            "Title with <script> & \"quotes\"",
            "Desc with <html> & \"chars\"",
            "https://i/x?a=1&b=2",
            "https://example.com/?q=1&r=2",
        );
        // Title text inside the <a>:
        assert!(result.contains(">Title with &lt;script&gt; &amp; &quot;quotes&quot;</a>"));
        // Description text inside the description div:
        assert!(result.contains(">Desc with &lt;html&gt; &amp; &quot;chars&quot;</div>"));
        // URLs in attributes — & must become &amp;:
        assert!(result.contains("src=\"https://i/x?a=1&amp;b=2\""));
        assert!(result.contains("href=\"https://example.com/?q=1&amp;r=2\""));
    }

    #[test]
    fn test_existing_convert_text_to_hashiverse_html_unchanged_after_refactor() {
        // Sanity-check that the post-refactor output is byte-identical for a
        // representative input — the inline format!s were lifted into helpers,
        // not changed.
        let hex = "a".repeat(64);
        let input = format!("Hi #Rust @{} bye", hex);
        let result = convert_text_to_hashiverse_html(&input);
        assert_eq!(
            result,
            format!(
                "Hi <hashtag hashtag=\"rust\"><span class=\"plugin-hashtag-left\">#</span><span class=\"plugin-hashtag-right\">Rust</span></hashtag> <mention client_id=\"{}\"></mention> bye",
                hex
            )
        );
    }
}
