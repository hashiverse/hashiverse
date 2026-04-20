//! # Meta-post data model: per-account profile & config
//!
//! A "meta-post" is the one post per account per month that carries the user's profile
//! and client-side config rather than a conversational payload. The public section
//! (nickname, status, avatar, follows) is readable by everyone on the network; the
//! private section (per-feedback-type thresholds, skip-warnings flags, anything we'd
//! rather keep account-local) is encrypted so only the owner can read it — see
//! [`crate::client::meta_post::meta_post_crypto`].
//!
//! Each field is wrapped in `VersionedField<T>` with a monotonic timestamp, giving the
//! whole document last-writer-wins CRDT semantics at field granularity. Two devices can
//! edit the profile concurrently and converge without either losing the other's changes
//! to unrelated fields.
//!
//! The [`MetaPost`] enum wraps the versioned variants (currently just `V1`) so that a
//! post found on a User bucket can be parsed back into a meta-post whenever the contents
//! match.

use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use crate::tools::json;
use crate::tools::time::{TimeMillis, MILLIS_IN_MONTH};
use crate::tools::types::Salt;

// ---------------------------------------------------------------------------
// VersionedField — generic per-field/per-item CRDT with last-writer-wins
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct VersionedField<T> {
    /// The current value, or `None` if this field/item has been deleted (tombstone).
    pub value: Option<T>,
    /// Millisecond timestamp of the last write.  Higher wins on merge.
    pub timestamp: TimeMillis,
}

impl<T> VersionedField<T> {
    pub fn new(value: T, timestamp: TimeMillis) -> Self {
        Self { value: Some(value), timestamp }
    }

    pub fn tombstone(timestamp: TimeMillis) -> Self {
        Self { value: None, timestamp }
    }

    pub fn is_tombstone(&self) -> bool {
        self.value.is_none()
    }
}

// ---------------------------------------------------------------------------
// MetaPost envelope (parsed from the JSON `meta` field)
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum MetaPost {
    None,
    MetaPostV1(MetaPostV1),
}

impl MetaPost {
    pub fn try_parse_meta_post(post: &str) -> anyhow::Result<MetaPost> {
        let post = post.as_bytes();
        let meta = json::bytes_to_struct::<MetaOnly>(post);
        match meta {
            Err(_) => Ok(Self::None),
            Ok(meta) => match meta.meta.as_str() {
                "MetaPostV1" => Ok(Self::MetaPostV1(json::bytes_to_struct(post)?)),
                meta => anyhow::bail!("Unsupported meta type: {}", meta),
            },
        }
    }
}

#[derive(Deserialize)]
struct MetaOnly {
    meta: String,
}

// ---------------------------------------------------------------------------
// MetaPostV1 — the unified config meta-post
// ---------------------------------------------------------------------------

const META_POST_V1_META: &str = "MetaPostV1";

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct MetaPostV1 {
    meta: String,
    pub client_id: String,
    pub encryption_salt: Salt,
    pub public: MetaPostPublicV1,
    /// Hex-encoded, strongly-encrypted blob containing a serialised
    /// `MetaPostPrivateV1`.  Other users cannot decrypt this.
    pub private_encrypted: String,
}

impl MetaPostV1 {
    pub fn new(client_id: String, encryption_salt: Salt, public: MetaPostPublicV1, private_encrypted: String) -> Self {
        Self {
            meta: META_POST_V1_META.to_string(),
            client_id,
            encryption_salt,
            public,
            private_encrypted,
        }
    }
}

// ---------------------------------------------------------------------------
// Public section — readable by everyone (bio data)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct MetaPostPublicV1 {
    pub nickname: VersionedField<String>,
    pub status: VersionedField<String>,
    pub selfie: VersionedField<String>,
    pub avatar: VersionedField<String>,
}

impl MetaPostPublicV1 {
    pub fn empty() -> Self {
        Self {
            nickname: VersionedField::tombstone(TimeMillis::zero()),
            status: VersionedField::tombstone(TimeMillis::zero()),
            selfie: VersionedField::tombstone(TimeMillis::zero()),
            avatar: VersionedField::tombstone(TimeMillis::zero()),
        }
    }

