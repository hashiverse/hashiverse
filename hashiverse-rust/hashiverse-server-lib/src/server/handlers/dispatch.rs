//! # Inbound RPC dispatch loop
//!
//! The hot path of the server: a single async loop that drains `IncomingRequest`s
//! from the transport's `mpsc::Receiver`, decodes each packet via
//! [`hashiverse_lib::protocol::rpc::rpc_request::RpcRequestPacketRx`], and routes
//! by [`hashiverse_lib::protocol::payload::payload::PayloadRequestKind`] to the
//! correct per-op handler (bootstrap, announce, get/submit/heal/cache post
//! bundles, get/submit/heal/cache feedback, fetch URL preview, trending
//! hashtags, ping).
//!
//! Per-request safety checks happen before any real work:
//!
//! - **PoW verification** — the packet's PoW must be sufficient for *this* server's
//!   identity. Anything under-powered or stale is dropped immediately.
//! - **Replay protection** — a short-lived salt cache rejects salts we've already
//!   seen, so a valid signed request can't be replayed from another network vantage.
//! - **Peer upgrade** — if the caller's embedded [`hashiverse_lib::protocol::peer::Peer`] carries a stronger PoW
//!   than what we have in the tracker, the tracker is upgraded in place.
//!
//! The loop respects a `CancellationToken` so graceful shutdown drains in-flight
//! work and stops accepting new requests cleanly.

use crate::environment::environment::PostBundleMetadata;
use crate::server::hashiverse_server::HashiverseServer;
use crate::tools::tools::is_ssrf_protected_ip;
use bytes::{Bytes, BytesMut};
use hashiverse_lib::anyhow_assert_eq;
use hashiverse_lib::protocol::payload::payload::{
    AnnounceResponseV1, AnnounceV1, BootstrapResponseV1, CachePostBundleFeedbackResponseV1, CachePostBundleFeedbackV1, CachePostBundleResponseV1, CachePostBundleV1, ErrorResponseV1, FetchUrlPreviewResponseV1, FetchUrlPreviewV1,
    GetPostBundleFeedbackResponseV1, GetPostBundleFeedbackV1, GetPostBundleResponseV1, GetPostBundleV1, HealPostBundleClaimResponseV1, HealPostBundleClaimTokenV1, HealPostBundleClaimV1, HealPostBundleCommitResponseV1, HealPostBundleCommitV1,
    HealPostBundleFeedbackResponseV1, HealPostBundleFeedbackV1, PayloadRequestKind, PayloadResponseKind, PingResponseV1, SubmitPostClaimResponseV1, SubmitPostClaimTokenV1, SubmitPostClaimV1, SubmitPostCommitResponseV1, SubmitPostCommitTokenV1,
    SubmitPostCommitV1, SubmitPostFeedbackResponseV1, SubmitPostFeedbackV1, TrendingHashtagV1, TrendingHashtagsFetchResponseV1, TrendingHashtagsFetchV1,
};
use hashiverse_lib::protocol::peer::PeerPow;
use hashiverse_lib::protocol::posting::amplification::get_minimum_post_pow;
use hashiverse_lib::protocol::posting::encoded_post::EncodedPostV1;
use hashiverse_lib::protocol::posting::encoded_post_bundle::{EncodedPostBundleHeaderV1, EncodedPostBundleV1};
use hashiverse_lib::protocol::posting::encoded_post_bundle_feedback::{EncodedPostBundleFeedbackHeaderV1, EncodedPostBundleFeedbackV1};
use hashiverse_lib::protocol::rpc::rpc_request::RpcRequestPacketRx;
use hashiverse_lib::protocol::rpc::rpc_response::{RpcResponsePacketTx, RpcResponsePacketTxFlags};
use hashiverse_lib::tools::buckets::{BucketLocation, BucketType, BUCKET_DURATIONS};
use hashiverse_lib::tools::time::{TimeMillis, MILLIS_IN_SECOND};
use hashiverse_lib::tools::hyper_log_log::HyperLogLog;
use hashiverse_lib::tools::types::{Id, Signature};
use hashiverse_lib::tools::{hashing, url_preview};
use hashiverse_lib::tools::{config, json, BytesGatherer};
use hashiverse_lib::transport::transport::IncomingRequest;
use log::{info, trace, warn};
use std::collections::HashSet;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Fallback hashtags used to top up a trending-hashtags response when the server
/// does not yet know enough real trending hashtags to satisfy the requested limit.
/// Applied in order, skipping any entry whose normalised form is already present
/// in the real trending list. Filler entries are returned with `count = 0` so
/// clients can distinguish seeded fillers from genuine trending data.
const TRENDING_HASHTAGS_FALLBACK: &[&str] = &["hashiverse", "news"];

/// Normalise a hashtag for equality comparison: lowercase, with any leading `#`
/// stripped. Mirrors the canonicalisation performed by `Id::from_hashtag_str`.
fn normalise_hashtag(hashtag: &str) -> String {
    let lowercased = hashtag.to_lowercase();
    match lowercased.strip_prefix('#') {
        Some(stripped) => stripped.to_string(),
        None => lowercased,
    }
}

/// Top up `trending_hashtags` from `fallback_hashtags` (in order) until it reaches
/// `limit`, skipping any fallback whose normalised form already appears in the list.
/// Filler entries are inserted with `count = 0`. No-op if the list is already at
/// or above the limit.
fn top_up_trending_hashtags_with_fallback(trending_hashtags: &mut Vec<TrendingHashtagV1>, limit: u16, fallback_hashtags: &[&str]) {
    let target_length = limit as usize;
    if trending_hashtags.len() >= target_length {
        return;
    }

    let mut existing_normalised_hashtags: HashSet<String> = trending_hashtags.iter()
        .map(|entry| normalise_hashtag(&entry.hashtag))
        .collect();

    for fallback_hashtag in fallback_hashtags {
        if trending_hashtags.len() >= target_length {
            break;
        }
        let normalised_fallback_hashtag = normalise_hashtag(fallback_hashtag);
        if existing_normalised_hashtags.contains(&normalised_fallback_hashtag) {
            continue;
        }
        trending_hashtags.push(TrendingHashtagV1 {
            hashtag: (*fallback_hashtag).to_string(),
            count: 0,
        });
        existing_normalised_hashtags.insert(normalised_fallback_hashtag);
    }
}

