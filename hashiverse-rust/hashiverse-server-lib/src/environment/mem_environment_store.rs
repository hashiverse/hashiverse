//! # In-memory [`EnvironmentStore`] for tests
//!
//! Volatile implementation of
//! [`crate::environment::environment_store::EnvironmentStore`] backed by a
//! `HashMap` for bundles/metadata and a `BTreeMap` for feedback (so range queries
//! over composite `(location, post, feedback_type)` keys work identically to the
//! disk store).
//!
//! One subtlety: the decimation path needs to hold a write lock while *also*
//! iterating the bundle set, and Rust's borrow rules don't allow that directly.
//! The module uses an `ouroboros` self-referential struct to safely carry the
//! guard alongside the iterator, matching what the disk store gets from its
//! `fjall` cursor.

use bytes::Bytes;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use async_trait::async_trait;
use log::info;
use ouroboros::self_referencing;
use parking_lot::{RwLock,RwLockReadGuard};
use hashiverse_lib::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1;
use hashiverse_lib::tools::time::{TimeMillis, TimeMillisBytes};
use hashiverse_lib::tools::types::Id;
use crate::environment::environment::{Environment, EnvironmentDimensions, EnvironmentFactory, PostBundleMetadata};
use crate::environment::environment_store::EnvironmentStore;

pub struct MemEnvironmentFactory {
    next_env_id: RwLock<usize>,
}
#[async_trait]
impl EnvironmentFactory for MemEnvironmentFactory {
    fn new(_base_path: &str) -> Self {
        Self {
            next_env_id: RwLock::new(0),
        }
    }
    async fn open_next_available(&self, environment_dimensions: EnvironmentDimensions) -> anyhow::Result<Environment> {
        let env_id = {
            let mut next_env_id = self.next_env_id.write();
            *next_env_id += 1;
            *next_env_id
        };

        let mem_environment_store = Arc::new(MemEnvironmentStore::new(env_id));
        let environment = Environment::new(mem_environment_store, environment_dimensions).await?;
        Ok(environment)
    }
}

/// MemEnvironmentStore implements the `EnvironmentStore` trait for use during testing.  Everything is stored in memory and is volatile to restarts.
pub struct MemEnvironmentStore {
    config: RwLock<HashMap<String, Vec<u8>>>,
    post_bundle_metadata: RwLock<HashMap<Id, Vec<u8>>>,
    post_bundle_last_accessed: RwLock<BTreeMap<Id, TimeMillisBytes>>,
    post_bundle_bytes: RwLock<HashMap<Id, Bytes>>,
    post_bundle_feedbacks: RwLock<HashMap<Id, Vec<EncodedPostFeedbackV1>>> // Map from post_bundle_location_id -> EncodedPostFeedbackV1
}

impl MemEnvironmentStore {
    fn new(env_id: usize) -> Self {
        info!("using MemEnvironmentStore {}", env_id);

        Self {
            config: RwLock::new(HashMap::new()),
            post_bundle_metadata: RwLock::new(HashMap::new()),
            post_bundle_last_accessed: RwLock::new(BTreeMap::new()),
            post_bundle_bytes: RwLock::new(HashMap::new()),
            post_bundle_feedbacks: RwLock::new(HashMap::new()),
        }
    }
}

impl EnvironmentStore for MemEnvironmentStore {

    fn post_bundle_count(&self) -> anyhow::Result<usize> {
        let len = self.post_bundle_last_accessed.read().len();
        Ok(len)
    }

    fn post_bundle_feedback_count(&self) -> anyhow::Result<usize> {
        let total: usize = self.post_bundle_feedbacks.read().values().map(|entries| entries.len()).sum();
        Ok(total)
    }

    fn post_bundle_metadata_get(&self, location_id: &Id) -> anyhow::Result<Option<PostBundleMetadata>> {
        let post_bundle_metadata = self.post_bundle_metadata.read();
        let bytes = post_bundle_metadata.get(location_id);
        match bytes {
            Some(bytes) => Ok(Some(postcard::from_bytes(bytes)?)),
            None => Ok(None),
        }
    }

    fn post_bundle_metadata_put(&self, location_id: &Id, post_bundle_metadata: &PostBundleMetadata) -> anyhow::Result<()> {
        let bytes = postcard::to_stdvec(post_bundle_metadata)?;
        self.post_bundle_metadata.write().insert(*location_id, bytes.to_vec());
        Ok(())
    }

    fn post_bundle_bytes_get(&self, location_id: &Id) -> anyhow::Result<Option<Bytes>> {
        Ok(self.post_bundle_bytes.read().get(location_id).cloned())
    }

    fn post_bundle_bytes_put(&self, location_id: &Id, bytes: &[u8]) -> anyhow::Result<()> {
        self.post_bundle_bytes.write().insert(*location_id, Bytes::copy_from_slice(bytes));
        Ok(())
    }

    fn post_bundles_last_accessed_flush(&self, post_bundles_last_accessed: &HashMap<Id, TimeMillis>) -> anyhow::Result<()> {
        let mut self_post_bundle_last_accessed = self.post_bundle_last_accessed.write();
        for (location_id, time_millis) in post_bundles_last_accessed.iter() {
            self_post_bundle_last_accessed.insert(*location_id, time_millis.encode_be());
        }
        Ok(())
    }

