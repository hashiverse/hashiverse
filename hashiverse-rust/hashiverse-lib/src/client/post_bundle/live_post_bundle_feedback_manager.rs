//! # Production [`PostBundleFeedbackManager`] — cache, fetch, heal
//!
//! Mirrors
//! [`crate::client::post_bundle::live_post_bundle_manager`] but for feedback: checks
//! `BUCKET_POST_BUNDLE_FEEDBACK` in
//! [`crate::client::client_storage::client_storage::ClientStorage`] (with an age-based
//! TTL since feedback mutates), falls back to `GetPostBundleFeedbackV1` against DHT
//! peers, and spawns
//! [`crate::client::post_bundle::post_bundle_feedback_healing`] in the background to
//! reconcile divergent copies. Cache propagation is guided by the same
//! [`crate::client::caching::cache_radius_tracker`] used by the post-bundle side.

use crate::anyhow_assert_eq;
use crate::client::caching::cache_radius_tracker::CacheRadiusTracker;
use crate::client::caching::post_bundle_cache_uploader;
use crate::client::client_storage::client_storage::{ClientStorage, BUCKET_POST_BUNDLE_FEEDBACK};
use crate::client::peer_tracker::peer_tracker::{PeerTracker, SharedPeerTracker};
use crate::client::post_bundle::post_bundle_feedback_healing;
use crate::client::post_bundle::post_bundle_feedback_manager::PostBundleFeedbackManager;
use crate::protocol::payload::payload::{CacheRequestTokenV1, GetPostBundleFeedbackResponseV1, GetPostBundleFeedbackV1, PayloadRequestKind, PayloadResponseKind};
use crate::protocol::posting::encoded_post_bundle_feedback::EncodedPostBundleFeedbackV1;
use crate::protocol::peer::Peer;
use crate::protocol::rpc;
use crate::tools::buckets::BucketLocation;
use crate::tools::config::{CLIENT_POST_BUNDLE_FEEDBACK_CACHE_DURATION};
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
use tokio::sync::Mutex;

pub struct LivePostBundleFeedbackManager {
    runtime_services: Arc<RuntimeServices>,
    client_storage: Arc<dyn ClientStorage>,
    peer_tracker: SharedPeerTracker,
    sponsor_id: Id,
    post_bundle_feedback_inflight: Mutex<HashMap<Id, Arc<Mutex<()>>>>,
    post_bundle_feedback_cache_radius_tracker: CacheRadiusTracker,
}

