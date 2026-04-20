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
use crate::client::peer_tracker::peer_tracker::PeerTracker;
use crate::client::post_bundle::post_bundle_feedback_healing;
use crate::client::post_bundle::post_bundle_feedback_manager::PostBundleFeedbackManager;
use crate::protocol::payload::payload::{CacheRequestTokenV1, GetPostBundleFeedbackResponseV1, GetPostBundleFeedbackV1, PayloadRequestKind, PayloadResponseKind};
use crate::protocol::posting::encoded_post_bundle_feedback::EncodedPostBundleFeedbackV1;
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
use tokio::sync::{Mutex, RwLock};

pub struct LivePostBundleFeedbackManager {
    runtime_services: Arc<RuntimeServices>,
    client_storage: Arc<dyn ClientStorage>,
    peer_tracker: Arc<RwLock<PeerTracker>>,
    sponsor_id: Id,
    post_bundle_feedback_inflight: Mutex<HashMap<Id, Arc<Mutex<()>>>>,
    post_bundle_feedback_cache_radius_tracker: CacheRadiusTracker,
}

impl LivePostBundleFeedbackManager {
    pub fn new(runtime_services: Arc<RuntimeServices>, sponsor_id: Id, client_storage: Arc<dyn ClientStorage>, peer_tracker: Arc<RwLock<PeerTracker>>) -> Self {
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
        let mut already_retrieved_peer_ids: Vec<Id> = Vec::new();
        let mut encoded_post_bundle_feedbacks = Vec::new();
        let mut cache_request_tokens: Vec<CacheRequestTokenV1> = Vec::new();
        let mut positive_responder_leading_agreement_bits: Vec<LeadingAgreementBits> = Vec::new();
        {
            let mut peer_tracker = self.peer_tracker.write().await;
            let mut peer_iter = peer_tracker.iterate_to_location(post_bundle_location_id, 2 * config::REDUNDANT_SERVERS_PER_POST, cache_radius).await?;
            while let Some((peer, leading_agreement_bits)) = peer_iter.next_peer() {
                if already_retrieved_peer_ids.contains(&peer.id) {
                    continue;
                }

                let result: anyhow::Result<()> = try {
                    info!("Requesting PostBundleFeedback with leading_agreement_bits={} from peer {}", leading_agreement_bits, peer);

                    let request = GetPostBundleFeedbackV1 {
                        bucket_location: bucket_location.clone(),
                        peers_visited: peers_visited.clone(),
                        already_retrieved_peer_ids: already_retrieved_peer_ids.clone(),
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
                        let process_result: anyhow::Result<()> = try {
                            let post_bundle_feedback = EncodedPostBundleFeedbackV1::from_bytes(cached_bytes)?;
                            post_bundle_feedback.header.verify()?;
                            trace!("Retrieved cached PostBundleFeedback from post_bundle_location_id={} with length {}", post_bundle_location_id, post_bundle_feedback.feedbacks_bytes.len());

                            already_retrieved_peer_ids.push(post_bundle_feedback.header.peer.id);
                            encoded_post_bundle_feedbacks.push(post_bundle_feedback);
                            found_bundle_from_this_peer = true;
                        };
                        if let Err(e) = process_result {
                            warn!("Error processing cached PostBundleFeedback: {}", e);
                        }
                    }

                    if let Some(post_bundle_feedback_raw) = response.encoded_post_bundle_feedback {
                        let post_bundle_feedback = EncodedPostBundleFeedbackV1::from_bytes(post_bundle_feedback_raw)?;
                        post_bundle_feedback.header.verify()?;
                        trace!("Retrieved PostBundleFeedback from post_bundle_location_id={} with length {}", post_bundle_location_id, post_bundle_feedback.feedbacks_bytes.len());

                        already_retrieved_peer_ids.push(post_bundle_feedback.header.peer.id);
                        encoded_post_bundle_feedbacks.push(post_bundle_feedback);
                        found_bundle_from_this_peer = true;
                    }

                    if found_bundle_from_this_peer {
                        positive_responder_leading_agreement_bits.push(leading_agreement_bits);
                    }

                    if encoded_post_bundle_feedbacks.len() >= config::REDUNDANT_SERVERS_PER_POST {
                        break;
                    }
                };

                if let Err(e) = result {
                    warn!("Error retrieving PostBundleFeedback from peer {}: {}", peer, e);
                    peer_iter.remove_peer(&peer);
                }
            }

            let result = peer_tracker.flush().await;
            if let Err(e) = result {
                warn!("Error flushing peer tracker: {}", e);
            }
        }

        trace!("Discovered {} post bundle feedbacks after visiting {} peers", encoded_post_bundle_feedbacks.len(), peers_visited.len());

        // Store our new cache radius - the peer "furthest out" from our bundle's location_id
        if let Some(new_radius) = positive_responder_leading_agreement_bits.iter().copied().min() {
            self.post_bundle_feedback_cache_radius_tracker.update(post_bundle_location_id, new_radius, time_millis);
        }

        // Merge all collected bundles, taking the best-pow entry per (post_id, feedback_type).
        let best_encoded_post_bundle_feedback = EncodedPostBundleFeedbackV1::merge(&encoded_post_bundle_feedbacks)
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
            &encoded_post_bundle_feedbacks,
            &best_encoded_post_bundle_feedback,
        );

        // Kick off remote cache populating
        post_bundle_cache_uploader::upload_post_bundle_feedback_caches(
            self.runtime_services.clone(),
            self.sponsor_id,
            cache_request_tokens,
            encoded_post_bundle_feedback_bytes,
        );

        Ok(best_encoded_post_bundle_feedback)
    }
}