    pub fn from_bio(timestamp: TimeMillis, nickname: String, status: String, selfie: String, avatar: String) -> Self {
        Self {
            nickname: VersionedField::new(nickname, timestamp),
            status: VersionedField::new(status, timestamp),
            selfie: VersionedField::new(selfie, timestamp),
            avatar: VersionedField::new(avatar, timestamp),
        }
    }

    /// The highest timestamp across all public fields — useful for
    /// deciding whether an incoming bio is newer than a cached one.
    pub fn max_timestamp(&self) -> TimeMillis {
        [self.nickname.timestamp, self.status.timestamp, self.selfie.timestamp, self.avatar.timestamp]
            .into_iter()
            .max()
            .unwrap_or(TimeMillis::zero())
    }
}

// ---------------------------------------------------------------------------
// Private section — encrypted, only the owning client can read
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct MetaPostPrivateV1 {
    /// Key = user-id hex string.  Value = `true` means followed, `None` = unfollowed tombstone.
    pub followed_client_ids: HashMap<String, VersionedField<bool>>,
    /// Key = hashtag string.
    pub followed_hashtags: HashMap<String, VersionedField<bool>>,
    /// Key = feedback_type (0-255).  Value = threshold count.
    pub content_thresholds: HashMap<u8, VersionedField<u32>>,
    pub skip_warnings_for_followed: VersionedField<bool>,
}

impl MetaPostPrivateV1 {
    pub fn empty() -> Self {
        Self {
            followed_client_ids: HashMap::new(),
            followed_hashtags: HashMap::new(),
            content_thresholds: HashMap::new(),
            skip_warnings_for_followed: VersionedField::tombstone(TimeMillis::zero()),
        }
    }
}

// ---------------------------------------------------------------------------
// Merge helpers
// ---------------------------------------------------------------------------

use crate::tools::time::DurationMillis;

const TOMBSTONE_MAX_AGE: DurationMillis = MILLIS_IN_MONTH.const_mul(3);

/// Merge a single scalar versioned field: last-writer-wins.
pub fn merge_scalar_field<T: Clone>(local: &VersionedField<T>, incoming: &VersionedField<T>) -> VersionedField<T> {
    if incoming.timestamp > local.timestamp {
        incoming.clone()
    } else {
        local.clone()
    }
}

/// Merge a versioned map (collection field): per-key last-writer-wins,
/// with tombstone garbage collection for entries older than 6 months.
pub fn merge_collection<K: Clone + Eq + std::hash::Hash, T: Clone>(local: &HashMap<K, VersionedField<T>>, incoming: &HashMap<K, VersionedField<T>>, now_millis: TimeMillis) -> HashMap<K, VersionedField<T>> {
    let mut merged: HashMap<K, VersionedField<T>> = local.clone();

    for (key, incoming_field) in incoming {
        match merged.get(key) {
            Some(local_field) => {
                if incoming_field.timestamp > local_field.timestamp {
                    merged.insert(key.clone(), incoming_field.clone());
                }
            }
            None => {
                merged.insert(key.clone(), incoming_field.clone());
            }
        }
    }

    // Garbage-collect old tombstones
    let cutoff = now_millis.saturating_sub_duration(TOMBSTONE_MAX_AGE);
    merged.retain(|_key, field| {
        !(field.is_tombstone() && field.timestamp < cutoff)
    });

    merged
}

/// Merge two full public sections.
pub fn merge_public(local: &MetaPostPublicV1, incoming: &MetaPostPublicV1) -> MetaPostPublicV1 {
    MetaPostPublicV1 {
        nickname: merge_scalar_field(&local.nickname, &incoming.nickname),
        status: merge_scalar_field(&local.status, &incoming.status),
        selfie: merge_scalar_field(&local.selfie, &incoming.selfie),
        avatar: merge_scalar_field(&local.avatar, &incoming.avatar),
    }
}

