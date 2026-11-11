//! Turning command results into text or JSON for the terminal.

use scraper::{ElementRef, Html, Node};
use serde::Serialize;

use crate::commands::{CliPost, CliPostSubmitted, CliTrendingHashtag};

const BLOCK_ELEMENTS: &[&str] = &["p", "div", "li", "ul", "ol", "blockquote", "pre", "h1", "h2", "h3", "h4", "h5", "h6"];

/// Renders a hashiverse post body (the HTML subset produced by the web client or by
/// `convert_text_to_hashiverse_html`) as plain text:
/// - `<hashtag>` → its visible text (`#Rust`), or `#<attr>` if it has none
/// - `<mention client_id="…">` → `@<client_id>`
/// - `<br>` and block-element boundaries → newlines
/// - `<a href>` → link text, followed by `<href>` when the two differ
/// - `<img src>` → `[image: src]`
/// - everything else → its text content
pub fn render_post_html_to_text(post_html: &str) -> String {
    let fragment = Html::parse_fragment(post_html);
    let mut output = String::new();
    render_children(fragment.root_element(), &mut output);
    output.trim_end_matches('\n').to_string()
}

fn ensure_line_break(output: &mut String) {
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
}

fn render_children(element_ref: ElementRef, output: &mut String) {
    for child in element_ref.children() {
        match child.value() {
            Node::Text(text) => output.push_str(text),
            Node::Element(_) => {
                if let Some(child_element_ref) = ElementRef::wrap(child) {
                    render_element(child_element_ref, output);
                }
            }
            _ => {}
        }
    }
}

fn render_element(element_ref: ElementRef, output: &mut String) {
    let element = element_ref.value();
    match element.name() {
        "br" => output.push('\n'),
        "mention" => {
            output.push('@');
            output.push_str(element.attr("client_id").unwrap_or_default());
        }
        "hashtag" => {
            let mut visible_text = String::new();
            render_children(element_ref, &mut visible_text);
            if visible_text.is_empty() {
                output.push('#');
                output.push_str(element.attr("hashtag").unwrap_or_default());
            } else {
                output.push_str(&visible_text);
            }
        }
        "img" => {
            output.push_str("[image: ");
            output.push_str(element.attr("src").unwrap_or_default());
            output.push(']');
        }
        "a" => {
            let mut link_text = String::new();
            render_children(element_ref, &mut link_text);
            output.push_str(&link_text);
            // An image link already shows its image src; appending the href too is noise.
            let link_text_without_images = element_ref.text().collect::<String>();
            if let Some(href) = element.attr("href") {
                if !link_text_without_images.trim().is_empty() && link_text.trim() != href {
                    output.push_str(" <");
                    output.push_str(href);
                    output.push('>');
                }
            }
        }
        name if BLOCK_ELEMENTS.contains(&name) => {
            ensure_line_break(output);
            render_children(element_ref, output);
            ensure_line_break(output);
        }
        _ => render_children(element_ref, output),
    }
}

pub fn format_time_millis(time_millis: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(time_millis) {
        Some(date_time) => date_time.format("%Y-%m-%d %H:%M:%SZ").to_string(),
        None => format!("{}ms", time_millis),
    }
}

pub fn format_posts_text(cli_posts: &[CliPost]) -> String {
    if cli_posts.is_empty() {
        return "(no posts)".to_string();
    }
    let mut output = String::new();
    for (index, cli_post) in cli_posts.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(&format!("{}  {}  post:{}{}\n", format_time_millis(cli_post.time_millis), cli_post.client_id, cli_post.post_id, if cli_post.healed { "  (healed)" } else { "" }));
        output.push_str(&render_post_html_to_text(&cli_post.post_html));
        output.push('\n');
    }
    output.trim_end_matches('\n').to_string()
}