impl LivePostBundleFeedbackManager {
    pub fn new(runtime_services: Arc<RuntimeServices>, sponsor_id: Id, client_storage: Arc<dyn ClientStorage>, peer_tracker: SharedPeerTracker) -> Self {
        Self {
            runtime_services,
            client_storage,
            peer_tracker,
            sponsor_id,
            post_bundle_feedback_inflight: Mutex::new(HashMap::new()),
            post_bundle_feedback_cache_radius_tracker: CacheRadiusTracker::new(CLIENT_POST_BUNDLE_FEEDBACK_CACHE_DURATION.const_mul(5)), // We let these cache radius caches last longer than our own local cache of the bundles...
        }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl PostBundleFeedbackManager for LivePostBundleFeedbackManager {
    async fn get_post_bundle_feedback(&self, bucket_location: BucketLocation, time_millis: TimeMillis) -> anyhow::Result<EncodedPostBundleFeedbackV1> {
        let post_bundle_location_id = bucket_location.location_id;
        let get_from_cache = async |location_id: Id, time_millis: TimeMillis, reason: &str| -> Option<EncodedPostBundleFeedbackV1> {
            let result: anyhow::Result<Option<EncodedPostBundleFeedbackV1>> = try {
                let raw = self.client_storage.get(BUCKET_POST_BUNDLE_FEEDBACK, &location_id.to_hex_str(), time_millis).await?;
                if let Some(raw) = raw {
                    let feedback = EncodedPostBundleFeedbackV1::from_bytes(Bytes::from(raw))?;
                    let duration = time_millis - feedback.header.time_millis;
                    if duration < CLIENT_POST_BUNDLE_FEEDBACK_CACHE_DURATION {
                        trace!("Using cached PostBundleFeedback for {} (age {}) at {}", location_id, duration, reason);
                        Some(feedback)
                    } else {
                        trace!("Cached PostBundleFeedback for {} expired (age {}) at {}", location_id, duration, reason);
                        None
                    }
                } else {
                    None
                }
            };

            result.unwrap_or_else(|e| {
                warn!("discarding problematic cached PostBundleFeedback: {}", e);
                None
            })
        };

        // Check the cache
        if let Some(cached) = get_from_cache(post_bundle_location_id, time_millis, "preflight").await {
            return Ok(cached);
        }

        // Inflight deduplication: get or create a per-key mutex
        let key_lock = {
            let mut inflight = self.post_bundle_feedback_inflight.lock().await;
            inflight.entry(post_bundle_location_id).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
        };
        let _inflight_guard = key_lock.lock().await;
        defer!(if let Ok(mut m) = self.post_bundle_feedback_inflight.try_lock() { m.remove(&post_bundle_location_id); });

        // Re-check the cache - a concurrent caller may have just finished fetching and stored the result.
        if let Some(cached) = get_from_cache(post_bundle_location_id, time_millis, "postflight").await {
            return Ok(cached);
        }

        let cache_radius = self.post_bundle_feedback_cache_radius_tracker.get(post_bundle_location_id, time_millis);

        // We need to refresh...
        let mut peers_visited = Vec::new();
        let mut accepted_post_bundle_feedbacks = AcceptedPostBundleFeedbacks::new(post_bundle_location_id);
        let mut cache_request_tokens: Vec<CacheRequestTokenV1> = Vec::new();
        let mut positive_responder_leading_agreement_bits: Vec<LeadingAgreementBits> = Vec::new();
        {
            let mut peer_iter = PeerTracker::iterate_to_location(&self.peer_tracker, post_bundle_location_id, 2 * config::REDUNDANT_SERVERS_PER_POST, cache_radius).await?;
            while let Some((peer, leading_agreement_bits)) = peer_iter.next_peer() {
                if accepted_post_bundle_feedbacks.accepted_originator_ids().contains(&peer.id) {
                    continue;
                }

                let result: anyhow::Result<()> = try {
                    info!("Requesting PostBundleFeedback with leading_agreement_bits={} from peer {}", leading_agreement_bits, peer);

                    let request = GetPostBundleFeedbackV1 {
                        bucket_location: bucket_location.clone(),
                        peers_visited: peers_visited.clone(),
                        already_retrieved_peer_ids: accepted_post_bundle_feedbacks.accepted_originator_ids().to_vec(),
                    };
                    let request = json::struct_to_bytes(&request)?;
                    let response = rpc::rpc::rpc_server_known(&self.runtime_services, &self.sponsor_id, &peer, PayloadRequestKind::GetPostBundleFeedbackV1, request).await?;
                    anyhow_assert_eq!(&PayloadResponseKind::GetPostBundleFeedbackResponseV1, &response.response_request_kind);
                    let response = GetPostBundleFeedbackResponseV1::from_bytes(response.bytes)?;

                    peers_visited.push(peer.clone());
                    peer_iter.add_peers(response.peers_nearer);

                    if let Some(token) = response.cache_request_token {
                        cache_request_tokens.push(token);
                    }

                    let mut found_bundle_from_this_peer = false;

                    for cached_bytes in response.post_bundle_feedbacks_cached {
                        match accepted_post_bundle_feedbacks.offer_cached(cached_bytes) {
                            Ok(accepted) => found_bundle_from_this_peer |= accepted,
                            Err(e) => warn!("Rejected cached PostBundleFeedback relayed by peer {}: {}", peer, e),
                        }
                    }

                    if let Some(post_bundle_feedback_raw) = response.encoded_post_bundle_feedback {
                        found_bundle_from_this_peer |= accepted_post_bundle_feedbacks.offer_direct(post_bundle_feedback_raw, &peer)?;
                    }

                    if found_bundle_from_this_peer {
                        positive_responder_leading_agreement_bits.push(leading_agreement_bits);
                    }

                    if accepted_post_bundle_feedbacks.quorum_count() >= config::REDUNDANT_SERVERS_PER_POST {
                        break;
                    }
                };

                if let Err(e) = result {
                    warn!("Error retrieving PostBundleFeedback from peer {}: {}", peer, e);
                    peer_iter.remove_peer(&peer);
                }
            }

            let result = PeerTracker::flush(&self.peer_tracker).await;
            if let Err(e) = result {
                warn!("Error flushing peer tracker: {}", e);
            }
        }

        trace!("Discovered {} post bundle feedbacks after visiting {} peers", accepted_post_bundle_feedbacks.quorum_count(), peers_visited.len());

        // Store our new cache radius - the peer "furthest out" from our bundle's location_id
        if let Some(new_radius) = positive_responder_leading_agreement_bits.iter().copied().min() {
            self.post_bundle_feedback_cache_radius_tracker.update(post_bundle_location_id, new_radius, time_millis);
        }

        // Merge all collected bundles, taking the best-pow entry per (post_id, feedback_type).
        // NB: the result is an unsigned client view (its header no longer covers its feedbacks_bytes) - fine for our own cache, never forward it.
        let best_encoded_post_bundle_feedback = EncodedPostBundleFeedbackV1::merge(accepted_post_bundle_feedbacks.bundles())
            .ok_or_else(|| anyhow::anyhow!("No post bundle feedbacks discovered for {}", post_bundle_location_id))?;

        // Cache it
        trace!("Caching PostBundleFeedback from post_bundle_location_id={} with length {}", post_bundle_location_id, best_encoded_post_bundle_feedback.feedbacks_bytes.len());
        let encoded_post_bundle_feedback_bytes = best_encoded_post_bundle_feedback.to_bytes()?;
        self.client_storage.put(BUCKET_POST_BUNDLE_FEEDBACK, &post_bundle_location_id.to_hex_str(), encoded_post_bundle_feedback_bytes.to_vec(), time_millis).await?;

        // Kick off healing
        post_bundle_feedback_healing::heal_post_bundle_feedbacks(
            self.runtime_services.clone(),
            self.sponsor_id,
            &peers_visited,
            accepted_post_bundle_feedbacks.bundles(),
            &best_encoded_post_bundle_feedback,
        );

        // Kick off remote cache populating
        post_bundle_cache_uploader::upload_post_bundle_feedback_caches(
            self.runtime_services.clone(),
            self.sponsor_id,
            cache_request_tokens,
            accepted_post_bundle_feedbacks.bundle_bytes_for_upload(),
        );

        Ok(best_encoded_post_bundle_feedback)
    }
}

/// The feedback bundles accepted so far while walking towards a bucket's location.  Mirrors
/// [`crate::client::post_bundle::live_post_bundle_manager`]'s `AcceptedPostBundles`: every accepted bundle comes from a genuine
/// (server-PoW) originator, is for this location, and counts once towards the quorum - so free self-signed bundles can neither
/// end the walk early nor mask a genuine originator's copy via `already_retrieved_peer_ids`.
struct AcceptedPostBundleFeedbacks {
    location_id: Id,
    accepted_originator_ids: Vec<Id>,
    bundles: Vec<EncodedPostBundleFeedbackV1>,
    /// The intact originator-signed bytes of every accepted, non-empty bundle that passes full verify(): the only thing we may forward to caches.
    bundle_bytes_for_upload: Vec<(Id, Bytes)>,
}

impl AcceptedPostBundleFeedbacks {
    fn new(location_id: Id) -> Self {
        Self { location_id, accepted_originator_ids: Vec::new(), bundles: Vec::new(), bundle_bytes_for_upload: Vec::new() }
    }

    /// Originators whose bundles we have accepted - passed to servers so they don't send us those again.
    fn accepted_originator_ids(&self) -> &[Id] {
        &self.accepted_originator_ids
    }

    /// How many distinct servers' views we have accepted.
    fn quorum_count(&self) -> usize {
        self.accepted_originator_ids.len()
    }

    fn bundles(&self) -> &[EncodedPostBundleFeedbackV1] {
        &self.bundles
    }

    fn bundle_bytes_for_upload(&self) -> Vec<(Id, Bytes)> {
        self.bundle_bytes_for_upload.clone()
    }

    /// A cached snapshot relayed by some peer.  Servers fully verify feedback before caching it, so a relayed copy must pass full
    /// verification, and an empty one proves nothing.  Returns whether it was accepted; errors if it was unacceptable.
    fn offer_cached(&mut self, encoded_post_bundle_feedback_bytes: Bytes) -> anyhow::Result<bool> {
        let encoded_post_bundle_feedback = EncodedPostBundleFeedbackV1::from_bytes(encoded_post_bundle_feedback_bytes.clone())?;
        if self.accepted_originator_ids.contains(&encoded_post_bundle_feedback.header.peer.id) {
            return Ok(false);
        }
        encoded_post_bundle_feedback.verify_for_location(&self.location_id)?;
        if encoded_post_bundle_feedback.feedbacks_bytes.is_empty() {
            trace!("Ignoring empty cached PostBundleFeedback from peer.id={}", encoded_post_bundle_feedback.header.peer.id);
            return Ok(false);
        }

        trace!("Accepted cached PostBundleFeedback from peer.id={} with length {}", encoded_post_bundle_feedback.header.peer.id, encoded_post_bundle_feedback.feedbacks_bytes.len());
        self.accept(encoded_post_bundle_feedback_bytes, encoded_post_bundle_feedback);
        Ok(true)
    }

    /// The bundle `responding_peer` holds itself.  It may be empty, and may hold individual entries with bad PoW (which merge drops).
    /// Returns whether it was accepted; errors if it was unacceptable.
    fn offer_direct(&mut self, encoded_post_bundle_feedback_bytes: Bytes, responding_peer: &Peer) -> anyhow::Result<bool> {
        let encoded_post_bundle_feedback = EncodedPostBundleFeedbackV1::from_bytes(encoded_post_bundle_feedback_bytes.clone())?;
        if encoded_post_bundle_feedback.header.peer.id != responding_peer.id {
            anyhow::bail!("PostBundleFeedback from peer {} claims to be published by {}", responding_peer.id, encoded_post_bundle_feedback.header.peer.id);
        }
        if self.accepted_originator_ids.contains(&encoded_post_bundle_feedback.header.peer.id) {
            return Ok(false);
        }
        encoded_post_bundle_feedback.verify_origin_for_location(&self.location_id)?;

        trace!("Accepted PostBundleFeedback from peer.id={} with length {}", encoded_post_bundle_feedback.header.peer.id, encoded_post_bundle_feedback.feedbacks_bytes.len());
        self.accept(encoded_post_bundle_feedback_bytes, encoded_post_bundle_feedback);
        Ok(true)
    }

    fn accept(&mut self, encoded_post_bundle_feedback_bytes: Bytes, encoded_post_bundle_feedback: EncodedPostBundleFeedbackV1) {
        let originator_id = encoded_post_bundle_feedback.header.peer.id;
        self.accepted_originator_ids.push(originator_id);
        if !encoded_post_bundle_feedback.feedbacks_bytes.is_empty() && encoded_post_bundle_feedback.verify().is_ok() {
            self.bundle_bytes_for_upload.push((originator_id, encoded_post_bundle_feedback_bytes));
        }
        self.bundles.push(encoded_post_bundle_feedback);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::posting::encoded_post_bundle_feedback::test_fixtures::{make_bundle_signed_by, make_forged_feedback, make_valid_feedback};
    use crate::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1;
    use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
    use crate::tools::server_id::ServerId;
    use crate::tools::time_provider::time_provider::RealTimeProvider;
    use crate::tools::types::Pow;

    async fn make_genuine_server_id() -> anyhow::Result<ServerId> {
        ServerId::new("genuine", &RealTimeProvider, config::SERVER_KEY_POW_MIN, true, &SingleThreadedPowGenerator::new()).await
    }

    async fn make_unworked_server_id() -> anyhow::Result<ServerId> {
        loop {
            let server_id = ServerId::new("unworked", &RealTimeProvider, Pow(0), true, &SingleThreadedPowGenerator::new()).await?;
            if server_id.pow < config::SERVER_KEY_POW_MIN {
                return Ok(server_id);
            }
        }
    }

    fn bundle_bytes(server_id: &ServerId, location_id: Id, encoded_post_feedbacks: &[EncodedPostFeedbackV1]) -> anyhow::Result<Bytes> {
        make_bundle_signed_by(server_id, location_id, TimeMillis(1000), encoded_post_feedbacks)?.to_bytes()
    }

    // Quorum stuffing: free self-signed bundles, and empty cached ones, must not end the walk.
    #[tokio::test]
    async fn self_signed_and_empty_cached_bundles_do_not_reach_quorum() -> anyhow::Result<()> {
        let location_id = Id::random();
        let mut accepted_post_bundle_feedbacks = AcceptedPostBundleFeedbacks::new(location_id);
        for _ in 0..config::REDUNDANT_SERVERS_PER_POST {
            let unworked_server_id = make_unworked_server_id().await?;
            assert!(accepted_post_bundle_feedbacks.offer_cached(bundle_bytes(&unworked_server_id, location_id, &[make_valid_feedback(Id::random(), 1)?])?).is_err());
            assert!(!accepted_post_bundle_feedbacks.offer_cached(bundle_bytes(&make_genuine_server_id().await?, location_id, &[])?)?);
        }
        assert_eq!(accepted_post_bundle_feedbacks.quorum_count(), 0);
        assert!(accepted_post_bundle_feedbacks.accepted_originator_ids().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn cached_bundle_must_be_for_this_location_and_fully_valid() -> anyhow::Result<()> {
        let location_id = Id::random();
        let server_id = make_genuine_server_id().await?;
        let mut accepted_post_bundle_feedbacks = AcceptedPostBundleFeedbacks::new(location_id);
        assert!(accepted_post_bundle_feedbacks.offer_cached(bundle_bytes(&server_id, Id::random(), &[make_valid_feedback(Id::random(), 1)?])?).is_err());
        assert!(accepted_post_bundle_feedbacks.offer_cached(bundle_bytes(&server_id, location_id, &[make_forged_feedback(Id::random(), 1)])?).is_err());
        assert_eq!(accepted_post_bundle_feedbacks.quorum_count(), 0);
        Ok(())
    }

    // Masking: a forged bundle claiming a genuine originator's id must not land that id in already_retrieved_peer_ids.
    #[tokio::test]
    async fn forged_bundle_cannot_claim_a_genuine_originator() -> anyhow::Result<()> {
        let location_id = Id::random();
        let genuine_server_id = make_genuine_server_id().await?;
        let impostor_server_id = make_genuine_server_id().await?;
        let mut forged_bundle = make_bundle_signed_by(&impostor_server_id, location_id, TimeMillis(1000), &[make_valid_feedback(Id::random(), 1)?])?;
        forged_bundle.header.peer = genuine_server_id.to_peer(&RealTimeProvider)?;
        forged_bundle.header.signature_generate(&impostor_server_id.keys.signature_key);

        let mut accepted_post_bundle_feedbacks = AcceptedPostBundleFeedbacks::new(location_id);
        assert!(accepted_post_bundle_feedbacks.offer_cached(forged_bundle.to_bytes()?).is_err());
        assert!(!accepted_post_bundle_feedbacks.accepted_originator_ids().contains(&genuine_server_id.id));

        // The genuine copy is then still accepted
        assert!(accepted_post_bundle_feedbacks.offer_cached(bundle_bytes(&genuine_server_id, location_id, &[make_valid_feedback(Id::random(), 1)?])?)?);
        assert_eq!(accepted_post_bundle_feedbacks.accepted_originator_ids(), &[genuine_server_id.id]);
        Ok(())
    }

    #[tokio::test]
    async fn direct_bundle_must_name_the_responding_server() -> anyhow::Result<()> {
        let location_id = Id::random();
        let responding_server_id = make_genuine_server_id().await?;
        let other_server_id = make_genuine_server_id().await?;
        let mut accepted_post_bundle_feedbacks = AcceptedPostBundleFeedbacks::new(location_id);
        let responding_peer = responding_server_id.to_peer(&RealTimeProvider)?;
        assert!(accepted_post_bundle_feedbacks.offer_direct(bundle_bytes(&other_server_id, location_id, &[make_valid_feedback(Id::random(), 1)?])?, &responding_peer).is_err());
        assert!(accepted_post_bundle_feedbacks.offer_direct(bundle_bytes(&responding_server_id, Id::random(), &[])?, &responding_peer).is_err(), "and be for this location");
        assert_eq!(accepted_post_bundle_feedbacks.quorum_count(), 0);
        Ok(())
    }

    // A server's own copy counts even when empty or holding a bad entry (merge drops it), but only fully valid, non-empty copies are uploaded.
    #[tokio::test]
    async fn direct_bundles_count_once_and_only_valid_ones_are_uploaded() -> anyhow::Result<()> {
        let location_id = Id::random();
        let mut accepted_post_bundle_feedbacks = AcceptedPostBundleFeedbacks::new(location_id);

        let empty_server_id = make_genuine_server_id().await?;
        assert!(accepted_post_bundle_feedbacks.offer_direct(bundle_bytes(&empty_server_id, location_id, &[])?, &empty_server_id.to_peer(&RealTimeProvider)?)?);

        let poisoned_server_id = make_genuine_server_id().await?;
        let valid_feedback = make_valid_feedback(Id::random(), 1)?;
        assert!(accepted_post_bundle_feedbacks.offer_direct(bundle_bytes(&poisoned_server_id, location_id, &[valid_feedback.clone(), make_forged_feedback(Id::random(), 2)])?, &poisoned_server_id.to_peer(&RealTimeProvider)?)?);

        let honest_server_id = make_genuine_server_id().await?;
        let honest_bundle_bytes = bundle_bytes(&honest_server_id, location_id, &[make_valid_feedback(Id::random(), 1)?])?;
        assert!(accepted_post_bundle_feedbacks.offer_direct(honest_bundle_bytes.clone(), &honest_server_id.to_peer(&RealTimeProvider)?)?);
        assert!(!accepted_post_bundle_feedbacks.offer_cached(honest_bundle_bytes.clone())?, "an originator counts once");

        assert_eq!(accepted_post_bundle_feedbacks.quorum_count(), 3);
        assert_eq!(accepted_post_bundle_feedbacks.bundle_bytes_for_upload(), vec![(honest_server_id.id, honest_bundle_bytes)]);
        let merged = EncodedPostBundleFeedbackV1::merge(accepted_post_bundle_feedbacks.bundles()).expect("non-empty input");
        assert_eq!(merged.get_post_pow_for_feedback_type(&valid_feedback.post_id, 1), valid_feedback.pow, "the poisoned server's honest entry still counts");
        Ok(())
    }
}
