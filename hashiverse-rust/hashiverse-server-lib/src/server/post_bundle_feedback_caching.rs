//! # Post-bundle feedback read cache
//!
//! Sister of [`crate::server::post_bundle_caching`] for the feedback layer.
//! One key difference: feedback is pre-merged by the client before it arrives at
//! the server, so the cache stores a single canonical entry per `location_id`
//! instead of one per originator. This also means the cached value is typically
//! much smaller (~8 KB placeholder weight vs. the post-bundle cache's 4 MB), so
//! more entries fit into the same memory budget.
//!
//! Cache-request token issuance and the hit-count heuristic work identically to
//! the post-bundle cache, so hot feedback gets gossiped out in the same way hot
//! bundles do.

use bytes::Bytes;
use hashiverse_lib::protocol::payload::payload::CacheRequestTokenV1;
use hashiverse_lib::protocol::peer::Peer;
use hashiverse_lib::tools::buckets::BucketLocation;
use hashiverse_lib::tools::server_id::ServerId;
use hashiverse_lib::tools::time::{TimeMillis, MILLIS_IN_MINUTE};
use hashiverse_lib::tools::types::Id;
use moka::sync::Cache;
use std::sync::{Arc, Mutex};

use crate::server::post_bundle_caching_shared::{CachedBundle, GetCacheResult, CACHE_HIT_THRESHOLD, CACHE_LOCATION_TTI, CACHE_REQUEST_TOKEN_TTL_DURATION, CACHE_REQUEST_TOKEN_TTL_DURATION_MILLIS};

/// Placeholder weight for feedback entries — approximates a merged feedback bundle.
/// Ballpark: 20 posts × 10 feedbacks × 50 bytes each ≈ 10 KB.
const POST_BUNDLE_FEEDBACK_PLACEHOLDER_WEIGHT: u32 = 8 * 1024;

// --------------------------------------------------------------------------------------------
// CachedPostBundleFeedbackLocationEntry
// --------------------------------------------------------------------------------------------

/// Per-location entry for feedback.
/// Only one canonical merged result is stored — the client already merges feedback across
/// servers before uploading, so multiple originator versions are not meaningful here.
struct CachedPostBundleFeedbackLocationEntry {
    /// The single merged feedback bundle, together with the originator's peer_id.
    bundle: Option<(Id, CachedBundle)>,
    hit_count: u32,
}

impl CachedPostBundleFeedbackLocationEntry {
    fn placeholder() -> Self {
        Self { bundle: None, hit_count: 0 }
    }

    fn weight(&self) -> u32 {
        self.bundle.as_ref().map(|(_, b)| b.bytes.len() as u32).unwrap_or(POST_BUNDLE_FEEDBACK_PLACEHOLDER_WEIGHT)
    }
}

// --------------------------------------------------------------------------------------------
// PostBundleFeedbackCache
// --------------------------------------------------------------------------------------------

/// Intermediate-server cache for `EncodedPostBundleFeedbackV1` data.
///
/// Two Moka caches:
/// - `bundles`: weighted `Cache<Id, Arc<Mutex<CachedPostBundleFeedbackLocationEntry>>>` with TTI.
///   If a location_id hasn't been queried within `CACHE_LOCATION_TTI`, the entry is evicted.
/// - `inflight`: `Cache<Id, ()>` with 30-second TTL — tracks pending upload tokens.
pub struct PostBundleFeedbackCache {
    bundles: Cache<Id, Arc<Mutex<CachedPostBundleFeedbackLocationEntry>>>,
    inflight: Cache<Id, ()>,
}

impl PostBundleFeedbackCache {
    pub fn new(max_bytes: u64) -> Self {
        let bundles = Cache::builder()
            .weigher(|_key: &Id, entry: &Arc<Mutex<CachedPostBundleFeedbackLocationEntry>>| {
                entry.lock().map(|e| e.weight()).unwrap_or(POST_BUNDLE_FEEDBACK_PLACEHOLDER_WEIGHT)
            })
            .max_capacity(max_bytes)
            .time_to_idle(CACHE_LOCATION_TTI)
            .build();

        let inflight = Cache::builder()
            .time_to_live(CACHE_REQUEST_TOKEN_TTL_DURATION)
            .build();

        Self { bundles, inflight }
    }

    /// Called by the dispatch handler when serving a `GetPostBundleFeedbackV1` request.
    pub fn on_get(
        &self,
        bucket_location: &BucketLocation,
        already_retrieved_peer_ids: &[Id],
        peer_self: &Peer,
        server_id: &ServerId,
        now: TimeMillis,
    ) -> GetCacheResult {
        let location_id = bucket_location.location_id;
        let entry_arc = self.bundles.get_with(location_id, || Arc::new(Mutex::new(CachedPostBundleFeedbackLocationEntry::placeholder())));

        let (cached_items, already_cached_peer_ids, should_issue_token) = {
            let mut entry = entry_arc.lock().unwrap();
            entry.hit_count += 1;

            let cached_items: Vec<Bytes> = entry.bundle
                .iter()
                .filter(|(originator_id, bundle)| !already_retrieved_peer_ids.contains(originator_id) && !bundle.is_stale(now))
                .map(|(_, bundle)| bundle.bytes.clone())
                .collect();

            let already_cached_peer_ids: Vec<Id> = entry.bundle.iter().map(|(id, _)| *id).collect();
            let should_issue_token = entry.hit_count >= CACHE_HIT_THRESHOLD && !self.inflight.contains_key(&location_id);
            (cached_items, already_cached_peer_ids, should_issue_token)
        };

        let cache_request_token = if should_issue_token {
            self.inflight.insert(location_id, ());
            let expires_at = now + CACHE_REQUEST_TOKEN_TTL_DURATION_MILLIS;
            Some(CacheRequestTokenV1::new(peer_self.clone(), bucket_location.clone(), expires_at, already_cached_peer_ids, &server_id.keys.signature_key))
        } else {
            None
        };

        GetCacheResult { cached_items, cache_request_token }
    }

