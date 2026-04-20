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
        let mut already_retrieved_peer_ids: Vec<Id> = Vec::new();
        let mut encoded_post_bundles = Vec::new();
        let mut cache_request_tokens: Vec<CacheRequestTokenV1> = Vec::new();
        let mut positive_responder_leading_agreement_bits: Vec<LeadingAgreementBits> = Vec::new();
        let mut bundle_bytes_for_upload: Vec<(Id, Bytes)> = Vec::new();
        {
            let mut peer_tracker = self.peer_tracker.write().await;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, 2 * config::REDUNDANT_SERVERS_PER_POST, cache_radius).await?;
            while let Some((peer, leading_agreement_bits)) = peer_iter.next_peer() {
                if already_retrieved_peer_ids.contains(&peer.id) {
                    continue;
                }

                let result: anyhow::Result<()> = try {
                    info!("Requesting PostBundle bucket_location={} with leading_agreement_bits={} from peer {}", bucket_location, leading_agreement_bits, peer);

                    let request = GetPostBundleV1 {
                        bucket_location: bucket_location.clone(),
                        peers_visited: peers_visited.clone(),
                        already_retrieved_peer_ids: already_retrieved_peer_ids.clone(),
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
                        let process_result: anyhow::Result<()> = try {
                            let post_bundle = EncodedPostBundleV1::from_bytes(cached_bytes.clone(), true)?;
                            post_bundle.header.verify()?;
                            trace!("Retrieved cached PostBundle from peer.id={} with {} posts", post_bundle.header.peer.id, post_bundle.header.num_posts);

                            already_retrieved_peer_ids.push(post_bundle.header.peer.id);
                            bundle_bytes_for_upload.push((post_bundle.header.peer.id, cached_bytes));
                            encoded_post_bundles.push(post_bundle);
                            found_bundle_from_this_peer = true;
                        };
                        if let Err(e) = process_result {
                            warn!("Error processing cached PostBundle: {}", e);
                        }
                    }

                    if let Some(post_bundle_raw) = response.post_bundle {
                        let post_bundle = EncodedPostBundleV1::from_bytes(post_bundle_raw.clone(), true)?;
                        post_bundle.header.verify()?;
                        trace!("Retrieved PostBundle from peer.id={} with {} posts", post_bundle.header.peer.id, post_bundle.header.num_posts);

                        already_retrieved_peer_ids.push(post_bundle.header.peer.id);
                        bundle_bytes_for_upload.push((post_bundle.header.peer.id, post_bundle_raw));
                        encoded_post_bundles.push(post_bundle);
                        found_bundle_from_this_peer = true;
                    }

                    if found_bundle_from_this_peer {
                        positive_responder_leading_agreement_bits.push(leading_agreement_bits);
                    }

                    if encoded_post_bundles.len() >= config::REDUNDANT_SERVERS_PER_POST {
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

        info!("Discovered {} post bundles after visiting {} peers", encoded_post_bundles.len(), peers_visited.len());

        // Store our new cache radius - the peer "furthest out" from our bundle's location_id
        if let Some(new_radius) = positive_responder_leading_agreement_bits.iter().copied().min() {
            self.post_bundle_cache_radius_tracker.update(bucket_location.location_id, new_radius, time_millis);
        }

        // Select the best bundle (most posts) from everything collected, including cached copies.
        let best_encoded_post_bundle = {
            encoded_post_bundles
                .iter()
                .max_by_key(|b| b.header.num_posts)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("No post bundles discovered for {}", bucket_location.location_id))?
        };

        // Cache it
        trace!("Caching PostBundle for {} with {} posts", bucket_location.location_id, best_encoded_post_bundle.header.num_posts);
        let encoded_post_bundle_bytes = best_encoded_post_bundle.to_bytes()?;
        self.client_storage.put(BUCKET_POST_BUNDLE, &bucket_location.location_id.to_hex_str(), encoded_post_bundle_bytes.to_vec(), time_millis).await?;

        // Kick off healing
        post_bundle_healing::heal_post_bundles(self.runtime_services.clone(), self.sponsor_id, bucket_location.clone(), &peers_visited, encoded_post_bundles);

        // Kick off remote cache populating
        post_bundle_cache_uploader::upload_post_bundle_caches(self.runtime_services.clone(), self.sponsor_id, cache_request_tokens, bundle_bytes_for_upload);

        Ok(best_encoded_post_bundle)
    }
}
