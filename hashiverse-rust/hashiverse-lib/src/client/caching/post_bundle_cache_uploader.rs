//! # Fire-and-forget cache replication
//!
//! After a successful post or feedback submission, the server returns a set of
//! `CachePostBundle[Feedback]Token` values that authorise the client to forward the
//! freshly-written bundle to *other* servers for caching. This module spawns those
//! forwards as background tasks so the original submission can return to the user
//! without blocking on network propagation.
//!
//! Bundles that already carry the remote server as an originator are filtered out —
//! there's no point re-uploading data to a server we already know holds it.

use std::sync::Arc;
use bytes::Bytes;
use crate::anyhow_assert_eq;
use crate::protocol::payload::payload::{
    CachePostBundleFeedbackResponseV1, CachePostBundleFeedbackV1, CachePostBundleResponseV1,
    CachePostBundleV1, CacheRequestTokenV1, PayloadRequestKind, PayloadResponseKind,
};
use crate::protocol::rpc;
use crate::tools::runtime_services::RuntimeServices;
use crate::tools::tools::spawn_background_task;
use crate::tools::types::Id;
use crate::tools::json;
use log::warn;

/// Fire-and-forget: for each token, uploads all collected bundles whose originator is not
/// already cached at the token's server.
pub fn upload_post_bundle_caches(
    runtime_services: Arc<RuntimeServices>,
    sponsor_id: Id,
    tokens: Vec<CacheRequestTokenV1>,
    bundles: Vec<(Id, Bytes)>,
) {
    if tokens.is_empty() || bundles.is_empty() {
        return;
    }

    spawn_background_task(async move {
        for token in &tokens {
            let now = runtime_services.time_provider.current_time_millis();
            if token.is_expired(now) {
                continue;
            }

            let filtered: Vec<&[u8]> = bundles
                .iter()
                .filter(|(originator_id, _)| !token.already_cached_peer_ids.contains(originator_id))
                .map(|(_, bytes)| bytes.as_ref())
                .collect();

            if filtered.is_empty() {
                continue;
            }

            let result: anyhow::Result<()> = async {
                let request_bytes = CachePostBundleV1::new_to_bytes(token, &filtered)?;
                let response = rpc::rpc::rpc_server_known(
                    &runtime_services,
                    &sponsor_id,
                    &token.peer,
                    PayloadRequestKind::CachePostBundleV1,
                    request_bytes,
                )
                .await?;
                anyhow_assert_eq!(&PayloadResponseKind::CachePostBundleResponseV1, &response.response_request_kind);
                json::bytes_to_struct::<CachePostBundleResponseV1>(&response.bytes)?;
                Ok(())
            }
            .await;

            if let Err(e) = result {
                warn!("Failed to upload post bundle cache to {}: {}", token.peer, e);
            }
        }
    });
}

/// Fire-and-forget: uploads the single combined-best feedback to every token's server.
pub fn upload_post_bundle_feedback_caches(
    runtime_services: Arc<RuntimeServices>,
    sponsor_id: Id,
    tokens: Vec<CacheRequestTokenV1>,
    feedback_bytes: Bytes,
) {
    if tokens.is_empty() || feedback_bytes.is_empty() {
        return;
    }

    spawn_background_task(async move {
        for token in &tokens {
            let now = runtime_services.time_provider.current_time_millis();
            if token.is_expired(now) {
                continue;
            }

            {
                let result: anyhow::Result<()> = async {
                    let request_bytes = CachePostBundleFeedbackV1::new_to_bytes(token, &feedback_bytes)?;
                    let response = rpc::rpc::rpc_server_known(
                        &runtime_services,
                        &sponsor_id,
                        &token.peer,
                        PayloadRequestKind::CachePostBundleFeedbackV1,
                        request_bytes,
                    )
                    .await?;
                    anyhow_assert_eq!(&PayloadResponseKind::CachePostBundleFeedbackResponseV1, &response.response_request_kind);
                    json::bytes_to_struct::<CachePostBundleFeedbackResponseV1>(&response.bytes)?;
                    Ok(())
                }
                .await;

                if let Err(e) = result {
                    warn!("Failed to upload post bundle feedback cache to {}: {}", token.peer, e);
                }
            }
        }
    });
}
