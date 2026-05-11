//! # [`EnvironmentStore`] trait — pluggable server-side persistence
//!
//! The contract every backend must implement: CRUD on post bundles, per-bundle
//! metadata, config, and aggregated feedback. Keeps the
//! [`crate::environment::environment::Environment`] facade independent of whether
//! the bytes actually live on disk ([`crate::environment::disk_environment_store`])
//! or in RAM for tests ([`crate::environment::mem_environment_store`]).
//!
//! Design notes:
//!
//! - **Last-accessed tracking** on every read so decimation can evict coldest-first
//!   without a second pass.
//! - **Composite feedback keys** `(location_id, post_id, feedback_type)` — the store
//!   only persists the write when the new entry's PoW is at least as strong as the
//!   existing one, so backend can resolve merges without returning any state to the
//!   caller.
//! - **Config helpers** (`get_config_usize` / `set_config_usize`) for the small set
//!   of server tunables that outlive any single restart.

use bytes::Bytes;
use crate::environment::environment::PostBundleMetadata;
use anyhow::Context;
use hashiverse_lib::tools::time::{TimeMillis, TimeMillisBytes};
use hashiverse_lib::tools::types::Id;
use std::collections::HashMap;
use hashiverse_lib::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1;

pub trait EnvironmentStore: Sync + Send {
    fn post_bundle_count(&self) -> anyhow::Result<usize>;
    fn post_bundle_feedback_count(&self) -> anyhow::Result<usize>;
    fn post_bundle_metadata_get(&self, location_id: &Id) -> anyhow::Result<Option<PostBundleMetadata>>;
    fn post_bundle_metadata_put(&self, location_id: &Id, post_bundle_metadata: &PostBundleMetadata) -> anyhow::Result<()>;
    fn post_bundle_bytes_get(&self, location_id: &Id) -> anyhow::Result<Option<Bytes>>;
    fn post_bundle_bytes_put(&self, location_id: &Id, bytes: &[u8]) -> anyhow::Result<()>;
    fn post_bundles_last_accessed_flush(&self, post_bundles_last_accessed: &HashMap<Id, TimeMillis>) -> anyhow::Result<()>;
    fn post_bundles_delete(&self, location_ids: &[Id]) -> anyhow::Result<()>;
    fn post_bundles_last_accessed_iter(&self, location_id: &Id) -> Box<dyn Iterator<Item = Result<(Id, TimeMillisBytes), anyhow::Error>> + '_>;
    fn config_get_bytes(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>>;
    fn config_put_bytes(&self, key: &str, v: Vec<u8>) -> anyhow::Result<()>;
    fn config_get_usize(&self, key: &str) -> anyhow::Result<Option<usize>> {
        let Some(bytes_guard) = self.config_get_bytes(key)?
        else {
            return Ok(None);
        };

        let bytes: &[u8] = bytes_guard.as_ref();
        let arr: [u8; size_of::<usize>()] = bytes.try_into().with_context(|| format!("stored data has wrong byte length: {}", bytes.len()))?;

        Ok(Some(usize::from_be_bytes(arr)))
    }
    fn config_put_usize(&self, key: &str, v: usize) -> anyhow::Result<()> {
        self.config_put_bytes(key, v.to_be_bytes().to_vec())?;
        Ok(())
    }

    /// Get all the feedbacks for a given post_bundle_location_id
    ///
    /// Returns a concatenated array of EncodedPostFeedbackV1 - may be []
    fn post_bundle_feedbacks_bytes_get(&self, post_bundle_location_id: &Id) -> anyhow::Result<Bytes>;

    /// Stores the post feedback if it is more powerful than the current feedback
    fn post_feedback_put_if_more_powerful(&self, location_id: &Id, encoded_post_feedback: &EncodedPostFeedbackV1) -> anyhow::Result<()>;
}