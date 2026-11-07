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
    AnnounceResponseV2, AnnounceV2, BootstrapResponseV1, CachePostBundleFeedbackResponseV1, CachePostBundleFeedbackV1, CachePostBundleResponseV1, CachePostBundleV1, ErrorResponseV1, FetchUrlPreviewResponseV1, FetchUrlPreviewV1,
    GetPostBundleFeedbackResponseV1, GetPostBundleFeedbackV1, GetPostBundleResponseV1, GetPostBundleV1, HealPostBundleClaimResponseV1, HealPostBundleClaimTokenV1, HealPostBundleClaimV1, HealPostBundleCommitResponseV1, HealPostBundleCommitV1,
    HealPostBundleFeedbackResponseV1, HealPostBundleFeedbackV1, PayloadRequestKind, PayloadResponseKind, PingResponseV1, PeerStatsRequestV1, PeerStatsResponseV1, SubmitPostClaimResponseV1, SubmitPostClaimTokenV1, SubmitPostClaimV1,
    SubmitPostCommitResponseV1, SubmitPostCommitTokenV1, SubmitPostCommitV1, SubmitPostFeedbackResponseV1, SubmitPostFeedbackV1, TrendingHashtagV1, TrendingHashtagsFetchResponseV1, TrendingHashtagsFetchV1,
};
use hashiverse_lib::protocol::peer::{Peer, PeerPow};
use hashiverse_lib::protocol::posting::amplification::get_minimum_post_pow;
use hashiverse_lib::protocol::posting::encoded_post::EncodedPostV1;
use hashiverse_lib::protocol::posting::encoded_post_bundle::{EncodedPostBundleHeaderV1, EncodedPostBundleV1};
use hashiverse_lib::protocol::posting::encoded_post_bundle_feedback::{EncodedPostBundleFeedbackHeaderV1, EncodedPostBundleFeedbackV1};
use hashiverse_lib::protocol::rpc::rpc_request::RpcRequestPacketRx;
use hashiverse_lib::protocol::rpc::rpc_response::{RpcResponsePacketTx, RpcResponsePacketTxFlags};
use hashiverse_lib::tools::buckets::{BucketLocation, BucketType, BUCKET_DURATIONS};
use hashiverse_lib::tools::time::{TimeMillis, MILLIS_IN_SECOND};
use hashiverse_lib::tools::types::{Id, Signature};
use hashiverse_lib::tools::{hashing, url_preview};
use hashiverse_lib::tools::{compression, config, json, signing, BytesGatherer};
use hashiverse_lib::tools::tools::{self, LeadingAgreementBits};
use hashiverse_lib::transport::transport::IncomingRequest;
use futures::FutureExt;
use log::{error, info, trace, warn};
use std::collections::HashSet;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::Ordering;

use crate::server::stats::{environment_stats_subtree, kademlia_stats_subtree, request_counts_subtree, system_stats_subtree};
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
/// Is `donor_id` close enough to `location_id` that it plausibly is (or was) one of the servers responsible for that location?
///
/// `nearest_peer_ids` is our own view of the nearest servers to the location (as returned by `get_peers_for_key(location_id, required_count)`,
/// which may include ties beyond `required_count`).  The donor must be no more than `HEAL_DONOR_LEADING_AGREEMENT_BITS_SLACK` bits further out
/// than the furthest of those.  Deliberately a distance test rather than "is the donor in our view": the donor may have died long ago.
/// If we do not yet know `required_count` servers we cannot rule anyone out.
fn is_plausible_heal_donor(donor_id: &Id, location_id: &Id, nearest_peer_ids: &[Id], required_count: usize) -> bool {
    if nearest_peer_ids.len() < required_count {
        return true;
    }

    let furthest_nearest_leading_agreement_bits = nearest_peer_ids
        .iter()
        .map(|nearest_peer_id| tools::leading_agreement_bits_xor(location_id.as_ref(), nearest_peer_id.as_ref()))
        .min()
        .unwrap_or(LeadingAgreementBits::MIN);
    let donor_leading_agreement_bits = tools::leading_agreement_bits_xor(location_id.as_ref(), donor_id.as_ref());

    donor_leading_agreement_bits >= furthest_nearest_leading_agreement_bits - config::HEAL_DONOR_LEADING_AGREEMENT_BITS_SLACK
}

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

/// Awaits a DNS lookup for a URL preview host, giving up after `dns_timeout`.  The dispatch loop is serial, so an
/// unbounded lookup here would stall every other request.  `lookup` is injected so the timeout can be tested without a network.
async fn resolve_url_preview_host<I>(host: &str, dns_timeout: std::time::Duration, lookup: impl Future<Output = std::io::Result<I>>) -> anyhow::Result<Vec<std::net::SocketAddr>>
where
    I: Iterator<Item = std::net::SocketAddr>,
{
    match tokio::time::timeout(dns_timeout, lookup).await {
        Err(_) => anyhow::bail!("FetchUrlPreviewV1: DNS resolution timed out for {}", host),
        Ok(Err(e)) => anyhow::bail!("FetchUrlPreviewV1 SSRF: DNS resolution failed for {}: {}", host, e),
        Ok(Ok(socket_addrs)) => Ok(socket_addrs.collect()),
    }
}

/// Awaits `future`, converting a panic inside it into an `Err` so that a single bad
/// request cannot unwind through the dispatch loop and stop the server.
async fn catch_panic_as_error<T>(future: impl Future<Output = T>) -> anyhow::Result<T> {
    match AssertUnwindSafe(future).catch_unwind().await {
        Ok(value) => Ok(value),
        Err(panic_payload) => {
            let panic_message = if let Some(panic_message) = panic_payload.downcast_ref::<&str>() {
                panic_message.to_string()
            } else if let Some(panic_message) = panic_payload.downcast_ref::<String>() {
                panic_message.clone()
            } else {
                "<non-string panic payload>".to_string()
            };
            Err(anyhow::anyhow!("panic: {}", panic_message))
        }
    }
}

