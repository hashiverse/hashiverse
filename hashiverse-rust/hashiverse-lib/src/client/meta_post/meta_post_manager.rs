//! # Meta-post lifecycle orchestration
//!
//! The glue between [`crate::client::meta_post::meta_post`] (data model) and the rest of
//! the client. `MetaPostManager`:
//!
//! - loads the current profile from `BUCKET_CONFIG` on startup,
//! - merges incoming meta-post updates (from other devices or from the network) via the
//!   per-field `VersionedField` CRDT rule,
//! - writes updates back to storage, and
//! - re-publishes the account's meta-post to the network if the current month's User
//!   bucket no longer contains a fresh enough copy — so following clients and other
//!   devices can discover current settings without every profile edit triggering an
//!   immediate post.

use std::collections::HashMap;
use std::sync::Arc;
use log::{info, warn};
use crate::client::client_storage::client_storage::{self as cs, ClientStorage, BUCKET_META_POST_PUBLIC, BUCKET_CONFIG, BUCKET_CONFIG_KEY_META_POST_V1_PUBLIC, BUCKET_CONFIG_KEY_META_POST_V1_PRIVATE};
use crate::client::key_locker::key_locker::KeyLocker;
use crate::client::meta_post::meta_post::{MetaPost, MetaPostPrivateV1, MetaPostPublicV1, MetaPostV1, VersionedField, merge_public, merge_private};
use crate::client::meta_post::meta_post_crypto;
use crate::client::post_bundle::post_bundle_manager::PostBundleManager;
use crate::protocol::posting::encoded_post::EncodedPostV1;
use crate::tools::buckets::{bucket_durations_for_type, generate_bucket_location, BucketType};
use crate::tools::client_id::ClientId;
use crate::tools::json;
use crate::tools::runtime_services::RuntimeServices;
use crate::tools::time::TimeMillis;
use crate::tools::types::{Id, Salt};

/// Owns all of a user's config state — profile bio, follows, content thresholds, skip-warnings,
/// etc. — backed by a `MetaPostV1` envelope that can be published to the network.
///
/// ## Storage
///
/// Public and private config are stored as two separate keys in `BUCKET_CONFIG`:
/// - `{client_id}.meta_post_v1_public`  → `MetaPostPublicV1`
/// - `{client_id}.meta_post_v1_private` → `MetaPostPrivateV1`
///
/// `MetaPostV1` is used strictly as the network envelope — it is assembled from the two
/// stored sections at publish time and disassembled on receive. Because every field carries
/// its own `TimeMillis` timestamp, concurrent edits from multiple devices merge cleanly
/// via last-writer-wins.
///
/// ## Publishing policy
///
/// Local config changes (follows, thresholds, skip_warnings, bio) are written to
/// `BUCKET_CONFIG` immediately but are **not** published to the network on every change —
/// that would spam the network whenever a user toggles follows or adjusts sliders. A
/// `MetaPostV1` is published to the network in exactly two situations:
///
/// 1. **Explicit publish** — the user saves their bio in the UI, which calls
///    `build_meta_post_json()` followed by `submit_post()` on the client. This bundles the
///    latest config (including any pending follow/threshold changes) into the post.
/// 2. **Auto-publish on startup** — `should_auto_publish()` checks whether the current
///    month's User bucket already contains a `MetaPostV1` from this client. If not (and the
///    bucket isn't full), the client publishes one so other users and the user's own
///    devices can always find an up-to-date config within a reasonable time window.
pub struct MetaPostManager {
    runtime_services: Arc<RuntimeServices>,
    client_storage: Arc<dyn ClientStorage>,
    key_locker: Arc<dyn KeyLocker>,
    client_id: ClientId,
}

impl MetaPostManager {
    pub fn new(
        runtime_services: Arc<RuntimeServices>,
        client_storage: Arc<dyn ClientStorage>,
        key_locker: Arc<dyn KeyLocker>,
        client_id: ClientId,
    ) -> Self {
        Self { runtime_services, client_storage, key_locker, client_id }
    }

    fn now(&self) -> TimeMillis {
        self.runtime_services.time_provider.current_time_millis()
    }

    fn public_config_key(&self) -> String {
        cs::config_key_for_user(self.client_id.id, BUCKET_CONFIG_KEY_META_POST_V1_PUBLIC)
    }

