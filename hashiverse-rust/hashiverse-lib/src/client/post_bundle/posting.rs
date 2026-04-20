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
use crate::client::peer_tracker::peer_tracker::PeerTracker;
use crate::protocol::posting::encoded_post::{EncodedPostBytesV1, EncodedPostV1};
use crate::protocol::payload::payload::{PayloadRequestKind, PayloadResponseKind, SubmitPostClaimResponseV1, SubmitPostClaimV1, SubmitPostCommitResponseV1, SubmitPostCommitTokenV1, SubmitPostCommitV1, SubmitPostFeedbackResponseV1, SubmitPostFeedbackV1};
use crate::protocol::rpc;
use crate::tools::buckets::BucketLocation;
use crate::tools::{config, json};
use crate::tools::runtime_services::RuntimeServices;
use log::{info, warn};
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::protocol::posting::amplification;
use crate::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1;
use crate::tools::types::Id;

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