pub fn format_trending_text(cli_trending_hashtags: &[CliTrendingHashtag]) -> String {
    if cli_trending_hashtags.is_empty() {
        return "(no trending hashtags)".to_string();
    }
    let count_width = cli_trending_hashtags.iter().map(|cli_trending_hashtag| cli_trending_hashtag.count.to_string().len()).max().unwrap_or(1);
    cli_trending_hashtags
        .iter()
        .map(|cli_trending_hashtag| format!("{:>count_width$}  #{}", cli_trending_hashtag.count, cli_trending_hashtag.hashtag))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn format_post_submitted_text(cli_post_submitted: &CliPostSubmitted) -> String {
    let mut output = format!("posted {}", cli_post_submitted.post_id);
    for bucket_location in &cli_post_submitted.bucket_locations {
        output.push_str(&format!("\n  {}", bucket_location));
    }
    output
}

pub fn format_json<T: Serialize + ?Sized>(value: &T) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(value)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hashiverse_lib::tools::plain_text_post::{convert_text_to_hashiverse_html, convert_text_to_hashiverse_html_x_url_preview};

    fn assert_round_trip(text: &str) {
        let post_html = convert_text_to_hashiverse_html(text);
        assert_eq!(render_post_html_to_text(&post_html), text, "html was: {}", post_html);
    }

    #[test]
    fn round_trip_plain_text() {
        assert_round_trip("hello world");
    }

    #[test]
    fn round_trip_hashtags_preserve_case() {
        assert_round_trip("I like #Rust and #wasm32 a lot");
    }

    #[test]
    fn round_trip_mention() {
        assert_round_trip(&format!("hi @{} how are you", "ab".repeat(32)));
    }

    #[test]
    fn round_trip_newlines() {
        assert_round_trip("line one\nline two\n\nline four");
    }

    #[test]
    fn round_trip_escaped_characters() {
        assert_round_trip("a < b && c > \"d\" <script>alert(1)</script>");
    }

    #[test]
    fn round_trip_lone_hash_and_at() {
        assert_round_trip("# not a tag, @ not a mention, @abc too short");
    }

    #[test]
    fn hashtag_without_visible_text_uses_attribute() {
        assert_eq!(render_post_html_to_text("<hashtag hashtag=\"rust\"></hashtag>"), "#rust");
    }

    #[test]
    fn paragraphs_become_lines() {
        assert_eq!(render_post_html_to_text("<p>first</p><p>second</p>"), "first\nsecond");
    }

    #[test]
    fn links_show_href_when_different() {
        assert_eq!(render_post_html_to_text("<a href=\"https://x.org\">x</a>"), "x <https://x.org>");
        assert_eq!(render_post_html_to_text("<a href=\"https://x.org\">https://x.org</a>"), "https://x.org");
        assert_eq!(render_post_html_to_text("<a href=\"https://x.org\"><img src=\"https://x.org/i.png\"></a>"), "[image: https://x.org/i.png]");
    }

    #[test]
    fn url_preview_card_renders_readably() {
        let post_html = convert_text_to_hashiverse_html_x_url_preview("Title", "Desc", "https://x.org/i.png", "https://x.org/a");
        assert_eq!(render_post_html_to_text(&post_html), "[image: https://x.org/i.png]\nx.org\nTitle <https://x.org/a>\nDesc");
    }

    fn sample_cli_post() -> CliPost {
        CliPost {
            post_id: "11".to_string(),
            time_millis: 1_700_000_000_000,
            client_id: "22".to_string(),
            bucket_location: "1~x".to_string(),
            post_html: convert_text_to_hashiverse_html("hi #there"),
            healed: false,
        }
    }

    #[test]
    fn posts_text_has_header_and_body() {
        assert_eq!(format_posts_text(&[sample_cli_post()]), "2023-11-14 22:13:20Z  22  post:11\nhi #there");
        assert_eq!(format_posts_text(&[]), "(no posts)");
    }

    #[test]
    fn posts_json_shape() {
        let json_value: serde_json::Value = serde_json::from_str(&format_json(&vec![sample_cli_post()]).unwrap()).unwrap();
        let first = &json_value[0];
        for field in ["post_id", "time_millis", "client_id", "bucket_location", "post_html", "healed"] {
            assert!(first.get(field).is_some(), "missing field {}", field);
        }
        assert_eq!(first["time_millis"], 1_700_000_000_000i64);
    }

    #[test]
    fn trending_text_aligns_counts() {
        let cli_trending_hashtags = vec![
            CliTrendingHashtag { hashtag: "rust".to_string(), count: 120 },
            CliTrendingHashtag { hashtag: "wasm".to_string(), count: 7 },
        ];
        assert_eq!(format_trending_text(&cli_trending_hashtags), "120  #rust\n  7  #wasm");
    }

    #[test]
    fn post_submitted_text() {
        let cli_post_submitted = CliPostSubmitted { post_id: "ab".to_string(), bucket_locations: vec!["loc1".to_string(), "loc2".to_string()] };
        assert_eq!(format_post_submitted_text(&cli_post_submitted), "posted ab\n  loc1\n  loc2");
    }
}