    fn private_config_key(&self) -> String {
        cs::config_key_for_user(self.client_id.id, BUCKET_CONFIG_KEY_META_POST_V1_PRIVATE)
    }

    // ------------------------------------------------------------------
    // Config read/write
    // ------------------------------------------------------------------

    pub async fn get_public(&self) -> anyhow::Result<MetaPostPublicV1> {
        let public = cs::get_struct::<MetaPostPublicV1>(self.client_storage.as_ref(), BUCKET_CONFIG, &self.public_config_key(), TimeMillis::zero()).await?;
        Ok(public.unwrap_or_else(MetaPostPublicV1::empty))
    }

    async fn put_public(&self, public: &MetaPostPublicV1) -> anyhow::Result<()> {
        cs::put_struct(self.client_storage.as_ref(), BUCKET_CONFIG, &self.public_config_key(), public, TimeMillis::zero()).await
    }

    pub async fn get_private(&self) -> anyhow::Result<MetaPostPrivateV1> {
        let private = cs::get_struct::<MetaPostPrivateV1>(self.client_storage.as_ref(), BUCKET_CONFIG, &self.private_config_key(), TimeMillis::zero()).await?;
        Ok(private.unwrap_or_else(MetaPostPrivateV1::empty))
    }

    async fn put_private(&self, private: &MetaPostPrivateV1) -> anyhow::Result<()> {
        cs::put_struct(self.client_storage.as_ref(), BUCKET_CONFIG, &self.private_config_key(), private, TimeMillis::zero()).await
    }

    // ------------------------------------------------------------------
    // Bio (public section)
    // ------------------------------------------------------------------

    /// Update the public bio fields in the local config.
    /// Does NOT automatically publish to the network.
    pub async fn set_bio(&self, nickname: String, status: String, selfie: String, avatar: String) -> anyhow::Result<()> {
        info!("set_bio: nickname={}, status={}, selfie={}, avatar={}", nickname, status, selfie, avatar);

        let time_millis = self.now();
        let local_public = self.get_public().await?;
        let incoming_public = MetaPostPublicV1::from_bio(time_millis, nickname, status, selfie, avatar);
        let merged_public = merge_public(&local_public, &incoming_public);

        // Store in our client_storage::BUCKET_CONFIG
        self.put_public(&merged_public).await?;

        // Store in our client_storage::BUCKET_META_POST_PUBLIC as if we already received this from the network so local get_meta_post_public() works immediately
        cs::put_struct(self.client_storage.as_ref(), BUCKET_META_POST_PUBLIC, &self.client_id.id.to_hex_str(), &merged_public, time_millis).await?;

        Ok(())
    }

    pub async fn get_meta_post_public(&self, id: Id) -> anyhow::Result<Option<MetaPostPublicV1>> {
        let meta_post_public = cs::get_struct::<MetaPostPublicV1>(self.client_storage.as_ref(), BUCKET_META_POST_PUBLIC, &id.to_hex_str(), self.now()).await?;
        Ok(meta_post_public)
    }

    pub async fn get_all_meta_post_publics(&self) -> anyhow::Result<Vec<(String, MetaPostPublicV1)>> {
        let mut meta_post_publics = Vec::new();
        let keys = self.client_storage.keys(BUCKET_META_POST_PUBLIC).await?;
        for key in keys {
            let result: anyhow::Result<()> = try {
                if let Some(meta_post_public) = cs::get_struct::<MetaPostPublicV1>(self.client_storage.as_ref(), BUCKET_META_POST_PUBLIC, &key, TimeMillis::zero()).await? {
                    meta_post_publics.push((key.clone(), meta_post_public));
                }
            };
            if let Err(e) = result {
                warn!("Failed to decode MetaPostPublicV1 with key {}: {}", key, e);
            }
        }
        Ok(meta_post_publics)
    }

    // ------------------------------------------------------------------
    // Publishing
    // ------------------------------------------------------------------

    /// Build the MetaPostV1 JSON string ready for `submit_post()`.
    /// Also updates the local BUCKET_META_POST_PUBLIC cache.
    pub async fn build_meta_post_json(&self) -> anyhow::Result<String> {
        info!("build_meta_post_json");

        let public = self.get_public().await?;
        let private = self.get_private().await?;
        let salt = Salt::random();
        let private_encrypted = meta_post_crypto::encrypt_private_section(self.key_locker.as_ref(), &salt, &private).await?;

        let meta_post = MetaPostV1::new(
            self.client_id.id_hex(),
            salt,
            public.clone(),
            private_encrypted,
        );

        // Update local meta post public cache
        cs::put_struct(self.client_storage.as_ref(), BUCKET_META_POST_PUBLIC, &self.client_id.id.to_hex_str(), &public, self.now()).await?;

        json::struct_to_string(&meta_post)
    }

