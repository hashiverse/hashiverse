//! # Production [`PostBundleManager`] — cache, fetch, heal
//!
//! The real implementation of
//! [`crate::client::post_bundle::post_bundle_manager::PostBundleManager`] used in
//! production. Lookup proceeds in three stages:
//!
//! 1. **Cache** — check `BUCKET_POST_BUNDLE` in
//!    [`crate::client::client_storage::client_storage::ClientStorage`]. Serve hot entries
//!    immediately; serve stale entries only if the bundle is already sealed.
//! 2. **Network** — otherwise a background walk visits peers closest to the bucket's location id via a
//!    [`crate::client::peer_tracker::peer_iterator::PeerIterator`], issuing
//!    `GetPostBundleV1` RPCs, gated by the
//!    [`crate::client::caching::cache_radius_tracker`] to avoid re-hammering
//!    already-cached peers. Concurrent lookups for the same location share one walk.
//!    [`PostBundleManager::get_post_bundle`] answers as soon as the walk accepts its first
//!    authoritative bundle (a *provisional* answer); [`PostBundleManager::get_post_bundle_settled`]
//!    waits for the walk's best-of-[`config::REDUNDANT_SERVERS_PER_POST`] answer.
//! 3. **Settle** — once the walk has its quorum it caches the best bundle locally, then spawns
//!    [`crate::client::post_bundle::post_bundle_healing`] to reconcile divergent servers and
//!    uploads the original signed bundles to the servers that asked to cache them.
//!
//! A provisional bundle is never stored or forwarded.  Its posts and `overflowed` flag reflect only the
//! bundles seen so far, so a timeline pass built on it may miss posts (or child buckets) that only later
//! responders hold; anything fetched after the bundle settles sees the full picture.

use crate::anyhow_assert_eq;
use crate::client::caching::cache_radius_tracker::CacheRadiusTracker;
use crate::client::caching::post_bundle_cache_uploader;
use crate::client::client_storage::client_storage::{ClientStorage, BUCKET_POST_BUNDLE};
use crate::client::peer_tracker::peer_tracker::{PeerTracker, SharedPeerTracker};
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
use crate::tools::tools::{spawn_background_task, LeadingAgreementBits};
use crate::tools::types::Id;
use crate::tools::{config, json};
use bytes::Bytes;
use log::{info, trace, warn};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::watch;

/// The production [`PostBundleManager`] implementation — the one that actually talks to the
/// network.
///
/// `LivePostBundleManager` is the glue between the pure-logic timeline code and the live
/// network. Each lookup:
///
/// 1. Checks [`ClientStorage`] under `BUCKET_POST_BUNDLE` for a sufficiently fresh cached
///    copy (freshness is governed by `CLIENT_POST_BUNDLE_CACHE_DURATION`).
/// 2. Otherwise joins (or starts) a background walk of `GetPostBundleV1` RPCs against the closest
///    peers in the [`PeerTracker`], attributing PoW to the locally-held `sponsor_id`.
/// 3. Returns the walk's first authoritative bundle (`get_post_bundle`) or its settled best
///    bundle (`get_post_bundle_settled`).  The walk itself carries on to cache, heal and upload —
///    see [`crate::client::post_bundle::post_bundle_healing`].
///
/// Concurrent requests for the same bundle share one walk via [`InflightPostBundleFetches`], so
/// a burst of timeline scrolls cannot fan out into redundant RPCs. The
/// [`crate::client::caching::cache_radius_tracker::CacheRadiusTracker`] helps the caching
/// role of a server decide how widely to mirror bundles.
pub struct LivePostBundleManager {
    inner: Arc<LivePostBundleManagerInner>,
}

/// Everything a walk needs, shared so that the walk can outlive the call that started it.
struct LivePostBundleManagerInner {
    runtime_services: Arc<RuntimeServices>,
    client_storage: Arc<dyn ClientStorage>,
    peer_tracker: SharedPeerTracker,
    sponsor_id: Id,
    inflight_post_bundle_fetches: InflightPostBundleFetches,
    post_bundle_cache_radius_tracker: CacheRadiusTracker,
}