impl HashiverseServer {
    pub async fn wrap_and_dispatch_network_envelopes(&self, cancellation_token: CancellationToken, mut rx: mpsc::Receiver<IncomingRequest>) -> Result<(), anyhow::Error> {
        loop {
            tokio::select! {
                _ = cancellation_token.cancelled() => { break },

                receipt = rx.recv() => {
                    match receipt {
                        Some(incoming) => {
                            // trace!("dispatch_network_envelopes received bytes={:?}", incoming.bytes);
                            let result = self.wrap_and_dispatch_network_envelope(cancellation_token.clone(), &incoming).await;
                            match result {
                                Ok(bytes) => {
                                    let result = incoming.reply.send(bytes);
                                    if result.is_err() { warn!("failed to send reply"); }
                                },
                                Err(e) => {
                                    warn!("failed to process packet from {}: {}", incoming.caller_address, e);
                                    incoming.report_bad_request();
                                    drop(incoming.reply);
                                },
                            }
                        },
                        None => {
                            warn!("channel closed");
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn wrap_and_dispatch_network_envelope(&self, cancellation_token: CancellationToken, incoming: &IncomingRequest) -> anyhow::Result<BytesGatherer> {
        let caller_address = incoming.caller_address.as_str();
        let current_time_millis = self.runtime_services.time_provider.current_time_millis();

        // Decode the envelope
        let rpc_request_packet_rx = RpcRequestPacketRx::decode(&current_time_millis, &self.server_id.keys.verification_key_bytes, &self.server_id.keys.pq_commitment_bytes, incoming.bytes.clone())?;
        // trace!("payload_request_kind={}", rpc_request_packet_rx.payload_request_kind);

        // Check that we have not seen this salt recently (stops replay attacks)
        {
            if self.seen_salts.contains_key(&rpc_request_packet_rx.pow_salt) {
                anyhow::bail!("replay detected: salt already seen");
            }
            self.seen_salts.insert(rpc_request_packet_rx.pow_salt, ());
        }

        // Keep this for our response
        let pow_content_hash = rpc_request_packet_rx.pow_content_hash;

        let dispatch_result: anyhow::Result<BytesGatherer> = try {
            // Check that the pow is meaningful
            let pow = match rpc_request_packet_rx.pow_server_known {
                true => {
                    let (pow, improved_pow_current_day, improved_pow_current_month) = {
                        let peer_self = self.peer_self.read(); // Remember alphabetical locking order!
                        let pow = PeerPow::new(
                            rpc_request_packet_rx.pow_sponsor_id,
                            &peer_self.verification_key_bytes,
                            &peer_self.pq_commitment_bytes,
                            rpc_request_packet_rx.pow_timestamp,
                            rpc_request_packet_rx.pow_content_hash,
                            rpc_request_packet_rx.pow_salt,
                        )?;

                        let improved_pow_current_day = pow.pow_decayed_day(current_time_millis) > peer_self.pow_current_day.pow_decayed_day(current_time_millis);
                        let improved_pow_current_month = pow.pow_decayed_month(current_time_millis) > peer_self.pow_current_month.pow_decayed_month(current_time_millis);

                        (pow, improved_pow_current_day, improved_pow_current_month)
                    };

                    // Check if we need to modify peer_self
                    if improved_pow_current_day || improved_pow_current_month {
                        let mut peer_self = self.peer_self.write(); // Remember alphabetical locking order!
                        if improved_pow_current_day {
                            trace!("pow_current_day upgraded {} -> {}", peer_self.pow_current_day, pow);
                            peer_self.pow_current_day = pow.clone();
                        }
                        if improved_pow_current_month {
                            trace!("pow_current_month upgraded {} -> {}", peer_self.pow_current_month, pow);
                            peer_self.pow_current_month = pow.clone();
                        }

                        peer_self.sign(self.runtime_services.time_provider.as_ref(), &self.server_id.keys.signature_key)?;
                    }

                    Some(pow)
                }

                false => {
                    // Only some request types are allowed anonymous pow
                    match rpc_request_packet_rx.payload_request_kind {
                        PayloadRequestKind::BootstrapV1 => {}
                        _ => anyhow::bail!("Anonymous pow not allowed for {}", rpc_request_packet_rx.payload_request_kind),
                    }

                    None
                }
            };

            // Dispatch appropriately
            let (compress_response, payload_response_kind, payload) = self.dispatch_network_envelope(cancellation_token, pow, rpc_request_packet_rx).await?;
            let response_flags = match compress_response {
                true => RpcResponsePacketTxFlags::COMPRESSED,
                false => RpcResponsePacketTxFlags::empty(),
            };

            // Encode response
            RpcResponsePacketTx::encode(
                &self.server_id.keys.signature_key,
                &self.server_id.keys.verification_key_bytes,
                &self.server_id.keys.pq_commitment_bytes,
                &self.server_id.sponsor_id,
                &self.server_id.timestamp,
                &self.server_id.hash,
                &self.server_id.salt,
                &pow_content_hash,
                response_flags,
                payload_response_kind,
                payload,
            )?
        };

        match dispatch_result {
            Ok(results) => Ok(results),
            Err(e) => {
                warn!("failed to dispatch packet from {}: {}", caller_address, e);
                incoming.report_bad_request();

                let payload_response_kind = PayloadResponseKind::ErrorResponseV1;
                let response = ErrorResponseV1 { code: 0, message: e.to_string() };
                let payload = BytesGatherer::from_bytes(json::struct_to_bytes(&response)?);

                // Encode response
                RpcResponsePacketTx::encode(
                    &self.server_id.keys.signature_key,
                    &self.server_id.keys.verification_key_bytes,
                    &self.server_id.keys.pq_commitment_bytes,
                    &self.server_id.sponsor_id,
                    &self.server_id.timestamp,
                    &self.server_id.hash,
                    &self.server_id.salt,
                    &pow_content_hash,
                    RpcResponsePacketTxFlags::COMPRESSED,
                    payload_response_kind,
                    payload,
                )
            }
        }
    }

    async fn dispatch_network_envelope(&self, cancellation_token: CancellationToken, pow: Option<PeerPow>, rpc_request_packet_rx: RpcRequestPacketRx) -> anyhow::Result<(bool, PayloadResponseKind, BytesGatherer)> {
        // Where do we want to decide if we should compress?  Here in one block, or at the end of each individual dispatch_xxx?
        let compress_response = match rpc_request_packet_rx.payload_request_kind {
            PayloadRequestKind::GetPostBundleV1 => false,   // We don't compress these again as they are already predominantly compressed
            PayloadRequestKind::CachePostBundleV1 => false, // We don't compress these again as they are already predominantly compressed
            _ => true,
        };

        let (payload_response_kind, payload) = match rpc_request_packet_rx.payload_request_kind {
            PayloadRequestKind::ErrorV1 => {
                anyhow::bail!("Received ErrorV1");
            }
            PayloadRequestKind::PingV1 => self.dispatch_network_payload_x_PingV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await?,
            PayloadRequestKind::BootstrapV1 => self.dispatch_network_payload_x_BootstrapV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await?,
            PayloadRequestKind::AnnounceV1 => self.dispatch_network_payload_x_AnnounceV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await?,
            PayloadRequestKind::GetPostBundleV1 => self.dispatch_network_payload_x_GetPostBundleV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await?,
            PayloadRequestKind::GetPostBundleFeedbackV1 => { self.dispatch_network_payload_x_GetPostBundleFeedbackV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await? }
            PayloadRequestKind::SubmitPostClaimV1 => { self.dispatch_network_payload_x_SubmitPostClaimV1(cancellation_token, pow, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await? }
            PayloadRequestKind::SubmitPostCommitV1 => { self.dispatch_network_payload_x_SubmitPostCommitV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await? }
            PayloadRequestKind::SubmitPostFeedbackV1 => { self.dispatch_network_payload_x_SubmitPostFeedbackV1(cancellation_token, pow, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await? }
            PayloadRequestKind::HealPostBundleClaimV1 => { self.dispatch_network_payload_x_HealPostBundleClaimV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await? }
            PayloadRequestKind::HealPostBundleCommitV1 => { self.dispatch_network_payload_x_HealPostBundleCommitV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await? }
            PayloadRequestKind::HealPostBundleFeedbackV1 => { self.dispatch_network_payload_x_HealPostBundleFeedbackV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await? }
            PayloadRequestKind::CachePostBundleV1 => self.dispatch_network_payload_x_CachePostBundleV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await?,
            PayloadRequestKind::CachePostBundleFeedbackV1 => { self.dispatch_network_payload_x_CachePostBundleFeedbackV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await? }
            PayloadRequestKind::FetchUrlPreviewV1 => self.dispatch_network_payload_x_FetchUrlPreviewV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await?,
            PayloadRequestKind::TrendingHashtagsFetchV1 => self.dispatch_network_payload_x_TrendingHashtagsFetchV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await?,
        };

        Ok((compress_response, payload_response_kind, payload))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_PingV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, _bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::PingV1, &payload_request_kind);
        let peer = self.peer_self.read().clone();
        let json = json::struct_to_bytes(&PingResponseV1 { peer })?;
        Ok((PayloadResponseKind::PingResponseV1, BytesGatherer::from_bytes(json)))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_BootstrapV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, _bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::BootstrapV1, &payload_request_kind);
        let peers_random = self.kademlia.read().get_peers_random(config::BOOTSTRAP_V1_NUM_PEERS);
        let json = json::struct_to_bytes(&BootstrapResponseV1 { peers_random })?;
        Ok((PayloadResponseKind::BootstrapResponseV1, BytesGatherer::from_bytes(json)))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_AnnounceV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::AnnounceV1, &payload_request_kind);

        let request = json::bytes_to_struct::<AnnounceV1>(&bytes)?;
        // trace!("received AnnounceV1 from peer={}", request.peer_self);

        let peer = request.peer_self;
        let peer_id = peer.id;

        // Check that this Peer checks out and add it to our kademlia
        self.add_potential_peer_to_kademlia(peer, self.runtime_services.time_provider.as_ref().current_time_millis()).await;

        let (peers_nearest, _) = self.kademlia.read().get_peers_for_key(&peer_id, config::ANNOUNCE_V1_NUM_PEERS);

        let json = json::struct_to_bytes(&AnnounceResponseV1 {
            peer_self: self.peer_self.read().clone(),
            peers_nearest,
        })?;
        Ok((PayloadResponseKind::AnnounceResponseV1, BytesGatherer::from_bytes(json)))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_GetPostBundleV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::GetPostBundleV1, &payload_request_kind);

        let time_millis = self.runtime_services.time_provider.as_ref().current_time_millis();

        let request = json::bytes_to_struct::<GetPostBundleV1>(&bytes)?;
        trace!("received GetPostBundleV1: bucket_location={}", request.bucket_location);

        // Check that the location_id makes sense
        request.bucket_location.validate()?;

        // Store the provided Peers
        {
            for peer in request.peers_visited {
                self.add_potential_peer_to_kademlia(peer, time_millis).await;
            }
        }

        let peer_self = self.peer_self.read().clone();

        // Check our own records to see that we are close enough to store this post
        let (peers_nearer, among_peers_nearer) = self.kademlia.read().get_peers_for_key(&request.bucket_location.location_id, config::REDUNDANT_SERVERS_PER_POST);
        if !among_peers_nearer {
            warn!("I am not in peers_nearer {}", peer_self);
        }

        let post_bundle = match among_peers_nearer {
            true => {
                // At some point, for parallelisms sake, we may wish to replace this with a read lock followed by a release and write lock and recheck
                // But then again - if our server is getting load, the federated cache mechanism should alleviate it, so this may be overkill...
                let _post_bundle_lock = self.environment.get_write_lock_for_location_id(&request.bucket_location.location_id);

                let mut encoded_post_bundle_bytes: Option<Bytes> = None;

                // Try to load bytes from the disk (if we even have them)
                let post_bundle_metadata = self.environment.get_post_bundle_metadata(time_millis, &request.bucket_location.location_id)?;
                if let Some(mut post_bundle_metadata) = post_bundle_metadata {
                    encoded_post_bundle_bytes = self.environment.get_post_bundle_bytes(time_millis, &request.bucket_location.location_id)?;

                    // Has the PostBundle become sealed since the last time it was written to disk?  Perhaps enough time has passed
                    if !post_bundle_metadata.sealed {
                        let sealed = time_millis > request.bucket_location.bucket_time_millis + request.bucket_location.duration + config::ENCODED_POST_BUNDLE_V1_ELAPSED_THRESHOLD_MILLIS;
                        if sealed {
                            // We can rewrite the postbundle on disk for the final time now that it is sealed
                            if let Some(encoded_post_bundle_bytes_old) = encoded_post_bundle_bytes {
                                let mut encoded_post_bundle = EncodedPostBundleV1::from_bytes(encoded_post_bundle_bytes_old, true)?;
                                encoded_post_bundle.header.time_millis = time_millis;
                                encoded_post_bundle.header.sealed = true;
                                encoded_post_bundle.header.signature_generate(&self.server_id.keys.signature_key)?;
                                let encoded_post_bundle_bytes_new = encoded_post_bundle.to_bytes()?;
                                self.environment.put_post_bundle_bytes(time_millis, &request.bucket_location.location_id, &encoded_post_bundle_bytes_new)?;
                                encoded_post_bundle_bytes = Some(encoded_post_bundle_bytes_new);
                            }

                            // And the updated metadata
                            post_bundle_metadata.sealed = true;
                            self.environment.put_post_bundle_metadata(time_millis, &request.bucket_location.location_id, &post_bundle_metadata, 0)?;
                        }
                        else {
                            // We must simply update the timestamp - we need this for client caching.  It's expensive, but caching should help us out here if it is a truly busy bucket
                            if let Some(encoded_post_bundle_bytes_old) = encoded_post_bundle_bytes {
                                let mut encoded_post_bundle = EncodedPostBundleV1::from_bytes(encoded_post_bundle_bytes_old, true)?;
                                encoded_post_bundle.header.time_millis = time_millis;
                                encoded_post_bundle.header.signature_generate(&self.server_id.keys.signature_key)?;
                                let encoded_post_bundle_bytes_new = encoded_post_bundle.to_bytes()?;
                                encoded_post_bundle_bytes = Some(encoded_post_bundle_bytes_new);
                            }
                        }
                    }
                };

                // If we dont have the metadata, or the bytes on disk, return a fresh one
                // Generally if we have the metadata, we should always have bytes on disk, except if a request ot post was granted but they then never came back with the data...
                if encoded_post_bundle_bytes.is_none() {
                    let sealed = time_millis > request.bucket_location.bucket_time_millis + request.bucket_location.duration + config::ENCODED_POST_BUNDLE_V1_ELAPSED_THRESHOLD_MILLIS;

                    let mut header = EncodedPostBundleHeaderV1 {
                        time_millis,
                        location_id: request.bucket_location.location_id,
                        overflowed: false,
                        sealed,
                        num_posts: 0,
                        encoded_post_ids: vec![],
                        encoded_post_lengths: vec![],
                        encoded_post_healed: HashSet::new(),
                        peer: peer_self.clone(),
                        signature: Signature::zero(),
                    };
                    header.signature_generate(&self.server_id.keys.signature_key)?;

                    let encoded_post_bundle = EncodedPostBundleV1 { header, encoded_posts_bytes: Bytes::new() };
                    encoded_post_bundle_bytes = Some(encoded_post_bundle.to_bytes()?);
                }

                encoded_post_bundle_bytes
            }
            false => None,
        };

        let cache_result = self.post_bundle_cache.on_get(&request.bucket_location, &request.already_retrieved_peer_ids, &peer_self, &self.server_id, time_millis);

        let get_post_bundle_response = GetPostBundleResponseV1 {
            peers_nearer,
            cache_request_token: cache_result.cache_request_token,
            post_bundles_cached: cache_result.cached_items,
            post_bundle,
        };
        Ok((PayloadResponseKind::GetPostBundleResponseV1, get_post_bundle_response.to_bytes_gatherer()?))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_GetPostBundleFeedbackV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::GetPostBundleFeedbackV1, &payload_request_kind);

        let time_millis = self.runtime_services.time_provider.as_ref().current_time_millis();

        let request = json::bytes_to_struct::<GetPostBundleFeedbackV1>(&bytes)?;
        trace!("received GetPostBundleFeedbackV1");

        // Store the provided Peers
        {
            for peer in request.peers_visited {
                self.add_potential_peer_to_kademlia(peer, time_millis).await;
            }
        }

        // Check our own records to see that we are close enough to store this post
        let (peers_nearer, among_peers_nearer) = self.kademlia.read().get_peers_for_key(&request.bucket_location.location_id, config::REDUNDANT_SERVERS_PER_POST);

        let mut post_bundle_encoded_feedbacks_bytes: Option<Bytes> = None;

        if among_peers_nearer {
            // We only support feedbacks if we know about this post_bundle
            let post_bundle_metadata = self.environment.get_post_bundle_metadata(time_millis, &request.bucket_location.location_id)?;
            if post_bundle_metadata.is_some() {
                post_bundle_encoded_feedbacks_bytes = Some(self.environment.get_post_bundle_encoded_post_feedbacks_bytes(time_millis, &request.bucket_location.location_id)?);
            }
        }

        // Wrap the feedbacks with a header (if we have any)
        let peer_self = self.peer_self.read().clone();
        let encoded_post_bundle_feedback = match post_bundle_encoded_feedbacks_bytes {
            Some(feedbacks_bytes) => {

                let feedbacks_bytes_hash = hashing::hash(feedbacks_bytes.as_ref());

                let mut header = EncodedPostBundleFeedbackHeaderV1 {
                    time_millis,
                    location_id: request.bucket_location.location_id,
                    feedbacks_bytes_hash,
                    peer: peer_self.clone(),
                    signature: Signature::zero(),
                };
                header.signature_generate(&self.server_id.keys.signature_key);

                let encoded_post_bundle_feedback = EncodedPostBundleFeedbackV1 {
                    header,
                    feedbacks_bytes,
                };
                Some(encoded_post_bundle_feedback.to_bytes()?)
            }
            None => None,
        };

        let cache_result = self.post_bundle_feedback_cache.on_get(&request.bucket_location, &request.already_retrieved_peer_ids, &peer_self, &self.server_id, time_millis);

        let get_post_bundle_feedback_response = GetPostBundleFeedbackResponseV1 {
            peers_nearer,
            cache_request_token: cache_result.cache_request_token,
            post_bundle_feedbacks_cached: cache_result.cached_items,
            encoded_post_bundle_feedback,
        };
        Ok((PayloadResponseKind::GetPostBundleFeedbackResponseV1, get_post_bundle_feedback_response.to_bytes_gatherer()?))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_SubmitPostClaimV1(&self, _cancellation_token: CancellationToken, pow: Option<PeerPow>, payload_request_kind: PayloadRequestKind, mut bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::SubmitPostClaimV1, &payload_request_kind);

        let time_millis = self.runtime_services.time_provider.as_ref().current_time_millis();

        let pow = match pow {
            Some(pow) => pow,
            None => anyhow::bail!("We need pow for a submit post claim"),
        };

        let request = SubmitPostClaimV1::from_bytes(&mut bytes)?;
        trace!("received SubmitPostClaimV1");

        // Check that the location_id makes sense
        request.bucket_location.validate()?;

        // Check that we support the bucket duration
        let bucket_duration = {
            let bucket_duration = BUCKET_DURATIONS.iter().find(|bucket_duration| **bucket_duration == request.bucket_location.duration);
            match bucket_duration {
                Some(bucket_duration) => *bucket_duration,
                None => anyhow::bail!("Unrecognised bucket duration provided"),
            }
        };

        let decoded_post = EncodedPostV1::decode_from_bytes(request.encoded_post_bytes, &request.bucket_location.base_id, false, false)?;

        // Check that enough pow has been done for this post
        {
            let pow_minimum = get_minimum_post_pow(decoded_post.header.post_length, decoded_post.header.linked_base_ids.len(), request.bucket_location.duration);
            if pow.pow < pow_minimum {
                anyhow::bail!("Insufficient proof of work for this post: actual={} < expected={}", pow.pow, pow_minimum);
            }
        }

        // Check that the post matches the bucket
        {
            // Ensure that the bucket timestamp fits the post
            let timestamp = BucketLocation::round_down_to_bucket_start(decoded_post.header.time_millis, bucket_duration);
            if timestamp != request.bucket_location.bucket_time_millis {
                anyhow::bail!("The post timestamp does not match the bucket");
            }
        }

        let client_id = decoded_post.header.client_id()?;

        // Ensure that the base_id is related to the post in the linked_base_ids.
        if !decoded_post.header.linked_base_ids.contains(&request.bucket_location.base_id) {
            anyhow::bail!("The base_id is not related to the post");
        }

        // Check that only the posting user is allowed to post to a bucket of type USER
        if request.bucket_location.bucket_type == BucketType::User && request.bucket_location.base_id != client_id.id {
            anyhow::bail!("Only the posting user is allowed to post to a bucket of type USER");
        }

        // For ReplyToPost and Sequel buckets, verify the referenced post is real (valid signature).
        // For Sequel buckets, additionally verify same-author.
        if matches!(request.bucket_location.bucket_type, BucketType::ReplyToPost | BucketType::Sequel) {
            let original_header_bytes = request.referenced_post_header_bytes
                .ok_or_else(|| anyhow::anyhow!("{:?} posts require the original post's header bytes", request.bucket_location.bucket_type))?;

            // Decode the original post header using the submitter's client_id as the decryption password.
            // decode_from_bytes verifies the signature — a forged header will fail here.
            let original_post = EncodedPostV1::decode_from_bytes(original_header_bytes, &client_id.id, false, false)?;

            // Verify the original post's post_id matches the bucket's base_id
            if original_post.post_id != request.bucket_location.base_id {
                anyhow::bail!("Referenced post header's post_id does not match the bucket's base_id");
            }

            // For Sequel buckets, additionally verify the sequel author matches the original post author
            if request.bucket_location.bucket_type == BucketType::Sequel {
                let original_client_id = original_post.header.client_id()?;
                if original_client_id != client_id {
                    anyhow::bail!("Sequel post author does not match original post author");
                }
            }
        }

        // Check that the post timestamp is reasonable
        {
            let delta = (time_millis - decoded_post.header.time_millis).abs();
            if delta > config::CLIENT_POST_TIMESTAMP_DELTA_THRESHOLD {
                anyhow::bail!("The post timestamp delta is too large ({} > {})", delta, config::CLIENT_POST_TIMESTAMP_DELTA_THRESHOLD);
            }
        }

        // Check our own records to see that we are close enough to store this post
        let (peers_nearer, among_peers_nearer) = self.kademlia.read().get_peers_for_key(&request.bucket_location.location_id, config::REDUNDANT_SERVERS_PER_POST);

        let submit_post_claim_token = match among_peers_nearer {
            true => {
                // Are we still willing to accept this post?
                let _post_bundle_lock = self.environment.get_write_lock_for_location_id(&request.bucket_location.location_id);
                let post_bundle_metadata = self.environment.get_post_bundle_metadata(time_millis, &request.bucket_location.location_id)?;
                let mut post_bundle_metadata = post_bundle_metadata.unwrap_or_else(PostBundleMetadata::zero);

                // If it is not yet sealed, update our metadata
                if !post_bundle_metadata.sealed {
                    post_bundle_metadata.num_posts_granted += 1;
                    post_bundle_metadata.overflowed = post_bundle_metadata.num_posts > config::ENCODED_POST_BUNDLE_V1_OVERFLOWED_NUM_POSTS || post_bundle_metadata.num_posts_granted > config::ENCODED_POST_BUNDLE_V1_OVERFLOWED_NUM_POSTS_GRANTED;
                    post_bundle_metadata.sealed = post_bundle_metadata.overflowed || time_millis > request.bucket_location.bucket_time_millis + request.bucket_location.duration + config::ENCODED_POST_BUNDLE_V1_ELAPSED_THRESHOLD_MILLIS;

                    self.environment.put_post_bundle_metadata(time_millis, &request.bucket_location.location_id, &post_bundle_metadata, 0)?;
                }

                // We grant the token if we are not yet sealed
                match post_bundle_metadata.sealed {
                    false => {
                        info!("Granted for {}: num_posts={} num_posts_granted={}", request.bucket_location, post_bundle_metadata.num_posts, post_bundle_metadata.num_posts_granted);
                        Some(SubmitPostClaimTokenV1::new(self.peer_self.read().clone(), request.bucket_location.clone(), decoded_post.post_id, &self.server_id.keys.signature_key))
                    }
                    true => {
                        info!(
                            "Not granting SubmitPostClaimTokenV1 to {} as we have num_posts={} num_posts_granted={}",
                            request.bucket_location, post_bundle_metadata.num_posts, post_bundle_metadata.num_posts_granted
                        );
                        None
                    }
                }
            }

            false => None,
        };

        // If we are willing to accept this post, then we are willing to track the trendiness of its hashtags
        if submit_post_claim_token.is_some() {
            // Track referenced hashtags for trending (only for User bucket posts, where the hashtags originate)
            if request.bucket_location.bucket_type == BucketType::User && !request.referenced_hashtags.is_empty() {
                let author_verification_key_bytes = &decoded_post.header.verification_key_bytes;
                for referenced_hashtag in &request.referenced_hashtags {
                    let hashtag_id = match Id::from_hashtag_str(referenced_hashtag) {
                        Ok(id) => id,
                        Err(_) => continue, // Skip invalid hashtags silently
                    };
                    if !decoded_post.header.linked_base_ids.contains(&hashtag_id) {
                        continue; // Hashtag not backed by a linked_base_id (no PoW), skip it
                    }
                    let mut hll = self.trending_hashtags.get(referenced_hashtag).unwrap_or_else(HyperLogLog::new);
                    hll.insert(author_verification_key_bytes.as_ref());
                    self.trending_hashtags.insert(referenced_hashtag.clone(), hll);
                }
            }
        }

        let response = SubmitPostClaimResponseV1 { peers_nearer, submit_post_claim_token };
        Ok((PayloadResponseKind::SubmitPostClaimResponseV1, BytesGatherer::from_bytes(json::struct_to_bytes(&response)?)))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_SubmitPostCommitV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, mut bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::SubmitPostCommitV1, &payload_request_kind);

        let time_millis = self.runtime_services.time_provider.as_ref().current_time_millis();

        let request = SubmitPostCommitV1::from_bytes(&mut bytes)?;
        trace!("received SubmitPostCommitV1");

        let peer_self = self.peer_self.read(); // Remember alphabetical locking order!

        // Is the submit_post_claim_token from us?
        if request.submit_post_claim_token.peer.id != peer_self.id {
            anyhow::bail!("The submit_post_claim_token is not from us");
        }

        // Check that the location_id still makes sense
        request.bucket_location.validate()?;
        if request.bucket_location != request.submit_post_claim_token.bucket_location {
            anyhow::bail!("The location_id in the SubmitPostCommit does not match the bucket_location in the SubmitPostClaimToken");
        }

        // Check that we can decode the post with the bucket's base_id as password
        let decoded_post = EncodedPostV1::decode_from_bytes(request.encoded_post_bytes.clone(), &request.bucket_location.base_id, true, false)?;

        // Check that the committed post matches what was claimed
        if decoded_post.post_id != request.submit_post_claim_token.post_id {
            anyhow::bail!("The post_id of the committed post does not match the post_id in the SubmitPostClaimToken");
        }

        // Update the postbundle and metadata
        let _post_bundle_lock = self.environment.get_write_lock_for_location_id(&request.bucket_location.location_id);
        let post_bundle_metadata = self.environment.get_post_bundle_metadata(time_millis, &request.bucket_location.location_id)?;
        let post_bundle_bytes = self.environment.get_post_bundle_bytes(time_millis, &request.bucket_location.location_id)?;

        // We should always have the metadata, but just in case!
        let mut post_bundle_metadata = post_bundle_metadata.unwrap_or_else(PostBundleMetadata::zero);

        // Make a PostBundle if we dont have one on disk
        let mut post_bundle = match post_bundle_bytes {
            Some(bytes) => {
                let bytes = Bytes::from_owner(bytes);
                let bundle = EncodedPostBundleV1::from_bytes(bytes, true)?;
                bundle
            }
            None => {
                let header = EncodedPostBundleHeaderV1 {
                    time_millis: TimeMillis::zero(),
                    location_id: request.bucket_location.location_id,
                    overflowed: false,
                    sealed: false,
                    num_posts: 0,
                    encoded_post_ids: vec![],
                    encoded_post_lengths: vec![],
                    encoded_post_healed: HashSet::new(),
                    peer: self.peer_self.read().clone(),
                    signature: Signature::zero(),
                };

                EncodedPostBundleV1 { header, encoded_posts_bytes: Bytes::new() }
            }
        };

        // Sanity check that we don't already have this post
        if post_bundle.header.encoded_post_ids.contains(&decoded_post.post_id) {
            anyhow::bail!("Post {} is already in the bundle", decoded_post.post_id);
        }

        // The post bundle
        post_bundle.header.time_millis = time_millis;
        post_bundle.header.num_posts += 1;
        post_bundle.header.overflowed = post_bundle.header.num_posts > config::ENCODED_POST_BUNDLE_V1_OVERFLOWED_NUM_POSTS || post_bundle_metadata.num_posts_granted > config::ENCODED_POST_BUNDLE_V1_OVERFLOWED_NUM_POSTS_GRANTED;
        post_bundle.header.sealed = post_bundle.header.overflowed || time_millis > request.bucket_location.bucket_time_millis + request.bucket_location.duration + config::ENCODED_POST_BUNDLE_V1_ELAPSED_THRESHOLD_MILLIS;
        post_bundle.header.encoded_post_ids.push(decoded_post.post_id);
        post_bundle.header.encoded_post_lengths.push(request.encoded_post_bytes.len());
        post_bundle.header.signature_generate(&self.server_id.keys.signature_key)?;
        let mut posts_mut = BytesMut::from(post_bundle.encoded_posts_bytes.as_ref());
        posts_mut.extend_from_slice(request.encoded_post_bytes.as_ref());
        post_bundle.encoded_posts_bytes = posts_mut.freeze();
        let post_bundle_bytes_new = post_bundle.to_bytes()?;

        // The metadata
        post_bundle_metadata.num_posts = post_bundle.header.num_posts;
        post_bundle_metadata.overflowed = post_bundle.header.overflowed;
        post_bundle_metadata.sealed = post_bundle.header.sealed;
        post_bundle_metadata.size = post_bundle.header.encoded_post_lengths.iter().sum();

        {
            self.environment.put_post_bundle_bytes(time_millis, &request.bucket_location.location_id, &post_bundle_bytes_new)?;
            self.environment
                .put_post_bundle_metadata(time_millis, &request.bucket_location.location_id, &post_bundle_metadata, request.encoded_post_bytes.len())?;
        }

        info!("Persisted for {}: num_posts={} num_posts_granted={}", request.bucket_location, post_bundle_metadata.num_posts, post_bundle_metadata.num_posts_granted);

        let submit_post_commit_token = SubmitPostCommitTokenV1::new(peer_self.clone(), request.bucket_location, decoded_post.post_id, &self.server_id.keys.signature_key);

        let response = SubmitPostCommitResponseV1 { submit_post_commit_token };
        Ok((PayloadResponseKind::SubmitPostCommitResponseV1, BytesGatherer::from_bytes(json::struct_to_bytes(&response)?)))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_SubmitPostFeedbackV1(&self, _cancellation_token: CancellationToken, pow: Option<PeerPow>, payload_request_kind: PayloadRequestKind, mut bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::SubmitPostFeedbackV1, &payload_request_kind);

        let time_millis = self.runtime_services.time_provider.as_ref().current_time_millis();

        let request = SubmitPostFeedbackV1::from_bytes(&mut bytes)?;
        trace!("received SubmitPostFeedbackV1");

        // Enforce minimum PoW for feedback submission
        let pow = pow.ok_or_else(|| anyhow::anyhow!("pow required for SubmitPostFeedbackV1"))?;
        if pow.pow < config::POW_MINIMUM_PER_FEEDBACK {
            anyhow::bail!("Insufficient pow for feedback: {} < {}", pow.pow, config::POW_MINIMUM_PER_FEEDBACK);
        }

        // Check the feedback makes sense
        request.encoded_post_feedback.pow_verify()?;

        let location_id = request.bucket_location.location_id;

        // Check our own records to see that we are close enough to store this post feedback
        let (peers_nearer, among_peers_nearer) = self.kademlia.read().get_peers_for_key(&location_id, config::REDUNDANT_SERVERS_PER_POST);

        let accepted = (|| -> anyhow::Result<bool> {
            if !among_peers_nearer {
                return Ok(false);
            }

            // Do we recognise the post associated with this feedback
            let _post_bundle_lock = self.environment.get_read_lock_for_location_id(&location_id);
            let Some(post_bundle_bytes) = self.environment.get_post_bundle_bytes(time_millis, &location_id)?
            else {
                return Ok(false);
            };

            let post_bundle = EncodedPostBundleV1::from_bytes(Bytes::from_owner(post_bundle_bytes), false)?;
            if !post_bundle.header.encoded_post_ids.contains(&request.encoded_post_feedback.post_id) {
                return Ok(false);
            }

            Ok(true)
        })()?;

        // Update our environment with the feedback
        if accepted {
            trace!("Accepted post feedback for location_id={} encoded_post_feedback={:?}", location_id, request.encoded_post_feedback);
            self.environment.put_post_feedback_if_more_powerful(time_millis, &location_id, &request.encoded_post_feedback)?;
        }

        let response = SubmitPostFeedbackResponseV1 { peers_nearer, accepted };
        Ok((PayloadResponseKind::SubmitPostFeedbackResponseV1, BytesGatherer::from_bytes(json::struct_to_bytes(&response)?)))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_HealPostBundleClaimV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, mut bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::HealPostBundleClaimV1, &payload_request_kind);

        fn generate_negatory_response() -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
            let response = HealPostBundleClaimResponseV1 { needed_post_ids: vec![], token: None };
            Ok((PayloadResponseKind::HealPostBundleClaimResponseV1, BytesGatherer::from_bytes(json::struct_to_bytes(&response)?)))
        }

        let time_millis = self.runtime_services.time_provider.as_ref().current_time_millis();
        let request = HealPostBundleClaimV1::from_bytes(&mut bytes)?;
        trace!("received HealPostBundleClaimV1");

        // Verify the bucket_location is internally consistent and maps to the donor_header's location_id
        request.bucket_location.validate()?;
        if request.bucket_location.location_id != request.donor_header.location_id {
            anyhow::bail!("HealPostBundleClaimV1: bucket_location.location_id does not match donor_header.location_id");
        }

        // Verify the donor_header provided by the client is self-consistent and properly signed
        request.donor_header.verify()?;

        // Only heal if we are among the nearest peers for this location
        let (_, among_peers_nearer) = self.kademlia.read().get_peers_for_key(&request.donor_header.location_id, config::REDUNDANT_SERVERS_PER_POST);
        if !among_peers_nearer {
            return generate_negatory_response();
        }

        // Reject if a heal for this location is already in progress (another client beat us to it)
        if self.heal_in_progress.contains_key(&request.donor_header.location_id) {
            return generate_negatory_response();
        }

        // Load our current bundle (header only) to see what post_ids we already have
        let _lock = self.environment.get_read_lock_for_location_id(&request.donor_header.location_id);
        let post_bundle_bytes = self.environment.get_post_bundle_bytes(time_millis, &request.donor_header.location_id)?;

        let our_post_ids: HashSet<Id> = match post_bundle_bytes {
            Some(bytes) => {
                let bundle = EncodedPostBundleV1::from_bytes(Bytes::from_owner(bytes), false)?;
                bundle.header.encoded_post_ids.into_iter().collect()
            }
            None => HashSet::new(),
        };

        // Posts in the donor_header that we do not yet have, in canonical order
        let needed_post_ids: Vec<Id> = request.donor_header.encoded_post_ids.iter().filter(|id| !our_post_ids.contains(*id)).copied().collect();

        if needed_post_ids.is_empty() {
            return generate_negatory_response();
        }

        self.heal_in_progress.insert(request.donor_header.location_id, ());

        let token = Some(HealPostBundleClaimTokenV1::new(
            self.peer_self.read().clone(),
            request.bucket_location,
            needed_post_ids.clone(),
            request.donor_header.signature,
            &self.server_id.keys.signature_key,
        ));
        let response = HealPostBundleClaimResponseV1 { needed_post_ids, token };
        Ok((PayloadResponseKind::HealPostBundleClaimResponseV1, BytesGatherer::from_bytes(json::struct_to_bytes(&response)?)))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_HealPostBundleCommitV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, mut bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::HealPostBundleCommitV1, &payload_request_kind);

