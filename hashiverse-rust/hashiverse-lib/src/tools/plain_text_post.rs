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
                    let hashtag_lower = hashtag_text.to_lowercase();
                    output.push_str(&format!(
                        "<hashtag hashtag=\"{}\"><span class=\"plugin-hashtag-left\">#</span><span class=\"plugin-hashtag-right\">{}</span></hashtag>",
                        hashtag_lower, hashtag_text
                    ));
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
                    output.push_str(&format!("<mention client_id=\"{}\"></mention>", hex_string));
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
}