impl LivePostBundleManager {
    pub fn new(
        runtime_services: Arc<RuntimeServices>,
        sponsor_id: Id,
        client_storage: Arc<dyn ClientStorage>,
        peer_tracker: SharedPeerTracker,
    ) -> Self {
        Self {
            inner: Arc::new(LivePostBundleManagerInner {
                runtime_services,
                client_storage,
                peer_tracker,
                sponsor_id,
                inflight_post_bundle_fetches: InflightPostBundleFetches::default(),
                post_bundle_cache_radius_tracker: CacheRadiusTracker::new(CLIENT_POST_BUNDLE_CACHE_DURATION.const_mul(5)), // We let these cache radius caches last longer than our own local cache of the bundles...
            }),
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl PostBundleManager
    for LivePostBundleManager
{
    async fn get_post_bundle(&self, bucket_location: &BucketLocation, time_millis: TimeMillis) -> anyhow::Result<EncodedPostBundleV1> {
        self.inner.fetch(bucket_location, time_millis, false).await
    }

    async fn get_post_bundle_settled(&self, bucket_location: &BucketLocation, time_millis: TimeMillis) -> anyhow::Result<EncodedPostBundleV1> {
        self.inner.fetch(bucket_location, time_millis, true).await
    }
}

impl LivePostBundleManagerInner {
    async fn get_from_cache(&self, location_id: Id, time_millis: TimeMillis, reason: &str) -> Option<EncodedPostBundleV1> {
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
    }

    /// Serves from the cache, else joins (or starts) the walk for this location and waits for a provisional or settled answer.
    async fn fetch(self: &Arc<Self>, bucket_location: &BucketLocation, time_millis: TimeMillis, wait_for_settled: bool) -> anyhow::Result<EncodedPostBundleV1> {
        let location_id = bucket_location.location_id;

        // Check the cache
        if let Some(cached) = self.get_from_cache(location_id, time_millis, "preflight").await {
            return Ok(cached);
        }

        let (mut fetch_state_receiver, fetch_state_sender) = self.inflight_post_bundle_fetches.join_or_start(location_id);
        if let Some(fetch_state_sender) = fetch_state_sender {
            // Re-check the cache - a walk may have settled and stored its result between our preflight check and joining
            if let Some(cached) = self.get_from_cache(location_id, time_millis, "postflight").await {
                fetch_state_sender.send_replace(PostBundleFetchState::Settled(Ok(cached)));
                self.inflight_post_bundle_fetches.finish(location_id);
            }
            else {
                spawn_background_task(self.clone().walk_and_settle(bucket_location.clone(), time_millis, fetch_state_sender));
            }
        }

        await_fetch_answer(location_id, &mut fetch_state_receiver, wait_for_settled).await
    }

    /// Runs the walk to its quorum, publishing the first authoritative bundle as soon as it is accepted, then settles:
    /// caches the best bundle, releases waiters, and kicks off healing and cache uploads.
    async fn walk_and_settle(self: Arc<Self>, bucket_location: BucketLocation, time_millis: TimeMillis, fetch_state_sender: watch::Sender<PostBundleFetchState>) {
        let location_id = bucket_location.location_id;

        match self.walk(&bucket_location, time_millis, &fetch_state_sender).await {
            Ok((best_encoded_post_bundle, accepted_post_bundles, cache_request_tokens)) => {
                fetch_state_sender.send_replace(PostBundleFetchState::Settled(Ok(best_encoded_post_bundle)));
                self.inflight_post_bundle_fetches.finish(location_id);

                // Kick off healing
                post_bundle_healing::heal_post_bundles(self.runtime_services.clone(), self.sponsor_id, bucket_location.clone(), &accepted_post_bundles.donor_bundles(), accepted_post_bundles.direct_bundles());

                // Kick off remote cache populating - with the original signed bytes of authoritative bundles only
                post_bundle_cache_uploader::upload_post_bundle_caches(self.runtime_services.clone(), self.sponsor_id, cache_request_tokens, accepted_post_bundles.bundle_bytes_for_upload());
            }
            Err(error) => {
                warn!("PostBundle walk for {} failed: {:#}", location_id, error);
                fetch_state_sender.send_replace(PostBundleFetchState::Settled(Err(Arc::new(error))));
                self.inflight_post_bundle_fetches.finish(location_id);
            }
        }
    }

    /// The walk proper.  Returns the best bundle (already written to `ClientStorage`) plus what healing and uploading need.
    async fn walk(&self, bucket_location: &BucketLocation, time_millis: TimeMillis, fetch_state_sender: &watch::Sender<PostBundleFetchState>) -> anyhow::Result<(EncodedPostBundleV1, AcceptedPostBundles, Vec<CacheRequestTokenV1>)> {
        let cache_radius = self.post_bundle_cache_radius_tracker.get(bucket_location.location_id, time_millis);

        // We need to refresh...
        let mut peers_visited = Vec::new();
        let mut accepted_post_bundles = AcceptedPostBundles::new(bucket_location.clone(), time_millis);
        let mut cache_request_tokens: Vec<CacheRequestTokenV1> = Vec::new();
        let mut positive_responder_leading_agreement_bits: Vec<LeadingAgreementBits> = Vec::new();
        let mut provisional_published = false;
        {
            let mut peer_iter = PeerTracker::iterate_to_location(&self.peer_tracker, bucket_location.location_id, 2 * config::REDUNDANT_SERVERS_PER_POST, cache_radius).await?;
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

                    // The first genuinely signed posts are good enough to show - waiters on get_post_bundle can go now
                    if !provisional_published && accepted_post_bundles.has_authoritative() {
                        if let Some(provisional_encoded_post_bundle) = accepted_post_bundles.select_best() {
                            trace!("Publishing provisional PostBundle for {} with {} posts", bucket_location.location_id, provisional_encoded_post_bundle.header.num_posts);
                            fetch_state_sender.send_replace(PostBundleFetchState::Provisional(provisional_encoded_post_bundle));
                            provisional_published = true;
                        }
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

            let result = PeerTracker::flush(&self.peer_tracker).await;
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

        Ok((best_encoded_post_bundle, accepted_post_bundles, cache_request_tokens))
    }
}

/// Waits until the walk has an answer for this caller - a provisional bundle is enough unless `wait_for_settled` - and returns it.
async fn await_fetch_answer(location_id: Id, fetch_state_receiver: &mut watch::Receiver<PostBundleFetchState>, wait_for_settled: bool) -> anyhow::Result<EncodedPostBundleV1> {
    let fetch_state = fetch_state_receiver
        .wait_for(|fetch_state| match fetch_state {
            PostBundleFetchState::Walking => false,
            PostBundleFetchState::Provisional(_) => !wait_for_settled,
            PostBundleFetchState::Settled(_) => true,
        })
        .await
        .map_err(|_| anyhow::anyhow!("PostBundle walk for {} ended without settling", location_id))?
        .clone();

    match fetch_state {
        PostBundleFetchState::Provisional(encoded_post_bundle) => Ok(encoded_post_bundle),
        PostBundleFetchState::Settled(Ok(encoded_post_bundle)) => Ok(encoded_post_bundle),
        PostBundleFetchState::Settled(Err(error)) => Err(anyhow::Error::new(PostBundleFetchError { location_id, error })),
        PostBundleFetchState::Walking => anyhow::bail!("PostBundle walk for {} is still walking", location_id),
    }
}

/// Where a location's walk has got to, as seen by everyone waiting on it.
#[derive(Clone)]
enum PostBundleFetchState {
    Walking,
    /// The best of the bundles accepted so far, published once the first authoritative bundle arrives.  Never stored or forwarded.
    Provisional(EncodedPostBundleV1),
    /// The best-of-quorum bundle, already written to `ClientStorage` - or why the walk failed.
    Settled(Result<EncodedPostBundleV1, Arc<anyhow::Error>>),
}

/// A walk's error, handed to every caller that waited on it (`anyhow::Error` is not `Clone`, so they share it).
/// Its `source()` is the original error: `{:#}` still prints the whole chain and `chain()`/`downcast_ref` still reach it.
#[derive(Debug)]
struct PostBundleFetchError {
    location_id: Id,
    error: Arc<anyhow::Error>,
}

impl std::fmt::Display for PostBundleFetchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "fetching PostBundle {} failed", self.location_id)
    }
}

impl std::error::Error for PostBundleFetchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        let error: &(dyn std::error::Error + Send + Sync + 'static) = self.error.as_ref().as_ref();
        Some(error)
    }
}

/// One shared walk per location: the first caller starts it, later callers watch its progress.
#[derive(Default)]
struct InflightPostBundleFetches {
    fetches: parking_lot::Mutex<HashMap<Id, watch::Receiver<PostBundleFetchState>>>,
}

impl InflightPostBundleFetches {
    /// Returns a receiver for this location's walk, plus the sender if the caller must start the walk itself.
    fn join_or_start(&self, location_id: Id) -> (watch::Receiver<PostBundleFetchState>, Option<watch::Sender<PostBundleFetchState>>) {
        let mut fetches = self.fetches.lock();
        if let Some(fetch_state_receiver) = fetches.get(&location_id) {
            // A walk whose task died without settling (its sender is gone) must not block every later caller
            if fetch_state_receiver.has_changed().is_ok() {
                return (fetch_state_receiver.clone(), None);
            }
        }
        let (fetch_state_sender, fetch_state_receiver) = watch::channel(PostBundleFetchState::Walking);
        fetches.insert(location_id, fetch_state_receiver.clone());
        (fetch_state_receiver, Some(fetch_state_sender))
    }

