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

            let filtered: Vec<&[u8]> = bundles_not_already_cached(&token.already_cached_peer_ids, &bundles).into_iter().map(|bytes| bytes.as_ref()).collect();

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

/// Fire-and-forget: for each token, uploads every collected feedback bundle whose originator is
/// not already cached at the token's server, one `CachePostBundleFeedbackV1` per bundle.
///
/// `bundles` must be the intact, originator-signed bytes (they have to pass `verify()` on the
/// receiving server) — never the client's merged view from `EncodedPostBundleFeedbackV1::merge`.
pub fn upload_post_bundle_feedback_caches(
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
            for feedback_bytes in bundles_not_already_cached(&token.already_cached_peer_ids, &bundles) {
                let now = runtime_services.time_provider.current_time_millis();
                if token.is_expired(now) {
                    break;
                }

                let result: anyhow::Result<()> = async {
                    let request_bytes = CachePostBundleFeedbackV1::new_to_bytes(token, feedback_bytes)?;
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

/// The bundles whose originator the token's server does not already hold in its cache.
fn bundles_not_already_cached<'a>(already_cached_peer_ids: &[Id], bundles: &'a [(Id, Bytes)]) -> Vec<&'a Bytes> {
    bundles.iter().filter(|(originator_id, _)| !already_cached_peer_ids.contains(originator_id)).map(|(_, bundle_bytes)| bundle_bytes).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_not_already_cached_skips_cached_originators() {
        let cached_originator_id = Id::random();
        let uncached_originator_id = Id::random();
        let cached_bundle_bytes = Bytes::from_static(b"cached");
        let uncached_bundle_bytes = Bytes::from_static(b"uncached");
        let bundles = vec![(cached_originator_id, cached_bundle_bytes), (uncached_originator_id, uncached_bundle_bytes.clone())];

        assert_eq!(bundles_not_already_cached(&[cached_originator_id], &bundles), vec![&uncached_bundle_bytes]);
        assert_eq!(bundles_not_already_cached(&[], &bundles).len(), 2);
        assert!(bundles_not_already_cached(&[cached_originator_id, uncached_originator_id], &bundles).is_empty());
    }
}