    fn post_bundles_delete(&self, location_ids: &[Id]) -> anyhow::Result<()> {
        let mut post_bundle_metadata = self.post_bundle_metadata.write();
        let mut post_bundle_last_accessed = self.post_bundle_last_accessed.write();
        let mut post_bundle_bytes = self.post_bundle_bytes.write();
        let mut post_bundle_feedbacks = self.post_bundle_feedbacks.write();

        for location_id in location_ids {
            post_bundle_metadata.remove(location_id);
            post_bundle_last_accessed.remove(location_id);
            post_bundle_bytes.remove(location_id);
            post_bundle_feedbacks.remove(location_id);
        }

        Ok(())
    }

    fn post_bundles_last_accessed_iter(&self, location_id: &Id) -> Box<dyn Iterator<Item = Result<(Id, TimeMillisBytes), anyhow::Error>> + '_> {
        let post_bundle_last_accessed = self.post_bundle_last_accessed.read();

        let iter = LockedPostBundleIterBuilder {
            guard: post_bundle_last_accessed,
            inner_builder: |post_bundle_last_accessed| {
                let it = post_bundle_last_accessed.range(location_id..)
                    .chain(post_bundle_last_accessed.range(..location_id))
                    .map(|(k, v)| Ok((*k, *v)));
                Box::new(it)
            },
        }.build();

        Box::new(iter)
    }

    fn config_get_bytes(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.config.read().get(key).map(|v| v.to_vec()))
    }

    fn config_put_bytes(&self, key: &str, v: Vec<u8>) -> anyhow::Result<()> {
        self.config.write().insert(key.to_string(), v);
        Ok(())
    }

    fn post_bundle_feedbacks_bytes_get(&self, post_bundle_location_id: &Id) -> anyhow::Result<Bytes> {
        let mut bytes = Vec::new();
        let feedbacks = self.post_bundle_feedbacks.read();
        if let Some(entries) = feedbacks.get(post_bundle_location_id) {
            for f in entries {
                EncodedPostFeedbackV1::append_encode_direct_to_bytes(&mut bytes, f.post_id.as_ref(), f.feedback_type, f.salt.as_ref(), f.pow)?;
            }
        }
        Ok(Bytes::from(bytes))
    }

    fn post_feedback_put_if_more_powerful(&self, location_id: &Id, encoded_post_feedback: &EncodedPostFeedbackV1) -> anyhow::Result<()> {
        let mut feedbacks = self.post_bundle_feedbacks.write();
        let entries = feedbacks.entry(*location_id).or_default();

        // Find existing entry for this (post_id, feedback_type) pair
        if let Some(existing) = entries.iter_mut().find(|f| f.post_id == encoded_post_feedback.post_id && f.feedback_type == encoded_post_feedback.feedback_type) {
            if existing.pow >= encoded_post_feedback.pow {
                // Already have equal or better feedback — do not downgrade
                return Ok(());
            }
            *existing = encoded_post_feedback.clone();
        } else {
            entries.push(encoded_post_feedback.clone());
        }

        Ok(())
    }
}


#[self_referencing]
pub struct LockedPostBundleIter<'a> {
    guard: RwLockReadGuard<'a, BTreeMap<Id, TimeMillisBytes>>,

    #[borrows(guard)]
    #[not_covariant] // Required for iterators
    inner: Box<dyn Iterator<Item = Result<(Id, TimeMillisBytes), anyhow::Error>> + 'this>,
}

impl<'a> Iterator for LockedPostBundleIter<'a> {
    type Item = Result<(Id, TimeMillisBytes), anyhow::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.with_inner_mut(|it| it.next())
    }
}



#[cfg(test)]
mod tests {
    use crate::environment;
    use crate::environment::mem_environment_store::MemEnvironmentFactory;

    #[tokio::test]
    async fn basics_test() -> anyhow::Result<()> {
        environment::environment::tests::basics_test::<MemEnvironmentFactory>().await
    }

    #[tokio::test]
    async fn decimation_test() -> anyhow::Result<()> {
        environment::environment::tests::decimation_test::<MemEnvironmentFactory>().await
    }

    #[tokio::test]
    async fn decimation_convergence_test() -> anyhow::Result<()> {
        environment::environment::tests::decimation_convergence_test::<MemEnvironmentFactory>(128 * 1000).await
    }
    #[tokio::test]
    async fn decimation_existence_test() -> anyhow::Result<()> {
        environment::environment::tests::decimation_existence_test::<MemEnvironmentFactory>().await
    }
    #[tokio::test]
    async fn decimation_feedback_deleted_test() -> anyhow::Result<()> {
        environment::environment::tests::decimation_feedback_deleted_test::<MemEnvironmentFactory>().await
    }

    #[tokio::test]
    async fn feedback_bytes_get_test() -> anyhow::Result<()> {
        environment::environment::tests::feedback_bytes_get_test::<MemEnvironmentFactory>().await
    }

    #[tokio::test]
    async fn feedback_put_if_more_powerful_test() -> anyhow::Result<()> {
        environment::environment::tests::feedback_put_if_more_powerful_test::<MemEnvironmentFactory>().await
    }
}
