//! # Peer-to-peer reconciliation of post-bundle feedback
//!
//! Analogue of [`crate::client::post_bundle::post_bundle_healing`] for the feedback
//! layer. After a fetch has pulled feedback from several peers, this module merges them
//! into a best-known view and spawns background `HealPostBundleFeedbackV1` RPCs to each
//! peer whose copy is missing reactions or has weaker per-client signed entries. No
//! two-phase token is needed for feedback because the payload is small and fully
//! self-describing.

use std::sync::Arc;
use crate::anyhow_assert_eq;
use crate::protocol::payload::payload::{HealPostBundleFeedbackResponseV1, HealPostBundleFeedbackV1, PayloadRequestKind, PayloadResponseKind};
use crate::protocol::peer::Peer;
use crate::protocol::posting::encoded_post_bundle_feedback::EncodedPostBundleFeedbackV1;
use crate::protocol::posting::encoded_post_feedback::{EncodedPostFeedbackV1, EncodedPostFeedbackViewV1};
use crate::protocol::rpc;
use crate::tools::runtime_services::RuntimeServices;
use crate::tools::types::{Id, Salt};
use log::{info, warn};

// For each bundle/peer, collect feedbacks that are missing or weaker than the global max
struct HealWork {
    target_peer: Peer,
    location_id: Id,
    feedbacks_to_send: Vec<EncodedPostFeedbackV1>,
}


/// Spawns a background task that sends each peer the feedbacks it is missing
/// relative to `merged` (the pre-computed union of all peer bundles).
/// Does nothing if there are fewer than 2 bundles or merged feedbacks are empty.
pub fn heal_post_bundle_feedbacks(
    runtime_services: Arc<RuntimeServices>,
    sponsor_id: Id,
    peers_visited: &[Peer],
    bundles: &[EncodedPostBundleFeedbackV1],
    best_encoded_post_bundle_feedback: &EncodedPostBundleFeedbackV1,
) {
    // Nothing to heal with a single bundle
    if bundles.len() < 2 || best_encoded_post_bundle_feedback.feedbacks_bytes.is_empty() {
        return;
    }

    let mut work: Vec<HealWork> = Vec::new();
    for bundle in bundles {
        if !peers_visited.iter().any(|p| p.id == bundle.header.peer.id) {
            continue;
        }
        let mut feedbacks_to_send = Vec::new();
        for view in EncodedPostFeedbackViewV1::iter(&best_encoded_post_bundle_feedback.feedbacks_bytes) {
            let Ok(view) = view else { continue };
            let Ok(post_id) = Id::from_slice(view.post_id_bytes()) else { continue };
            let peer_pow = bundle.get_post_pow_for_feedback_type(&post_id, view.feedback_type());
            if view.pow() > peer_pow {
                feedbacks_to_send.push(EncodedPostFeedbackV1 {
                    post_id,
                    feedback_type: view.feedback_type(),
                    salt: Salt::from_slice(view.salt_bytes()).unwrap_or_else(|_| Salt::zero()),
                    pow: view.pow(),
                });
            }
        }
        if !feedbacks_to_send.is_empty() {
            work.push(HealWork {
                target_peer: bundle.header.peer.clone(),
                location_id: bundle.header.location_id,
                feedbacks_to_send,
            });
        }
    }

    if work.is_empty() {
        return;
    }

    crate::tools::tools::spawn_background_task(async move {
        for task in work {
            let result = try_heal_single_bundle_feedback(
                &runtime_services,
                &sponsor_id,
                &task.target_peer,
                task.location_id,
                &task.feedbacks_to_send,
            )
            .await;
            if let Err(e) = result {
                warn!("Healing post bundle feedback for {} failed: {}", task.target_peer, e);
            }
        }
    });
}

async fn try_heal_single_bundle_feedback(
    runtime_services: &RuntimeServices,
    sponsor_id: &Id,
    target_peer: &Peer,
    location_id: Id,
    feedbacks_to_send: &[EncodedPostFeedbackV1],
) -> anyhow::Result<()> {
    let request_bytes = HealPostBundleFeedbackV1::new_to_bytes(&location_id, feedbacks_to_send)?;
    let response = rpc::rpc::rpc_server_known(
        runtime_services,
        sponsor_id,
        target_peer,
        PayloadRequestKind::HealPostBundleFeedbackV1,
        request_bytes,
    )
    .await?;
    anyhow_assert_eq!(&PayloadResponseKind::HealPostBundleFeedbackResponseV1, &response.response_request_kind);
    let response = crate::tools::json::bytes_to_struct::<HealPostBundleFeedbackResponseV1>(&response.bytes)?;

    if response.accepted_count > 0 {
        info!("Healed {} feedback(s) on {}", response.accepted_count, target_peer);
    }

    Ok(())
}