    /// Called by the dispatch handler when a `CachePostBundleFeedbackV1` upload arrives.
    /// Unconditionally replaces any previously cached feedback — the uploader has the merged best.
    /// Returns `true` if accepted, `false` if the entry was evicted before the upload arrived.
    pub fn on_upload(
        &self,
        location_id: Id,
        originator_peer_id: Id,
        feedback_bytes: Bytes,
        server_time: TimeMillis,
        is_sealed: bool,
    ) -> bool {
        let entry_arc = match self.bundles.get(&location_id) {
            Some(e) => e,
            None => return false,
        };

        let mut entry = entry_arc.lock().unwrap();
        let expires_at = if is_sealed { None } else { Some(server_time + MILLIS_IN_MINUTE.const_mul(5)) };
        entry.bundle = Some((originator_peer_id, CachedBundle { bytes: feedback_bytes, expires_at }));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use hashiverse_lib::tools::buckets::{BucketLocation, BucketType, BUCKET_DURATIONS};
    use hashiverse_lib::tools::server_id::ServerId;
    use hashiverse_lib::tools::time::TimeMillis;
    use hashiverse_lib::tools::time_provider::time_provider::RealTimeProvider;
    use hashiverse_lib::tools::parallel_pow_generator::StubParallelPowGenerator;
    use hashiverse_lib::tools::types::{Id, Pow};

    async fn make_test_server_and_peer() -> anyhow::Result<(ServerId, Peer)> {
        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new(&time_provider, Pow(0), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;
        Ok((server_id, peer))
    }

    fn make_test_bucket_location() -> BucketLocation {
        BucketLocation::new(BucketType::User, Id::random(), BUCKET_DURATIONS[0], TimeMillis(1_000_000)).unwrap()
    }

    #[tokio::test]
    async fn test_below_threshold_no_token() -> anyhow::Result<()> {
        let (server_id, peer_self) = make_test_server_and_peer().await?;
        let cache = PostBundleFeedbackCache::new(16 * 1024 * 1024);
        let bucket_location = make_test_bucket_location();
        let now = TimeMillis(1_000_000);

        for _ in 0..(CACHE_HIT_THRESHOLD - 1) {
            let result = cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);
            assert!(result.cache_request_token.is_none());
            assert!(result.cached_items.is_empty());
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_at_threshold_token_issued_then_deduplicated() -> anyhow::Result<()> {
        let (server_id, peer_self) = make_test_server_and_peer().await?;
        let cache = PostBundleFeedbackCache::new(16 * 1024 * 1024);
        let bucket_location = make_test_bucket_location();
        let now = TimeMillis(1_000_000);

        for _ in 0..(CACHE_HIT_THRESHOLD - 1) {
            let result = cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);
            assert!(result.cache_request_token.is_none());
        }

        let result = cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);
        assert!(result.cache_request_token.is_some());

        let result2 = cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);
        assert!(result2.cache_request_token.is_none());

        Ok(())
    }

    #[tokio::test]
    async fn test_upload_and_retrieval() -> anyhow::Result<()> {
        let (server_id, peer_self) = make_test_server_and_peer().await?;
        let cache = PostBundleFeedbackCache::new(16 * 1024 * 1024);
        let bucket_location = make_test_bucket_location();
        let location_id = bucket_location.location_id;
        let now = TimeMillis(1_000_000);
        let originator_id = Id::random();
        let feedback_bytes = Bytes::from_static(b"test_feedback");

        cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);

        let accepted = cache.on_upload(location_id, originator_id, feedback_bytes.clone(), now, false);
        assert!(accepted);

        let result = cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);
        assert_eq!(result.cached_items, vec![feedback_bytes]);

        Ok(())
    }

    #[tokio::test]
    async fn test_already_retrieved_filtered() -> anyhow::Result<()> {
        let (server_id, peer_self) = make_test_server_and_peer().await?;
        let cache = PostBundleFeedbackCache::new(16 * 1024 * 1024);
        let bucket_location = make_test_bucket_location();
        let location_id = bucket_location.location_id;
        let now = TimeMillis(1_000_000);
        let originator_id = Id::random();

        cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);
        cache.on_upload(location_id, originator_id, Bytes::from_static(b"feedback"), now, false);

        let result = cache.on_get(&bucket_location, &[originator_id], &peer_self, &server_id, now);
        assert!(result.cached_items.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_upload_returns_false_when_not_in_cache() -> anyhow::Result<()> {
        let cache = PostBundleFeedbackCache::new(16 * 1024 * 1024);
        let location_id = Id::random();
        let originator_id = Id::random();

        let accepted = cache.on_upload(location_id, originator_id, Bytes::from_static(b"feedback"), TimeMillis(1_000_000), false);
        assert!(!accepted);

        Ok(())
    }
}
