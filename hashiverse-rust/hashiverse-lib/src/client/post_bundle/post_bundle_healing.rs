//! # Peer-to-peer reconciliation of divergent post bundles
//!
//! When multiple peers return different versions of the same post bundle — one
//! might hold posts the others have missed — this module spawns a background task that
//! sends each laggard peer the posts it's missing.
//!
//! The sync is a two-phase, PoW-gated protocol:
//!
//! 1. **Claim** (`HealPostBundleClaimV1`) — the initiator describes what it holds; the
//!    remote server returns which post IDs it's still missing and issues a commit token
//!    authorising the follow-up upload.
//! 2. **Commit** (`HealPostBundleCommitV1`) — the initiator uploads just those missing
//!    post bytes under the token.
//!
//! Splitting into claim + commit keeps the bandwidth-hungry upload from happening until
//! the receiver has confirmed it actually wants the data, and it makes per-server
//! response authentication cheap (the token is what the commit carries).

use crate::anyhow_assert_eq;
use crate::protocol::payload::payload::{HealPostBundleClaimResponseV1, HealPostBundleClaimV1, HealPostBundleCommitResponseV1, HealPostBundleCommitV1, PayloadRequestKind, PayloadResponseKind};
use crate::protocol::peer::Peer;
use crate::protocol::posting::encoded_post_bundle::{EncodedPostBundleHeaderV1, EncodedPostBundleV1};
use crate::protocol::rpc;
use crate::tools::buckets::BucketLocation;
use crate::tools::json;
use crate::tools::runtime_services::RuntimeServices;
use crate::tools::types::Id;
use bytes::Bytes;
use log::{info, warn};
use std::collections::HashMap;
use std::sync::Arc;

struct HealWork {
    target_peer: Peer,
    bucket_location: BucketLocation,
    donor_header: EncodedPostBundleHeaderV1,
    donor_posts_bytes: Bytes,
    post_byte_map: HashMap<Id, (usize, usize)>,
}

/// Selects and returns the best bundle (most posts), then spawns a background task that
/// serially heals every server whose bundle is missing posts that any other server has.
/// Returns None if bundles is empty.
pub fn heal_post_bundles(runtime_services: Arc<RuntimeServices>, sponsor_id: Id, bucket_location: BucketLocation, peers_visited: &[Peer], bundles: Vec<EncodedPostBundleV1>) {
    if bundles.is_empty() {
        return;
    }

    // Build a post_byte_map for each bundle once (offset, len per post_id)
    let byte_maps: Vec<HashMap<Id, (usize, usize)>> = bundles
        .iter()
        .map(|bundle| {
            let mut map = HashMap::new();
            let mut offset = 0usize;
            for (post_id, &len) in bundle.header.encoded_post_ids.iter().zip(bundle.header.encoded_post_lengths.iter()) {
                map.insert(*post_id, (offset, len));
                offset += len;
            }
            map
        })
        .collect();

    // For each (donor, target) pair: if donor has posts that target is missing, queue a heal.
    // Only target servers we directly visited — we should not heal servers where we have only a cached snapshot of their data.
    // This might queue up duplicate heal tasks for the server, but the server will deny the second heal attempt.
    let mut work: Vec<HealWork> = Vec::new();
    for donor_idx in 0..bundles.len() {
        for target_idx in 0..bundles.len() {
            if donor_idx == target_idx {
                continue;
            }
            let donor = &bundles[donor_idx];
            let target = &bundles[target_idx];
            let donor_map = &byte_maps[donor_idx];
            if !peers_visited.iter().any(|p| p.id == target.header.peer.id) {
                continue;
            }
            if donor.header.encoded_post_ids.iter().any(|id| !target.header.encoded_post_ids.contains(id)) {
                info!("Queueing healing for target_peer={} bucket_location={}", target.header.peer, bucket_location);
                work.push(HealWork {
                    target_peer: target.header.peer.clone(),
                    bucket_location: bucket_location.clone(),
                    donor_header: donor.header.clone(),
                    donor_posts_bytes: donor.encoded_posts_bytes.clone(),
                    post_byte_map: donor_map.clone(),
                });
            }
        }
    }

    if !work.is_empty() {
        crate::tools::tools::spawn_background_task(async move {
            for task in work {
                let result = try_heal_single_bundle(&runtime_services, &sponsor_id, &task.target_peer, task.bucket_location, task.donor_header, &task.donor_posts_bytes, &task.post_byte_map).await;
                if let Err(e) = result {
                    warn!("Healing failed for target_peer={}: {}", task.target_peer, e);
                }
            }
        });
    }
}

async fn try_heal_single_bundle(
    runtime_services: &RuntimeServices,
    sponsor_id: &Id,
    target_peer: &Peer,
    bucket_location: BucketLocation,
    donor_header: EncodedPostBundleHeaderV1,
    donor_posts_bytes: &[u8],
    post_byte_map: &HashMap<Id, (usize, usize)>,
) -> anyhow::Result<()> {
    info!("About to heal {}", target_peer);

    // Phase 1: Claim — ask the server which posts it needs from this donor bundle
    let request_bytes = HealPostBundleClaimV1::new_to_bytes(&donor_header, &bucket_location)?;
    let response = rpc::rpc::rpc_server_known(runtime_services, sponsor_id, target_peer, PayloadRequestKind::HealPostBundleClaimV1, request_bytes).await?;
    anyhow_assert_eq!(&PayloadResponseKind::HealPostBundleClaimResponseV1, &response.response_request_kind);
    let response = json::bytes_to_struct::<HealPostBundleClaimResponseV1>(&response.bytes)?;

    let Some(token) = response.token
    else {
        info!("Healing skipped because of no HealPostBundleClaimTokenV1 for {}", target_peer);
        return Ok(()); // Server is up to date or is busy with another heal
    };

    // Assemble the bytes for the posts the server needs, in token order
    let mut encoded_posts_bytes = Vec::new();
    for post_id in &token.needed_post_ids {
        let &(offset, len) = post_byte_map.get(post_id).ok_or_else(|| anyhow::anyhow!("needed post {} not in donor bundle", post_id))?;
        encoded_posts_bytes.extend_from_slice(&donor_posts_bytes[offset..offset + len]);
    }

    // Phase 2: Commit — send the missing post bytes
    let request_bytes = HealPostBundleCommitV1::new_to_bytes(&token, &donor_header, &encoded_posts_bytes)?;
    let response = rpc::rpc::rpc_server_known_with_no_compression(runtime_services, sponsor_id, target_peer, PayloadRequestKind::HealPostBundleCommitV1, request_bytes).await?;
    anyhow_assert_eq!(&PayloadResponseKind::HealPostBundleCommitResponseV1, &response.response_request_kind);
    let response = json::bytes_to_struct::<HealPostBundleCommitResponseV1>(&response.bytes)?;

    if response.accepted {
        info!("Healed {} post(s) on {} from {}", token.needed_post_ids.len(), target_peer, donor_header.peer);
    }
    else {
        info!("Healing not accepted  for {} post(s) on {} from {}", token.needed_post_ids.len(), target_peer, donor_header.peer);
    }

    Ok(())
}
