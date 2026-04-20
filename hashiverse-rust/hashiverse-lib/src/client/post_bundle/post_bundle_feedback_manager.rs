//! # Post-bundle feedback lookup trait
//!
//! Counterpart of
//! [`crate::client::post_bundle::post_bundle_manager::PostBundleManager`]. Where the
//! post-bundle manager fetches the posts themselves, this trait fetches the
//! [`crate::protocol::posting::encoded_post_bundle_feedback::EncodedPostBundleFeedbackV1`]
//! carrying reactions, flags, and visibility hints.
//!
//! They are split because feedback mutates long after the underlying bundle has sealed,
//! and many clients (e.g. a read-only archival cache) don't need the feedback layer at
//! all. Separating the trait keeps both concerns independently cacheable and testable.

use crate::protocol::posting::encoded_post_bundle_feedback::EncodedPostBundleFeedbackV1;
use crate::tools::buckets::BucketLocation;
use crate::tools::time::TimeMillis;

/// The counterpart to [`PostBundleManager`] that fetches the signed feedback attached to a
/// post bundle — the reactions, flags, and visibility hints that clients use to filter and
/// rank the posts they display.
///
/// Feedback is carried separately from the post bundle on purpose: feedback mutates long
/// after the original bundle has been sealed, and not every client needs it (for instance
/// timelines that bypass moderation thresholds can skip the fetch). Implementors follow the
/// same cache-then-network pattern as `PostBundleManager`.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait PostBundleFeedbackManager: Send + Sync {
    async fn get_post_bundle_feedback(&self, bucket_location: BucketLocation, time_millis: TimeMillis) -> anyhow::Result<EncodedPostBundleFeedbackV1>;
}