    /// The walk has settled (and stored its result): later callers start afresh from the cache.
    fn finish(&self, location_id: Id) {
        self.fetches.lock().remove(&location_id);
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

    /// Whether we hold at least one authoritative bundle (verified, with posts) - enough to show provisionally.
    fn has_authoritative(&self) -> bool {
        !self.authoritative_bundles.is_empty()
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

    #[tokio::test]
    async fn has_authoritative_only_once_a_bundle_with_posts_is_accepted() -> anyhow::Result<()> {
        let fixture = BucketBundleFixture::new().await?;
        let posts = fixture.make_own_posts(1).await?;
        let mut accepted_post_bundles = new_accepted_post_bundles(&fixture);
        assert!(!accepted_post_bundles.has_authoritative());

        // A direct empty counts towards the quorum but has nothing to show
        let empty_server_id = BucketBundleFixture::new_server_id().await?;
        assert!(accepted_post_bundles.offer_direct(fixture.make_bundle_from(&empty_server_id, &[])?.to_bytes()?, &empty_server_id.to_peer(&RealTimeProvider)?)?);
        assert!(!accepted_post_bundles.has_authoritative());

        // A cached empty is ignored altogether
        let cached_empty_server_id = BucketBundleFixture::new_server_id().await?;
        assert!(!accepted_post_bundles.offer_cached(fixture.make_bundle_from(&cached_empty_server_id, &[])?.to_bytes()?)?);
        assert!(!accepted_post_bundles.has_authoritative());

        let server_id = BucketBundleFixture::new_server_id().await?;
        assert!(accepted_post_bundles.offer_cached(fixture.make_bundle_from(&server_id, &posts)?.to_bytes()?)?);
        assert!(accepted_post_bundles.has_authoritative());
        Ok(())
    }

    // ── InflightPostBundleFetches / await_fetch_answer ──

    async fn some_encoded_post_bundle() -> anyhow::Result<EncodedPostBundleV1> {
        let fixture = BucketBundleFixture::new().await?;
        let posts = fixture.make_own_posts(1).await?;
        let server_id = BucketBundleFixture::new_server_id().await?;
        fixture.make_bundle_from(&server_id, &posts)
    }

    #[tokio::test]
    async fn first_caller_starts_the_walk_and_later_callers_join_it() {
        let inflight_post_bundle_fetches = InflightPostBundleFetches::default();
        let location_id = Id::random();

        let (_, starter_fetch_state_sender) = inflight_post_bundle_fetches.join_or_start(location_id);
        assert!(starter_fetch_state_sender.is_some(), "the first caller starts the walk");
        let (_, joiner_fetch_state_sender) = inflight_post_bundle_fetches.join_or_start(location_id);
        assert!(joiner_fetch_state_sender.is_none(), "a second caller joins it");

        let (_, other_location_fetch_state_sender) = inflight_post_bundle_fetches.join_or_start(Id::random());
        assert!(other_location_fetch_state_sender.is_some(), "other locations get their own walk");
    }

    #[tokio::test]
    async fn finish_lets_the_next_caller_start_afresh() {
        let inflight_post_bundle_fetches = InflightPostBundleFetches::default();
        let location_id = Id::random();

        let (_, fetch_state_sender) = inflight_post_bundle_fetches.join_or_start(location_id);
        let _fetch_state_sender = fetch_state_sender.expect("starter");
        inflight_post_bundle_fetches.finish(location_id);
        assert!(inflight_post_bundle_fetches.join_or_start(location_id).1.is_some());
    }

    #[tokio::test]
    async fn a_walk_that_died_without_settling_does_not_block_later_callers() {
        let inflight_post_bundle_fetches = InflightPostBundleFetches::default();
        let location_id = Id::random();

        let (mut fetch_state_receiver, fetch_state_sender) = inflight_post_bundle_fetches.join_or_start(location_id);
        drop(fetch_state_sender); // e.g. the walk task panicked
        assert!(await_fetch_answer(location_id, &mut fetch_state_receiver, false).await.is_err(), "its own waiters get an error, not a hang");
        assert!(inflight_post_bundle_fetches.join_or_start(location_id).1.is_some(), "the next caller starts a new walk");
    }

    #[tokio::test]
    async fn joiners_see_a_provisional_bundle_immediately() -> anyhow::Result<()> {
        let inflight_post_bundle_fetches = InflightPostBundleFetches::default();
        let location_id = Id::random();
        let provisional_encoded_post_bundle = some_encoded_post_bundle().await?;

        let (_, fetch_state_sender) = inflight_post_bundle_fetches.join_or_start(location_id);
        let fetch_state_sender = fetch_state_sender.expect("starter");
        fetch_state_sender.send_replace(PostBundleFetchState::Provisional(provisional_encoded_post_bundle.clone()));

        let (mut joiner_fetch_state_receiver, _) = inflight_post_bundle_fetches.join_or_start(location_id);
        assert_eq!(provisional_encoded_post_bundle, await_fetch_answer(location_id, &mut joiner_fetch_state_receiver, false).await?);
        Ok(())
    }

    #[tokio::test]
    async fn settled_waiters_ignore_the_provisional_bundle() -> anyhow::Result<()> {
        let location_id = Id::random();
        let provisional_encoded_post_bundle = some_encoded_post_bundle().await?;
        let settled_encoded_post_bundle = some_encoded_post_bundle().await?;
        let (fetch_state_sender, fetch_state_receiver) = watch::channel(PostBundleFetchState::Walking);

        let mut early_fetch_state_receiver = fetch_state_receiver.clone();
        let mut settled_fetch_state_receiver = fetch_state_receiver.clone();
        let early_answer = tokio::spawn(async move { await_fetch_answer(location_id, &mut early_fetch_state_receiver, false).await });
        let settled_answer = tokio::spawn(async move { await_fetch_answer(location_id, &mut settled_fetch_state_receiver, true).await });

        fetch_state_sender.send_replace(PostBundleFetchState::Provisional(provisional_encoded_post_bundle.clone()));
        assert_eq!(provisional_encoded_post_bundle, early_answer.await??);
        tokio::task::yield_now().await;
        assert!(!settled_answer.is_finished(), "a settled waiter must not wake on the provisional bundle");

        fetch_state_sender.send_replace(PostBundleFetchState::Settled(Ok(settled_encoded_post_bundle.clone())));
        assert_eq!(settled_encoded_post_bundle, settled_answer.await??);
        Ok(())
    }

    #[derive(Debug)]
    struct WalkRootCause;
    impl std::fmt::Display for WalkRootCause {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "the root cause")
        }
    }
    impl std::error::Error for WalkRootCause {}

