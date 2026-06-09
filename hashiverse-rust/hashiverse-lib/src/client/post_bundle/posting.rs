//! # Submit-post and submit-feedback client-side orchestration
//!
//! Two async functions that the higher-level client API calls when a user posts or
//! reacts:
//!
//! - `post_to_location` — two-phase commit for a new post against the server(s)
//!   responsible for the target bucket. `SubmitPostClaimV1` first reserves a slot and
//!   returns a commit token (the server has now done its admission-control work);
//!   `SubmitPostCommitV1` then uploads the full post bytes. Split into two so a big
//!   upload can't be aborted halfway without the server having first agreed it wants
//!   the post.
//! - `post_feedback_to_location` — smaller single-phase RPC (`SubmitPostFeedbackV1`)
//!   used for reactions / flags which carry their own PoW but no big payload to
//!   reserve against.

use crate::anyhow_assert_eq;
use crate::client::key_locker::key_locker::KeyLocker;
use crate::client::peer_tracker::peer_tracker::PeerTracker;
use crate::client::post_bundle::live_post_bundle_manager::LivePostBundleManager;
use crate::client::post_bundle::post_bundle_manager::PostBundleManager;
use crate::client::timeline::recent_posts_pen::RecentPostsPen;
use crate::protocol::posting::encoded_post::{EncodedPostBytesV1, EncodedPostV1};
use crate::protocol::payload::payload::{PayloadRequestKind, PayloadResponseKind, SubmitPostClaimResponseV1, SubmitPostClaimV1, SubmitPostCommitResponseV1, SubmitPostCommitTokenV1, SubmitPostCommitV1, SubmitPostFeedbackResponseV1, SubmitPostFeedbackV1};
use crate::protocol::rpc;
use crate::tools::buckets::{bucket_durations_for_type, generate_bucket_location, BucketLocation, BucketType};
use crate::tools::client_id::ClientId;
use crate::tools::{config, json};
use crate::tools::runtime_services::RuntimeServices;
use bytes::Bytes;
use log::{info, trace, warn};
use scraper::{Html, Selector};
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::protocol::posting::amplification;
use crate::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1;
use crate::tools::types::Id;

