//! # Post-bundle read cache
//!
//! A `moka` weighted cache that holds recently-served post bundles in RAM so repeated
//! reads for the same `(location_id, originator)` don't thrash disk. Multiple versions
//! per `location_id` are kept because different peer originators hold different slices
//! of the same bucket — the cache index is keyed by `(location_id, originator_id)`.
//!
//! The cache doubles as a **propagation hint engine**: once a hot entry has been
//! requested more than [`crate::server::post_bundle_caching_shared::CACHE_HIT_THRESHOLD`]
//! times (10), the next requester's response includes a signed
//! [`hashiverse_lib::protocol::payload::payload::CacheRequestTokenV1`] asking them to
//! forward the bundle to additional servers. This turns read pressure into better
//! cache distribution without requiring explicit coordination.
//!
//! An `inflight` sibling cache tracks outstanding tokens so the same hot bundle doesn't
//! generate redundant cache-request tokens in parallel.

use bytes::Bytes;
use hashiverse_lib::protocol::payload::payload::CacheRequestTokenV1;
use hashiverse_lib::protocol::peer::Peer;
use hashiverse_lib::tools::buckets::BucketLocation;
use hashiverse_lib::tools::server_id::ServerId;
use hashiverse_lib::tools::time::{TimeMillis, MILLIS_IN_MINUTE};
use hashiverse_lib::tools::time_provider::moka_clock::TimeProviderMokaClock;
use hashiverse_lib::tools::time_provider::time_provider::TimeProvider;
use hashiverse_lib::tools::tools::leading_agreement_bits_xor;
use hashiverse_lib::tools::types::Id;
use moka::sync::Cache;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::server::post_bundle_caching_shared::{CachedBundle, GetCacheResult, CACHE_HIT_THRESHOLD, CACHE_LOCATION_TTI, CACHE_REQUEST_TOKEN_TTL_DURATION, CACHE_REQUEST_TOKEN_TTL_DURATION_MILLIS};

/// Placeholder weight for post bundle entries — approximates the real data that will replace it.
/// Ballpark: 20 posts × 50 KB each × up to 5 originators ≈ 5 MB per location.
const POST_BUNDLE_PLACEHOLDER_WEIGHT: u32 = 4 * 1024 * 1024;

// --------------------------------------------------------------------------------------------
// CachedPostBundleLocationEntry
// --------------------------------------------------------------------------------------------

/// Per-location entry for post bundles.
/// Multiple originator versions are stored — different servers may hold different subsets of posts.
struct CachedPostBundleLocationEntry {
    /// Originator peer_id → cached bundle.
    bundles: HashMap<Id, CachedBundle>,
    hit_count: u32,
}

impl CachedPostBundleLocationEntry {
    fn placeholder() -> Self {
        Self { bundles: HashMap::new(), hit_count: 0 }
    }

    fn weight(&self) -> u32 {
        let total: u32 = self.bundles.values().map(|b| b.bytes.len() as u32).sum();
        if total == 0 { POST_BUNDLE_PLACEHOLDER_WEIGHT } else { total }
    }
}

// --------------------------------------------------------------------------------------------
// PostBundleCache
// --------------------------------------------------------------------------------------------

/// Intermediate-server cache for `EncodedPostBundleV1` data.
///
/// Two Moka caches:
/// - `bundles`: weighted `Cache<Id, Arc<Mutex<CachedPostBundleLocationEntry>>>` with TTI.
///   If a location_id hasn't been queried within `CACHE_LOCATION_TTI`, the entire entry is evicted.
///   Individual bundles within an entry may also be stale (live bundles have a per-bundle
///   `expires_at`; sealed bundles are never individually stale).
/// - `inflight`: `Cache<Id, ()>` with 30-second TTL — tracks locations for which a
///   `CacheRequestToken` has been issued but the client hasn't uploaded yet.
pub struct PostBundleCache {
    max_originators_per_location: usize,
    bundles: Cache<Id, Arc<Mutex<CachedPostBundleLocationEntry>>>,
    inflight: Cache<Id, ()>,
}