    #[tokio::test]
    async fn walk_errors_reach_every_waiter_with_their_chain_intact() -> anyhow::Result<()> {
        let location_id = Id::random();
        let (fetch_state_sender, fetch_state_receiver) = watch::channel(PostBundleFetchState::Walking);
        let walk_error = anyhow::Error::new(WalkRootCause).context("walking towards the bucket");
        fetch_state_sender.send_replace(PostBundleFetchState::Settled(Err(Arc::new(walk_error))));

        for wait_for_settled in [false, true] {
            let mut waiter_fetch_state_receiver = fetch_state_receiver.clone();
            let error = await_fetch_answer(location_id, &mut waiter_fetch_state_receiver, wait_for_settled).await.expect_err("the walk failed");

            let formatted_error = format!("{:#}", error);
            assert!(formatted_error.contains(&location_id.to_string()), "names the location: {}", formatted_error);
            assert!(formatted_error.contains("walking towards the bucket"), "keeps the context: {}", formatted_error);
            assert!(formatted_error.contains("the root cause"), "keeps the root cause: {}", formatted_error);
            assert!(error.chain().any(|cause| cause.downcast_ref::<WalkRootCause>().is_some()), "the original error stays reachable");
            assert!(error.downcast_ref::<PostBundleFetchError>().is_some());
        }
        Ok(())
    }
}