    /// Check whether the current month's User bucket already contains a
    /// MetaPostV1 from this client.  Returns `true` if we should publish.
    pub async fn should_auto_publish(&self, post_bundle_manager: &dyn PostBundleManager) -> anyhow::Result<bool> {
        let timestamp = self.now();

        let bucket_durations = bucket_durations_for_type(BucketType::User);
        let bucket_duration = bucket_durations.first().ok_or_else(|| anyhow::anyhow!("No bucket durations for User type"))?;
        let bucket_location = generate_bucket_location(BucketType::User, self.client_id.id, *bucket_duration, timestamp)?;

        let post_bundle = post_bundle_manager.get_post_bundle(&bucket_location, timestamp).await?;

        if post_bundle.header.overflowed || post_bundle.header.sealed {
            info!("Current bucket is full/sealed, skipping auto-publish of MetaPostV1");
            return Ok(false);
        }

        let mut offset = 0;
        for i in 0..(post_bundle.header.num_posts as usize) {
            let len = post_bundle.header.encoded_post_lengths[i];
            let post_bytes = post_bundle.encoded_posts_bytes.slice(offset..offset + len);
            offset += len;

            let decode_result = EncodedPostV1::decode_from_bytes(post_bytes, &bucket_location.base_id, true, true);
            if let Ok(encoded_post) = decode_result {
                if let Ok(post_client_id) = encoded_post.header.client_id() {
                    if post_client_id.id == self.client_id.id {
                        if let Ok(MetaPost::MetaPostV1(_)) = MetaPost::try_parse_meta_post(&encoded_post.post) {
                            info!("MetaPostV1 already exists in current bucket, skipping auto-publish");
                            return Ok(false);
                        }
                    }
                }
            }
        }

        info!("No MetaPostV1 found in most-recent, most-granular bucket, should auto-publish");
        Ok(true)
    }

    // ------------------------------------------------------------------
    // Receiving
    // ------------------------------------------------------------------