        let time_millis = self.runtime_services.time_provider.as_ref().current_time_millis();
        let request = HealPostBundleCommitV1::from_bytes(&mut bytes)?;
        trace!("received HealPostBundleCommitV1");

        // Verify the token was issued by this server
        let peer_self = self.peer_self.read().clone();
        if request.token.peer.id != peer_self.id {
            anyhow::bail!("HealPostBundleCommitV1: token was not issued by this server");
        }
        request.token.verify()?;

        // Verify the donor_header matches what the token was issued for
        if request.donor_header.signature != request.token.donor_header_signature {
            anyhow::bail!("HealPostBundleCommitV1: donor_header signature does not match token");
        }
        request.donor_header.verify()?;

        if request.token.bucket_location.location_id != request.donor_header.location_id {
            anyhow::bail!("HealPostBundleCommitV1: token location_id does not match donor_header");
        }

        let location_id = request.donor_header.location_id;

        // Parse the post bytes supplied by the client (one entry per token.needed_post_id, in order)
        let mut remaining_bytes = request.encoded_posts_bytes.clone();
        let mut posts_to_add: Vec<(Id, Bytes)> = Vec::new();
        for post_id in &request.token.needed_post_ids {
            let len = request
                .donor_header
                .encoded_post_ids
                .iter()
                .zip(request.donor_header.encoded_post_lengths.iter())
                .find(|(id, _)| *id == post_id)
                .map(|(_, len)| *len)
                .ok_or_else(|| anyhow::anyhow!("needed_post_id {} not found in donor_header", post_id))?;
            if remaining_bytes.len() < len {
                anyhow::bail!("HealPostBundleCommitV1: not enough bytes for post {}", post_id);
            }
            let post_bytes = remaining_bytes.split_to(len);
            posts_to_add.push((*post_id, post_bytes));
        }
        if !remaining_bytes.is_empty() {
            anyhow::bail!("HealPostBundleCommitV1: {} excess bytes", remaining_bytes.len());
        }

