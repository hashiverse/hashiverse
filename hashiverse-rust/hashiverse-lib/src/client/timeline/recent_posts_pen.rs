//! # Scratch pad for just-authored local posts
//!
//! When the user hits "post", the resulting signed post must make its way out onto the
//! DHT before it can be read back through the usual bucket fetch. The network path
//! takes at least a round trip plus proof-of-work — far too long for "post disappears
//! for 10 seconds then reappears in my own timeline" to feel acceptable.
//!
//! `RecentPostsPen` closes that gap. Every locally-authored post is deposited here
//! indexed by `(BucketType, base_id)` with a short TTL (~10 minutes). Every timeline
//! walk consults it before diving into the bucket fetch, so the user's own posts surface
//! in their own timelines immediately. Old entries age out automatically, and
//! deduplication against the timeline's seen set prevents duplicate rendering once the
//! network fetch catches up.

use bytes::Bytes;
use std::collections::HashSet;

use crate::tools::buckets::{BucketLocation, BucketType};
use crate::tools::time::{DurationMillis, MILLIS_IN_MINUTE, TimeMillis};
use crate::tools::types::Id;

const RECENT_POSTS_PEN_TTL: DurationMillis = MILLIS_IN_MINUTE.const_mul(10);

pub struct RecentPostsPenEntry {
    pub bucket_location: BucketLocation,
    pub post_id: Id,
    pub encoded_post_bytes: Bytes,
    pub time_millis: TimeMillis,
}

/// Short-lived scratch space for posts the local client has just submitted.
///
/// After a successful `submit_post`, the resulting commit tokens and encoded post bytes are
/// recorded here. When any `SingleTimeline` calls `get_more_posts`, it consults the pen for
/// entries whose `BucketLocation` matches the timeline being viewed. This ensures the user's
/// own posts appear immediately — even before the post bundles have propagated through the
/// network caches — and works across all timeline types (User, Hashtag, Mention, Reply, Sequel, etc.).
///
/// Entries expire after 10 minutes (by which time the network caches will have refreshed and
/// the posts will appear naturally from the `PostBundleManager`). Deduplication against the
/// `SingleTimeline`'s `post_ids_already_seen` set prevents a post from showing up twice once
/// it does arrive from the network.
pub struct RecentPostsPen {
    entries: Vec<RecentPostsPenEntry>,
}

impl Default for RecentPostsPen {
    fn default() -> Self {
        Self::new()
    }
}

impl RecentPostsPen {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Add pen entries from commit tokens. Each token represents the post committed to a
    /// particular timeline (User, Hashtag, Mention, etc.) — we store one entry per token.
    pub fn add_all(&mut self, bucket_locations_and_post_ids: &[(BucketLocation, Id)], encoded_post_bytes: Bytes, time_millis: TimeMillis) {
        for (bucket_location, post_id) in bucket_locations_and_post_ids {
            self.entries.push(RecentPostsPenEntry {
                bucket_location: bucket_location.clone(),
                post_id: *post_id,
                encoded_post_bytes: encoded_post_bytes.clone(),
                time_millis,
            });
        }
    }