#[allow(clippy::too_many_arguments)] // each arg is a distinct piece of the client state submitting a post; bundling adds indirection without simplifying the call site
pub async fn submit_post(
    runtime_services: &RuntimeServices,
    client_id: &ClientId,
    key_locker: &Arc<dyn KeyLocker>,
    post_bundle_manager: &LivePostBundleManager,
    peer_tracker: &Arc<RwLock<PeerTracker>>,
    recent_posts_pen: &Arc<RwLock<RecentPostsPen>>,
    post: &str,
) -> anyhow::Result<(Vec<SubmitPostCommitTokenV1>, (EncodedPostV1, Bytes))> {
    trace!("submitting post: {}", post);

    if post.is_empty() {
        anyhow::bail!("Post cannot be empty");
    }

    let timestamp = runtime_services.time_provider.current_time_millis();

    struct LinkedBaseIdDetail {
        linked_base_id: Id,
        bucket_type: BucketType,
        referenced_post_header_bytes: Option<Bytes>,
    }

    // Parse the post for #hashtags, @mentions, replies, sequels, etc.
    let mut linked_base_id_details: Vec<LinkedBaseIdDetail> = vec![];
    let mut referenced_hashtags: Vec<String> = vec![];

    // The original post itself is always present
    linked_base_id_details.push(LinkedBaseIdDetail { linked_base_id: client_id.id, bucket_type: BucketType::User, referenced_post_header_bytes: None });

    {
        let html = Html::parse_fragment(post);
        {
            // Returns true if the element is nested inside a <reply> or <repost> — meaning it
            // belongs to a quoted post and should not be indexed under the new post's buckets.
            let is_quoted = |element: scraper::ElementRef| -> bool {
                let mut node = element.parent();
                while let Some(n) = node {
                    if let Some(el) = scraper::ElementRef::wrap(n) {
                        if matches!(el.value().name(), "reply" | "repost" | "sequel") { return true; }
                    }
                    node = n.parent();
                }
                false
            };

            let selector_hashtag = Selector::parse("hashtag").map_err(|e| anyhow::anyhow!("Failed to parse hashtag selector: {}", e))?;
            for element in html.select(&selector_hashtag) {
                if is_quoted(element) { continue; }
                if let Some(hashtag) = element.attr("hashtag") {
                    trace!("hashtag={:?}", hashtag);
                    referenced_hashtags.push(hashtag.to_string());
                    linked_base_id_details.push(LinkedBaseIdDetail { linked_base_id: Id::from_hashtag_str(hashtag)?, bucket_type: BucketType::Hashtag, referenced_post_header_bytes: None });
                } else {
                    warn!("hashtag attribute not found in element {:?}", element);
                }
            }

            let selector_mention = Selector::parse("mention").map_err(|e| anyhow::anyhow!("Failed to parse mention selector: {}", e))?;
            for element in html.select(&selector_mention) {
                if is_quoted(element) { continue; }
                if let Some(client_id_str) = element.attr("client_id") {
                    match Id::from_hex_str(client_id_str) {
                        Ok(client_id) => {
                            trace!("mention_id={:?}", client_id);
                            linked_base_id_details.push(LinkedBaseIdDetail { linked_base_id: client_id, bucket_type: BucketType::Mention, referenced_post_header_bytes: None });
                        }
                        Err(e) => warn!("mention_id corrupted in element {:?}:, {}", element, e),
                    }
                } else {
                    warn!("mention attribute not found in element {:?}", element);
                }
            }

            let selector_reply = Selector::parse("reply").map_err(|e| anyhow::anyhow!("Failed to parse reply selector: {}", e))?;
            for element in html.select(&selector_reply) {
                if is_quoted(element) { continue; }
                if let Some(post_id_str) = element.attr("post_id") {
                    match Id::from_hex_str(post_id_str) {
                        Ok(post_id) => {
                            trace!("reply post_id={:?}", post_id);
                            let referenced_post_header_bytes = element.attr("post_header_hex")
                                .and_then(|hex_str| hex::decode(hex_str).ok())
                                .map(Bytes::from);
                            linked_base_id_details.push(LinkedBaseIdDetail { linked_base_id: post_id, bucket_type: BucketType::ReplyToPost, referenced_post_header_bytes });
                        }
                        Err(e) => warn!("reply post_id corrupted in element {:?}: {}", element, e),
                    }
                } else {
                    warn!("post_id attribute not found in reply element {:?}", element);
                }
            }

            let selector_sequel = Selector::parse("sequel").map_err(|e| anyhow::anyhow!("Failed to parse sequel selector: {}", e))?;
            for element in html.select(&selector_sequel) {
                if is_quoted(element) { continue; }
                if let Some(post_id_str) = element.attr("post_id") {
                    match Id::from_hex_str(post_id_str) {
                        Ok(post_id) => {
                            trace!("sequel post_id={:?}", post_id);
                            let referenced_post_header_bytes = element.attr("post_header_hex")
                                .and_then(|hex_str| hex::decode(hex_str).ok())
                                .map(Bytes::from);
                            linked_base_id_details.push(LinkedBaseIdDetail { linked_base_id: post_id, bucket_type: BucketType::Sequel, referenced_post_header_bytes });
                        }
                        Err(e) => warn!("sequel post_id corrupted in element {:?}: {}", element, e),
                    }
                } else {
                    warn!("post_id attribute not found in sequel element {:?}", element);
                }
            }
        }
    }

    // Encode the post
    let linked_base_ids: Vec<Id> = linked_base_id_details.iter().map(|d| d.linked_base_id).collect();
    let mut encoded_post = EncodedPostV1::new(client_id, timestamp, linked_base_ids, post);
    let encoded_post_bytes = encoded_post.encode_to_bytes_direct(key_locker).await?;

    // The commit tokens for the User's post
    let mut post_commit_tokens = Vec::new();

    // Start the post machine
    for linked_base_id_detail in &linked_base_id_details {
        trace!("Posting to bucket type: {:?}, linked_base_id: {}", linked_base_id_detail.bucket_type, linked_base_id_detail.linked_base_id);
        let try_result = try {
            // Where are we going to post it?  Start at the widest bucket and work our way narrower till we succeed
            for &bucket_duration in bucket_durations_for_type(linked_base_id_detail.bucket_type) {
                let bucket_location = generate_bucket_location(linked_base_id_detail.bucket_type, linked_base_id_detail.linked_base_id, bucket_duration, timestamp)?;
                info!("checking posting availability of {:?}", bucket_location);

                let post_bundle = post_bundle_manager.get_post_bundle(&bucket_location, timestamp).await?;
                if !post_bundle.header.overflowed && !post_bundle.header.sealed {
                    info!("Posting to {:?}", bucket_location);
                    let result = post_to_location(runtime_services, &client_id.id, peer_tracker, &bucket_location, &encoded_post, &encoded_post_bytes, linked_base_id_detail.referenced_post_header_bytes.as_deref(), &referenced_hashtags).await;
                    match result {
                        Ok(mut result) => {
                            post_commit_tokens.append(&mut result);
                            break;
                        }
                        Err(e) => {
                            warn!("Failed to post to {:?}: {}", bucket_location, e);
                            continue;
                        }
                    }
                }

                else {
                    trace!("no availability: overflowed={} sealed={}", post_bundle.header.overflowed, post_bundle.header.sealed);
                }
            }
        };

        if let Err(e) = try_result {
            warn!("Failed to post to any bucket location for linked_base_id {:?}: {}", linked_base_id_detail.linked_base_id, e);
        }

        // Check that we managed at least one commit by the end of the User bucket (healing should fix the rest).  We can happily continue with a failed hashtag, mention, etc.
        if linked_base_id_detail.bucket_type == BucketType::User && post_commit_tokens.is_empty() {
            anyhow::bail!("Failed to post to any User buckets, so bailing,");
        }
    }

    let encoded_post_bytes_raw = Bytes::copy_from_slice(encoded_post_bytes.bytes());

    // Record in the recent posts pen so these posts appear immediately in timeline fetches
    {
        let bucket_locations_and_post_ids: Vec<_> = post_commit_tokens.iter()
            .map(|token| (token.bucket_location.clone(), token.post_id))
            .collect();
        recent_posts_pen.write().await.add_all(&bucket_locations_and_post_ids, encoded_post_bytes_raw.clone(), timestamp);
    }

    Ok((post_commit_tokens, (encoded_post, encoded_post_bytes_raw)))
}