/// Merge two full private sections.
pub fn merge_private(local: &MetaPostPrivateV1, incoming: &MetaPostPrivateV1, now_millis: TimeMillis) -> MetaPostPrivateV1 {
    MetaPostPrivateV1 {
        followed_client_ids: merge_collection(&local.followed_client_ids, &incoming.followed_client_ids, now_millis),
        followed_hashtags: merge_collection(&local.followed_hashtags, &incoming.followed_hashtags, now_millis),
        content_thresholds: merge_collection(&local.content_thresholds, &incoming.content_thresholds, now_millis),
        skip_warnings_for_followed: merge_scalar_field(&local.skip_warnings_for_followed, &incoming.skip_warnings_for_followed),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::json;
    use crate::tools::types::Id;

    fn t(millis: i64) -> TimeMillis { TimeMillis(millis) }

    #[test]
    fn meta_post_v1_roundtrip() -> anyhow::Result<()> {
        let public = MetaPostPublicV1 {
            nickname: VersionedField::new("alice".to_string(), t(100)),
            status: VersionedField::new("hello world".to_string(), t(100)),
            selfie: VersionedField::new("base64data".to_string(), t(100)),
            avatar: VersionedField::new("seed123".to_string(), t(100)),
        };

        let meta_post = MetaPostV1::new(
            Id::random().to_hex_str(),
            Salt::random(),
            public,
            "deadbeef".to_string(),
        );

        let post_bytes = json::struct_to_bytes(&meta_post)?;
        let post_str = std::str::from_utf8(&post_bytes)?;

        let parsed = MetaPost::try_parse_meta_post(post_str)?;
        match parsed {
            MetaPost::MetaPostV1(restored) => assert_eq!(restored, meta_post),
            other => anyhow::bail!("Expected MetaPostV1, got {:?}", other),
        }

        Ok(())
    }

    #[test]
    fn normal_posts_return_none() -> anyhow::Result<()> {
        let post = "This is a simple post";
        assert_eq!(MetaPost::None, MetaPost::try_parse_meta_post(post)?);
        Ok(())
    }

    #[test]
    fn malformed_json_is_just_like_a_normal_post() -> anyhow::Result<()> {
        let post = r#"{"meta":"MetaPostV1", the rest is garbage"#;
        assert_eq!(MetaPost::None, MetaPost::try_parse_meta_post(post)?);
        Ok(())
    }

    #[test]
    fn well_formed_but_incomplete_json_is_an_error() -> anyhow::Result<()> {
        let post = r#"{"meta":"MetaPostV1", "name": "terry pratt" }"#;
        assert!(MetaPost::try_parse_meta_post(post).is_err());
        Ok(())
    }

    #[test]
    fn unknown_meta_is_an_error() -> anyhow::Result<()> {
        let post = r#"{"meta":"WhackoV123", "name": "terry pratt" }"#;
        assert!(MetaPost::try_parse_meta_post(post).is_err());
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Merge tests
    // -----------------------------------------------------------------------

    #[test]
    fn merge_scalar_field_higher_timestamp_wins() {
        let local = VersionedField::new("old".to_string(), t(100));
        let incoming = VersionedField::new("new".to_string(), t(200));
        let merged = merge_scalar_field(&local, &incoming);
        assert_eq!(merged.value, Some("new".to_string()));
        assert_eq!(merged.timestamp, t(200));
    }

    #[test]
    fn merge_scalar_field_local_wins_on_tie() {
        let local = VersionedField::new("local".to_string(), t(100));
        let incoming = VersionedField::new("incoming".to_string(), t(100));
        let merged = merge_scalar_field(&local, &incoming);
        assert_eq!(merged.value, Some("local".to_string()));
    }

    #[test]
    fn merge_scalar_field_tombstone_wins_if_newer() {
        let local = VersionedField::new("alive".to_string(), t(100));
        let incoming: VersionedField<String> = VersionedField::tombstone(t(200));
        let merged = merge_scalar_field(&local, &incoming);
        assert!(merged.is_tombstone());
        assert_eq!(merged.timestamp, t(200));
    }

    #[test]
    fn merge_collection_combines_disjoint_keys() {
        let mut local: HashMap<String, VersionedField<bool>> = HashMap::new();
        local.insert("a".to_string(), VersionedField::new(true, t(100)));

        let mut incoming: HashMap<String, VersionedField<bool>> = HashMap::new();
        incoming.insert("b".to_string(), VersionedField::new(true, t(200)));

        let merged = merge_collection(&local, &incoming, t(300));
        assert_eq!(merged.len(), 2);
        assert!(merged.contains_key("a"));
        assert!(merged.contains_key("b"));
    }

    #[test]
    fn merge_collection_higher_timestamp_wins_per_key() {
        let mut local: HashMap<String, VersionedField<bool>> = HashMap::new();
        local.insert("x".to_string(), VersionedField::new(true, t(100)));

        let mut incoming: HashMap<String, VersionedField<bool>> = HashMap::new();
        incoming.insert("x".to_string(), VersionedField::tombstone(t(200)));

        let merged = merge_collection(&local, &incoming, t(300));
        assert!(merged["x"].is_tombstone());
        assert_eq!(merged["x"].timestamp, t(200));
    }

    #[test]
    fn merge_collection_garbage_collects_old_tombstones() {
        let now = t(TOMBSTONE_MAX_AGE.0 * 2); // well past the GC window
        let seven_months_ago = TimeMillis(now.0 - TOMBSTONE_MAX_AGE.0 - 1);

        let mut local: HashMap<String, VersionedField<bool>> = HashMap::new();
        local.insert("old_deleted".to_string(), VersionedField::tombstone(seven_months_ago));
        local.insert("recent_deleted".to_string(), VersionedField::tombstone(TimeMillis(now.0 - 1)));
        local.insert("alive".to_string(), VersionedField::new(true, TimeMillis(now.0 - 500)));

        let incoming: HashMap<String, VersionedField<bool>> = HashMap::new();

        let merged = merge_collection(&local, &incoming, now);
        assert!(!merged.contains_key("old_deleted"), "Old tombstone should be garbage collected");
        assert!(merged.contains_key("recent_deleted"), "Recent tombstone should be kept");
        assert!(merged.contains_key("alive"), "Alive entry should be kept");
    }

    #[test]
    fn merge_public_picks_newer_fields() {
        let local = MetaPostPublicV1 {
            nickname: VersionedField::new("old_nick".to_string(), t(100)),
            status: VersionedField::new("old_status".to_string(), t(200)),
            selfie: VersionedField::new("old_selfie".to_string(), t(300)),
            avatar: VersionedField::new("old_avatar".to_string(), t(400)),
        };

        let incoming = MetaPostPublicV1 {
            nickname: VersionedField::new("new_nick".to_string(), t(200)),   // newer
            status: VersionedField::new("new_status".to_string(), t(100)),   // older
            selfie: VersionedField::new("new_selfie".to_string(), t(300)),   // tie -> local wins
            avatar: VersionedField::new("new_avatar".to_string(), t(500)),   // newer
        };

        let merged = merge_public(&local, &incoming);
        assert_eq!(merged.nickname.value, Some("new_nick".to_string()));
        assert_eq!(merged.status.value, Some("old_status".to_string()));
        assert_eq!(merged.selfie.value, Some("old_selfie".to_string()));
        assert_eq!(merged.avatar.value, Some("new_avatar".to_string()));
    }

    #[test]
    fn merge_private_merges_all_sections() {
        let mut local = MetaPostPrivateV1::empty();
        local.followed_client_ids.insert("client_a".to_string(), VersionedField::new(true, t(100)));
        local.skip_warnings_for_followed = VersionedField::new(false, t(100));

        let mut incoming = MetaPostPrivateV1::empty();
        incoming.followed_client_ids.insert("client_b".to_string(), VersionedField::new(true, t(200)));
        incoming.skip_warnings_for_followed = VersionedField::new(true, t(200));

        let merged = merge_private(&local, &incoming, t(300));
        assert_eq!(merged.followed_client_ids.len(), 2);
        assert!(merged.followed_client_ids.contains_key("client_a"));
        assert!(merged.followed_client_ids.contains_key("client_b"));
        assert_eq!(merged.skip_warnings_for_followed.value, Some(true));
    }

    #[test]
    fn private_v1_roundtrip() -> anyhow::Result<()> {
        let mut private = MetaPostPrivateV1::empty();
        private.followed_client_ids.insert("abc123".to_string(), VersionedField::new(true, t(100)));
        private.followed_client_ids.insert("def456".to_string(), VersionedField::tombstone(t(200)));
        private.followed_hashtags.insert("rust".to_string(), VersionedField::new(true, t(150)));
        private.content_thresholds.insert(101, VersionedField::new(100, t(300)));
        private.skip_warnings_for_followed = VersionedField::new(true, t(400));

        let bytes = json::struct_to_bytes(&private)?;
        let restored: MetaPostPrivateV1 = json::bytes_to_struct(&bytes)?;
        assert_eq!(private, restored);
        Ok(())
    }
}
