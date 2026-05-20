use napi_derive::napi;
use std::sync::OnceLock;

pub mod hashiverse_client_nodejs;
pub mod napi_err;

static RUSTLS_INIT: OnceLock<()> = OnceLock::new();

pub(crate) fn ensure_rustls_init() {
    RUSTLS_INIT.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Initialise Rust-side logging. Optional. By default Rust log records are silent
/// in Node; calling `initLogging()` installs `env_logger`, which honours the
/// `RUST_LOG` environment variable (e.g. `RUST_LOG=info node app.js`).
///
/// Process-wide, one-shot. A second call returns the underlying error rather
/// than silently no-op'ing, matching the Python crate's behaviour.
#[napi(js_name = "initLogging")]
pub fn init_logging() -> napi::Result<()> {
    env_logger::try_init().map_err(|e| napi::Error::from_reason(format!("init_logging failed: {e}")))?;
    Ok(())
}

#[napi(js_name = "convertTextToHashiverseHtml")]
pub fn convert_text_to_hashiverse_html(text: String) -> String {
    hashiverse_lib::tools::plain_text_post::convert_text_to_hashiverse_html(&text)
}

#[napi(js_name = "convertTextToHashiverseHtmlXHashtag")]
pub fn convert_text_to_hashiverse_html_x_hashtag(hashtag: String) -> String {
    hashiverse_lib::tools::plain_text_post::convert_text_to_hashiverse_html_x_hashtag(&hashtag)
}

#[napi(js_name = "convertTextToHashiverseHtmlXMention")]
pub fn convert_text_to_hashiverse_html_x_mention(client_id: String) -> String {
    hashiverse_lib::tools::plain_text_post::convert_text_to_hashiverse_html_x_mention(&client_id)
}

#[napi(js_name = "convertTextToHashiverseHtmlXUrlPreview")]
pub fn convert_text_to_hashiverse_html_x_url_preview(
    title: String,
    description: String,
    image_url: String,
    url: String,
) -> String {
    hashiverse_lib::tools::plain_text_post::convert_text_to_hashiverse_html_x_url_preview(&title, &description, &image_url, &url)
}