impl PostBundleCache {
    pub fn new(max_originators_per_location: usize, max_bytes: u64, time_provider: Arc<dyn TimeProvider>) -> Self {
        // Drive moka's TTI/TTL from our TimeProvider (scaled in tests) rather than wall time.
        let clock = Arc::new(TimeProviderMokaClock::new(time_provider));

        let bundles = Cache::builder()
            .weigher(|_key: &Id, entry: &Arc<Mutex<CachedPostBundleLocationEntry>>| {
                entry.lock().map(|e| e.weight()).unwrap_or(POST_BUNDLE_PLACEHOLDER_WEIGHT)
            })
            .max_capacity(max_bytes)
            .time_to_idle(CACHE_LOCATION_TTI)
            .external_clock(clock.clone())
            .build();

        let inflight = Cache::builder()
            .time_to_live(CACHE_REQUEST_TOKEN_TTL_DURATION)
            .external_clock(clock)
            .build();

        Self { max_originators_per_location, bundles, inflight }
    }

    /// Called by the dispatch handler when serving a `GetPostBundleV1` request.
    ///
    /// - `bucket_location` — used as the cache key (`location_id`) and included in any token issued.
    /// - `already_retrieved_peer_ids` — originator IDs the client already has; filtered out of `cached_items`.
    /// - `peer_self` / `server_id` — used to sign the `CacheRequestToken` if one is issued.
    /// - `now` — current time.
    pub fn on_get(
        &self,
        bucket_location: &BucketLocation,
        already_retrieved_peer_ids: &[Id],
        peer_self: &Peer,
        server_id: &ServerId,
        now: TimeMillis,
    ) -> GetCacheResult {
        let location_id = bucket_location.location_id;
        let entry_arc = self.bundles.get_with(location_id, || Arc::new(Mutex::new(CachedPostBundleLocationEntry::placeholder())));

        let (cached_items, already_cached_peer_ids, should_issue_token) = {
            let mut entry = entry_arc.lock().unwrap();
            entry.hit_count += 1;

            let already_retrieved_set: std::collections::HashSet<Id> = already_retrieved_peer_ids.iter().copied().collect();
            let cached_items: Vec<Bytes> = entry.bundles
                .iter()
                .filter(|(originator_id, bundle)| !already_retrieved_set.contains(originator_id) && !bundle.is_stale(now))
                .map(|(_, bundle)| bundle.bytes.clone())
                .collect();

            let already_cached_peer_ids: Vec<Id> = entry.bundles.keys().copied().collect();
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

    /// Called by the dispatch handler when a `CachePostBundleV1` upload arrives.
    /// The token must have been verified and expiry-checked by the caller.
    /// Returns `true` if accepted, `false` if the entry was evicted before the upload arrived.
    pub fn on_upload(
        &self,
        location_id: Id,
        originator_peer_id: Id,
        bundle_bytes: Bytes,
        server_time: TimeMillis,
        is_sealed: bool,
    ) -> bool {
        let entry_arc = match self.bundles.get(&location_id) {
            Some(e) => e,
            None => return false,   // Entry evicted between token issuance and upload — reject.
        };

        let mut entry = entry_arc.lock().unwrap();
        let expires_at = if is_sealed { None } else { Some(server_time + MILLIS_IN_MINUTE.const_mul(5)) };
        let bundle = CachedBundle { bytes: bundle_bytes, expires_at };

        // Insert (or update) the new originator
        entry.bundles.insert(originator_peer_id, bundle);

        // If over capacity, evict the worst entry: furthest from location_id,
        // breaking ties by expires_at (stalest loses). This may evict the entry
        // we just inserted if it's the worst — that's correct.
        // This will also prevent sybils trying to insert garbage cache items - they would have to control the location_id
        while entry.bundles.len() > self.max_originators_per_location {
            let evict_key = entry.bundles
                .iter()
                .min_by(|(id_a, bundle_a), (id_b, bundle_b)| {
                    let distance_a = leading_agreement_bits_xor(id_a.as_ref(), location_id.as_ref());
                    let distance_b = leading_agreement_bits_xor(id_b.as_ref(), location_id.as_ref());
                    distance_a.cmp(&distance_b).then_with(|| {
                        let expires_a = bundle_a.expires_at.unwrap_or(TimeMillis(i64::MAX));
                        let expires_b = bundle_b.expires_at.unwrap_or(TimeMillis(i64::MAX));
                        expires_a.cmp(&expires_b)
                    })
                })
                .map(|(id, _)| *id);
            if let Some(k) = evict_key {
                entry.bundles.remove(&k);
            }
        }

        // Return whether our insertion survived eviction
        entry.bundles.contains_key(&originator_peer_id)
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
    use hashiverse_lib::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
    use hashiverse_lib::tools::types::{Id, Pow};

    async fn make_test_server_and_peer() -> anyhow::Result<(ServerId, hashiverse_lib::protocol::peer::Peer)> {
        let time_provider = RealTimeProvider;
        let pow_generator = SingleThreadedPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(0), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;
        Ok((server_id, peer))
    }

    fn make_test_bucket_location() -> BucketLocation {
        BucketLocation::new(BucketType::User, Id::random(), BUCKET_DURATIONS[0], TimeMillis(1_000_000)).unwrap()
    }

    #[tokio::test]
    async fn test_below_threshold_no_token() -> anyhow::Result<()> {
        let (server_id, peer_self) = make_test_server_and_peer().await?;
        let cache = PostBundleCache::new(5, 64 * 1024 * 1024, Arc::new(RealTimeProvider));
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
        let cache = PostBundleCache::new(5, 64 * 1024 * 1024, Arc::new(RealTimeProvider));
        let bucket_location = make_test_bucket_location();
        let now = TimeMillis(1_000_000);

        for _ in 0..(CACHE_HIT_THRESHOLD - 1) {
            let result = cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);
            assert!(result.cache_request_token.is_none());
        }

        // The threshold-th call issues a token
        let result = cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);
        assert!(result.cache_request_token.is_some());

        // Subsequent calls must NOT double-issue (inflight dedupe)
        let result2 = cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);
        assert!(result2.cache_request_token.is_none());

        Ok(())
    }