#[allow(clippy::too_many_arguments)] // each arg is a distinct piece of the post-to-location operation; bundling adds indirection without simplifying call sites
pub async fn post_to_location(
    runtime_services: &RuntimeServices,
    sponsor_id: &Id,
    peer_tracker: &Arc<RwLock<PeerTracker>>,
    bucket_location: &BucketLocation,
    encoded_post: &EncodedPostV1,
    encoded_post_bytes: &EncodedPostBytesV1,
    referenced_post_header_bytes: Option<&[u8]>,
    referenced_hashtags: &[String],
) -> anyhow::Result<Vec<SubmitPostCommitTokenV1>> {
    let mut peers_visited = Vec::new();
    let mut submit_post_claim_tokens = Vec::new();
    let mut submit_post_commit_tokens = Vec::new();

    let mut peer_tracker = peer_tracker.write().await;
    let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, 2 * config::REDUNDANT_SERVERS_PER_POST, None).await?;
    while let Some((peer, leading_agreement_bits)) = peer_iter.next_peer() {
        let result: anyhow::Result<()> = try {
            info!("Requesting SubmitPostClaim with leading_agreement_bits={} from peer {}", leading_agreement_bits, peer);

            // First we need to get permission to post
            let request = SubmitPostClaimV1::new_to_bytes(bucket_location, referenced_post_header_bytes, referenced_hashtags, encoded_post_bytes.bytes_without_body())?;
            let requisite_pow = amplification::get_minimum_post_pow(encoded_post.header.post_length, encoded_post.header.linked_base_ids.len(), bucket_location.duration);
            let response = rpc::rpc::rpc_server_known_with_requisite_pow_and_no_compression(runtime_services, sponsor_id, &peer, PayloadRequestKind::SubmitPostClaimV1, request, requisite_pow).await?;
            anyhow_assert_eq!(&PayloadResponseKind::SubmitPostClaimResponseV1, &response.response_request_kind);
            let response = json::bytes_to_struct::<SubmitPostClaimResponseV1>(&response.bytes)?;

            peers_visited.push(peer.clone());

            peer_iter.add_peers(response.peers_nearer);

            if let Some(submit_post_claim_token) = response.submit_post_claim_token {
                info!("Received a SubmitPostClaim from peer {}", peer);

                submit_post_claim_tokens.push(submit_post_claim_token.clone());

                // We have permission, so do the post!
                info!("Posting to peer {}", peer);
                let request = SubmitPostCommitV1::new_to_bytes(bucket_location, &submit_post_claim_token, encoded_post_bytes.bytes())?;
                let response = rpc::rpc::rpc_server_known_with_no_compression(runtime_services, sponsor_id, &peer, PayloadRequestKind::SubmitPostCommitV1, request).await?;
                anyhow_assert_eq!(&PayloadResponseKind::SubmitPostCommitResponseV1, &response.response_request_kind);
                let response = json::bytes_to_struct::<SubmitPostCommitResponseV1>(&response.bytes)?;

                submit_post_commit_tokens.push(response.submit_post_commit_token);
                if submit_post_commit_tokens.len() >= config::REDUNDANT_SERVERS_PER_POST {
                    break;
                }
            }
        };

        if let Err(e) = result {
            warn!("Error retrieving SubmitPostClaim from peer {}: {}", peer, e);
            peer_iter.remove_peer(&peer);
        }
    }

    if submit_post_claim_tokens.is_empty() {
        anyhow::bail!("Despite expecting availability, we were unable claim anywhere at {}", bucket_location);
    }

    if submit_post_commit_tokens.is_empty() {
        anyhow::bail!("Despite expecting availability, we were unable commit anywhere at {}", bucket_location);
    }

    Ok(submit_post_commit_tokens)
}