/// Replies to (or abandons) `incoming` according to how dispatching it went.
/// The outer `Err` is a handler panic (see [`catch_panic_as_error`]); it is scored as a bad request just like
/// an ordinary `Err`, so a caller who has found a panic can't keep triggering it without being DDoS-scored.
fn complete_incoming_request(incoming: IncomingRequest, dispatch_outcome: anyhow::Result<anyhow::Result<BytesGatherer>>) {
    match dispatch_outcome {
        Ok(Ok(bytes)) => {
            let result = incoming.reply.send(bytes);
            if result.is_err() { warn!("failed to send reply"); }
        },
        Err(handler_panic) => {
            error!("handler panicked processing packet from {}: {}", incoming.caller_address, handler_panic);
            incoming.report_bad_request();
            drop(incoming.reply);
        },
        Ok(Err(e)) => {
            warn!("failed to process packet from {}: {}", incoming.caller_address, e);
            incoming.report_bad_request();
            drop(incoming.reply);
        },
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
                            // A panic in any handler must not unwind out of this loop: it runs inside the server's
                            // try_join! and would take the whole server down with it.
                            let dispatch_outcome = catch_panic_as_error(self.wrap_and_dispatch_network_envelope(cancellation_token.clone(), &incoming)).await;
                            complete_incoming_request(incoming, dispatch_outcome);
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

        // Count this inbound request. After decoding (so malformed traffic doesn't
        // pollute the totals) but before the per-handler PoW check (so failed-PoW
        // attempts still show up — adversarial load is the load we'd want to see).
        let payload_request_kind_index = rpc_request_packet_rx.payload_request_kind.clone() as usize;
        self.request_counters[payload_request_kind_index].fetch_add(1, Ordering::Relaxed);
        // Also feed the decaying per-window rate estimates so the stats reflect
        // recent load, not just the monotonic all-time total.
        self.request_rate_windows[payload_request_kind_index].lock().record(current_time_millis);

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
            PayloadRequestKind::RetiredAnnounceV1 => {
                anyhow::bail!("AnnounceV1 is retired; use AnnounceV2");
            }
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
            PayloadRequestKind::FetchUrlPreviewV1 => self.dispatch_network_payload_x_FetchUrlPreviewV1(cancellation_token, pow, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await?,
            PayloadRequestKind::TrendingHashtagsFetchV1 => self.dispatch_network_payload_x_TrendingHashtagsFetchV1(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await?,
            PayloadRequestKind::PeerStatsRequestV1 => self.dispatch_network_payload_x_PeerStatsRequestV1(cancellation_token, pow, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await?,
            PayloadRequestKind::AnnounceV2 => self.dispatch_network_payload_x_AnnounceV2(cancellation_token, rpc_request_packet_rx.payload_request_kind, rpc_request_packet_rx.bytes).await?,
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
    async fn dispatch_network_payload_x_AnnounceV2(&self, _cancellation_token: CancellationToken, payload_request_kind: PayloadRequestKind, bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::AnnounceV2, &payload_request_kind);

        let request = json::bytes_to_struct::<AnnounceV2>(&bytes)?;

        let peer = request.peer_self;
        let peer_id = peer.id;
        let now = self.runtime_services.time_provider.as_ref().current_time_millis();

        // Gate Kademlia admission on the transport-specific ownership proof. For HTTPS that's
        // an offline X.509 chain validation against bundled webpki-roots with an IP-SAN match;
        // for mem / TCP it's the empty-marker check. A failed proof is a hard error: no
        // Kademlia add, no nearest-peers list returned. The dispatcher's Err -> ErrorResponseV1
        // wrapper also reports the request as bad to the DDoS layer, so repeat offenders get
        // throttled by per-IP scoring.
        let ownership_proof = self.transport_server.get_transport_ownership_proof();
        if !ownership_proof.prove(&peer, &request.proof_payload, now) {
            anyhow::bail!("AnnounceV2 from peer {} at {} failed ownership proof", peer_id, peer.address);
        }
        self.add_potential_peer_to_kademlia(peer, now).await;

        let (peers_nearest, _) = self.kademlia.read().get_peers_for_key(&peer_id, config::ANNOUNCE_NUM_PEERS);

        let json = json::struct_to_bytes(&AnnounceResponseV2 {
            peer_self: self.peer_self.read().clone(),
            peers_nearest,
        })?;
        Ok((PayloadResponseKind::AnnounceResponseV2, BytesGatherer::from_bytes(json)))
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
                        let sealed = request.bucket_location.is_sealed_at(time_millis, false);
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
                    let sealed = request.bucket_location.is_sealed_at(time_millis, false);

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
            let pow_minimum = get_minimum_post_pow(decoded_post.header.post_length, decoded_post.header.linked_base_ids.len(), request.bucket_location.duration)?;
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
                    post_bundle_metadata.sealed = request.bucket_location.is_sealed_at(time_millis, post_bundle_metadata.overflowed);

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
                    let mut hll = self.trending_hashtags.get(referenced_hashtag).unwrap_or_default();
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
                
                EncodedPostBundleV1::from_bytes(bytes, true)?
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
        post_bundle.header.sealed = request.bucket_location.is_sealed_at(time_millis, post_bundle.header.overflowed);
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

    /// A heal lets a client push posts into our bundle that we never admitted ourselves (and so never saw the post PoW for).
    /// We only accept that on the word of a donor bundle published by a genuine server that is - or was, it may since have died -
    /// plausibly responsible for this location: that server admitted the posts, or itself healed them from such a server.
    fn verify_heal_donor_header(&self, donor_header: &EncodedPostBundleHeaderV1, time_millis: TimeMillis) -> anyhow::Result<()> {
        donor_header.verify()?;
        donor_header.peer.verify().map_err(|e| anyhow::anyhow!("heal donor peer {} failed verification: {}", donor_header.peer.id, e))?;

        // The donor's timestamp bounds the post timestamps we will accept, so it must not be in the future
        if donor_header.time_millis > time_millis + config::CLIENT_POST_TIMESTAMP_DELTA_THRESHOLD {
            anyhow::bail!("heal donor header time_millis {} is in the future (now {})", donor_header.time_millis, time_millis);
        }

        let (nearest_peers, _) = self.kademlia.read().get_peers_for_key(&donor_header.location_id, config::REDUNDANT_SERVERS_PER_POST);
        let nearest_peer_ids: Vec<Id> = nearest_peers.iter().map(|peer| peer.id).collect();
        if !is_plausible_heal_donor(&donor_header.peer.id, &donor_header.location_id, &nearest_peer_ids, config::REDUNDANT_SERVERS_PER_POST) {
            anyhow::bail!("heal donor peer {} is too far from location_id {}", donor_header.peer.id, donor_header.location_id);
        }

        Ok(())
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

        // Verify the donor_header provided by the client is self-consistent, properly signed, and published by a plausible server
        self.verify_heal_donor_header(&request.donor_header, time_millis)?;

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

        // From here on the token is genuinely ours, so whatever happens we release the claim it was issued under.
        // Keyed on the token's (signed) location, not anything else the client sent, so a forged or foreign token
        // can never release a claim held by someone else. Without this, any failed commit would block healing for
        // this location until the heal_in_progress TTL expired.
        let token_location_id = request.token.bucket_location.location_id;
        let commit_result = self.commit_heal_post_bundle(&request, &peer_self, time_millis);
        self.heal_in_progress.invalidate(&token_location_id);
        let added_any = commit_result?;

        let response = HealPostBundleCommitResponseV1 { accepted: added_any };
        Ok((PayloadResponseKind::HealPostBundleCommitResponseV1, BytesGatherer::from_bytes(json::struct_to_bytes(&response)?)))
    }

    /// The body of a HealPostBundleCommitV1 once its token has been verified as ours. Returns whether any post was added.
    fn commit_heal_post_bundle(&self, request: &HealPostBundleCommitV1, peer_self: &Peer, time_millis: TimeMillis) -> anyhow::Result<bool> {
        // Verify the donor_header matches what the token was issued for
        if request.donor_header.signature != request.token.donor_header_signature {
            anyhow::bail!("HealPostBundleCommitV1: donor_header signature does not match token");
        }
        self.verify_heal_donor_header(&request.donor_header, time_millis)?;

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

        // Validate each post: it must decrypt and verify under the bucket's base_id, be the post it claims to be,
        // and pass the same admission rules we would have applied had it been submitted to us directly.
        // The reference time is the earlier of the donor's time and ours: the donor header may itself be up to the threshold ahead of
        // our clock, and using it alone would let healed posts sit twice the threshold in the future.
        let admission_reference_time_millis = std::cmp::min(request.donor_header.time_millis, time_millis);
        for (post_id, post_bytes) in &posts_to_add {
            let decoded_post = EncodedPostV1::decode_from_bytes(post_bytes.clone(), &request.token.bucket_location.base_id, true, true)
                .map_err(|e| anyhow::anyhow!("HealPostBundleCommitV1: post {} failed decryption: {}", post_id, e))?;
            if decoded_post.post_id != *post_id {
                anyhow::bail!("HealPostBundleCommitV1: post bytes are for {} not {}", decoded_post.post_id, post_id);
            }
            decoded_post
                .check_admissible_to_bucket(&request.token.bucket_location, admission_reference_time_millis)
                .map_err(|e| anyhow::anyhow!("HealPostBundleCommitV1: {}", e))?;
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
                    overflowed: false, // Never adopt the donor's flags - they are recomputed from our own state below
                    sealed: false,
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
            if post_bundle.header.encoded_post_ids.len() >= u8::MAX as usize {
                warn!("HealPostBundleCommitV1: bundle for location_id={} is full so not healing any more posts into it", location_id);
                break;
            }
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
            post_bundle.header.overflowed = post_bundle.header.num_posts > config::ENCODED_POST_BUNDLE_V1_OVERFLOWED_NUM_POSTS || post_bundle_metadata.num_posts_granted > config::ENCODED_POST_BUNDLE_V1_OVERFLOWED_NUM_POSTS_GRANTED;
            post_bundle.header.sealed = request.token.bucket_location.is_sealed_at(time_millis, post_bundle.header.overflowed);
            post_bundle.header.signature_generate(&self.server_id.keys.signature_key)?;

            let new_bytes = post_bundle.to_bytes()?;
            post_bundle_metadata.num_posts = post_bundle.header.num_posts;
            post_bundle_metadata.size = post_bundle.header.encoded_post_lengths.iter().sum();
            post_bundle_metadata.overflowed = post_bundle.header.overflowed;
            post_bundle_metadata.sealed = post_bundle.header.sealed;

            self.environment.put_post_bundle_bytes(time_millis, &location_id, &new_bytes)?;
            self.environment.put_post_bundle_metadata(time_millis, &location_id, &post_bundle_metadata, 0)?;

            info!("Healed {} post(s) for location_id={}", post_bundle.header.encoded_post_healed.len(), location_id);
        }

        Ok(added_any)
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
            // Same check as SubmitPostFeedbackV1: the claimed pow must be what the salt actually achieves,
            // otherwise anyone could plant pow=255 on any (post, feedback_type) with one cheap RPC.
            if let Err(e) = feedback.pow_verify() {
                warn!("Rejecting healed feedback with invalid pow for post_id={} at location_id={}: {}", feedback.post_id, request.location_id, e);
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
                let mut encoded_post_bundle = EncodedPostBundleV1::from_bytes(bundle_bytes.clone(), true)?;

                // Check that this proposed cache content is a genuine server's bundle for this very bucket - otherwise
                // free self-signed (or empty) bundles could squat this location's originator slots.
                encoded_post_bundle.verify_for_bucket(&request.token.bucket_location, time_millis)?;
                if encoded_post_bundle.header.num_posts == 0 {
                    Err(anyhow::anyhow!("empty bundle is not worth caching"))?; // NB: not bail!, which would return from the handler rather than this try block
                }

                // Never trust the publisher's own sealed flag: a sealed bundle is cached with no expiry.
                // NB: only the flag is taken from the normalised copy - we cache the original signed bytes.
                encoded_post_bundle.normalise_flags_for_bucket(&request.token.bucket_location, time_millis);
                let originator_peer_id = encoded_post_bundle.header.peer.id;
                let is_sealed = encoded_post_bundle.header.sealed;
                let bundle_time_millis = encoded_post_bundle.header.time_millis;
                if self.post_bundle_cache.on_upload(request.token.bucket_location.location_id, originator_peer_id, bundle_bytes, bundle_time_millis, time_millis, is_sealed) {
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

            encoded_post_bundle_feedback.verify_for_location(&request.token.bucket_location.location_id)?;

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
    async fn dispatch_network_payload_x_FetchUrlPreviewV1(&self, _cancellation_token: CancellationToken, pow: Option<PeerPow>, payload_request_kind: PayloadRequestKind, mut bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::FetchUrlPreviewV1, &payload_request_kind);

        // Outbound fetches are expensive for us and for the third-party site, so demand the extra pow before doing any work.
        let pow = pow.ok_or_else(|| anyhow::anyhow!("pow required for FetchUrlPreviewV1"))?;
        if pow.pow < config::POW_MINIMUM_PER_URL_FETCH {
            anyhow::bail!("Insufficient pow for FetchUrlPreviewV1: {} < {}", pow.pow, config::POW_MINIMUM_PER_URL_FETCH);
        }

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
        let resolved_socket_addrs: Vec<std::net::SocketAddr> = resolve_url_preview_host(host, config::URL_FETCH_DNS_TIMEOUT_MILLIS.into(), tokio::net::lookup_host((host, 443u16))).await?;
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
            .connect_timeout(config::URL_FETCH_CONNECT_TIMEOUT_MILLIS.into())
            .timeout(config::URL_FETCH_TOTAL_TIMEOUT_MILLIS.into())
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

                trending_hashtags.sort_by_key(|entry| std::cmp::Reverse(entry.count));

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

    #[allow(non_snake_case)]
    async fn dispatch_network_payload_x_PeerStatsRequestV1(&self, _cancellation_token: CancellationToken, pow: Option<PeerPow>, payload_request_kind: PayloadRequestKind, bytes: Bytes) -> anyhow::Result<(PayloadResponseKind, BytesGatherer)> {
        anyhow_assert_eq!(&PayloadRequestKind::PeerStatsRequestV1, &payload_request_kind);

        let _request = PeerStatsRequestV1::from_bytes(&bytes)?;

        let pow = pow.ok_or_else(|| anyhow::anyhow!("pow required for PeerStatsRequestV1"))?;
        if pow.pow < config::POW_MINIMUM_PER_PEER_STATS {
            anyhow::bail!("Insufficient pow for PeerStatsRequestV1: {} < {}", pow.pow, config::POW_MINIMUM_PER_PEER_STATS);
        }

        let time_millis = self.runtime_services.time_provider.current_time_millis();

        // Cache check — hand back the same signed blob within the TTL so callers
        // re-sharing it operate on a single canonical byte sequence per minute.
        let cached_response = {
            let cache = self.peer_stats_response_cache.lock();
            match cache.as_ref() {
                Some((cached_time, cached_response)) if (time_millis - *cached_time) < MILLIS_IN_SECOND.const_mul(60) => Some(cached_response.clone()),
                _ => None,
            }
        };

        let response = match cached_response {
            Some(cached) => cached,
            None => {
                let doc = serde_json::json!({
                    "version":     env!("CARGO_PKG_VERSION"),
                    "requests":    request_counts_subtree(&self.request_counters, &self.request_rate_windows, time_millis),
                    "system":      system_stats_subtree(),
                    "kademlia":    kademlia_stats_subtree(&self.kademlia.read()),
                    "environment": environment_stats_subtree(&self.environment),
                });

                let json_bytes = serde_json::to_vec(&doc)?;
                let json_compressed = compression::compress_for_speed(&json_bytes)?.to_bytes();

                let signing_input = PeerStatsResponseV1::signing_input(time_millis, &json_compressed);
                let signature = signing::sign(&self.server_id.keys.signature_key, &signing_input);

                let response = PeerStatsResponseV1 {
                    peer: self.peer_self.read().clone(),
                    timestamp: time_millis,
                    json_compressed,
                    signature,
                };

                *self.peer_stats_response_cache.lock() = Some((time_millis, response.clone()));
                response
            }
        };

        Ok((PayloadResponseKind::PeerStatsResponseV1, BytesGatherer::from_bytes(response.to_bytes()?)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod complete_incoming_request_outcomes {
        use super::*;
        use hashiverse_lib::transport::ddos::ddos::{DdosConnectionGuard, DdosProtection};
        use hashiverse_lib::transport::ddos::mem_ddos::MemDdosProtection;
        use hashiverse_lib::tools::time_provider::manual_time_provider::ManualTimeProvider;
        use std::sync::Arc;
        use tokio::sync::oneshot;

        const CALLER_IP: &str = "10.1.2.3";

        /// Threshold 1, no decay, penalty 1: a single bad-request report blocks the IP.
        fn make_incoming_request() -> (Arc<MemDdosProtection>, IncomingRequest, oneshot::Receiver<BytesGatherer>) {
            let ddos = Arc::new(MemDdosProtection::new(1.0, 0.0, 1.0, 8, Arc::new(ManualTimeProvider::default())));
            let ddos_connection_guard = DdosConnectionGuard::try_new(ddos.clone(), CALLER_IP).expect("fresh ip must get a connection slot");
            let (reply_tx, reply_rx) = oneshot::channel();
            let incoming = IncomingRequest::new(CALLER_IP.to_string(), Bytes::new(), reply_tx, Arc::new(ddos_connection_guard));
            (ddos, incoming, reply_rx)
        }

        /// Probe the ban state without touching the score (`allow_request` itself adds a point per call).
        fn is_blocked(ddos: &MemDdosProtection) -> bool {
            !ddos.try_acquire_connection(CALLER_IP)
        }

        #[tokio::test]
        async fn panic_outcome_reports_bad_request() {
            let (ddos, incoming, reply_rx) = make_incoming_request();
            assert!(!is_blocked(&ddos));
            complete_incoming_request(incoming, Err(anyhow::anyhow!("panic: boom")));
            assert!(is_blocked(&ddos), "a handler panic must count against the caller");
            assert!(reply_rx.await.is_err(), "no reply must be sent for a panic");
        }

        #[tokio::test]
        async fn error_outcome_reports_bad_request() {
            let (ddos, incoming, reply_rx) = make_incoming_request();
            complete_incoming_request(incoming, Ok(Err(anyhow::anyhow!("bad packet"))));
            assert!(is_blocked(&ddos));
            assert!(reply_rx.await.is_err());
        }

        #[tokio::test]
        async fn success_outcome_does_not_report_and_replies() {
            let (ddos, incoming, reply_rx) = make_incoming_request();
            complete_incoming_request(incoming, Ok(Ok(BytesGatherer::from_bytes(Bytes::from_static(b"ok")))));
            assert!(!is_blocked(&ddos));
            assert_eq!(reply_rx.await.expect("reply must be sent").to_bytes(), Bytes::from_static(b"ok"));
        }
    }

    #[tokio::test]
    async fn catch_panic_as_error_passes_through_value() {
        let result = catch_panic_as_error(async { 42 }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn catch_panic_as_error_converts_str_panic() {
        let result: anyhow::Result<()> = catch_panic_as_error(async { panic!("boom") }).await;
        let error_message = result.unwrap_err().to_string();
        assert!(error_message.contains("boom"), "error_message={}", error_message);
    }

    #[tokio::test]
    async fn catch_panic_as_error_converts_formatted_panic() {
        let duration = 60000;
        let result: anyhow::Result<()> = catch_panic_as_error(async move { panic!("bad duration {}", duration) }).await;
        let error_message = result.unwrap_err().to_string();
        assert!(error_message.contains("bad duration 60000"), "error_message={}", error_message);
    }

    #[tokio::test]
    async fn catch_panic_as_error_converts_assert_panic() {
        let result: anyhow::Result<()> = catch_panic_as_error(async { assert!(1 > 2) }).await;
        assert!(result.is_err());
    }

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

    mod fetch_url_preview {
        use super::*;
        use crate::environment::mem_environment_store::MemEnvironmentFactory;
        use crate::environment::environment::EnvironmentFactory;
        use crate::server::args::Args;
        use crate::server::hashiverse_server::HashiverseServer;
        use hashiverse_lib::protocol::peer::PeerPow;
        use hashiverse_lib::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
        use hashiverse_lib::tools::runtime_services::RuntimeServices;
        use hashiverse_lib::tools::time_provider::time_provider::RealTimeProvider;
        use hashiverse_lib::tools::types::Pow;
        use hashiverse_lib::transport::mem_transport::MemTransportFactory;
        use std::sync::Arc;
        use std::time::Duration;

        async fn make_server() -> anyhow::Result<Arc<HashiverseServer>> {
            let time_provider = Arc::new(RealTimeProvider);
            let transport_factory = MemTransportFactory::default();
            let pow_generator = Arc::new(SingleThreadedPowGenerator::new());
            let runtime_services = Arc::new(RuntimeServices { time_provider, transport_factory, pow_generator });
            let environment_factory = Arc::new(MemEnvironmentFactory::new(""));
            let args = Args::default_for_testing();
            HashiverseServer::new(runtime_services, environment_factory, args).await
        }

        fn synthetic_pow(pow: Pow) -> PeerPow {
            let mut peer_pow = PeerPow::zero();
            peer_pow.pow = pow;
            peer_pow
        }

        /// A non-https URL: if the pow check passes, the handler rejects it on the SSRF check without touching the network.
        fn request_bytes() -> Bytes {
            FetchUrlPreviewV1::new_to_bytes("http://example.com/").expect("FetchUrlPreviewV1 must serialise")
        }

        async fn fetch_url_preview_error(pow: Option<PeerPow>) -> String {
            let server = make_server().await.expect("server must start");
            let result = server.dispatch_network_payload_x_FetchUrlPreviewV1(CancellationToken::new(), pow, PayloadRequestKind::FetchUrlPreviewV1, request_bytes()).await;
            result.err().expect("handler must reject this request").to_string()
        }

        #[tokio::test]
        async fn rejects_missing_pow() {
            let error = fetch_url_preview_error(None).await;
            assert!(error.contains("pow required for FetchUrlPreviewV1"), "unexpected error: {}", error);
        }

        #[tokio::test]
        async fn rejects_insufficient_pow() {
            let error = fetch_url_preview_error(Some(synthetic_pow(Pow(config::POW_MINIMUM_PER_URL_FETCH.0 - 1)))).await;
            assert!(error.contains("Insufficient pow for FetchUrlPreviewV1"), "unexpected error: {}", error);
        }

        #[tokio::test]
        async fn accepts_threshold_pow() {
            let error = fetch_url_preview_error(Some(synthetic_pow(config::POW_MINIMUM_PER_URL_FETCH))).await;
            assert!(error.contains("only https:// URLs are allowed"), "pow check should pass and fall through to the SSRF check, got: {}", error);
        }

        #[tokio::test]
        async fn dns_lookup_times_out() {
            let never_resolving_lookup = std::future::pending::<std::io::Result<std::vec::IntoIter<std::net::SocketAddr>>>();
            let error = resolve_url_preview_host("example.com", Duration::from_millis(50), never_resolving_lookup).await.err().expect("lookup must time out");
            assert!(error.to_string().contains("DNS resolution timed out for example.com"), "unexpected error: {}", error);
        }

        #[tokio::test]
        async fn dns_lookup_passes_through_results_and_errors() {
            let socket_addr: std::net::SocketAddr = "93.184.216.34:443".parse().unwrap();
            let resolved = resolve_url_preview_host("example.com", Duration::from_secs(1), async move { Ok(vec![socket_addr].into_iter()) }).await.expect("lookup must succeed");
            assert_eq!(resolved, vec![socket_addr]);

            let failed_lookup = async { Err::<std::vec::IntoIter<std::net::SocketAddr>, _>(std::io::Error::other("nxdomain")) };
            let error = resolve_url_preview_host("example.com", Duration::from_secs(1), failed_lookup).await.err().expect("lookup must fail");
            assert!(error.to_string().contains("DNS resolution failed for example.com"), "unexpected error: {}", error);
        }
    }

    mod peer_stats {
        use super::*;
        use crate::environment::mem_environment_store::MemEnvironmentFactory;
        use crate::environment::environment::EnvironmentFactory;
        use crate::server::args::Args;
        use crate::server::hashiverse_server::HashiverseServer;
        use hashiverse_lib::protocol::payload::payload::{PAYLOAD_REQUEST_KIND_COUNT, PeerStatsRequestV1, PeerStatsResponseV1};
        use hashiverse_lib::protocol::peer::PeerPow;
        use hashiverse_lib::tools::compression;
        use hashiverse_lib::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
        use hashiverse_lib::tools::runtime_services::RuntimeServices;
        use hashiverse_lib::tools::time::TimeMillis;
        use hashiverse_lib::tools::time_provider::time_provider::RealTimeProvider;
        use hashiverse_lib::tools::types::{Pow, VerificationKey};
        use hashiverse_lib::transport::mem_transport::MemTransportFactory;
        use std::sync::Arc;
        use std::sync::atomic::Ordering;

        async fn make_server() -> anyhow::Result<Arc<HashiverseServer>> {
            let time_provider = Arc::new(RealTimeProvider);
            let transport_factory = MemTransportFactory::default();
            let pow_generator = Arc::new(SingleThreadedPowGenerator::new());
            let runtime_services = Arc::new(RuntimeServices { time_provider, transport_factory, pow_generator });
            let environment_factory = Arc::new(MemEnvironmentFactory::new(""));
            let args = Args::default_for_testing();
            HashiverseServer::new(runtime_services, environment_factory, args).await
        }

        /// Build a synthetic PeerPow with the requested pow value. The handler under
        /// test only checks the threshold against `config::POW_MINIMUM_PER_PEER_STATS`
        /// and does not re-verify the underlying PoW computation, so this is enough.
        fn synthetic_pow(pow: Pow) -> PeerPow {
            let mut peer_pow = PeerPow::zero();
            peer_pow.pow = pow;
            peer_pow
        }

        fn empty_request_bytes() -> Bytes {
            PeerStatsRequestV1 {}.to_bytes().expect("PeerStatsRequestV1 must serialise")
        }

        fn decode_doc(response: &PeerStatsResponseV1) -> serde_json::Value {
            let bytes = compression::decompress(&response.json_compressed).expect("decompress doc").to_bytes();
            serde_json::from_slice(&bytes).expect("doc must be valid JSON")
        }

        #[tokio::test]
        async fn rejects_insufficient_pow() {
            let server = make_server().await.expect("server must start");
            let result = server
                .dispatch_network_payload_x_PeerStatsRequestV1(
                    CancellationToken::new(),
                    Some(synthetic_pow(Pow(config::POW_MINIMUM_PER_PEER_STATS.0.saturating_sub(1)))),
                    PayloadRequestKind::PeerStatsRequestV1,
                    empty_request_bytes(),
                )
                .await;
            assert!(result.is_err(), "expected insufficient PoW to be rejected");
        }

        #[tokio::test]
        async fn returns_response_with_expected_top_level_keys() {
            let server = make_server().await.expect("server must start");
            let (response_kind, gatherer) = server
                .dispatch_network_payload_x_PeerStatsRequestV1(
                    CancellationToken::new(),
                    Some(synthetic_pow(config::POW_MINIMUM_PER_PEER_STATS)),
                    PayloadRequestKind::PeerStatsRequestV1,
                    empty_request_bytes(),
                )
                .await
                .expect("handler must succeed at threshold pow");
            assert_eq!(response_kind, PayloadResponseKind::PeerStatsResponseV1);

            let response_bytes = gatherer.to_bytes();
            let response = PeerStatsResponseV1::from_bytes(&response_bytes).expect("response must decode");
            let doc = decode_doc(&response);

            assert!(doc.get("version").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false), "version must be a non-empty string");
            assert_eq!(doc["version"].as_str().unwrap(), env!("CARGO_PKG_VERSION"));
            assert!(doc.get("requests").is_some(), "requests subtree missing");
            assert!(doc.get("system").is_some(), "system subtree missing");
            assert!(doc.get("kademlia").is_some(), "kademlia subtree missing");
            assert!(doc.get("environment").is_some(), "environment subtree missing");

            for key in ["memory_total_bytes", "memory_free_bytes", "disk_total_bytes", "disk_free_bytes", "load_1m", "load_5m", "load_15m"] {
                assert!(doc["system"].get(key).map(|v| v.is_number()).unwrap_or(false), "system.{key} must be a number");
            }
            for key in ["post_bundle_count", "post_bundle_feedback_count", "post_bundle_total_bytes"] {
                assert!(doc["environment"].get(key).map(|v| v.is_number()).unwrap_or(false), "environment.{key} must be a number");
            }
        }

        #[tokio::test]
        async fn counters_reflect_recorded_dispatches() {
            let server = make_server().await.expect("server must start");

            // Simulate inbound PingV1 dispatches by bumping the same counter the
            // wrap_and_dispatch path bumps. This avoids the full RPC envelope dance
            // while exercising the read path used by the stats handler.
            for _ in 0..7 {
                server.request_counters[PayloadRequestKind::PingV1 as usize].fetch_add(1, Ordering::Relaxed);
            }

            let (_, gatherer) = server
                .dispatch_network_payload_x_PeerStatsRequestV1(
                    CancellationToken::new(),
                    Some(synthetic_pow(config::POW_MINIMUM_PER_PEER_STATS)),
                    PayloadRequestKind::PeerStatsRequestV1,
                    empty_request_bytes(),
                )
                .await
                .expect("handler must succeed");
            let response = PeerStatsResponseV1::from_bytes(&gatherer.to_bytes()).expect("response must decode");
            let doc = decode_doc(&response);
            assert_eq!(doc["requests"]["PingV1"]["total"].as_u64(), Some(7));
        }

        #[tokio::test]
        async fn cache_returns_byte_identical_response_within_ttl() {
            let server = make_server().await.expect("server must start");
            let pow = synthetic_pow(config::POW_MINIMUM_PER_PEER_STATS);

            let (_, gatherer_a) = server
                .dispatch_network_payload_x_PeerStatsRequestV1(CancellationToken::new(), Some(pow.clone()), PayloadRequestKind::PeerStatsRequestV1, empty_request_bytes())
                .await
                .expect("first call must succeed");
            let bytes_a = gatherer_a.to_bytes();

            // Mutate a counter between calls; if the cache hands back a freshly
            // built response we'd see the new count, but the cached blob predates it.
            server.request_counters[PayloadRequestKind::PingV1 as usize].fetch_add(99, Ordering::Relaxed);

            let (_, gatherer_b) = server
                .dispatch_network_payload_x_PeerStatsRequestV1(CancellationToken::new(), Some(pow), PayloadRequestKind::PeerStatsRequestV1, empty_request_bytes())
                .await
                .expect("second call must succeed");
            let bytes_b = gatherer_b.to_bytes();

            assert_eq!(bytes_a, bytes_b, "cached response should be byte-identical across the TTL");
        }

        #[tokio::test]
        async fn signature_verifies_and_fails_on_tamper() {
            let server = make_server().await.expect("server must start");
            let (_, gatherer) = server
                .dispatch_network_payload_x_PeerStatsRequestV1(
                    CancellationToken::new(),
                    Some(synthetic_pow(config::POW_MINIMUM_PER_PEER_STATS)),
                    PayloadRequestKind::PeerStatsRequestV1,
                    empty_request_bytes(),
                )
                .await
                .expect("handler must succeed");
            let response = PeerStatsResponseV1::from_bytes(&gatherer.to_bytes()).expect("response must decode");

            let verification_key = VerificationKey::from_bytes(&response.peer.verification_key_bytes).expect("verification key must decode");
            let signing_input = PeerStatsResponseV1::signing_input(response.timestamp, &response.json_compressed);
            signing::verify(&verification_key, &response.signature, &signing_input).expect("signature must verify against transmitted bytes");

            // Mutate one byte of the compressed JSON: verification must fail.
            let mut tampered = response.json_compressed.to_vec();
            let tamper_index = tampered.len() / 2;
            tampered[tamper_index] ^= 0xff;
            let tampered_signing_input = PeerStatsResponseV1::signing_input(response.timestamp, &tampered);
            assert!(signing::verify(&verification_key, &response.signature, &tampered_signing_input).is_err(), "verification must fail when json_compressed is tampered");

            // Mutate the timestamp: verification must also fail.
            let bumped_signing_input = PeerStatsResponseV1::signing_input(TimeMillis(response.timestamp.0 + 1), &response.json_compressed);
            assert!(signing::verify(&verification_key, &response.signature, &bumped_signing_input).is_err(), "verification must fail when timestamp is mutated");
        }

        #[test]
        fn request_counts_subtree_covers_every_variant() {
            // Belt-and-braces guard for PAYLOAD_REQUEST_KIND_COUNT staying in lockstep
            // with the enum at the server-stats layer.
            let counters: [std::sync::atomic::AtomicU64; PAYLOAD_REQUEST_KIND_COUNT] = std::array::from_fn(|_| std::sync::atomic::AtomicU64::new(0));
            let windows: [parking_lot::Mutex<crate::server::stats::RequestRateWindows>; PAYLOAD_REQUEST_KIND_COUNT] =
                std::array::from_fn(|_| parking_lot::Mutex::new(crate::server::stats::RequestRateWindows::new()));
            let subtree = request_counts_subtree(&counters, &windows, TimeMillis::zero());
            let map = subtree.as_object().expect("request_counts subtree must be an object");
            assert_eq!(map.len(), PAYLOAD_REQUEST_KIND_COUNT);
        }
    }

    mod announce_v2 {
        use super::*;
        use crate::environment::mem_environment_store::MemEnvironmentFactory;
        use crate::environment::environment::EnvironmentFactory;
        use crate::server::args::Args;
        use crate::server::hashiverse_server::HashiverseServer;
        use hashiverse_lib::protocol::payload::payload::AnnounceV2;
        use hashiverse_lib::protocol::peer::Peer;
        use hashiverse_lib::tools::pow_generator::pow_generator::PowGenerator;
        use hashiverse_lib::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
        use hashiverse_lib::tools::runtime_services::RuntimeServices;
        use hashiverse_lib::tools::server_id::ServerId;
        use hashiverse_lib::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
        use hashiverse_lib::transport::mem_transport::MemTransportFactory;
        use std::sync::Arc;

        async fn make_test_server() -> Arc<HashiverseServer> {
            let time_provider = Arc::new(RealTimeProvider);
            let transport_factory = MemTransportFactory::default();
            let pow_generator = Arc::new(SingleThreadedPowGenerator::new());
            let runtime_services = Arc::new(RuntimeServices { time_provider, transport_factory, pow_generator });
            let environment_factory = Arc::new(MemEnvironmentFactory::new(""));
            let args = Args::default_for_testing();
            HashiverseServer::new(runtime_services, environment_factory, args).await.expect("server must start")
        }

        /// Build a freshly-signed `Peer` to play the role of an inbound announcer. Uses a
        /// brand-new `ServerId` (separate identity from the receiving server) so the announce
        /// looks like it comes from a different node.
        async fn make_announcing_peer(time_provider: &dyn TimeProvider, pow_generator: &dyn PowGenerator, address: &str) -> Peer {
            let server_id = ServerId::new("own_pow", time_provider, config::SERVER_KEY_POW_MIN, true, pow_generator).await.expect("ServerId::new must succeed in tests (PoW reduced under cfg(test))");
            let mut peer = server_id.to_peer(time_provider).expect("to_peer must succeed");
            peer.address = address.to_string();
            peer.sign(time_provider, &server_id.keys.signature_key).expect("sign must succeed");
            peer
        }

        #[tokio::test]
        async fn v2_admits_peer_when_proof_is_valid_for_mem_transport() {
            let server = make_test_server().await;
            let pow_generator = SingleThreadedPowGenerator::new();
            let announcing_peer = make_announcing_peer(server.runtime_services.time_provider.as_ref(), &pow_generator, "9999").await;
            let announcing_peer_id = announcing_peer.id;

            let baseline_len = server.kademlia.read().len();

            // Mem-transport's EmptyMarkerOwnershipProof accepts the empty proof — the announce should be admitted.
            let request_bytes = json::struct_to_bytes(&AnnounceV2 { peer_self: announcing_peer, proof_payload: vec![] }).expect("encode V2");

            let (response_kind, _gatherer) = server
                .dispatch_network_payload_x_AnnounceV2(CancellationToken::new(), PayloadRequestKind::AnnounceV2, request_bytes)
                .await
                .expect("valid V2 must succeed on mem-transport");

            assert_eq!(response_kind, PayloadResponseKind::AnnounceResponseV2);
            assert_eq!(server.kademlia.read().len(), baseline_len + 1, "announcer should now be in Kademlia");

            // The peer is actually findable by id, not just that the count went up.
            let (peers, _) = server.kademlia.read().get_peers_for_key(&announcing_peer_id, 8);
            assert!(peers.iter().any(|p| p.id == announcing_peer_id), "announcing peer must be reachable through get_peers_for_key");
        }

        #[tokio::test]
        async fn v2_bails_and_does_not_admit_when_proof_fails() {
            let server = make_test_server().await;
            let pow_generator = SingleThreadedPowGenerator::new();
            let announcing_peer = make_announcing_peer(server.runtime_services.time_provider.as_ref(), &pow_generator, "8888").await;
            let announcing_peer_id = announcing_peer.id;

            let baseline_len = server.kademlia.read().len();

            // Non-empty proof bytes on a mem-transport receiver — EmptyMarkerOwnershipProof::prove
            // returns false, so the handler should bail. This is the cross-transport-mismatch case
            // (an HTTPS-shaped postcard blob arriving at a mem-transport server).
            let request_bytes = json::struct_to_bytes(&AnnounceV2 { peer_self: announcing_peer, proof_payload: vec![1, 2, 3, 4] }).expect("encode V2");

            let result = server
                .dispatch_network_payload_x_AnnounceV2(CancellationToken::new(), PayloadRequestKind::AnnounceV2, request_bytes)
                .await;

            assert!(result.is_err(), "failed proof must bail so the dispatcher wraps it as ErrorResponseV1");
            assert_eq!(server.kademlia.read().len(), baseline_len, "no Kademlia mutation on failed proof");

            // The peer is not findable by id.
            let (peers, _) = server.kademlia.read().get_peers_for_key(&announcing_peer_id, 8);
            assert!(!peers.iter().any(|p| p.id == announcing_peer_id), "rejected peer must not be reachable through get_peers_for_key");
        }

        // AnnounceV1 had no transport ownership proof, so it was a way into Kademlia that bypassed V2's gate.
        // Its wire slot is kept reserved but the dispatcher must reject it outright.
        #[tokio::test]
        async fn retired_announce_v1_is_rejected() -> anyhow::Result<()> {
            use hashiverse_lib::protocol::rpc::rpc_request::{RpcRequestPacketTx, RpcRequestPacketTxFlags};

            let server = make_test_server().await;
            let time_provider = server.runtime_services.time_provider.clone();
            let pow_generator = SingleThreadedPowGenerator::new();
            let announcing_peer = make_announcing_peer(time_provider.as_ref(), &pow_generator, "7777").await;
            let announcing_peer_id = announcing_peer.id;
            let baseline_len = server.kademlia.read().len();

            // The old V1 body was just the peer record — send exactly that under the retired kind.
            #[derive(serde::Serialize)]
            struct LegacyAnnounceV1 {
                peer_self: Peer,
            }
            let legacy_payload = json::struct_to_bytes(&LegacyAnnounceV1 { peer_self: announcing_peer })?;

            let rpc_request_packet_tx = RpcRequestPacketTx::encode(
                time_provider.as_ref(),
                config::POW_MINIMUM_PER_RPC_SERVER_KNOWN,
                RpcRequestPacketTxFlags::SERVER_KNOWN,
                PayloadRequestKind::RetiredAnnounceV1,
                &server.server_id.id,
                &server.server_id.keys.verification_key_bytes,
                &server.server_id.keys.pq_commitment_bytes,
                legacy_payload,
                &pow_generator,
            )
            .await?;
            let rpc_request_packet_rx = RpcRequestPacketRx::decode(&time_provider.current_time_millis(), &server.server_id.keys.verification_key_bytes, &server.server_id.keys.pq_commitment_bytes, rpc_request_packet_tx.bytes)?;
            assert_eq!(rpc_request_packet_rx.payload_request_kind, PayloadRequestKind::RetiredAnnounceV1);

            let error = server.dispatch_network_envelope(CancellationToken::new(), None, rpc_request_packet_rx).await.err().expect("retired AnnounceV1 must be rejected");
            assert!(error.to_string().contains("retired"), "error={}", error);

            assert_eq!(server.kademlia.read().len(), baseline_len, "no Kademlia mutation from a retired announce");
            let (peers, _) = server.kademlia.read().get_peers_for_key(&announcing_peer_id, 8);
            assert!(!peers.iter().any(|p| p.id == announcing_peer_id));
            Ok(())
        }
    }

    mod heal_donor_plausibility {
        use super::*;

        /// An id that agrees with `location_id` in exactly `leading_agreement_bits` leading bits.
        fn id_with_leading_agreement_bits(location_id: &Id, leading_agreement_bits: usize) -> Id {
            let mut bytes = location_id.as_ref().to_vec();
            bytes[leading_agreement_bits / 8] ^= 0x80 >> (leading_agreement_bits % 8);
            Id::from_slice(&bytes).expect("32 bytes")
        }

        #[test]
        fn id_helper_produces_requested_agreement() {
            let location_id = Id::random();
            for leading_agreement_bits in [0usize, 1, 7, 8, 13, 40] {
                let id = id_with_leading_agreement_bits(&location_id, leading_agreement_bits);
                assert_eq!(tools::leading_agreement_bits_xor(location_id.as_ref(), id.as_ref()), leading_agreement_bits as LeadingAgreementBits);
            }
        }

        #[test]
        fn accepts_anyone_when_we_know_too_few_servers() {
            let location_id = Id::random();
            let nearest_peer_ids = vec![id_with_leading_agreement_bits(&location_id, 20), id_with_leading_agreement_bits(&location_id, 21)];
            let far_donor_id = id_with_leading_agreement_bits(&location_id, 0);
            assert!(is_plausible_heal_donor(&far_donor_id, &location_id, &nearest_peer_ids, 3));
        }

        #[test]
        fn accepts_donors_within_slack_of_furthest_nearest_and_refuses_beyond() {
            let location_id = Id::random();
            // Our view: the 3 nearest live servers agree with the location in 12, 14 and 10 bits; the furthest of them is 10
            let nearest_peer_ids = vec![
                id_with_leading_agreement_bits(&location_id, 12),
                id_with_leading_agreement_bits(&location_id, 14),
                id_with_leading_agreement_bits(&location_id, 10),
            ];
            let slack = config::HEAL_DONOR_LEADING_AGREEMENT_BITS_SLACK as usize;

            // A current stand-in, or a dead original that was nearer than anyone we know now
            assert!(is_plausible_heal_donor(&id_with_leading_agreement_bits(&location_id, 10), &location_id, &nearest_peer_ids, 3));
            assert!(is_plausible_heal_donor(&id_with_leading_agreement_bits(&location_id, 30), &location_id, &nearest_peer_ids, 3));

            // A dead original that has since been slightly out-ranked by newer, closer servers
            assert!(is_plausible_heal_donor(&id_with_leading_agreement_bits(&location_id, 10 - slack), &location_id, &nearest_peer_ids, 3));

            // Too far away to ever have been responsible
            assert!(!is_plausible_heal_donor(&id_with_leading_agreement_bits(&location_id, 10 - slack - 1), &location_id, &nearest_peer_ids, 3));
            assert!(!is_plausible_heal_donor(&id_with_leading_agreement_bits(&location_id, 0), &location_id, &nearest_peer_ids, 3));
        }
    }

    mod submit_post_claim {
        use super::*;
        use crate::environment::environment::EnvironmentFactory;
        use crate::environment::mem_environment_store::MemEnvironmentFactory;
        use crate::server::args::Args;
        use hashiverse_lib::client::key_locker::key_locker::{KeyLocker, KeyLockerManager};
        use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
        use hashiverse_lib::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
        use hashiverse_lib::tools::runtime_services::RuntimeServices;
        use hashiverse_lib::tools::time::MILLIS_IN_MINUTE;
        use hashiverse_lib::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
        use hashiverse_lib::transport::mem_transport::MemTransportFactory;
        use std::sync::Arc;

        // Regression: a SubmitPostClaimV1 into a 1-minute bucket used to hit an assert! in get_minimum_post_pow,
        // unwinding through the dispatch loop and stopping the whole server.
        // With zero PoW it must now get past the minimum-PoW computation and be rejected as under-powered.
        #[tokio::test]
        async fn one_minute_bucket_is_rejected_for_insufficient_pow_not_panic() -> anyhow::Result<()> {
            let runtime_services = Arc::new(RuntimeServices { time_provider: Arc::new(RealTimeProvider), transport_factory: MemTransportFactory::default(), pow_generator: Arc::new(SingleThreadedPowGenerator::new()) });
            let server = HashiverseServer::new(runtime_services, Arc::new(MemEnvironmentFactory::new("")), Args::default_for_testing()).await?;

            let key_locker_manager = MemKeyLockerManager::new().await?;
            let key_locker: Arc<dyn KeyLocker> = key_locker_manager.create("one minute keyphrase".to_string()).await?;
            let time_millis_now = RealTimeProvider.current_time_millis();
            let bucket_location = BucketLocation::new(BucketType::User, key_locker.client_id().id, MILLIS_IN_MINUTE, time_millis_now)?;

            let mut encoded_post = EncodedPostV1::new(key_locker.client_id(), time_millis_now, vec![bucket_location.base_id], "one minute post");
            let encoded_post_bytes = encoded_post.encode_to_bytes_direct(&key_locker).await?;
            let request_bytes = SubmitPostClaimV1::new_to_bytes(&bucket_location, None, &[], encoded_post_bytes.bytes_without_body())?;

            let result = catch_panic_as_error(server.dispatch_network_payload_x_SubmitPostClaimV1(CancellationToken::new(), Some(PeerPow::zero()), PayloadRequestKind::SubmitPostClaimV1, request_bytes)).await?;
            let error_message = result.err().expect("zero PoW must be rejected").to_string();
            assert!(error_message.contains("Insufficient proof of work"), "error_message={}", error_message);
            Ok(())
        }
    }

    mod heal_post_bundle {
        use super::*;
        use crate::environment::environment::EnvironmentFactory;
        use crate::environment::mem_environment_store::MemEnvironmentFactory;
        use crate::server::args::Args;
        use crate::server::hashiverse_server::HashiverseServer;
        use hashiverse_lib::client::key_locker::key_locker::{KeyLocker, KeyLockerManager};
        use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
        use hashiverse_lib::protocol::peer::Peer;
        use hashiverse_lib::protocol::posting::encoded_post_feedback::{self, EncodedPostFeedbackV1};
        use hashiverse_lib::tools::pow;
        use hashiverse_lib::tools::types::Salt;
        use hashiverse_lib::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
        use hashiverse_lib::tools::runtime_services::RuntimeServices;
        use hashiverse_lib::tools::server_id::ServerId;
        use hashiverse_lib::tools::time::MILLIS_IN_MONTH;
        use hashiverse_lib::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
        use hashiverse_lib::tools::types::Pow;
        use hashiverse_lib::transport::mem_transport::MemTransportFactory;
        use std::sync::Arc;

        pub(super) struct HealFixture {
            pub(super) server: Arc<HashiverseServer>,
            pub(super) key_locker: Arc<dyn KeyLocker>,
            pub(super) bucket_location: BucketLocation,
            pub(super) time_millis_now: TimeMillis,
        }

        impl HealFixture {
            pub(super) async fn new() -> anyhow::Result<Self> {
                let time_provider = Arc::new(RealTimeProvider);
                let transport_factory = MemTransportFactory::default();
                let pow_generator = Arc::new(SingleThreadedPowGenerator::new());
                let runtime_services = Arc::new(RuntimeServices { time_provider, transport_factory, pow_generator });
                let environment_factory = Arc::new(MemEnvironmentFactory::new(""));
                let server = HashiverseServer::new(runtime_services, environment_factory, Args::default_for_testing()).await?;

                let key_locker_manager = MemKeyLockerManager::new().await?;
                let key_locker: Arc<dyn KeyLocker> = key_locker_manager.create("heal keyphrase".to_string()).await?;
                let time_millis_now = RealTimeProvider.current_time_millis();
                let bucket_location = BucketLocation::new(BucketType::User, key_locker.client_id().id, MILLIS_IN_MONTH, time_millis_now)?;
                Ok(Self { server, key_locker, bucket_location, time_millis_now })
            }

            pub(super) async fn make_server_id(pow_min: Pow) -> anyhow::Result<ServerId> {
                ServerId::new("heal_donor", &RealTimeProvider, pow_min, true, &SingleThreadedPowGenerator::new()).await
            }

            async fn make_post_by(key_locker: &Arc<dyn KeyLocker>, timestamp: TimeMillis, linked_base_ids: Vec<Id>) -> anyhow::Result<(Id, Bytes)> {
                let mut encoded_post = EncodedPostV1::new(key_locker.client_id(), timestamp, linked_base_ids, "healed post");
                let encoded_post_bytes = encoded_post.encode_to_bytes_direct(key_locker).await?;
                Ok((encoded_post.post_id, Bytes::copy_from_slice(encoded_post_bytes.bytes())))
            }

            pub(super) async fn make_own_post(&self) -> anyhow::Result<(Id, Bytes)> {
                Self::make_post_by(&self.key_locker, self.time_millis_now, vec![self.bucket_location.base_id]).await
            }

            pub(super) fn make_donor_header(&self, donor_server_id: &ServerId, posts: &[(Id, Bytes)], customise: impl FnOnce(&mut EncodedPostBundleHeaderV1)) -> anyhow::Result<EncodedPostBundleHeaderV1> {
                let mut donor_header = EncodedPostBundleHeaderV1 {
                    time_millis: self.time_millis_now,
                    location_id: self.bucket_location.location_id,
                    overflowed: false,
                    sealed: false,
                    num_posts: posts.len() as u8,
                    encoded_post_ids: posts.iter().map(|(post_id, _)| *post_id).collect(),
                    encoded_post_lengths: posts.iter().map(|(_, post_bytes)| post_bytes.len()).collect(),
                    encoded_post_healed: HashSet::new(),
                    peer: donor_server_id.to_peer(&RealTimeProvider)?,
                    signature: Signature::zero(),
                };
                customise(&mut donor_header);
                donor_header.signature_generate(&donor_server_id.keys.signature_key)?;
                Ok(donor_header)
            }

            async fn claim(&self, donor_header: &EncodedPostBundleHeaderV1) -> anyhow::Result<HealPostBundleClaimResponseV1> {
                let request_bytes = HealPostBundleClaimV1::new_to_bytes(donor_header, &self.bucket_location)?;
                let (_, bytes_gatherer) = self.server.dispatch_network_payload_x_HealPostBundleClaimV1(CancellationToken::new(), PayloadRequestKind::HealPostBundleClaimV1, request_bytes).await?;
                json::bytes_to_struct::<HealPostBundleClaimResponseV1>(&bytes_gatherer.to_bytes())
            }

            /// Claims then commits; returns whether the commit was accepted.
            async fn heal(&self, donor_header: &EncodedPostBundleHeaderV1, posts: &[(Id, Bytes)]) -> anyhow::Result<bool> {
                let claim_response = self.claim(donor_header).await?;
                let token = claim_response.token.ok_or_else(|| anyhow::anyhow!("no claim token"))?;
                let encoded_posts_bytes = Self::encoded_posts_bytes_for_token(&token, posts)?;
                self.commit(&token, donor_header, &encoded_posts_bytes).await
            }

            /// The concatenated post bytes a commit must carry: one entry per `token.needed_post_ids`, in order.
            fn encoded_posts_bytes_for_token(token: &HealPostBundleClaimTokenV1, posts: &[(Id, Bytes)]) -> anyhow::Result<Vec<u8>> {
                let mut encoded_posts_bytes = Vec::new();
                for needed_post_id in &token.needed_post_ids {
                    let (_, post_bytes) = posts.iter().find(|(post_id, _)| post_id == needed_post_id).ok_or_else(|| anyhow::anyhow!("missing post"))?;
                    encoded_posts_bytes.extend_from_slice(post_bytes);
                }
                Ok(encoded_posts_bytes)
            }

            async fn commit(&self, token: &HealPostBundleClaimTokenV1, donor_header: &EncodedPostBundleHeaderV1, encoded_posts_bytes: &[u8]) -> anyhow::Result<bool> {
                let request_bytes = HealPostBundleCommitV1::new_to_bytes(token, donor_header, encoded_posts_bytes)?;
                let (_, bytes_gatherer) = self.server.dispatch_network_payload_x_HealPostBundleCommitV1(CancellationToken::new(), PayloadRequestKind::HealPostBundleCommitV1, request_bytes).await?;
                Ok(json::bytes_to_struct::<HealPostBundleCommitResponseV1>(&bytes_gatherer.to_bytes())?.accepted)
            }

            fn stored_bundle(&self) -> anyhow::Result<EncodedPostBundleV1> {
                let post_bundle_bytes = self.server.environment.get_post_bundle_bytes(self.time_millis_now, &self.bucket_location.location_id)?.ok_or_else(|| anyhow::anyhow!("no stored bundle"))?;
                EncodedPostBundleV1::from_bytes(post_bundle_bytes, true)
            }

            async fn make_server_id_at_distance(&self, predicate: impl Fn(LeadingAgreementBits) -> bool) -> anyhow::Result<ServerId> {
                loop {
                    let server_id = Self::make_server_id(config::SERVER_KEY_POW_MIN).await?;
                    if predicate(tools::leading_agreement_bits_xor(self.bucket_location.location_id.as_ref(), server_id.id.as_ref())) {
                        return Ok(server_id);
                    }
                }
            }

            /// Fills our kademlia with genuine servers that agree with the location in at least `minimum_leading_agreement_bits` bits.
            async fn add_near_servers_to_kademlia(&self, count: usize, minimum_leading_agreement_bits: LeadingAgreementBits) -> anyhow::Result<()> {
                for _ in 0..count {
                    let server_id = self.make_server_id_at_distance(|leading_agreement_bits| leading_agreement_bits >= minimum_leading_agreement_bits).await?;
                    let peer: Peer = server_id.to_peer(&RealTimeProvider)?;
                    self.server.kademlia.write().add_peer(peer, self.time_millis_now)?;
                }
                Ok(())
            }
        }

        async fn heal_feedbacks(fixture: &HealFixture, encoded_post_feedbacks: &[EncodedPostFeedbackV1]) -> anyhow::Result<u32> {
            let request_bytes = HealPostBundleFeedbackV1::new_to_bytes(&fixture.bucket_location.location_id, encoded_post_feedbacks)?;
            let (_, bytes_gatherer) = fixture.server.dispatch_network_payload_x_HealPostBundleFeedbackV1(CancellationToken::new(), PayloadRequestKind::HealPostBundleFeedbackV1, request_bytes).await?;
            Ok(json::bytes_to_struct::<HealPostBundleFeedbackResponseV1>(&bytes_gatherer.to_bytes())?.accepted_count)
        }

        fn stored_feedbacks(fixture: &HealFixture) -> anyhow::Result<Vec<EncodedPostFeedbackV1>> {
            let feedbacks_bytes = fixture.server.environment.get_post_bundle_encoded_post_feedbacks_bytes(fixture.time_millis_now, &fixture.bucket_location.location_id)?;
            feedbacks_bytes.chunks_exact(encoded_post_feedback::ENTRY_SIZE).map(EncodedPostFeedbackV1::decode_from_bytes).collect()
        }

        fn make_valid_feedback(post_id: Id, feedback_type: u8) -> anyhow::Result<EncodedPostFeedbackV1> {
            let salt = Salt::random();
            let (measured_pow, _) = pow::pow_measure(&[post_id.as_bytes(), &[feedback_type]], &salt)?;
            Ok(EncodedPostFeedbackV1::new(post_id, feedback_type, salt, measured_pow))
        }

        /// Heals one post into the server so it holds a bundle, and returns that post's id.
        async fn fixture_with_stored_post() -> anyhow::Result<(HealFixture, Id)> {
            let fixture = HealFixture::new().await?;
            let donor_server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            let posts = vec![fixture.make_own_post().await?];
            let donor_header = fixture.make_donor_header(&donor_server_id, &posts, |_| {})?;
            assert!(fixture.heal(&donor_header, &posts).await?);
            let post_id = posts[0].0;
            Ok((fixture, post_id))
        }

        // Regression for C3: a healed feedback claiming pow=255 with a salt that doesn't achieve it must not be stored.
        #[tokio::test]
        async fn heal_feedback_rejects_forged_pow() -> anyhow::Result<()> {
            let (fixture, post_id) = fixture_with_stored_post().await?;
            let forged_feedback = EncodedPostFeedbackV1::new(post_id, 3, Salt::random(), Pow(255));
            assert!(forged_feedback.pow_verify().is_err());

            assert_eq!(heal_feedbacks(&fixture, &[forged_feedback]).await?, 0);
            assert!(stored_feedbacks(&fixture)?.is_empty(), "forged feedback must not be stored");
            Ok(())
        }

        #[tokio::test]
        async fn heal_feedback_accepts_valid_pow_and_skips_forged_in_same_request() -> anyhow::Result<()> {
            let (fixture, post_id) = fixture_with_stored_post().await?;
            let valid_feedback = make_valid_feedback(post_id, 1)?;
            let forged_feedback = EncodedPostFeedbackV1::new(post_id, 2, Salt::random(), Pow(255));

            assert_eq!(heal_feedbacks(&fixture, &[forged_feedback, valid_feedback.clone()]).await?, 1);
            assert_eq!(stored_feedbacks(&fixture)?, vec![valid_feedback]);
            Ok(())
        }

        // A commit that fails after its token has been verified as ours must release the claim straight away,
        // rather than blocking every other client from healing this location until heal_in_progress's TTL expires.
        #[tokio::test]
        async fn failed_commit_releases_heal_claim() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let donor_server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            let posts = vec![fixture.make_own_post().await?];
            let donor_header = fixture.make_donor_header(&donor_server_id, &posts, |_| {})?;

            let token = fixture.claim(&donor_header).await?.token.expect("first claim must get a token");
            assert!(fixture.claim(&donor_header).await?.token.is_none(), "claim must be held while a heal is in progress");

            let mut encoded_posts_bytes = HealFixture::encoded_posts_bytes_for_token(&token, &posts)?;
            encoded_posts_bytes.push(0u8);
            let commit_error = fixture.commit(&token, &donor_header, &encoded_posts_bytes).await.expect_err("commit with excess bytes must fail");
            assert!(commit_error.to_string().contains("excess bytes"), "commit_error={}", commit_error);

            assert!(fixture.claim(&donor_header).await?.token.is_some(), "a failed commit must release the claim");
            Ok(())
        }

        // A commit carrying a token we didn't issue must not be able to release a claim held by someone else.
        #[tokio::test]
        async fn commit_with_foreign_token_does_not_release_heal_claim() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let donor_server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            let posts = vec![fixture.make_own_post().await?];
            let donor_header = fixture.make_donor_header(&donor_server_id, &posts, |_| {})?;

            let genuine_token = fixture.claim(&donor_header).await?.token.expect("first claim must get a token");

            let foreign_server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            let foreign_token = HealPostBundleClaimTokenV1::new(foreign_server_id.to_peer(&RealTimeProvider)?, fixture.bucket_location.clone(), genuine_token.needed_post_ids.clone(), donor_header.signature, &foreign_server_id.keys.signature_key);
            let encoded_posts_bytes = HealFixture::encoded_posts_bytes_for_token(&foreign_token, &posts)?;
            let commit_error = fixture.commit(&foreign_token, &donor_header, &encoded_posts_bytes).await.expect_err("commit with a foreign token must fail");
            assert!(commit_error.to_string().contains("not issued by this server"), "commit_error={}", commit_error);

            assert!(fixture.claim(&donor_header).await?.token.is_none(), "a foreign token must not release the claim");
            Ok(())
        }

        #[tokio::test]
        async fn honest_heal_succeeds_and_does_not_adopt_donor_flags() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let donor_server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            let posts = vec![fixture.make_own_post().await?];
            // The donor claims sealed + overflowed for a bucket that is neither
            let donor_header = fixture.make_donor_header(&donor_server_id, &posts, |donor_header| {
                donor_header.sealed = true;
                donor_header.overflowed = true;
            })?;

            assert!(fixture.heal(&donor_header, &posts).await?);

            let stored_bundle = fixture.stored_bundle()?;
            assert_eq!(stored_bundle.header.encoded_post_ids, vec![posts[0].0]);
            assert!(stored_bundle.header.encoded_post_healed.contains(&posts[0].0));
            assert!(!stored_bundle.header.sealed, "donor's sealed flag must not be adopted");
            assert!(!stored_bundle.header.overflowed, "donor's overflowed flag must not be adopted");
            stored_bundle.verify_for_bucket(&fixture.bucket_location, fixture.time_millis_now)?;
            Ok(())
        }

        #[tokio::test]
        async fn stand_in_donor_whose_posts_are_all_healed_is_accepted() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let donor_server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            let posts = vec![fixture.make_own_post().await?, fixture.make_own_post().await?];
            let donor_header = fixture.make_donor_header(&donor_server_id, &posts, |donor_header| {
                donor_header.encoded_post_healed = donor_header.encoded_post_ids.iter().copied().collect();
            })?;

            assert!(fixture.heal(&donor_header, &posts).await?);
            assert_eq!(fixture.stored_bundle()?.header.num_posts, 2);
            Ok(())
        }

        #[tokio::test]
        async fn donor_without_server_pow_is_refused() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let unworked_server_id = loop {
                let server_id = HealFixture::make_server_id(Pow(0)).await?;
                if server_id.pow < config::SERVER_KEY_POW_MIN {
                    break server_id;
                }
            };
            let posts = vec![fixture.make_own_post().await?];
            let donor_header = fixture.make_donor_header(&unworked_server_id, &posts, |_| {})?;
            donor_header.verify()?; // The old, self-keyed check was happy with this

            assert!(fixture.claim(&donor_header).await.is_err());
            assert!(fixture.stored_bundle().is_err(), "nothing must have been stored");
            Ok(())
        }

        #[tokio::test]
        async fn donor_with_future_timestamp_is_refused() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let donor_server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            let posts = vec![fixture.make_own_post().await?];
            let donor_header = fixture.make_donor_header(&donor_server_id, &posts, |donor_header| {
                donor_header.time_millis = donor_header.time_millis + config::CLIENT_POST_TIMESTAMP_DELTA_THRESHOLD + MILLIS_IN_SECOND;
            })?;

            assert!(fixture.claim(&donor_header).await.is_err());
            Ok(())
        }

        #[tokio::test]
        async fn future_post_is_judged_against_our_clock_not_a_future_donor_clock() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let donor_server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;

            // The donor header is just inside the future-time allowance, and the post is within the allowance of the donor's time
            // but beyond the allowance of ours
            let donor_time_millis = fixture.time_millis_now + config::CLIENT_POST_TIMESTAMP_DELTA_THRESHOLD - MILLIS_IN_SECOND;
            let future_post_time_millis = fixture.time_millis_now + config::CLIENT_POST_TIMESTAMP_DELTA_THRESHOLD + MILLIS_IN_SECOND.const_mul(60);
            if BucketLocation::round_down_to_bucket_start(future_post_time_millis, fixture.bucket_location.duration) != fixture.bucket_location.bucket_time_millis {
                return Ok(()); // Too close to the end of the month for this test to be meaningful
            }
            let future_post = HealFixture::make_post_by(&fixture.key_locker, future_post_time_millis, vec![fixture.bucket_location.base_id]).await?;
            let posts = vec![future_post];
            let donor_header = fixture.make_donor_header(&donor_server_id, &posts, |donor_header| {
                donor_header.time_millis = donor_time_millis;
            })?;

            assert!(fixture.heal(&donor_header, &posts).await.is_err());
            assert!(fixture.stored_bundle().is_err(), "nothing must have been stored");
            Ok(())
        }

        #[tokio::test]
        async fn far_donor_is_refused_but_dead_near_donor_is_accepted() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let slack = config::HEAL_DONOR_LEADING_AGREEMENT_BITS_SLACK;
            let near_leading_agreement_bits = slack + 2;

            // Our view of the location's nearest servers is populated with servers at least this close
            fixture.add_near_servers_to_kademlia(config::REDUNDANT_SERVERS_PER_POST, near_leading_agreement_bits).await?;
            let posts = vec![fixture.make_own_post().await?];

            // A donor that could never have been responsible for this location
            let far_donor_server_id = fixture.make_server_id_at_distance(|leading_agreement_bits| leading_agreement_bits < near_leading_agreement_bits - slack).await?;
            let far_donor_header = fixture.make_donor_header(&far_donor_server_id, &posts, |_| {})?;
            assert!(fixture.claim(&far_donor_header).await.is_err());

            // A donor at least as close as our nearest servers, which is not in our kademlia at all (e.g. it has since died)
            let dead_near_donor_server_id = fixture.make_server_id_at_distance(|leading_agreement_bits| leading_agreement_bits >= near_leading_agreement_bits).await?;
            assert!(!fixture.server.kademlia.read().get_all_peers().iter().any(|peer| peer.id == dead_near_donor_server_id.id));
            let dead_near_donor_header = fixture.make_donor_header(&dead_near_donor_server_id, &posts, |_| {})?;
            // Passes the donor checks (whether we then go on to heal depends on whether we are among the nearest ourselves)
            fixture.claim(&dead_near_donor_header).await?;
            Ok(())
        }

        #[tokio::test]
        async fn out_of_bucket_post_is_refused() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let donor_server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            // A genuine post by the same user, but from the previous bucket
            let old_post = HealFixture::make_post_by(&fixture.key_locker, fixture.bucket_location.bucket_time_millis - MILLIS_IN_SECOND, vec![fixture.bucket_location.base_id]).await?;
            let posts = vec![old_post];
            let donor_header = fixture.make_donor_header(&donor_server_id, &posts, |_| {})?;

            assert!(fixture.heal(&donor_header, &posts).await.is_err());
            assert!(fixture.stored_bundle().is_err(), "nothing must have been stored");
            Ok(())
        }

        #[tokio::test]
        async fn foreign_author_in_user_bucket_is_refused() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let donor_server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;

            // An attacker links the victim's id so that their post decrypts under the victim's User bucket
            let attacker_key_locker_manager = MemKeyLockerManager::new().await?;
            let attacker_key_locker: Arc<dyn KeyLocker> = attacker_key_locker_manager.create("attacker keyphrase".to_string()).await?;
            let foreign_post = HealFixture::make_post_by(&attacker_key_locker, fixture.time_millis_now, vec![fixture.bucket_location.base_id]).await?;
            let posts = vec![foreign_post];
            let donor_header = fixture.make_donor_header(&donor_server_id, &posts, |_| {})?;

            assert!(fixture.heal(&donor_header, &posts).await.is_err());
            assert!(fixture.stored_bundle().is_err(), "nothing must have been stored");
            Ok(())
        }
    }

    // H4: a cache upload must not let free self-signed, empty, mis-located or self-"sealed" bundles squat a location's cache slots.
    mod cache_upload {
        use super::*;
        use super::heal_post_bundle::HealFixture;
        use crate::server::post_bundle_caching_shared::CACHE_HIT_THRESHOLD;
        use hashiverse_lib::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1;
        use hashiverse_lib::tools::pow;
        use hashiverse_lib::tools::server_id::ServerId;
        use hashiverse_lib::tools::time::MILLIS_IN_MINUTE;
        use hashiverse_lib::tools::time_provider::time_provider::RealTimeProvider;
        use hashiverse_lib::tools::types::{Pow, Salt};

        async fn make_unworked_server_id() -> anyhow::Result<ServerId> {
            loop {
                let server_id = HealFixture::make_server_id(Pow(0)).await?;
                if server_id.pow < config::SERVER_KEY_POW_MIN {
                    return Ok(server_id);
                }
            }
        }

        /// Signs a post bundle holding `posts`, as `server_id` would publish it.
        fn make_post_bundle_bytes(fixture: &HealFixture, server_id: &ServerId, posts: &[(Id, Bytes)], customise: impl FnOnce(&mut EncodedPostBundleHeaderV1)) -> anyhow::Result<Bytes> {
            let header = fixture.make_donor_header(server_id, posts, customise)?;
            let encoded_posts_bytes: Vec<u8> = posts.iter().flat_map(|(_, post_bytes)| post_bytes.iter().copied()).collect();
            EncodedPostBundleV1 { header, encoded_posts_bytes: Bytes::from(encoded_posts_bytes) }.to_bytes()
        }

        /// Gets a genuine token by hitting the cache up to its threshold, then uploads `encoded_post_bundle_bytes` with it.
        async fn upload_post_bundle(fixture: &HealFixture, encoded_post_bundle_bytes: &Bytes) -> anyhow::Result<bool> {
            let peer_self = fixture.server.peer_self.read().clone();
            let mut cache_request_token = None;
            for _ in 0..CACHE_HIT_THRESHOLD {
                cache_request_token = cache_request_token.or(fixture.server.post_bundle_cache.on_get(&fixture.bucket_location, &[], &peer_self, &fixture.server.server_id, fixture.time_millis_now).cache_request_token);
            }
            let cache_request_token = cache_request_token.ok_or_else(|| anyhow::anyhow!("no cache request token issued"))?;
            let request_bytes = CachePostBundleV1::new_to_bytes(&cache_request_token, &[encoded_post_bundle_bytes.as_ref()])?;
            let (_, bytes_gatherer) = fixture.server.dispatch_network_payload_x_CachePostBundleV1(CancellationToken::new(), PayloadRequestKind::CachePostBundleV1, request_bytes).await?;
            Ok(json::bytes_to_struct::<CachePostBundleResponseV1>(&bytes_gatherer.to_bytes())?.accepted)
        }

        fn cached_post_bundle_count_at(fixture: &HealFixture, time_millis: TimeMillis) -> usize {
            let peer_self = fixture.server.peer_self.read().clone();
            fixture.server.post_bundle_cache.on_get(&fixture.bucket_location, &[], &peer_self, &fixture.server.server_id, time_millis).cached_items.len()
        }

        #[tokio::test]
        async fn genuine_bundle_is_cached() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            let posts = vec![fixture.make_own_post().await?];
            assert!(upload_post_bundle(&fixture, &make_post_bundle_bytes(&fixture, &server_id, &posts, |_| {})?).await?);
            assert_eq!(cached_post_bundle_count_at(&fixture, fixture.time_millis_now), 1);
            Ok(())
        }

        #[tokio::test]
        async fn empty_bundle_is_rejected() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            assert!(!upload_post_bundle(&fixture, &make_post_bundle_bytes(&fixture, &server_id, &[], |_| {})?).await?);
            assert_eq!(cached_post_bundle_count_at(&fixture, fixture.time_millis_now), 0);
            Ok(())
        }

        #[tokio::test]
        async fn unworked_publisher_is_rejected() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let unworked_server_id = make_unworked_server_id().await?;
            let posts = vec![fixture.make_own_post().await?];
            assert!(!upload_post_bundle(&fixture, &make_post_bundle_bytes(&fixture, &unworked_server_id, &posts, |_| {})?).await?);
            assert_eq!(cached_post_bundle_count_at(&fixture, fixture.time_millis_now), 0);
            Ok(())
        }

        #[tokio::test]
        async fn bundle_for_other_location_is_rejected() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            let posts = vec![fixture.make_own_post().await?];
            let encoded_post_bundle_bytes = make_post_bundle_bytes(&fixture, &server_id, &posts, |header| header.location_id = Id::random())?;
            assert!(!upload_post_bundle(&fixture, &encoded_post_bundle_bytes).await?);
            assert_eq!(cached_post_bundle_count_at(&fixture, fixture.time_millis_now), 0);
            Ok(())
        }

        #[tokio::test]
        async fn self_asserted_sealed_is_not_pinned() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            let posts = vec![fixture.make_own_post().await?];
            // The bucket is current and nowhere near overflowing, so it cannot really be sealed.
            let encoded_post_bundle_bytes = make_post_bundle_bytes(&fixture, &server_id, &posts, |header| header.sealed = true)?;
            assert!(upload_post_bundle(&fixture, &encoded_post_bundle_bytes).await?);
            assert_eq!(cached_post_bundle_count_at(&fixture, fixture.time_millis_now), 1);
            assert_eq!(cached_post_bundle_count_at(&fixture, fixture.time_millis_now + MILLIS_IN_MINUTE.const_mul(6)), 0, "a self-asserted seal must not pin the bundle in the cache");
            Ok(())
        }

        fn make_feedback_bundle_bytes(server_id: &ServerId, location_id: Id, time_millis: TimeMillis) -> anyhow::Result<Bytes> {
            let post_id = Id::random();
            let salt = Salt::random();
            let (measured_pow, _) = pow::pow_measure(&[post_id.as_bytes(), &[1u8]], &salt)?;
            let mut feedbacks_bytes = Vec::new();
            EncodedPostFeedbackV1::new(post_id, 1, salt, measured_pow).append_encode_to_bytes(&mut feedbacks_bytes)?;
            let feedbacks_bytes = Bytes::from(feedbacks_bytes);
            let mut header = EncodedPostBundleFeedbackHeaderV1 {
                time_millis,
                location_id,
                feedbacks_bytes_hash: hashing::hash(feedbacks_bytes.as_ref()),
                peer: server_id.to_peer(&RealTimeProvider)?,
                signature: Signature::zero(),
            };
            header.signature_generate(&server_id.keys.signature_key);
            EncodedPostBundleFeedbackV1 { header, feedbacks_bytes }.to_bytes()
        }

        async fn upload_post_bundle_feedback(fixture: &HealFixture, encoded_post_bundle_feedback_bytes: &Bytes) -> anyhow::Result<bool> {
            let peer_self = fixture.server.peer_self.read().clone();
            let mut cache_request_token = None;
            for _ in 0..CACHE_HIT_THRESHOLD {
                cache_request_token = cache_request_token.or(fixture.server.post_bundle_feedback_cache.on_get(&fixture.bucket_location, &[], &peer_self, &fixture.server.server_id, fixture.time_millis_now).cache_request_token);
            }
            let cache_request_token = cache_request_token.ok_or_else(|| anyhow::anyhow!("no cache request token issued"))?;
            let request_bytes = CachePostBundleFeedbackV1::new_to_bytes(&cache_request_token, encoded_post_bundle_feedback_bytes)?;
            let (_, bytes_gatherer) = fixture.server.dispatch_network_payload_x_CachePostBundleFeedbackV1(CancellationToken::new(), PayloadRequestKind::CachePostBundleFeedbackV1, request_bytes).await?;
            Ok(json::bytes_to_struct::<CachePostBundleFeedbackResponseV1>(&bytes_gatherer.to_bytes())?.accepted)
        }

        #[tokio::test]
        async fn genuine_feedback_is_cached() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            assert!(upload_post_bundle_feedback(&fixture, &make_feedback_bundle_bytes(&server_id, fixture.bucket_location.location_id, fixture.time_millis_now)?).await?);
            Ok(())
        }

        #[tokio::test]
        async fn feedback_from_unworked_publisher_is_rejected() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let unworked_server_id = make_unworked_server_id().await?;
            assert!(!upload_post_bundle_feedback(&fixture, &make_feedback_bundle_bytes(&unworked_server_id, fixture.bucket_location.location_id, fixture.time_millis_now)?).await?);
            Ok(())
        }

        #[tokio::test]
        async fn feedback_for_other_location_is_rejected() -> anyhow::Result<()> {
            let fixture = HealFixture::new().await?;
            let server_id = HealFixture::make_server_id(config::SERVER_KEY_POW_MIN).await?;
            assert!(!upload_post_bundle_feedback(&fixture, &make_feedback_bundle_bytes(&server_id, Id::random(), fixture.time_millis_now)?).await?);
            Ok(())
        }
    }
}