        // Validate each post decrypts successfully with the provided base_id
        for (post_id, post_bytes) in &posts_to_add {
            EncodedPostV1::decode_from_bytes(post_bytes.clone(), &request.token.bucket_location.base_id, true, true).map_err(|e| anyhow::anyhow!("HealPostBundleCommitV1: post {} failed decryption: {}", post_id, e))?;
        }

        // Load our current bundle (or start empty)
        let _lock = self.environment.get_write_lock_for_location_id(&location_id);
        let post_bundle_bytes = self.environment.get_post_bundle_bytes(time_millis, &location_id)?;
        let post_bundle_metadata = self.environment.get_post_bundle_metadata(time_millis, &location_id)?;
        let mut post_bundle_metadata = post_bundle_metadata.unwrap_or_else(PostBundleMetadata::zero);

        let mut post_bundle = match post_bundle_bytes {
            Some(b) => EncodedPostBundleV1::from_bytes(Bytes::from_owner(b), true)?,
            None => EncodedPostBundleV1 {
                header: EncodedPostBundleHeaderV1 {
                    time_millis: TimeMillis::zero(),
                    location_id,
                    overflowed: request.donor_header.overflowed,
                    sealed: request.donor_header.sealed,
                    num_posts: 0,
                    encoded_post_ids: vec![],
                    encoded_post_lengths: vec![],
                    encoded_post_healed: HashSet::new(),
                    peer: peer_self.clone(),
                    signature: Signature::zero(),
                },
                encoded_posts_bytes: Bytes::new(),
            },
        };

