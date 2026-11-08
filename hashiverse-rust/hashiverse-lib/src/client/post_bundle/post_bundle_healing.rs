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

/// Spawns a background task that serially heals every target server whose bundle is missing posts that a donor bundle has.
///
/// - `donor_bundles` must be authoritative bundles (verified, holding at least one genuinely signed post) - direct responses or cached snapshots,
///   including snapshots signed by servers that have since died.
/// - `target_bundles` are the bundles returned directly by the servers we visited, paired with the [`Peer`] we actually sent the RPC to.
///   We only heal servers we have spoken to (never one we only hold a cached snapshot of), and we address them using that authenticated
///   peer rather than the `peer` inside the bundle header, whose address is whatever the bundle's publisher chose to put there.
pub fn heal_post_bundles(runtime_services: Arc<RuntimeServices>, sponsor_id: Id, bucket_location: BucketLocation, donor_bundles: &[EncodedPostBundleV1], target_bundles: &[(Peer, EncodedPostBundleV1)]) {
    let work = build_heal_work(&bucket_location, donor_bundles, target_bundles);

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

/// For each (donor, target) pair: if the donor has posts that the target is missing, queue a heal of the target from that donor.
/// This might queue up duplicate heal tasks for a server, but the server will deny the second heal attempt.
fn build_heal_work(bucket_location: &BucketLocation, donor_bundles: &[EncodedPostBundleV1], target_bundles: &[(Peer, EncodedPostBundleV1)]) -> Vec<HealWork> {
    // Build a post_byte_map for each donor once (offset, len per post_id)
    let donor_byte_maps: Vec<HashMap<Id, (usize, usize)>> = donor_bundles
        .iter()
        .map(|donor_bundle| {
            let mut map = HashMap::new();
            let mut offset = 0usize;
            for (post_id, &len) in donor_bundle.header.encoded_post_ids.iter().zip(donor_bundle.header.encoded_post_lengths.iter()) {
                map.insert(*post_id, (offset, len));
                offset += len;
            }
            map
        })
        .collect();

    let mut work: Vec<HealWork> = Vec::new();
    for (donor_bundle, donor_byte_map) in donor_bundles.iter().zip(donor_byte_maps.iter()) {
        for (target_peer, target_bundle) in target_bundles {
            if donor_bundle.header.peer.id == target_peer.id {
                continue;
            }
            if donor_bundle.header.encoded_post_ids.iter().any(|id| !target_bundle.header.encoded_post_ids.contains(id)) {
                info!("Queueing healing for target_peer={} bucket_location={}", target_peer, bucket_location);
                work.push(HealWork {
                    target_peer: target_peer.clone(),
                    bucket_location: bucket_location.clone(),
                    donor_header: donor_bundle.header.clone(),
                    donor_posts_bytes: donor_bundle.encoded_posts_bytes.clone(),
                    post_byte_map: donor_byte_map.clone(),
                });
            }
        }
    }
    work
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::posting::encoded_post_bundle::test_fixtures::BucketBundleFixture;
    use crate::tools::time_provider::time_provider::RealTimeProvider;

    #[tokio::test]
    async fn heals_targets_missing_donor_posts_addressed_via_the_rpc_peer() -> anyhow::Result<()> {
        let fixture = BucketBundleFixture::new().await?;
        let posts = fixture.make_own_posts(2).await?;

        let donor_server_id = BucketBundleFixture::new_server_id().await?;
        let donor_bundle = fixture.make_bundle_from(&donor_server_id, &posts)?;

        // The target's own bundle claims an address chosen by its publisher; the peer we actually spoke to has a different one
        let target_server_id = BucketBundleFixture::new_server_id().await?;
        let mut header_peer = target_server_id.to_peer(&RealTimeProvider)?;
        header_peer.address = "attacker-chosen-address".to_string();
        header_peer.sign(&RealTimeProvider, &target_server_id.keys.signature_key)?;
        let target_bundle = fixture.make_bundle_signed_by(&target_server_id, header_peer, fixture.time_millis_now, &posts[..1])?;
        let mut rpc_peer = target_server_id.to_peer(&RealTimeProvider)?;
        rpc_peer.address = "the-address-we-actually-used".to_string();

        let work = build_heal_work(&fixture.bucket_location, std::slice::from_ref(&donor_bundle), &[(rpc_peer, target_bundle)]);
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].target_peer.address, "the-address-we-actually-used");
        assert_eq!(work[0].donor_header, donor_bundle.header);
        Ok(())
    }

    #[tokio::test]
    async fn does_not_heal_up_to_date_targets_or_donors_themselves() -> anyhow::Result<()> {
        let fixture = BucketBundleFixture::new().await?;
        let posts = fixture.make_own_posts(2).await?;

        let donor_server_id = BucketBundleFixture::new_server_id().await?;
        let donor_bundle = fixture.make_bundle_from(&donor_server_id, &posts)?;
        let up_to_date_server_id = BucketBundleFixture::new_server_id().await?;
        let up_to_date_bundle = fixture.make_bundle_from(&up_to_date_server_id, &posts)?;

        let target_bundles = vec![
            (donor_server_id.to_peer(&RealTimeProvider)?, donor_bundle.clone()),
            (up_to_date_server_id.to_peer(&RealTimeProvider)?, up_to_date_bundle),
        ];
        assert!(build_heal_work(&fixture.bucket_location, std::slice::from_ref(&donor_bundle), &target_bundles).is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn empty_direct_responder_is_a_target() -> anyhow::Result<()> {
        let fixture = BucketBundleFixture::new().await?;
        let posts = fixture.make_own_posts(1).await?;

        let donor_server_id = BucketBundleFixture::new_server_id().await?;
        let donor_bundle = fixture.make_bundle_from(&donor_server_id, &posts)?;
        let empty_server_id = BucketBundleFixture::new_server_id().await?;
        let empty_bundle = fixture.make_bundle_from(&empty_server_id, &[])?;

        let work = build_heal_work(&fixture.bucket_location, &[donor_bundle], &[(empty_server_id.to_peer(&RealTimeProvider)?, empty_bundle)]);
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].target_peer.id, empty_server_id.id);
        Ok(())
    }
}