    /// Process an incoming MetaPostV1 from the network.
    /// Caches the public section for any user; decrypts and merges the private section
    /// if the post is from our own client_id.
    pub async fn process_incoming_meta_post(&self, meta_post_v1: &MetaPostV1, post_client_id: &ClientId) -> anyhow::Result<()> {
        let id = post_client_id.id.to_hex_str();
        let now = self.now();

        // Always cache the public section for any user
        let incoming_timestamp = meta_post_v1.public.max_timestamp();
        let existing_timestamp = cs::get_struct::<MetaPostPublicV1>(self.client_storage.as_ref(), BUCKET_META_POST_PUBLIC, &id, now).await?.map(|b| b.max_timestamp());
        if existing_timestamp.is_none_or(|t| incoming_timestamp > t) {
            cs::put_struct(self.client_storage.as_ref(), BUCKET_META_POST_PUBLIC, &id, &meta_post_v1.public, now).await?;
        }

        // If this is our own post, decrypt and merge both sections
        if post_client_id.id == self.client_id.id {
            // Merge public
            let local_public = self.get_public().await?;
            let merged_public = merge_public(&local_public, &meta_post_v1.public);
            self.put_public(&merged_public).await?;

            // Merge private
            match meta_post_crypto::decrypt_private_section(self.key_locker.as_ref(), &meta_post_v1.encryption_salt, &meta_post_v1.private_encrypted).await {
                Ok(incoming_private) => {
                    let local_private = self.get_private().await?;
                    let merged_private = merge_private(&local_private, &incoming_private, now);
                    self.put_private(&merged_private).await?;
                }
                Err(e) => {
                    warn!("Failed to decrypt own MetaPostV1 private section: {}", e);
                }
            }
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // Followed users
    // ------------------------------------------------------------------

    pub async fn get_followed_client_ids(&self) -> anyhow::Result<Vec<Id>> {
        let private = self.get_private().await?;
        let client_ids: Vec<Id> = private.followed_client_ids.iter()
            .filter(|(_key, field)| field.value == Some(true))
            .map(|(key, _field)| Id::from_hex_str(key))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(client_ids)
    }

    pub async fn set_followed_client_ids(&self, client_ids: Vec<Id>) -> anyhow::Result<()> {
        let timestamp = self.now();
        let mut private = self.get_private().await?;

        for field in private.followed_client_ids.values_mut() {
            if field.value == Some(true) {
                *field = VersionedField::tombstone(timestamp);
            }
        }
        for client_id in &client_ids {
            private.followed_client_ids.insert(client_id.to_hex_str(), VersionedField::new(true, timestamp));
        }

        self.put_private(&private).await
    }

    pub async fn set_followed_client_id(&self, client_id: Id, is_followed: bool) -> anyhow::Result<()> {
        let timestamp = self.now();
        let mut private = self.get_private().await?;

        let field = if is_followed {
            VersionedField::new(true, timestamp)
        } else {
            VersionedField::tombstone(timestamp)
        };
        private.followed_client_ids.insert(client_id.to_hex_str(), field);

        self.put_private(&private).await
    }

    // ------------------------------------------------------------------
    // Followed hashtags
    // ------------------------------------------------------------------

    pub async fn get_followed_hashtags(&self) -> anyhow::Result<Vec<String>> {
        let private = self.get_private().await?;
        let hashtags: Vec<String> = private.followed_hashtags.iter()
            .filter(|(_key, field)| field.value == Some(true))
            .map(|(key, _field)| key.clone())
            .collect();
        Ok(hashtags)
    }

    pub async fn set_followed_hashtags(&self, hashtags: Vec<String>) -> anyhow::Result<()> {
        let timestamp = self.now();
        let mut private = self.get_private().await?;

        for field in private.followed_hashtags.values_mut() {
            if field.value == Some(true) {
                *field = VersionedField::tombstone(timestamp);
            }
        }
        for hashtag in &hashtags {
            private.followed_hashtags.insert(hashtag.clone(), VersionedField::new(true, timestamp));
        }

        self.put_private(&private).await
    }

    pub async fn set_followed_hashtag(&self, hashtag: String, is_followed: bool) -> anyhow::Result<()> {
        let timestamp = self.now();
        let mut private = self.get_private().await?;

        let field = if is_followed {
            VersionedField::new(true, timestamp)
        } else {
            VersionedField::tombstone(timestamp)
        };
        private.followed_hashtags.insert(hashtag, field);

        self.put_private(&private).await
    }

    // ------------------------------------------------------------------
    // Content thresholds
    // ------------------------------------------------------------------

    pub async fn get_content_thresholds(&self) -> anyhow::Result<HashMap<u8, u32>> {
        let private = self.get_private().await?;
        let thresholds: HashMap<u8, u32> = private.content_thresholds.iter()
            .filter_map(|(key, field)| {
                let threshold = field.value?;
                Some((*key, threshold))
            })
            .collect();
        Ok(thresholds)
    }

    pub async fn set_content_threshold(&self, feedback_type: u8, threshold: u32) -> anyhow::Result<()> {
        let timestamp = self.now();
        let mut private = self.get_private().await?;
        private.content_thresholds.insert(feedback_type, VersionedField::new(threshold, timestamp));
        self.put_private(&private).await
    }

    pub async fn set_content_thresholds(&self, thresholds: HashMap<u8, u32>) -> anyhow::Result<()> {
        let timestamp = self.now();
        let mut private = self.get_private().await?;
        for (feedback_type, threshold) in thresholds {
            private.content_thresholds.insert(feedback_type, VersionedField::new(threshold, timestamp));
        }
        self.put_private(&private).await
    }

    // ------------------------------------------------------------------
    // Skip warnings for followed
    // ------------------------------------------------------------------

    pub async fn get_skip_warnings_for_followed(&self) -> anyhow::Result<bool> {
        let private = self.get_private().await?;
        Ok(private.skip_warnings_for_followed.value.unwrap_or(false))
    }

    pub async fn set_skip_warnings_for_followed(&self, value: bool) -> anyhow::Result<()> {
        let timestamp = self.now();
        let mut private = self.get_private().await?;
        private.skip_warnings_for_followed = VersionedField::new(value, timestamp);
        self.put_private(&private).await
    }
}