pub async fn post_feedback_to_location(
    runtime_services: &RuntimeServices,
    sponsor_id: &Id,
    peer_tracker: &Arc<RwLock<PeerTracker>>,
    bucket_location: &BucketLocation,
    encoded_post_feedback: &EncodedPostFeedbackV1,
) -> anyhow::Result<()> {

    let mut peers_visited = Vec::new();
    let mut submit_post_feedback_accepts = Vec::new();

    let mut peer_tracker = peer_tracker.write().await;
    let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, 2 * config::REDUNDANT_SERVERS_PER_POST, None).await?;
    while let Some((peer, leading_agreement_bits)) = peer_iter.next_peer() {
        let result: anyhow::Result<()> = try {
            info!("Requesting SubmitPostFeedbackV1 with leading_agreement_bits={} from peer {}", leading_agreement_bits, peer);

            let request = SubmitPostFeedbackV1::new_to_bytes(bucket_location, encoded_post_feedback)?;
            let response = rpc::rpc::rpc_server_known_with_requisite_pow(runtime_services, sponsor_id, &peer, PayloadRequestKind::SubmitPostFeedbackV1, request, config::POW_MINIMUM_PER_FEEDBACK).await?;
            anyhow_assert_eq!(&PayloadResponseKind::SubmitPostFeedbackResponseV1, &response.response_request_kind);
            let response = json::bytes_to_struct::<SubmitPostFeedbackResponseV1>(&response.bytes)?;

            peers_visited.push(peer.clone());

            peer_iter.add_peers(response.peers_nearer);

            if response.accepted {
                info!("Received an accept from peer {}", peer);

                submit_post_feedback_accepts.push(peer.clone());
                if submit_post_feedback_accepts.len() >= config::REDUNDANT_SERVERS_PER_POST {
                    break;
                }
            }
        };

        if let Err(e) = result {
            warn!("Error retrieving SubmitPostFeedbackV1 from peer {}: {}", peer, e);
            peer_iter.remove_peer(&peer);
        }
    }

    if submit_post_feedback_accepts.is_empty() {
        anyhow::bail!("Despite expecting availability, we were unable commit anywhere at {}", bucket_location.location_id);
    }

    Ok(())
}
