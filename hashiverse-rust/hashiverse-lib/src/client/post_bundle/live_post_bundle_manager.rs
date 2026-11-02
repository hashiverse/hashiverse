//! # Production [`PostBundleManager`] — cache, fetch, heal
//!
//! The real implementation of
//! [`crate::client::post_bundle::post_bundle_manager::PostBundleManager`] used in
//! production. Lookup proceeds in three stages:
//!
//! 1. **Cache** — check `BUCKET_POST_BUNDLE` in
//!    [`crate::client::client_storage::client_storage::ClientStorage`]. Serve hot entries
//!    immediately; serve stale entries only if the bundle is already sealed.
//! 2. **Network** — otherwise walk peers closest to the bucket's location id via a
//!    [`crate::client::peer_tracker::peer_iterator::PeerIterator`] and issue
//!    `GetPostBundleV1` RPCs, gated by the
//!    [`crate::client::caching::cache_radius_tracker`] to avoid re-hammering
//!    already-cached peers. Concurrent lookups for the same `(location, time)` are
//!    de-duplicated via a per-key `Mutex` so only one RPC actually goes out.
//! 3. **Heal** — if multiple peers return divergent bundles, spawn
//!    [`crate::client::post_bundle::post_bundle_healing`] in the background to reconcile
//!    them.

use crate::anyhow_assert_eq;
use crate::client::caching::cache_radius_tracker::CacheRadiusTracker;
use crate::client::caching::post_bundle_cache_uploader;
use crate::client::client_storage::client_storage::{ClientStorage, BUCKET_POST_BUNDLE};
use crate::client::peer_tracker::peer_tracker::PeerTracker;
use crate::client::post_bundle::post_bundle_healing;
use crate::client::post_bundle::post_bundle_manager::PostBundleManager;
use crate::protocol::payload::payload::{CacheRequestTokenV1, GetPostBundleResponseV1, GetPostBundleV1, PayloadRequestKind, PayloadResponseKind};
use crate::protocol::peer::Peer;
use crate::protocol::posting::encoded_post_bundle::EncodedPostBundleV1;
use crate::protocol::rpc;
use crate::tools::buckets::BucketLocation;
use crate::tools::config::CLIENT_POST_BUNDLE_CACHE_DURATION;
use crate::tools::runtime_services::RuntimeServices;
use crate::tools::time::TimeMillis;
use crate::tools::tools::LeadingAgreementBits;
use crate::tools::types::Id;
use crate::tools::{config, json};
use bytes::Bytes;
use log::{info, trace, warn};
use scopeguard::defer;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// The production [`PostBundleManager`] implementation — the one that actually talks to the
/// network.
///
/// `LivePostBundleManager` is the glue between the pure-logic timeline code and the live
/// network. Each `get_post_bundle` call:
///
/// 1. Checks [`ClientStorage`] under `BUCKET_POST_BUNDLE` for a sufficiently fresh cached
///    copy (freshness is governed by `CLIENT_POST_BUNDLE_CACHE_DURATION`).
/// 2. Falls back to a `GetPostBundleV1` RPC against the closest peers in the
///    [`PeerTracker`], attributing PoW to the locally-held `sponsor_id`.
/// 3. Initiates post-bundle healing when the bundle has gaps — see
///    [`crate::client::post_bundle::post_bundle_healing`].
///
/// Concurrent requests for the same bundle are de-duplicated via `post_bundle_inflight` so
/// a burst of timeline scrolls cannot fan out into redundant RPCs. The
/// [`crate::client::caching::cache_radius_tracker::CacheRadiusTracker`] helps the caching
/// role of a server decide how widely to mirror bundles.
pub struct LivePostBundleManager {
    runtime_services: Arc<RuntimeServices>,
    client_storage: Arc<dyn ClientStorage>,
    peer_tracker: Arc<RwLock<PeerTracker>>,
    sponsor_id: Id,
    post_bundle_inflight: Mutex<HashMap<Id, Arc<Mutex<()>>>>,
    post_bundle_cache_radius_tracker: CacheRadiusTracker,
}