        let our_post_ids: HashSet<Id> = post_bundle.header.encoded_post_ids.iter().copied().collect();

        let mut posts_mut = BytesMut::from(post_bundle.encoded_posts_bytes.as_ref());
        let mut added_any = false;
        for (post_id, post_bytes) in posts_to_add {
            if !our_post_ids.contains(&post_id) {
                let len = post_bytes.len();
                posts_mut.extend_from_slice(&post_bytes);
                post_bundle.header.encoded_post_ids.push(post_id);
                post_bundle.header.encoded_post_lengths.push(len);
                post_bundle.header.encoded_post_healed.insert(post_id);
                added_any = true;
            }
        }
        post_bundle.encoded_posts_bytes = posts_mut.freeze();

        if added_any {
            post_bundle.header.time_millis = time_millis;
            post_bundle.header.num_posts = post_bundle.header.encoded_post_ids.len() as u8;
            post_bundle.header.signature_generate(&self.server_id.keys.signature_key)?;

            let new_bytes = post_bundle.to_bytes()?;
            post_bundle_metadata.num_posts = post_bundle.header.num_posts;
            post_bundle_metadata.size = post_bundle.header.encoded_post_lengths.iter().sum();

            self.environment.put_post_bundle_bytes(time_millis, &location_id, &new_bytes)?;
            self.environment.put_post_bundle_metadata(time_millis, &location_id, &post_bundle_metadata, 0)?;

            info!("Healed {} post(s) for location_id={}", post_bundle.header.encoded_post_healed.len(), location_id);
        }