    #[tokio::test]
    async fn test_upload_and_retrieval() -> anyhow::Result<()> {
        let (server_id, peer_self) = make_test_server_and_peer().await?;
        let cache = PostBundleCache::new(5, 64 * 1024 * 1024, Arc::new(RealTimeProvider));
        let bucket_location = make_test_bucket_location();
        let location_id = bucket_location.location_id;
        let now = TimeMillis(1_000_000);
        let originator_id = Id::random();
        let bundle_bytes = Bytes::from_static(b"test_bundle");

        // Register the placeholder entry via on_get
        cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);

        let accepted = cache.on_upload(location_id, originator_id, bundle_bytes.clone(), now, false);
        assert!(accepted);

        let result = cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);
        assert_eq!(result.cached_items, vec![bundle_bytes]);

        Ok(())
    }

    #[tokio::test]
    async fn test_already_retrieved_filtered() -> anyhow::Result<()> {
        let (server_id, peer_self) = make_test_server_and_peer().await?;
        let cache = PostBundleCache::new(5, 64 * 1024 * 1024, Arc::new(RealTimeProvider));
        let bucket_location = make_test_bucket_location();
        let location_id = bucket_location.location_id;
        let now = TimeMillis(1_000_000);
        let originator_id = Id::random();

        cache.on_get(&bucket_location, &[], &peer_self, &server_id, now);
        cache.on_upload(location_id, originator_id, Bytes::from_static(b"bundle"), now, false);

        let result = cache.on_get(&bucket_location, &[originator_id], &peer_self, &server_id, now);
        assert!(result.cached_items.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_upload_returns_false_when_not_in_cache() -> anyhow::Result<()> {
        let cache = PostBundleCache::new(5, 64 * 1024 * 1024, Arc::new(RealTimeProvider));
        let location_id = Id::random();
        let originator_id = Id::random();

        // No on_get call — entry was never inserted — upload must be rejected
        let accepted = cache.on_upload(location_id, originator_id, Bytes::from_static(b"bundle"), TimeMillis(1_000_000), false);
        assert!(!accepted);

        Ok(())
    }
}
