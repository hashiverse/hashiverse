//! # Post-bundle lookup trait
//!
//! One trait — [`PostBundleManager`] — that maps a `(BucketLocation, TimeMillis)` to an
//! [`EncodedPostBundleV1`]. Implementations decide whether to serve from the local cache,
//! pull from one peer, or heal from several, but timeline logic
//! ([`crate::client::timeline`]) only sees the trait. That keeps the walk algorithm
//! network-free and lets tests plug in deterministic stubs.

use crate::protocol::posting::encoded_post_bundle::EncodedPostBundleV1;
use crate::tools::buckets::BucketLocation;
use crate::tools::time::TimeMillis;

/// The abstract lookup from "give me the posts at this (location, time) bucket" to an
/// [`EncodedPostBundleV1`].
///
/// Timeline code above this trait does not care whether the bundle came from the local
/// cache, from a single network peer, or was stitched together from several peers after a
/// healing round — it just calls `get_post_bundle`. The production implementation is
/// [`crate::client::post_bundle::live_post_bundle_manager::LivePostBundleManager`], which
/// checks local [`ClientStorage`] first, falls back to the best peer from
/// [`crate::client::peer_tracker::PeerTracker`], and transparently heals missing posts.
/// Tests swap in stub implementations that return canned bundles so the timeline logic can
/// be exercised without any network.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait PostBundleManager: Send + Sync {
    /// The bundle for this bucket, as soon as one is good enough to show.  An implementation may answer with a *provisional* bundle
    /// (e.g. the first authoritative reply) while it keeps reconciling in the background, so the posts or `overflowed` flag may be
    /// incomplete.  Use this for display; use [`Self::get_post_bundle_settled`] when a decision depends on the bundle being complete.
    async fn get_post_bundle(&self, bucket_location: &BucketLocation, time_millis: TimeMillis) -> anyhow::Result<EncodedPostBundleV1>;

    /// The bundle once it has fully settled (e.g. best-of-several-servers).  Implementations that never answer provisionally
    /// can rely on this default.
    async fn get_post_bundle_settled(&self, bucket_location: &BucketLocation, time_millis: TimeMillis) -> anyhow::Result<EncodedPostBundleV1> {
        self.get_post_bundle(bucket_location, time_millis).await
    }
}