impl LivePostBundleManager {
    pub fn new(
        runtime_services: Arc<RuntimeServices>,
        sponsor_id: Id,
        client_storage: Arc<dyn ClientStorage>,
        peer_tracker: Arc<RwLock<PeerTracker>>,
    ) -> Self {
        Self {
            runtime_services,
            client_storage,
            peer_tracker,
            sponsor_id,
            post_bundle_inflight: Mutex::new(HashMap::new()),
            post_bundle_cache_radius_tracker: CacheRadiusTracker::new(CLIENT_POST_BUNDLE_CACHE_DURATION.const_mul(5)), // We let these cache radius caches last longer than our own local cache of the bundles...
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl PostBundleManager
    for LivePostBundleManager
{
    async fn get_post_bundle(&self, bucket_location: &BucketLocation, time_millis: TimeMillis) -> anyhow::Result<EncodedPostBundleV1> {
        let get_from_cache = async |location_id: Id, time_millis: TimeMillis, reason: &str| -> Option<EncodedPostBundleV1> {
            let result: anyhow::Result<Option<EncodedPostBundleV1>> = try {
                let raw = self.client_storage.get(BUCKET_POST_BUNDLE, &location_id.to_hex_str(), time_millis).await?;
                if let Some(raw) = raw {
                    // NB: deliberately not re-verified.  We only ever cache bundles we verified on receipt, but their overflowed/sealed flags
                    // were then merged/normalised locally (see AcceptedPostBundles::select_best) so the header signature may not cover them.
                    let bundle = EncodedPostBundleV1::from_bytes(Bytes::from(raw), true)?;
                    if bundle.header.sealed {
                        trace!("Using cached sealed PostBundle for {} at {}", location_id, reason);
                        Some(bundle)
                    }
                    else {
                        let duration = time_millis - bundle.header.time_millis;
                        if duration < CLIENT_POST_BUNDLE_CACHE_DURATION {
                            trace!("Using cached PostBundle for {} (age {}) at {}", location_id, duration, reason);
                            Some(bundle)
                        }
                        else {
                            trace!("Cached PostBundle for {} expired (age {}) at {}", location_id, duration, reason);
                            None
                        }
                    }
                }
                else {
                    None
                }
            };

            result.unwrap_or_else(|e| {
                warn!("discarding problematic cached PostBundle: {}", e);
                None
            })
        };

        // Check the cache
        if let Some(cached) = get_from_cache(bucket_location.location_id, time_millis, "preflight").await {
            return Ok(cached);
        }

        // Inflight deduplication: get or create a per-key mutex
        let key_lock = {
            let mut inflight = self.post_bundle_inflight.lock().await;
            inflight.entry(bucket_location.location_id).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
        };
        let _inflight_guard = key_lock.lock().await;
        defer!(if let Ok(mut m) = self.post_bundle_inflight.try_lock() {
            m.remove(&bucket_location.location_id);
        });

        // Re-check the cache - a concurrent caller may have just finished fetching and stored the result.
        if let Some(cached) = get_from_cache(bucket_location.location_id, time_millis, "postflight").await {
            return Ok(cached);
        }

        let cache_radius = self.post_bundle_cache_radius_tracker.get(bucket_location.location_id, time_millis);

        // We need to refresh...
        let mut peers_visited = Vec::new();
        let mut accepted_post_bundles = AcceptedPostBundles::new(bucket_location.clone(), time_millis);
        let mut cache_request_tokens: Vec<CacheRequestTokenV1> = Vec::new();
        let mut positive_responder_leading_agreement_bits: Vec<LeadingAgreementBits> = Vec::new();
        {
            let mut peer_tracker = self.peer_tracker.write().await;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, 2 * config::REDUNDANT_SERVERS_PER_POST, cache_radius).await?;
            while let Some((peer, leading_agreement_bits)) = peer_iter.next_peer() {
                if accepted_post_bundles.accepted_originator_ids().contains(&peer.id) {
                    continue;
                }

                let result: anyhow::Result<()> = try {
                    info!("Requesting PostBundle bucket_location={} with leading_agreement_bits={} from peer {}", bucket_location, leading_agreement_bits, peer);

                    let request = GetPostBundleV1 {
                        bucket_location: bucket_location.clone(),
                        peers_visited: peers_visited.clone(),
                        already_retrieved_peer_ids: accepted_post_bundles.accepted_originator_ids().to_vec(),
                    };
                    let request = json::struct_to_bytes(&request)?;
                    let response = rpc::rpc::rpc_server_known(&self.runtime_services, &self.sponsor_id, &peer, PayloadRequestKind::GetPostBundleV1, request).await?;
                    anyhow_assert_eq!(&PayloadResponseKind::GetPostBundleResponseV1, &response.response_request_kind);
                    let response = GetPostBundleResponseV1::from_bytes(response.bytes)?;

                    peers_visited.push(peer.clone());
                    peer_iter.add_peers(response.peers_nearer);

                    if let Some(token) = response.cache_request_token {
                        cache_request_tokens.push(token);
                    }

                    let mut found_bundle_from_this_peer = false;

                    for cached_bytes in response.post_bundles_cached {
                        match accepted_post_bundles.offer_cached(cached_bytes) {
                            Ok(accepted) => found_bundle_from_this_peer |= accepted,
                            Err(e) => warn!("Rejecting cached PostBundle from peer {}: {}", peer, e),
                        }
                    }

                    // A peer that hands us an unacceptable bundle of its own is misbehaving, so this one propagates and drops the peer
                    if let Some(post_bundle_raw) = response.post_bundle {
                        found_bundle_from_this_peer |= accepted_post_bundles.offer_direct(post_bundle_raw, &peer)?;
                    }

                    if found_bundle_from_this_peer {
                        positive_responder_leading_agreement_bits.push(leading_agreement_bits);
                    }

                    if accepted_post_bundles.quorum_count() >= config::REDUNDANT_SERVERS_PER_POST {
                        break;
                    }
                };

                if let Err(e) = result {
                    warn!("Error retrieving PostBundle from peer {}: {}", peer, e);
                    peer_iter.remove_peer(&peer);
                }
            }

            let result = peer_tracker.flush().await;
            if let Err(e) = result {
                warn!("Error flushing peer tracker: {}", e);
            }
        }

        info!("Accepted post bundles from {} originators after visiting {} peers", accepted_post_bundles.quorum_count(), peers_visited.len());

        // Store our new cache radius - the peer "furthest out" from our bundle's location_id
        if let Some(new_radius) = positive_responder_leading_agreement_bits.iter().copied().min() {
            self.post_bundle_cache_radius_tracker.update(bucket_location.location_id, new_radius, time_millis);
        }

        let best_encoded_post_bundle = accepted_post_bundles.select_best().ok_or_else(|| anyhow::anyhow!("No post bundles discovered for {}", bucket_location.location_id))?;

        // Cache it.  NB: its flags have been merged/normalised locally, so its header signature may no longer cover them - fine for our own cache, never forward it.
        trace!("Caching PostBundle for {} with {} posts", bucket_location.location_id, best_encoded_post_bundle.header.num_posts);
        let encoded_post_bundle_bytes = best_encoded_post_bundle.to_bytes()?;
        self.client_storage.put(BUCKET_POST_BUNDLE, &bucket_location.location_id.to_hex_str(), encoded_post_bundle_bytes.to_vec(), time_millis).await?;

        // Kick off healing
        post_bundle_healing::heal_post_bundles(self.runtime_services.clone(), self.sponsor_id, bucket_location.clone(), &accepted_post_bundles.donor_bundles(), accepted_post_bundles.direct_bundles());

        // Kick off remote cache populating - with the original signed bytes of authoritative bundles only
        post_bundle_cache_uploader::upload_post_bundle_caches(self.runtime_services.clone(), self.sponsor_id, cache_request_tokens, accepted_post_bundles.bundle_bytes_for_upload());

        Ok(best_encoded_post_bundle)
    }
}

/// The post bundles accepted so far while walking towards a bucket's location.
///
/// - An **authoritative** bundle passes [`EncodedPostBundleV1::verify_for_bucket`] and holds at least one post.  Only a genuinely signed,
///   admissible post proves anything - any server can sign an empty bundle.  Authoritative bundles may come directly from the peer we asked,
///   or be cached snapshots it relays (possibly signed by servers that have since died).
/// - A **direct-empty** bundle passes the same checks but holds no posts, and came directly from the peer we asked.  It counts towards
///   deciding we have asked enough servers (empty buckets are the common case), but carries no content.  Empty cached snapshots are ignored.
///
/// Each originating server counts at most once.
struct AcceptedPostBundles {
    bucket_location: BucketLocation,
    time_millis: TimeMillis,
    accepted_originator_ids: Vec<Id>,
    authoritative_bundles: Vec<(Bytes, EncodedPostBundleV1)>,
    direct_bundles: Vec<(Peer, EncodedPostBundleV1)>,
}

impl AcceptedPostBundles {
    fn new(bucket_location: BucketLocation, time_millis: TimeMillis) -> Self {
        Self { bucket_location, time_millis, accepted_originator_ids: Vec::new(), authoritative_bundles: Vec::new(), direct_bundles: Vec::new() }
    }

    /// Originators whose bundles we have accepted - passed to servers so they don't send us those again.
    fn accepted_originator_ids(&self) -> &[Id] {
        &self.accepted_originator_ids
    }

    /// How many distinct servers' views we have accepted, authoritative or direct-empty.
    fn quorum_count(&self) -> usize {
        self.accepted_originator_ids.len()
    }

    /// A cached snapshot relayed by some peer.  Returns whether it was accepted; errors if it was unacceptable.
    fn offer_cached(&mut self, encoded_post_bundle_bytes: Bytes) -> anyhow::Result<bool> {
        let encoded_post_bundle = EncodedPostBundleV1::from_bytes(encoded_post_bundle_bytes.clone(), true)?;
        if !self.verify_new_originator(&encoded_post_bundle)? {
            return Ok(false);
        }
        if encoded_post_bundle.header.num_posts == 0 {
            trace!("Ignoring empty cached PostBundle from peer.id={}", encoded_post_bundle.header.peer.id);
            return Ok(false);
        }

        trace!("Accepted cached PostBundle from peer.id={} with {} posts", encoded_post_bundle.header.peer.id, encoded_post_bundle.header.num_posts);
        self.accepted_originator_ids.push(encoded_post_bundle.header.peer.id);
        self.authoritative_bundles.push((encoded_post_bundle_bytes, encoded_post_bundle));
        Ok(true)
    }

    /// The bundle `responding_peer` holds itself.  Returns whether it was accepted; errors if it was unacceptable.
    fn offer_direct(&mut self, encoded_post_bundle_bytes: Bytes, responding_peer: &Peer) -> anyhow::Result<bool> {
        let encoded_post_bundle = EncodedPostBundleV1::from_bytes(encoded_post_bundle_bytes.clone(), true)?;
        if encoded_post_bundle.header.peer.id != responding_peer.id {
            anyhow::bail!("PostBundle from peer {} claims to be published by {}", responding_peer.id, encoded_post_bundle.header.peer.id);
        }
        if !self.verify_new_originator(&encoded_post_bundle)? {
            return Ok(false);
        }

        trace!("Accepted PostBundle from peer.id={} with {} posts", encoded_post_bundle.header.peer.id, encoded_post_bundle.header.num_posts);
        self.accepted_originator_ids.push(encoded_post_bundle.header.peer.id);
        if encoded_post_bundle.header.num_posts > 0 {
            self.authoritative_bundles.push((encoded_post_bundle_bytes, encoded_post_bundle.clone()));
        }
        self.direct_bundles.push((responding_peer.clone(), encoded_post_bundle));
        Ok(true)
    }

    /// Ok(false) if we have already accepted a bundle from this originator; Err if the bundle is not legitimate for our bucket.
    fn verify_new_originator(&self, encoded_post_bundle: &EncodedPostBundleV1) -> anyhow::Result<bool> {
        if self.accepted_originator_ids.contains(&encoded_post_bundle.header.peer.id) {
            return Ok(false);
        }
        encoded_post_bundle.verify_for_bucket(&self.bucket_location, self.time_millis)?;
        Ok(true)
    }

    /// The bundle to hand to the caller: the authoritative bundle with the most posts (falling back to a direct-empty one).
    /// Healing is relied upon to eventually give every responsible server the union of posts.
    ///
    /// Its `overflowed` flag is the OR across every accepted bundle: a bundle can win on post count while claiming not to have overflowed,
    /// which would hide the child buckets, whereas a false "overflowed" only costs some extra fetches.  `sealed` is then recomputed locally.
    fn select_best(&self) -> Option<EncodedPostBundleV1> {
        let authoritative = self.authoritative_bundles.iter().map(|(_, encoded_post_bundle)| encoded_post_bundle);
        let direct = self.direct_bundles.iter().map(|(_, encoded_post_bundle)| encoded_post_bundle);

        let mut best_encoded_post_bundle = authoritative.clone().max_by_key(|encoded_post_bundle| encoded_post_bundle.header.num_posts).or_else(|| direct.clone().next())?.clone();
        best_encoded_post_bundle.header.overflowed = authoritative
            .chain(direct)
            .any(|encoded_post_bundle| encoded_post_bundle.header.overflowed || encoded_post_bundle.header.num_posts > config::ENCODED_POST_BUNDLE_V1_OVERFLOWED_NUM_POSTS);
        best_encoded_post_bundle.normalise_flags_for_bucket(&self.bucket_location, self.time_millis);
        Some(best_encoded_post_bundle)
    }

    fn donor_bundles(&self) -> Vec<EncodedPostBundleV1> {
        self.authoritative_bundles.iter().map(|(_, encoded_post_bundle)| encoded_post_bundle.clone()).collect()
    }

    fn direct_bundles(&self) -> &[(Peer, EncodedPostBundleV1)] {
        &self.direct_bundles
    }

    fn bundle_bytes_for_upload(&self) -> Vec<(Id, Bytes)> {
        self.authoritative_bundles.iter().map(|(encoded_post_bundle_bytes, encoded_post_bundle)| (encoded_post_bundle.header.peer.id, encoded_post_bundle_bytes.clone())).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::posting::encoded_post_bundle::test_fixtures::BucketBundleFixture;
    use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
    use crate::tools::server_id::ServerId;
    use crate::tools::time_provider::time_provider::RealTimeProvider;
    use crate::tools::types::Pow;

    fn new_accepted_post_bundles(fixture: &BucketBundleFixture) -> AcceptedPostBundles {
        AcceptedPostBundles::new(fixture.bucket_location.clone(), fixture.time_millis_now)
    }

    #[tokio::test]
    async fn forged_or_empty_cached_bundles_do_not_count() -> anyhow::Result<()> {
        let fixture = BucketBundleFixture::new().await?;
        let posts = fixture.make_own_posts(1).await?;
        let mut accepted_post_bundles = new_accepted_post_bundles(&fixture);

        // Three empty bundles from genuine servers, relayed as cached snapshots: proves nothing
        for _ in 0..config::REDUNDANT_SERVERS_PER_POST {
            let server_id = BucketBundleFixture::new_server_id().await?;
            assert!(!accepted_post_bundles.offer_cached(fixture.make_bundle_from(&server_id, &[])?.to_bytes()?)?);
        }

        // A bundle of genuine posts from a keypair with no server PoW behind it
        let unworked_server_id = loop {
            let server_id = ServerId::new("unworked", &RealTimeProvider, Pow(0), true, &SingleThreadedPowGenerator::new()).await?;
            if server_id.pow < config::SERVER_KEY_POW_MIN {
                break server_id;
            }
        };
        assert!(accepted_post_bundles.offer_cached(fixture.make_bundle_from(&unworked_server_id, &posts)?.to_bytes()?).is_err());

        assert_eq!(accepted_post_bundles.quorum_count(), 0);
        assert!(accepted_post_bundles.accepted_originator_ids().is_empty());
        assert!(accepted_post_bundles.select_best().is_none());
        assert!(accepted_post_bundles.bundle_bytes_for_upload().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn direct_bundle_naming_another_peer_is_rejected() -> anyhow::Result<()> {
        let fixture = BucketBundleFixture::new().await?;
        let posts = fixture.make_own_posts(1).await?;
        let mut accepted_post_bundles = new_accepted_post_bundles(&fixture);

        let responding_server_id = BucketBundleFixture::new_server_id().await?;
        let other_server_id = BucketBundleFixture::new_server_id().await?;
        let other_bundle = fixture.make_bundle_from(&other_server_id, &posts)?;
        assert!(accepted_post_bundles.offer_direct(other_bundle.to_bytes()?, &responding_server_id.to_peer(&RealTimeProvider)?).is_err());
        assert_eq!(accepted_post_bundles.quorum_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn empty_bucket_reaches_quorum_via_direct_empties() -> anyhow::Result<()> {
        let fixture = BucketBundleFixture::new().await?;
        let mut accepted_post_bundles = new_accepted_post_bundles(&fixture);

        for _ in 0..config::REDUNDANT_SERVERS_PER_POST {
            let server_id = BucketBundleFixture::new_server_id().await?;
            assert!(accepted_post_bundles.offer_direct(fixture.make_bundle_from(&server_id, &[])?.to_bytes()?, &server_id.to_peer(&RealTimeProvider)?)?);
        }

        assert_eq!(accepted_post_bundles.quorum_count(), config::REDUNDANT_SERVERS_PER_POST);
        let best_encoded_post_bundle = accepted_post_bundles.select_best().expect("an empty bundle");
        assert_eq!(best_encoded_post_bundle.header.num_posts, 0);
        assert!(accepted_post_bundles.donor_bundles().is_empty(), "empty bundles have nothing to donate");
        assert!(accepted_post_bundles.bundle_bytes_for_upload().is_empty(), "empty bundles are not worth uploading to caches");
        assert_eq!(accepted_post_bundles.direct_bundles().len(), config::REDUNDANT_SERVERS_PER_POST, "but they are heal targets");
        Ok(())
    }

    #[tokio::test]
    async fn each_originator_counts_once() -> anyhow::Result<()> {
        let fixture = BucketBundleFixture::new().await?;
        let posts = fixture.make_own_posts(1).await?;
        let mut accepted_post_bundles = new_accepted_post_bundles(&fixture);

        let server_id = BucketBundleFixture::new_server_id().await?;
        let encoded_post_bundle_bytes = fixture.make_bundle_from(&server_id, &posts)?.to_bytes()?;
        assert!(accepted_post_bundles.offer_cached(encoded_post_bundle_bytes.clone())?);
        assert!(!accepted_post_bundles.offer_cached(encoded_post_bundle_bytes.clone())?);
        assert!(!accepted_post_bundles.offer_direct(encoded_post_bundle_bytes, &server_id.to_peer(&RealTimeProvider)?)?);
        assert_eq!(accepted_post_bundles.quorum_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn selects_largest_bundle_with_overflowed_or_merged() -> anyhow::Result<()> {
        let fixture = BucketBundleFixture::new().await?;
        let posts = fixture.make_own_posts(5).await?;
        let mut accepted_post_bundles = new_accepted_post_bundles(&fixture);

        // A: 3 posts, says overflowed (e.g. it granted many posts that have not all arrived yet)
        let server_id_a = BucketBundleFixture::new_server_id().await?;
        let mut encoded_post_bundle_a = fixture.make_bundle_from(&server_id_a, &posts[..3])?;
        encoded_post_bundle_a.header.overflowed = true;
        encoded_post_bundle_a.header.sealed = true;
        encoded_post_bundle_a.header.signature_generate(&server_id_a.keys.signature_key)?;
        let encoded_post_bundle_bytes_a = encoded_post_bundle_a.to_bytes()?;

        // B: 5 posts, says not overflowed
        let server_id_b = BucketBundleFixture::new_server_id().await?;
        let encoded_post_bundle_b = fixture.make_bundle_from(&server_id_b, &posts)?;
        let encoded_post_bundle_bytes_b = encoded_post_bundle_b.to_bytes()?;

        assert!(accepted_post_bundles.offer_direct(encoded_post_bundle_bytes_a.clone(), &server_id_a.to_peer(&RealTimeProvider)?)?);
        assert!(accepted_post_bundles.offer_cached(encoded_post_bundle_bytes_b.clone())?);

        let best_encoded_post_bundle = accepted_post_bundles.select_best().expect("a bundle");
        assert_eq!(best_encoded_post_bundle.header.encoded_post_ids, encoded_post_bundle_b.header.encoded_post_ids, "posts come from the largest bundle");
        assert!(best_encoded_post_bundle.header.overflowed, "overflowed is OR-ed across all accepted bundles");
        assert!(best_encoded_post_bundle.header.sealed, "an overflowed bucket is sealed");

        // The merged flag survives the round trip through our local cache format
        let cached_encoded_post_bundle = EncodedPostBundleV1::from_bytes(best_encoded_post_bundle.to_bytes()?, true)?;
        assert!(cached_encoded_post_bundle.header.overflowed);

        // Nothing we pass on to the network carries the mutated header
        let mut bundle_bytes_for_upload = accepted_post_bundles.bundle_bytes_for_upload();
        bundle_bytes_for_upload.sort_by_key(|(originator_id, _)| *originator_id);
        let mut expected_bundle_bytes_for_upload = vec![(server_id_a.id, encoded_post_bundle_bytes_a), (server_id_b.id, encoded_post_bundle_bytes_b)];
        expected_bundle_bytes_for_upload.sort_by_key(|(originator_id, _)| *originator_id);
        assert_eq!(bundle_bytes_for_upload, expected_bundle_bytes_for_upload);
        assert!(accepted_post_bundles.donor_bundles().iter().any(|donor_bundle| *donor_bundle == encoded_post_bundle_b));
        Ok(())
    }

    #[tokio::test]
    async fn premature_sealed_claim_is_dropped() -> anyhow::Result<()> {
        let fixture = BucketBundleFixture::new().await?;
        let posts = fixture.make_own_posts(1).await?;
        let mut accepted_post_bundles = new_accepted_post_bundles(&fixture);

        let server_id = BucketBundleFixture::new_server_id().await?;
        let mut encoded_post_bundle = fixture.make_bundle_from(&server_id, &posts)?;
        encoded_post_bundle.header.sealed = true; // Would pin this bundle in our cache forever
        encoded_post_bundle.header.signature_generate(&server_id.keys.signature_key)?;
        assert!(accepted_post_bundles.offer_cached(encoded_post_bundle.to_bytes()?)?);

        assert!(!accepted_post_bundles.select_best().expect("a bundle").header.sealed);
        Ok(())
    }
}
