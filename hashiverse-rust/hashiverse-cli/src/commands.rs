//! The CLI commands. Each takes a plain `&HashiverseClient` so the same code runs against the
//! production client (via `HashiverseBuilder`) and against mem-transport clients in tests.

use anyhow::{anyhow, Result};
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::protocol::posting::encoded_post::EncodedPostV1;
use hashiverse_lib::tools::buckets::{BucketLocation, BucketType};
use hashiverse_lib::tools::plain_text_post::convert_text_to_hashiverse_html;
use hashiverse_lib::tools::types::Id;
use serde::Serialize;

use crate::cli_args::{Command, ENV_KEYPHRASE, ENV_PASSWORD, ENV_PROFILE};
use crate::unlock::is_guest;
use crate::render::{format_json, format_post_submitted_text, format_posts_text, format_trending_text};

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct CliPost {
    pub post_id: String,
    pub time_millis: i64,
    pub client_id: String,
    pub bucket_location: String,
    pub post_html: String,
    pub healed: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct CliPostSubmitted {
    pub post_id: String,
    pub bucket_locations: Vec<String>,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct CliTrendingHashtag {
    pub hashtag: String,
    pub count: u64,
}

pub fn cli_post_from_timeline_entry(bucket_location: &BucketLocation, encoded_post: EncodedPostV1, healed: bool) -> Result<CliPost> {
    let client_id = encoded_post.header.client_id()?;
    Ok(CliPost {
        post_id: encoded_post.post_id.to_hex_str(),
        time_millis: encoded_post.header.time_millis.0,
        client_id: client_id.id_hex(),
        bucket_location: bucket_location.to_html_attr(),
        post_html: encoded_post.post,
        healed,
    })
}

/// Strips one leading `#` and surrounding whitespace.
pub fn normalise_hashtag(hashtag: &str) -> String {
    let trimmed = hashtag.trim();
    trimmed.strip_prefix('#').unwrap_or(trimmed).to_string()
}

pub async fn post(hashiverse_client: &HashiverseClient, text: &str) -> Result<CliPostSubmitted> {
    if is_guest(hashiverse_client) {
        return Err(anyhow!("posting requires an identity: set {} or {}, together with {} (or `login <client_id>` in the REPL)", ENV_KEYPHRASE, ENV_PROFILE, ENV_PASSWORD));
    }

    let post_html = convert_text_to_hashiverse_html(text);
    let (submit_post_commit_tokens, (encoded_post, _encoded_post_bytes)) = hashiverse_client.submit_post(&post_html).await?;

    let mut bucket_locations: Vec<String> = Vec::new();
    for submit_post_commit_token in &submit_post_commit_tokens {
        let bucket_location = submit_post_commit_token.bucket_location.to_html_attr();
        if !bucket_locations.contains(&bucket_location) {
            bucket_locations.push(bucket_location);
        }
    }

    Ok(CliPostSubmitted {
        post_id: encoded_post.post_id.to_hex_str(),
        bucket_locations,
    })
}

async fn timeline(hashiverse_client: &HashiverseClient, bucket_type: BucketType, base_id: &Id, pages: u32) -> Result<Vec<CliPost>> {
    let mut cli_posts = Vec::new();
    for _ in 0..pages.max(1) {
        let (timeline_entries, _oldest_processed_time_millis) = hashiverse_client.single_timeline_get_more(bucket_type, base_id).await?;
        for (bucket_location, encoded_post, _encoded_post_bytes, healed) in timeline_entries {
            match cli_post_from_timeline_entry(&bucket_location, encoded_post, healed) {
                Ok(cli_post) => cli_posts.push(cli_post),
                Err(error) => log::warn!("Skipping post with bad header: {}", error),
            }
        }
    }
    Ok(cli_posts)
}

pub async fn user_timeline(hashiverse_client: &HashiverseClient, client_id_hex: &str, pages: u32) -> Result<Vec<CliPost>> {
    let base_id = Id::from_hex_str(client_id_hex.trim())?;
    timeline(hashiverse_client, BucketType::User, &base_id, pages).await
}

pub async fn hashtag_timeline(hashiverse_client: &HashiverseClient, hashtag: &str, pages: u32) -> Result<Vec<CliPost>> {
    let base_id = Id::from_hashtag_str(&normalise_hashtag(hashtag))?;
    timeline(hashiverse_client, BucketType::Hashtag, &base_id, pages).await
}

pub async fn trending(hashiverse_client: &HashiverseClient, limit: u16) -> Result<Vec<CliTrendingHashtag>> {
    let trending_hashtags_fetch_response = hashiverse_client.fetch_trending_hashtags(limit).await?;
    Ok(trending_hashtags_fetch_response
        .trending_hashtags
        .into_iter()
        .map(|trending_hashtag| CliTrendingHashtag { hashtag: trending_hashtag.hashtag, count: trending_hashtag.count })
        .collect())
}

/// Runs one command and returns its output, formatted as text or JSON. Shared by the
/// one-shot entry point and the REPL.
pub async fn run_command(hashiverse_client: &HashiverseClient, command: &Command, json: bool) -> Result<String> {
    match command {
        Command::Post { text } => {
            let cli_post_submitted = post(hashiverse_client, &text.join(" ")).await?;
            if json { format_json(&cli_post_submitted) } else { Ok(format_post_submitted_text(&cli_post_submitted)) }
        }
        Command::User { client_id_hex, pages } => {
            let cli_posts = user_timeline(hashiverse_client, client_id_hex, *pages).await?;
            if json { format_json(&cli_posts) } else { Ok(format_posts_text(&cli_posts)) }
        }
        Command::Hashtag { hashtag, pages } => {
            let cli_posts = hashtag_timeline(hashiverse_client, hashtag, *pages).await?;
            if json { format_json(&cli_posts) } else { Ok(format_posts_text(&cli_posts)) }
        }
        Command::Trending { limit } => {
            let cli_trending_hashtags = trending(hashiverse_client, *limit).await?;
            if json { format_json(&cli_trending_hashtags) } else { Ok(format_trending_text(&cli_trending_hashtags)) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_hashtag_strips_one_hash_and_whitespace() {
        assert_eq!(normalise_hashtag("#Rust"), "Rust");
        assert_eq!(normalise_hashtag("  rust "), "rust");
        assert_eq!(normalise_hashtag("##rust"), "#rust");
    }

    #[test]
    fn hashtag_with_and_without_hash_map_to_same_bucket_id() {
        let with_hash = Id::from_hashtag_str(&normalise_hashtag("#Rust")).unwrap();
        let without_hash = Id::from_hashtag_str(&normalise_hashtag("rust")).unwrap();
        assert_eq!(with_hash, without_hash);
    }
}
