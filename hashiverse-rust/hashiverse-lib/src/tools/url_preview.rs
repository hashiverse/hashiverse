//! # Open Graph / link-preview extraction
//!
//! Parses the `<head>` of an HTML page and extracts the fields needed to render a link
//! preview card in a post: title, description, image, and canonical URL.
//!
//! Open Graph (`og:title`, `og:description`, `og:image`, `og:url`) is preferred; the
//! extractor falls back to the page's `<title>` element and `<meta name="description">`
//! when OG isn't present. The [`UrlPreviewData`] struct is what callers hand up to the
//! protocol layer — servers fetch the target URL under an RPC budget gated by
//! `POW_MINIMUM_PER_URL_FETCH` and return this struct back to the client so the preview
//! card can render without every client individually fetching (and thus leaking its IP to)
//! the target site.

use scraper::{Html, Selector};

pub struct UrlPreviewData {
    pub title: String,
    pub description: String,
    pub image_url: String,
    pub canonical_url: String,
}

pub fn extract_url_preview(html: &str) -> UrlPreviewData {
    let document = Html::parse_document(html);

    let og_title = select_meta_content(&document, "meta[property='og:title']");
    let og_description = select_meta_content(&document, "meta[property='og:description']");
    let og_image = select_meta_content(&document, "meta[property='og:image']");
    let og_url = select_meta_content(&document, "meta[property='og:url']");

    let title = og_title.or_else(|| select_title(&document)).unwrap_or_default();
    let description = og_description
        .or_else(|| select_meta_content(&document, "meta[name='description']"))
        .unwrap_or_default();

    UrlPreviewData {
        title,
        description,
        image_url: og_image.unwrap_or_default(),
        canonical_url: og_url.unwrap_or_default(),
    }
}

fn select_meta_content(document: &Html, selector_str: &str) -> Option<String> {
    let selector = Selector::parse(selector_str).ok()?;
    document.select(&selector).next()?.value().attr("content").map(|s| s.to_string())
}

fn select_title(document: &Html) -> Option<String> {
    let selector = Selector::parse("title").ok()?;
    Some(document.select(&selector).next()?.text().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_url_preview_with_og_tags() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head>
                <meta property="og:title" content="OG Title" />
                <meta property="og:description" content="OG Description" />
                <meta property="og:image" content="https://example.com/og.png" />
                <meta property="og:url" content="https://example.com/canonical" />
                <title>Page Title</title>
            </head>
            <body></body>
            </html>
        "#;

        let data = extract_url_preview(html);
        assert_eq!(data.title, "OG Title");
        assert_eq!(data.description, "OG Description");
        assert_eq!(data.image_url, "https://example.com/og.png");
        assert_eq!(data.canonical_url, "https://example.com/canonical");
    }

    #[test]
    fn test_extract_url_preview_fallback_to_title_and_meta_description() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head>
                <title>Fallback Title</title>
                <meta name="description" content="Fallback Description" />
            </head>
            <body></body>
            </html>
        "#;

        let data = extract_url_preview(html);
        assert_eq!(data.title, "Fallback Title");
        assert_eq!(data.description, "Fallback Description");
        assert_eq!(data.image_url, "");
        assert_eq!(data.canonical_url, "");
    }

    #[test]
    fn test_extract_url_preview_empty_html() {
        let data = extract_url_preview("");
        assert_eq!(data.title, "");
        assert_eq!(data.description, "");
        assert_eq!(data.image_url, "");
        assert_eq!(data.canonical_url, "");
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod bolero_fuzz {
        use super::*;

        #[test]
        fn fuzz_extract_url_preview() {
            bolero::check!().for_each(|data: &[u8]| {
                if let Ok(html) = std::str::from_utf8(data) {
                    let _ = extract_url_preview(html);
                }
            });
        }
    }

    #[test]
    fn test_extract_url_preview_og_overrides_title() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head>
                <title>Page Title</title>
                <meta property="og:title" content="OG Title" />
            </head>
            <body></body>
            </html>
        "#;

        let data = extract_url_preview(html);
        assert_eq!(data.title, "OG Title");
    }
}