        self.heal_in_progress.invalidate(&location_id);

        let response = HealPostBundleCommitResponseV1 { accepted: added_any };
        Ok((PayloadResponseKind::HealPostBundleCommitResponseV1, BytesGatherer::from_bytes(json::struct_to_bytes(&response)?)))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_HealPostBundleFeedbackV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, mut bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::HealPostBundleFeedbackV1, &payload_request_kind);

        let time_millis = self.runtime_services.time_provider.as_ref().current_time_millis();
        let request = HealPostBundleFeedbackV1::from_bytes(&mut bytes)?;
        trace!("received HealPostBundleFeedbackV1 for location_id={}", request.location_id);

        // Only accept if we are among the nearest peers for this location
        let (_, among_peers_nearer) = self.kademlia.read().get_peers_for_key(&request.location_id, config::REDUNDANT_SERVERS_PER_POST);
        if !among_peers_nearer {
            let response = HealPostBundleFeedbackResponseV1 { accepted_count: 0 };
            return Ok((PayloadResponseKind::HealPostBundleFeedbackResponseV1, BytesGatherer::from_bytes(json::struct_to_bytes(&response)?)));
        }

        // Check we actually have a post bundle for this location (same guard as SubmitPostFeedbackV1)
        let _post_bundle_lock = self.environment.get_read_lock_for_location_id(&request.location_id);
        let post_bundle_bytes = self.environment.get_post_bundle_bytes(time_millis, &request.location_id)?;
        let Some(post_bundle_bytes) = post_bundle_bytes
        else {
            let response = HealPostBundleFeedbackResponseV1 { accepted_count: 0 };
            return Ok((PayloadResponseKind::HealPostBundleFeedbackResponseV1, BytesGatherer::from_bytes(json::struct_to_bytes(&response)?)));
        };
        let post_bundle = EncodedPostBundleV1::from_bytes(Bytes::from_owner(post_bundle_bytes), false)?;

        let mut accepted_count: u32 = 0;
        for feedback in &request.encoded_post_feedbacks {
            // Only store feedback for posts we actually hold
            if !post_bundle.header.encoded_post_ids.contains(&feedback.post_id) {
                continue;
            }
            self.environment.put_post_feedback_if_more_powerful(time_millis, &request.location_id, feedback)?;
            accepted_count += 1;
        }

        if accepted_count > 0 {
            trace!("Accepted {} healed feedback(s) for location_id={}", accepted_count, request.location_id);
        }

        let response = HealPostBundleFeedbackResponseV1 { accepted_count };
        Ok((PayloadResponseKind::HealPostBundleFeedbackResponseV1, BytesGatherer::from_bytes(json::struct_to_bytes(&response)?)))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_CachePostBundleV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, mut bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::CachePostBundleV1, &payload_request_kind);

        let time_millis = self.runtime_services.time_provider.as_ref().current_time_millis();
        let request = CachePostBundleV1::from_bytes(&mut bytes)?;
        trace!("received CachePostBundleV1 for bucket_location={}", request.token.bucket_location);

        // Verify token was issued by this server and has not expired
        let peer_self = self.peer_self.read().clone();
        if request.token.peer.id != peer_self.id {
            anyhow::bail!("CachePostBundleV1: token was not issued by this server");
        }
        request.token.verify()?;
        if request.token.is_expired(time_millis) {
            anyhow::bail!("CachePostBundleV1: token has expired");
        }

        let mut any_accepted = false;
        for bundle_bytes in request.encoded_post_bundles {
            let parse_result: anyhow::Result<()> = try {
                let encoded_post_bundle = EncodedPostBundleV1::from_bytes(bundle_bytes.clone(), true)?;

                // Check that this proposed cache content is legitimate
                encoded_post_bundle.verify(&request.token.bucket_location.base_id)?;

                let originator_peer_id = encoded_post_bundle.header.peer.id;
                let is_sealed = encoded_post_bundle.header.sealed;
                if self.post_bundle_cache.on_upload(request.token.bucket_location.location_id, originator_peer_id, bundle_bytes, time_millis, is_sealed) {
                    any_accepted = true;
                }
            };
            if let Err(e) = &parse_result {
                warn!("CachePostBundleV1: failed to parse bundle: {}", e);
            }
        }
        let response = CachePostBundleResponseV1 { accepted: any_accepted };
        Ok((PayloadResponseKind::CachePostBundleResponseV1, BytesGatherer::from_bytes(json::struct_to_bytes(&response)?)))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_CachePostBundleFeedbackV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, mut bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::CachePostBundleFeedbackV1, &payload_request_kind);

        let time_millis = self.runtime_services.time_provider.as_ref().current_time_millis();
        let request = CachePostBundleFeedbackV1::from_bytes(&mut bytes)?;
        trace!("received CachePostBundleFeedbackV1 for bucket_location={}", request.token.bucket_location);

        // Verify token was issued by this server and has not expired
        let peer_self = self.peer_self.read().clone();
        if request.token.peer.id != peer_self.id {
            anyhow::bail!("CachePostBundleFeedbackV1: token was not issued by this server");
        }
        request.token.verify()?;
        if request.token.is_expired(time_millis) {
            anyhow::bail!("CachePostBundleFeedbackV1: token has expired");
        }

        let result: anyhow::Result<bool> = try {
            let encoded_post_bundle_feedback = EncodedPostBundleFeedbackV1::from_bytes(request.encoded_post_bundle_feedback_bytes.clone())?;

            encoded_post_bundle_feedback.verify()?;

            let originator_peer_id = encoded_post_bundle_feedback.header.peer.id;
            // Feedback bundles have no sealed flag — treat as live (5-min TTL)
            self.post_bundle_feedback_cache
                .on_upload(request.token.bucket_location.location_id, originator_peer_id, request.encoded_post_bundle_feedback_bytes, time_millis, false)
        };
        let accepted = result.unwrap_or_else(|e| {
            warn!("CachePostBundleFeedbackV1: parse error: {}", e);
            false
        });

        let response = CachePostBundleFeedbackResponseV1 { accepted };
        Ok((PayloadResponseKind::CachePostBundleFeedbackResponseV1, BytesGatherer::from_bytes(json::struct_to_bytes(&response)?)))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_FetchUrlPreviewV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, mut bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::FetchUrlPreviewV1, &payload_request_kind);

        let request = FetchUrlPreviewV1::from_bytes(&mut bytes)?;
        trace!("received FetchUrlPreviewV1 for url={}", request.url);

        // SSRF protection:
        // https:// only — blocks plaintext metadata endpoints and forces TLS cert validation.
        if !request.url.starts_with("https://") {
            anyhow::bail!("FetchUrlPreviewV1 SSRF: only https:// URLs are allowed");
        }

        // Extract host; reject bare IP literals (IPv4 and IPv6 bracket form).
        let host_and_port = request.url["https://".len()..].split(&['/', '?', '#'][..]).next().unwrap_or("");
        let host = if host_and_port.starts_with('[') {
            // IPv6 literal [addr] or [addr]:port
            host_and_port.trim_start_matches('[').split(']').next().unwrap_or("")
        } else {
            // hostname or IPv4 — strip optional port
            host_and_port.split(':').next().unwrap_or(host_and_port)
        };
        if host.is_empty() {
            anyhow::bail!("FetchUrlPreviewV1 SSRF: could not extract host from URL");
        }
        if host.parse::<std::net::IpAddr>().is_ok() {
            anyhow::bail!("FetchUrlPreviewV1 SSRF: bare IP addresses are not allowed");
        }

        // Resolve once and validate every returned address.  Collecting into a Vec lets us
        // re-use the same addresses to pin the reqwest client, closing the DNS-rebinding window
        // (nip.io, metadata.google.internal, TTL=0 rebind, etc.).
        let resolved_socket_addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, 443u16))
            .await
            .map_err(|e| anyhow::anyhow!("FetchUrlPreviewV1 SSRF: DNS resolution failed for {}: {}", host, e))?
            .collect();
        if resolved_socket_addrs.is_empty() {
            anyhow::bail!("FetchUrlPreviewV1 SSRF: DNS returned no addresses for {}", host);
        }
        for socket_addr in &resolved_socket_addrs {
            let ip = socket_addr.ip();
            if is_ssrf_protected_ip(ip) {
                anyhow::bail!("FetchUrlPreviewV1 SSRF: {} resolved to protected address {}", host, ip);
            }
        }

        // Build a client that:
        //   - resolve_to_addrs: skips re-resolution entirely — the validated IPs are used directly
        //   - redirect::none: prevents a server-side redirect to an unvalidated internal URL
        //   - no_proxy: ignores HTTP_PROXY / NO_PROXY env vars that could route to internal hosts
        //   - times out quickly in unreasonable scenarios
        //   - limits download size
        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(1))
            .timeout(std::time::Duration::from_secs(3))
            .user_agent("hashiverse-preview/1.0")
            .resolve_to_addrs(host, &resolved_socket_addrs)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()?;

        const URL_FETCH_MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
        let mut http_response = http_client.get(&request.url).send().await?;

        // Reject early if Content-Length already exceeds the limit — before reading any body bytes.
        if let Some(content_length) = http_response.content_length() {
            if content_length > URL_FETCH_MAX_BODY_BYTES as u64 {
                anyhow::bail!("FetchUrlPreviewV1: Content-Length {} exceeds {} byte limit", content_length, URL_FETCH_MAX_BODY_BYTES);
            }
        }
        let mut body_bytes = BytesMut::new();
        while let Some(chunk) = http_response.chunk().await? {
            // body_bytes.len() is already within the limit; only the new chunk can overflow.
            // We copy only as many bytes as fit rather than bailing, so a single large chunk
            // (which reqwest has already buffered) doesn't cause an error — we just truncate.
            // This is sufficient for HTML preview extraction which only needs the <head>.
            let remaining = URL_FETCH_MAX_BODY_BYTES - body_bytes.len();
            body_bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
            if body_bytes.len() >= URL_FETCH_MAX_BODY_BYTES {
                break;
            }
        }
        let html = String::from_utf8_lossy(&body_bytes).into_owned();

        let preview_data = url_preview::extract_url_preview(&html);

        let response = FetchUrlPreviewResponseV1 {
            url: if preview_data.canonical_url.is_empty() { request.url } else { preview_data.canonical_url },
            title: preview_data.title,
            description: preview_data.description,
            image_url: preview_data.image_url,
        };

        Ok((PayloadResponseKind::FetchUrlPreviewResponseV1, BytesGatherer::from_bytes(response.to_bytes()?)))
    }

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_TrendingHashtagsFetchV1(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, mut bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::TrendingHashtagsFetchV1, &payload_request_kind);

        let request = TrendingHashtagsFetchV1::from_bytes(&mut bytes)?;
        trace!("received TrendingHashtagsFetchV1 with limit={}", request.limit);

        let time_millis = self.runtime_services.time_provider.current_time_millis();

        // Check if we have a cached response that is less than a few minutes old
        let cached_response = {
            let cache = self.trending_hashtags_response_cache.lock();
            match cache.as_ref() {
                Some((cached_time, cached_response)) if (time_millis - *cached_time) < MILLIS_IN_SECOND.const_mul(30) => {
                    Some(cached_response.clone())
                }
                _ => None,
            }
        };

        let mut response = match cached_response {
            Some(mut cached) => {
                cached.trending_hashtags.truncate(request.limit as usize);
                cached
            }
            None => {
                // Recalculate: iterate the trending_hashtags cache, sort by unique author count
                let mut trending_hashtags: Vec<TrendingHashtagV1> = self.trending_hashtags.iter()
                    .map(|(hashtag, hll)| TrendingHashtagV1 {
                        hashtag: hashtag.as_ref().clone(),
                        count: hll.count(),
                    })
                    .filter(|entry| entry.count > 0)
                    .collect();

                trending_hashtags.sort_by(|a, b| b.count.cmp(&a.count));

                let full_response = TrendingHashtagsFetchResponseV1 { trending_hashtags };

                // Cache the full response (real trending only — fallbacks are applied per-response after truncation)
                {
                    let mut cache = self.trending_hashtags_response_cache.lock();
                    *cache = Some((time_millis, full_response.clone()));
                }

                let mut truncated_response = full_response;
                truncated_response.trending_hashtags.truncate(request.limit as usize);
                truncated_response
            }
        };

        top_up_trending_hashtags_with_fallback(&mut response.trending_hashtags, request.limit, TRENDING_HASHTAGS_FALLBACK);

        Ok((PayloadResponseKind::TrendingHashtagsFetchResponseV1, BytesGatherer::from_bytes(response.to_bytes()?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trending_hashtag(hashtag: &str, count: u64) -> TrendingHashtagV1 {
        TrendingHashtagV1 { hashtag: hashtag.to_string(), count }
    }

    #[test]
    fn top_up_adds_fallback_when_empty() {
        let mut trending_hashtags: Vec<TrendingHashtagV1> = vec![];
        top_up_trending_hashtags_with_fallback(&mut trending_hashtags, 5, &["#hashiverse", "#news"]);
        assert_eq!(trending_hashtags.len(), 2);
        assert_eq!(trending_hashtags[0].hashtag, "#hashiverse");
        assert_eq!(trending_hashtags[0].count, 0);
        assert_eq!(trending_hashtags[1].hashtag, "#news");
        assert_eq!(trending_hashtags[1].count, 0);
    }

    #[test]
    fn top_up_respects_limit_smaller_than_fallback_list() {
        let mut trending_hashtags: Vec<TrendingHashtagV1> = vec![];
        top_up_trending_hashtags_with_fallback(&mut trending_hashtags, 1, &["#hashiverse", "#news"]);
        assert_eq!(trending_hashtags.len(), 1);
        assert_eq!(trending_hashtags[0].hashtag, "#hashiverse");
    }

    #[test]
    fn top_up_preserves_fallback_order() {
        let mut trending_hashtags: Vec<TrendingHashtagV1> = vec![];
        top_up_trending_hashtags_with_fallback(&mut trending_hashtags, 3, &["#first", "#second", "#third"]);
        assert_eq!(trending_hashtags[0].hashtag, "#first");
        assert_eq!(trending_hashtags[1].hashtag, "#second");
        assert_eq!(trending_hashtags[2].hashtag, "#third");
    }

    #[test]
    fn top_up_is_noop_when_already_at_limit() {
        let mut trending_hashtags = vec![make_trending_hashtag("#rust", 10), make_trending_hashtag("#golang", 5)];
        top_up_trending_hashtags_with_fallback(&mut trending_hashtags, 2, &["#hashiverse", "#news"]);
        assert_eq!(trending_hashtags.len(), 2);
        assert_eq!(trending_hashtags[0].hashtag, "#rust");
        assert_eq!(trending_hashtags[1].hashtag, "#golang");
    }

    #[test]
    fn top_up_partially_fills_when_real_trending_exists() {
        let mut trending_hashtags = vec![make_trending_hashtag("#rust", 10)];
        top_up_trending_hashtags_with_fallback(&mut trending_hashtags, 3, &["#hashiverse", "#news"]);
        assert_eq!(trending_hashtags.len(), 3);
        assert_eq!(trending_hashtags[0].hashtag, "#rust");
        assert_eq!(trending_hashtags[0].count, 10);
        assert_eq!(trending_hashtags[1].hashtag, "#hashiverse");
        assert_eq!(trending_hashtags[1].count, 0);
        assert_eq!(trending_hashtags[2].hashtag, "#news");
        assert_eq!(trending_hashtags[2].count, 0);
    }

    #[test]
    fn top_up_skips_fallback_already_present_exact_match() {
        let mut trending_hashtags = vec![make_trending_hashtag("#hashiverse", 42)];
        top_up_trending_hashtags_with_fallback(&mut trending_hashtags, 3, &["#hashiverse", "#news"]);
        assert_eq!(trending_hashtags.len(), 2);
        assert_eq!(trending_hashtags[0].hashtag, "#hashiverse");
        assert_eq!(trending_hashtags[0].count, 42, "real trending entry must not be overwritten by filler");
        assert_eq!(trending_hashtags[1].hashtag, "#news");
        assert_eq!(trending_hashtags[1].count, 0);
    }

    #[test]
    fn top_up_dedup_is_case_insensitive_and_prefix_agnostic() {
        // Existing entry "HashiVerse" (no `#`, mixed case) should match fallback "#hashiverse"
        let mut trending_hashtags = vec![make_trending_hashtag("HashiVerse", 7)];
        top_up_trending_hashtags_with_fallback(&mut trending_hashtags, 3, &["#hashiverse", "#news"]);
        assert_eq!(trending_hashtags.len(), 2);
        assert_eq!(trending_hashtags[0].hashtag, "HashiVerse");
        assert_eq!(trending_hashtags[1].hashtag, "#news");
    }

    #[test]
    fn top_up_with_empty_fallback_is_noop() {
        let mut trending_hashtags: Vec<TrendingHashtagV1> = vec![];
        top_up_trending_hashtags_with_fallback(&mut trending_hashtags, 5, &[]);
        assert_eq!(trending_hashtags.len(), 0);
    }

    #[test]
    fn top_up_handles_zero_limit() {
        let mut trending_hashtags: Vec<TrendingHashtagV1> = vec![];
        top_up_trending_hashtags_with_fallback(&mut trending_hashtags, 0, &["#hashiverse", "#news"]);
        assert_eq!(trending_hashtags.len(), 0);
    }

    #[test]
    fn top_up_exhausts_fallback_without_reaching_limit() {
        // Limit is larger than what real trending + fallback together can satisfy
        let mut trending_hashtags: Vec<TrendingHashtagV1> = vec![];
        top_up_trending_hashtags_with_fallback(&mut trending_hashtags, 10, &["#hashiverse", "#news"]);
        assert_eq!(trending_hashtags.len(), 2, "should stop at the end of the fallback list, not pad further");
    }
}