    /// Returns matching pen posts for the given timeline, excluding expired and already-seen entries.
    /// Multiple entries for the same post_id may be returned (e.g. from multiple commit tokens);
    /// `SingleTimeline` handles deduplication via `post_ids_already_seen`.
    pub fn get_matching_posts(
        &mut self,
        bucket_type: BucketType,
        base_id: &Id,
        already_seen_ids: &HashSet<Id>,
        current_time: TimeMillis,
    ) -> Vec<(BucketLocation, Bytes, Id)> {
        // Prune expired entries
        let cutoff = current_time - RECENT_POSTS_PEN_TTL;
        self.entries.retain(|entry| entry.time_millis >= cutoff);

        let mut matching_posts: Vec<(BucketLocation, Bytes, Id)> = Vec::new();

        for entry in &self.entries {
            if entry.bucket_location.bucket_type != bucket_type || entry.bucket_location.base_id != *base_id {
                continue;
            }
            if already_seen_ids.contains(&entry.post_id) {
                continue;
            }

            matching_posts.push((entry.bucket_location.clone(), entry.encoded_post_bytes.clone(), entry.post_id));
        }

        matching_posts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::time::MILLIS_IN_MINUTE;

    fn make_entry(bucket_type: BucketType, base_id: Id, post_id: Id, time: TimeMillis) -> (BucketLocation, Id) {
        let bucket_location = BucketLocation::new(bucket_type, base_id, MILLIS_IN_MINUTE, time).unwrap();
        (bucket_location, post_id)
    }

    #[test]
    fn test_matching_by_bucket_type_and_base_id() {
        let mut pen = RecentPostsPen::new();
        let base_id = Id::random();
        let other_base_id = Id::random();
        let post_id = Id::random();
        let time = TimeMillis::from_epoch_offset_str("1M").unwrap();

        let entries = vec![
            make_entry(BucketType::User, base_id, post_id, time),
            make_entry(BucketType::Hashtag, other_base_id, post_id, time),
        ];
        pen.add_all(&entries, Bytes::from_static(b"test post"), time);

        let already_seen = HashSet::new();

        // Should match User timeline for base_id
        let result = pen.get_matching_posts(BucketType::User, &base_id, &already_seen, time);
        assert_eq!(result.len(), 1);

        // Should match Hashtag timeline for other_base_id
        let result = pen.get_matching_posts(BucketType::Hashtag, &other_base_id, &already_seen, time);
        assert_eq!(result.len(), 1);

        // Should NOT match User timeline for other_base_id
        let result = pen.get_matching_posts(BucketType::User, &other_base_id, &already_seen, time);
        assert_eq!(result.len(), 0);

        // Should NOT match Hashtag timeline for base_id
        let result = pen.get_matching_posts(BucketType::Hashtag, &base_id, &already_seen, time);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_ttl_expiration() {
        let mut pen = RecentPostsPen::new();
        let base_id = Id::random();
        let post_id = Id::random();
        let time = TimeMillis::from_epoch_offset_str("1M").unwrap();

        pen.add_all(&[make_entry(BucketType::User, base_id, post_id, time)], Bytes::from_static(b"post"), time);

        let already_seen = HashSet::new();

        // Still within TTL
        let within_ttl = time + MILLIS_IN_MINUTE.const_mul(9);
        let result = pen.get_matching_posts(BucketType::User, &base_id, &already_seen, within_ttl);
        assert_eq!(result.len(), 1);

        // Past TTL
        let past_ttl = time + MILLIS_IN_MINUTE.const_mul(11);
        let result = pen.get_matching_posts(BucketType::User, &base_id, &already_seen, past_ttl);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_multiple_tokens_same_post_returns_all() {
        let mut pen = RecentPostsPen::new();
        let base_id = Id::random();
        let post_id = Id::random();
        let time = TimeMillis::from_epoch_offset_str("1M").unwrap();

        // 3 commit tokens from 3 different peers for the same post on the same timeline —
        // the pen returns all of them; deduplication by post_id is SingleTimeline's job.
        let entries = vec![
            make_entry(BucketType::User, base_id, post_id, time),
            make_entry(BucketType::User, base_id, post_id, time),
            make_entry(BucketType::User, base_id, post_id, time),
        ];
        pen.add_all(&entries, Bytes::from_static(b"post"), time);

        let already_seen = HashSet::new();
        let result = pen.get_matching_posts(BucketType::User, &base_id, &already_seen, time);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_already_seen_filtering() {
        let mut pen = RecentPostsPen::new();
        let base_id = Id::random();
        let post_id = Id::random();
        let time = TimeMillis::from_epoch_offset_str("1M").unwrap();

        pen.add_all(&[make_entry(BucketType::User, base_id, post_id, time)], Bytes::from_static(b"post"), time);

        let mut already_seen = HashSet::new();
        already_seen.insert(post_id);

        let result = pen.get_matching_posts(BucketType::User, &base_id, &already_seen, time);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_single_post_multiple_timelines() {
        let mut pen = RecentPostsPen::new();
        let user_id = Id::random();
        let hashtag_id = Id::random();
        let mention_id = Id::random();
        let post_id = Id::random();
        let time = TimeMillis::from_epoch_offset_str("1M").unwrap();

        let entries = vec![
            make_entry(BucketType::User, user_id, post_id, time),
            make_entry(BucketType::Hashtag, hashtag_id, post_id, time),
            make_entry(BucketType::Mention, mention_id, post_id, time),
        ];
        pen.add_all(&entries, Bytes::from_static(b"post"), time);

        let already_seen = HashSet::new();

        // Each timeline should independently find the post
        assert_eq!(pen.get_matching_posts(BucketType::User, &user_id, &already_seen, time).len(), 1);
        assert_eq!(pen.get_matching_posts(BucketType::Hashtag, &hashtag_id, &already_seen, time).len(), 1);
        assert_eq!(pen.get_matching_posts(BucketType::Mention, &mention_id, &already_seen, time).len(), 1);
    }
}
