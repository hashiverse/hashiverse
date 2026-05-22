//! # Server-side data environment — persistence, quotas, decimation
//!
//! [`Environment`] is the facade every handler in
//! [`crate::server`] writes through when it needs to read or mutate persisted
//! data: post bundles, per-bundle metadata, feedback, and server config. It wraps a
//! pluggable [`crate::environment::environment_store::EnvironmentStore`] and layers
//! on top:
//!
//! - **Striped locks** — 256 mutexes keyed by the first byte of the bundle id, so
//!   unrelated bundles never contend. Hot locations stay local to their own stripe.
//! - **Quota enforcement** — disk usage is tracked against the operator's configured
//!   cap (from [`crate::server::args::Args`]). When the cap is crossed, decimation
//!   runs as an offline `tokio::task::spawn_blocking` job, evicting the
//!   least-recently-accessed bundles until the store is back under quota — without
//!   stalling the live request loop.
//! - **Feedback merging** — writes are monotonic: a new feedback entry is kept only
//!   if its proof-of-work is at least as strong as any existing entry for the same
//!   `(location, post, feedback_type)`, so sybil floods can't overwrite real votes.

use crate::environment::environment_store::EnvironmentStore;
use async_trait::async_trait;
use bytes::Bytes;
use hashiverse_lib::tools::time::TimeMillis;
use hashiverse_lib::tools::types::Id;
use log::{info, warn};
use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use serde::{Deserialize, Serialize};
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use serde::de::DeserializeOwned;
use tokio_util::sync::CancellationToken;
use hashiverse_lib::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1;
use hashiverse_lib::tools::{compression, json};

/// An environment is a 'folder' location that contains everything to persist a server
///  - e.g. config , keys, posts, etc
pub const CONFIG_SERVER_ID: &str = "server_id";
pub const CONFIG_KADEMLIA_PEER_BUCKETS: &str = "kademlia_peer_buckets";
pub const CONFIG_POST_BUNDLE_CURRENT_SIZE_BYTES: &str = "post_bundle_current_size_bytes";

#[derive(Clone, Copy)]
pub struct EnvironmentDimensions {
    pub max_size_bytes: usize,
    pub max_quota_used: f64,
}

impl EnvironmentDimensions {
    pub fn with_max_size_bytes(&self, max_size_bytes: usize) -> Self {
        Self { max_size_bytes, ..*self }
    }
}

impl Default for EnvironmentDimensions {
    fn default() -> Self {
        Self {
            max_size_bytes: 20 * 1024 * 1024 * 1024,
            max_quota_used: 0.9,
        }
    }
}

#[async_trait]
pub trait EnvironmentFactory: Send + Sync {
    fn new(base_path: &str) -> Self
    where
        Self: Sized;
    async fn open_next_available(&self, environment_dimensions: EnvironmentDimensions) -> anyhow::Result<Environment>;
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct PostBundleMetadata {
    pub size: usize,
    pub num_posts: u8,
    pub num_posts_granted: u8,
    pub overflowed: bool,
    pub sealed: bool,
}

impl PostBundleMetadata {
    pub fn zero() -> PostBundleMetadata {
        Self {
            size: 0,
            num_posts: 0,
            num_posts_granted: 0,
            overflowed: false,
            sealed: false,
        }
    }
}

pub struct Environment {
    environment_dimensions: EnvironmentDimensions,
    environment_store: Arc<dyn EnvironmentStore>,

    post_bundle_locks: [RwLock<()>; 256], // We do striped locking to allow better parallelism
    post_bundles_last_touched_batch: RwLock<HashMap<Id, TimeMillis>>,
    post_bundle_current_size_bytes: AtomicUsize,
    decimation_lock: RwLock<()>, // Allows only one decimation process at a time
}

impl Environment {
    pub async fn new(environment_store: Arc<dyn EnvironmentStore>, environment_dimensions: EnvironmentDimensions) -> anyhow::Result<Environment> {
        let post_bundle_locks: [RwLock<()>; 256] = std::array::from_fn(|_| RwLock::new(()));
        let post_bundles_last_touched = RwLock::new(HashMap::new());
        let post_bundle_current_size_bytes = environment_store.config_get_usize(CONFIG_POST_BUNDLE_CURRENT_SIZE_BYTES)?.unwrap_or(0);
        let post_bundle_current_size_bytes = AtomicUsize::new(post_bundle_current_size_bytes);

        Ok(Environment {
            environment_dimensions,
            environment_store,
            post_bundle_locks,
            post_bundles_last_touched_batch: post_bundles_last_touched,
            post_bundle_current_size_bytes,
            decimation_lock: RwLock::new(()),
        })
    }

    pub fn get_read_lock_for_location_id(&self, location_id: &Id) -> RwLockReadGuard<'_, ()> {
        self.post_bundle_locks[location_id.0[0] as usize].read()
    }

    pub fn get_write_lock_for_location_id(&self, location_id: &Id) -> RwLockWriteGuard<'_, ()> {
        self.post_bundle_locks[location_id.0[0] as usize].write()
    }

    pub fn config_get_bytes(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        self.environment_store.config_get_bytes(key)
    }

    pub fn config_put_bytes(&self, key: &str, v: Vec<u8>) -> anyhow::Result<()> {
        self.environment_store.config_put_bytes(key, v)
    }

    pub fn config_put_struct<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<()> {
        let bytes = json::struct_to_bytes(value)?;
        let bytes_compressed = compression::compress_for_speed(&bytes)?.to_bytes();
        self.config_put_bytes(key, bytes_compressed.to_vec())
    }

    pub fn config_get_struct<T: DeserializeOwned>(&self, key: &str) -> anyhow::Result<Option<T>> {
        let result = self.config_get_bytes(key)?;
        match result {
            Some(bytes_compressed) => {
                let bytes = compression::decompress(&bytes_compressed)?.to_bytes();
                let value = json::bytes_to_struct::<T>(&bytes)?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }


    pub fn get_post_bundle_metadata(&self, time_millis: TimeMillis, location_id: &Id) -> anyhow::Result<Option<PostBundleMetadata>> {
        self.post_bundles_last_touched_batch.write().insert(*location_id, time_millis);
        self.environment_store.post_bundle_metadata_get(location_id)
    }

    pub fn get_post_bundle_bytes(&self, time_millis: TimeMillis, location_id: &Id) -> anyhow::Result<Option<Bytes>> {
        self.post_bundles_last_touched_batch.write().insert(*location_id, time_millis);
        self.environment_store.post_bundle_bytes_get(location_id)
    }

    pub fn get_post_bundle_encoded_post_feedbacks_bytes(&self, time_millis: TimeMillis, location_id: &Id) -> anyhow::Result<Bytes> {
        self.post_bundles_last_touched_batch.write().insert(*location_id, time_millis);
        self.environment_store.post_bundle_feedbacks_bytes_get(location_id)
    }
    
    pub fn put_post_feedback_if_more_powerful(&self, time_millis: TimeMillis, location_id: &Id, encoded_post_feedback: &EncodedPostFeedbackV1) -> anyhow::Result<()> {
        self.post_bundles_last_touched_batch.write().insert(*location_id, time_millis);
        self.environment_store.post_feedback_put_if_more_powerful(location_id, encoded_post_feedback)
    }

    pub fn put_post_bundle_bytes(&self, time_millis: TimeMillis, location_id: &Id, bytes: &[u8]) -> anyhow::Result<()> {
        self.post_bundles_last_touched_batch.write().insert(*location_id, time_millis);
        self.environment_store.post_bundle_bytes_put(location_id, bytes)?;
        Ok(())
    }

    pub fn put_post_bundle_metadata(&self, time_millis: TimeMillis, location_id: &Id, post_bundle_metadata: &PostBundleMetadata, additional_bytes: usize) -> anyhow::Result<()> {
        self.post_bundles_last_touched_batch.write().insert(*location_id, time_millis);
        self.environment_store.post_bundle_metadata_put(location_id, post_bundle_metadata)?;
        self.post_bundle_current_size_bytes.fetch_add(additional_bytes, Ordering::Relaxed);
        Ok(())
    }

    fn post_bundle_current_size_bytes(&self) -> usize {
        self.post_bundle_current_size_bytes.load(Ordering::Relaxed)
    }

    /// Total on-disk bytes occupied by stored post bundles, as tracked by the
    /// quota counter. Exposed for the server-stats RPC.
    pub fn post_bundle_total_bytes(&self) -> usize {
        self.post_bundle_current_size_bytes()
    }

    /// Number of post bundles currently in the store.
    pub fn post_bundle_count(&self) -> anyhow::Result<usize> {
        self.environment_store.post_bundle_count()
    }

    /// Number of distinct (location, post, feedback_type) feedback entries currently
    /// in the store.
    pub fn post_bundle_feedback_count(&self) -> anyhow::Result<usize> {
        self.environment_store.post_bundle_feedback_count()
    }

    pub async fn do_maintenance(self: &Arc<Self>, cancellation_token: &CancellationToken, time_millis: TimeMillis) -> anyhow::Result<()> {
        // log::trace!("do_maintenance");

        // Push the current total disk used to store
        let post_bundle_current_size_bytes = self.post_bundle_current_size_bytes();
        self.environment_store.config_put_usize(CONFIG_POST_BUNDLE_CURRENT_SIZE_BYTES, post_bundle_current_size_bytes)?;

        // Flush the last_accessed times to store
        {
            let mut post_bundles_last_touched = self.post_bundles_last_touched_batch.write();
            if !post_bundles_last_touched.is_empty() {
                log::trace!("Flushing {} last accessed post bundles", post_bundles_last_touched.len());
                self.environment_store.post_bundles_last_accessed_flush(&post_bundles_last_touched)?;
                post_bundles_last_touched.clear();
            }
        }

        // Have we passed our allowed quota of available space?
        let quota_used = post_bundle_current_size_bytes as f64 / self.environment_dimensions.max_size_bytes as f64;
        if quota_used > self.environment_dimensions.max_quota_used {
            let env = self.clone();
            let cancellation_token = cancellation_token.clone();
            let max_quota_used = self.environment_dimensions.max_quota_used;
            tokio::task::spawn_blocking(move || env.do_decimation(cancellation_token, time_millis, quota_used, max_quota_used)).await??;
        }

        Ok(())
    }

    fn do_decimation(self: &Arc<Self>, cancellation_token: CancellationToken, _time_millis: TimeMillis, quota_used: f64, max_quota_used: f64) -> anyhow::Result<()> {
        let lock = self.decimation_lock.try_read();
        if lock.is_none() {
            log::trace!("A decimation is already in progress.");
            return Ok(());
        }

        // Make sure we flush our size update to the database
        scopeguard::defer! {
            let post_bundle_current_size_bytes = self.post_bundle_current_size_bytes();
            let _ = self.environment_store.config_put_usize(CONFIG_POST_BUNDLE_CURRENT_SIZE_BYTES, post_bundle_current_size_bytes);
        }

        // Work out how much we need to decimate
        let total_rows = self.environment_store.post_bundle_count()?;
        let decimation_count = (total_rows as f64) * (1f64 - 0.98f64 * max_quota_used / quota_used);

        if 0.0 >= decimation_count {
            warn!("Decimation count is unexpectedly zero, skipping decimation");
            return Ok(());
        }

        // Find a neatish rounding of batches and rows per batch
        let rows_per_batch = 100f64.min(decimation_count);
        let num_batches = decimation_count / rows_per_batch;
        let num_batches = num_batches.ceil() as usize;
        let rows_per_batch = decimation_count as usize / num_batches;

        info!("Decimation of {}/{} items will be done in {} batches of {} deletes each", decimation_count, total_rows, num_batches, rows_per_batch);
        let post_bundle_current_size_bytes_before = self.post_bundle_current_size_bytes();
        info!(
            "Total size before decimation is {} ({}%)",
            post_bundle_current_size_bytes_before,
            100 * post_bundle_current_size_bytes_before / self.environment_dimensions.max_size_bytes
        );

        for batch in 0..num_batches {
            if cancellation_token.is_cancelled() {
                break;
            }

            // Who will we kill
            let mut heap_of_locations_to_decimate = BinaryHeap::new();
            {
                let random_prefix = Id::random();
                log::trace!("Decimation batch {} prefix: {}", batch, random_prefix);
                self.environment_store.post_bundles_last_accessed_iter(&random_prefix).take(5 * rows_per_batch).for_each(|pair| {
                    match pair {
                        Ok((location_id, time_millis_bytes)) => {
                            if heap_of_locations_to_decimate.len() >= rows_per_batch {
                                let &(most_recent_time_millis_bytes, _) = heap_of_locations_to_decimate.peek().expect("should always work as we have checked the length");
                                if most_recent_time_millis_bytes > time_millis_bytes {
                                    heap_of_locations_to_decimate.pop();
                                    heap_of_locations_to_decimate.push((time_millis_bytes, location_id));
                                }
                            }
                            else {
                                heap_of_locations_to_decimate.push((time_millis_bytes, location_id));
                            }
                        }
                        Err(e) => {
                            warn!("Error while decimating: {}", e);
                        }
                    }
                });
            }

            // Kill the victims
            {
                let mut location_ids: Vec<Id> = Vec::new();
                {
                    let mut post_bundles_last_touched = self.post_bundles_last_touched_batch.write();

                    for (_time_millis_bytes, location_id) in heap_of_locations_to_decimate {
                        location_ids.push(location_id);
                        post_bundles_last_touched.remove(&location_id);

                        let metadata = self.environment_store.post_bundle_metadata_get(&location_id);
                        if let Ok(metadata) = metadata {
                            if let Some(metadata) = metadata {
                                self.post_bundle_current_size_bytes.fetch_sub(metadata.size, Ordering::Relaxed);
                            }
                        }
                    }
                }

                self.environment_store.post_bundles_delete(&location_ids)?;
            }
        }

        let post_bundle_current_size_bytes_after = self.post_bundle_current_size_bytes();
        info!(
            "Total size after decimation is {} ({}%): delta={}",
            post_bundle_current_size_bytes_after,
            100 * post_bundle_current_size_bytes_after / self.environment_dimensions.max_size_bytes,
            post_bundle_current_size_bytes_before - post_bundle_current_size_bytes_after
        );

        Ok(())
    }
}

// ----------------------------------------------------------------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use crate::environment::environment::{EnvironmentDimensions, EnvironmentFactory, PostBundleMetadata};
    use hashiverse_lib::anyhow_assert_ge;
    use hashiverse_lib::tools::tools;
    use hashiverse_lib::tools::tools::get_temp_dir;
    use hashiverse_lib::tools::types::Id;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;
    use hashiverse_lib::tools::time_provider::time_provider::{ScaledTimeProvider, TimeProvider};

    pub async fn basics_test<TEnvironmentFactory: EnvironmentFactory>() -> anyhow::Result<()> {
        let time_provider = Arc::new(ScaledTimeProvider::new(60.0));
        // configure_logging_with_time_provider("trace", time_provider.clone());

        let (_temp_dir, temp_dir_path) = get_temp_dir()?;
        let environment_dimensions = EnvironmentDimensions::default().with_max_size_bytes(1024 * 1024);
        let environment_factory = TEnvironmentFactory::new(&temp_dir_path);
        let environment = environment_factory.open_next_available(environment_dimensions).await?;

        let delete_post_bundle = |location_id: &Id| -> anyhow::Result<()> {
            let vec = [*location_id].to_vec();
            environment.environment_store.post_bundles_delete(&vec)
        };

        // Put some metadata
        {
            let num_bytes = 16 * 1024;
            let time_millis = time_provider.current_time_millis();
            let location_id = Id::random();
            let post_bundle_metadata = PostBundleMetadata::zero();
            environment.put_post_bundle_metadata(time_millis, &location_id, &post_bundle_metadata, num_bytes)?;

            let post_bundle_metadata_check = environment.get_post_bundle_metadata(time_millis, &location_id)?;
            assert_eq!(post_bundle_metadata_check, Some(post_bundle_metadata));

            delete_post_bundle(&location_id)?;
            let post_bundle_metadata_check = environment.get_post_bundle_metadata(time_millis, &location_id)?;
            assert_eq!(post_bundle_metadata_check, None);
        }

        // Put some bytes
        {
            let num_bytes = 16 * 1024;
            let time_millis = time_provider.current_time_millis();
            let location_id = Id::random();
            let bytes = tools::random_bytes(num_bytes);
            environment.put_post_bundle_bytes(time_millis, &location_id, &bytes)?;
            assert_eq!(num_bytes, environment.post_bundle_current_size_bytes());

            let bytes_check = environment.get_post_bundle_bytes(time_millis, &location_id)?;
            assert_eq!(bytes_check, Some(Bytes::from(bytes)));

            delete_post_bundle(&location_id)?;
            let bytes_check = environment.get_post_bundle_bytes(time_millis, &location_id)?;
            assert_eq!(bytes_check, None);
        }

        Ok(())
    }

    fn get_random_post_bundle(max_size: usize) -> (Id, PostBundleMetadata, Vec<u8>) {
        let num_bytes = tools::random_usize_bounded(max_size);
        let location_id = Id::random();
        let bytes = tools::random_bytes(num_bytes);
        let mut post_bundle_metadata = PostBundleMetadata::zero();
        post_bundle_metadata.size = num_bytes;

        (location_id, post_bundle_metadata, bytes)
    }

    pub async fn decimation_test<TEnvironmentFactory: EnvironmentFactory>() -> anyhow::Result<()> {
        let time_provider = Arc::new(ScaledTimeProvider::new(60.0));
        // configure_logging_with_time_provider("trace", time_provider.clone());
        let cancellation_token = CancellationToken::new();

        let (_temp_dir, temp_dir_path) = get_temp_dir()?;
        let environment_dimensions = EnvironmentDimensions::default().with_max_size_bytes(4 * 1024 * 1024);
        let environment_factory = TEnvironmentFactory::new(&temp_dir_path);
        let environment = Arc::new(environment_factory.open_next_available(environment_dimensions).await?);

        // Post to many locations
        let num_iterations = 10000;
        for i in 0..num_iterations {
            let time_millis = time_provider.current_time_millis();
            let (location_id, post_bundle_metadata, _bytes) = get_random_post_bundle(8 * 1024);

            environment.put_post_bundle_metadata(time_millis, &location_id, &post_bundle_metadata, post_bundle_metadata.size)?;
            // We dont actually have to write the bundles to disk for this test - we just want to confirm the eviction is taking place...
            // environment.put_post_bundle_bytes(time_millis, &location_id, &bytes)?;

            if 0 == i % 10 {
                environment.do_maintenance(&cancellation_token, time_millis).await?;
            }
        }

        {
            let time_millis = time_provider.current_time_millis();
            environment.do_maintenance(&cancellation_token, time_millis).await?;

            anyhow_assert_ge!(environment_dimensions.max_size_bytes, environment.post_bundle_current_size_bytes());
        }

        Ok(())
    }

    pub async fn decimation_convergence_test<TEnvironmentFactory: EnvironmentFactory>(num_posts: usize) -> anyhow::Result<()> {
        let time_provider = Arc::new(ScaledTimeProvider::new(60.0));
        // configure_logging_with_time_provider("trace", time_provider.clone());
        let cancellation_token = CancellationToken::new();

        let (_temp_dir, temp_dir_path) = get_temp_dir()?;
        let environment_dimensions = EnvironmentDimensions::default().with_max_size_bytes(4 * 1024 * 1024);
        let environment_factory = TEnvironmentFactory::new(&temp_dir_path);
        let environment = Arc::new(environment_factory.open_next_available(environment_dimensions).await?);

        // Post to many locations
        for i in 0..num_posts {
            let time_millis = time_provider.current_time_millis();
            let (location_id, post_bundle_metadata, _bytes) = get_random_post_bundle(1024);

            environment.put_post_bundle_metadata(time_millis, &location_id, &post_bundle_metadata, post_bundle_metadata.size)?;
            // We dont actually have to write the bundles to disk for this test - we just want to confirm the eviction is taking place...
            //environment.put_post_bundle_bytes(time_millis, &location_id, &bytes)?;

            if 0 == i % 100 {
                environment.do_maintenance(&cancellation_token, time_millis).await?;
            }
        }

        {
            let time_millis = time_provider.current_time_millis();
            environment.do_maintenance(&cancellation_token, time_millis).await?;
            assert!(environment.post_bundle_current_size_bytes() < environment_dimensions.max_size_bytes * 11 / 10);
        }

        Ok(())
    }

    pub async fn decimation_existence_test<TEnvironmentFactory: EnvironmentFactory>() -> anyhow::Result<()> {
        let time_provider = Arc::new(ScaledTimeProvider::new(60.0));
        // configure_logging_with_time_provider("trace", time_provider.clone());
        let cancellation_token = CancellationToken::new();

        let (_temp_dir, temp_dir_path) = get_temp_dir()?;
        let environment_dimensions = EnvironmentDimensions::default().with_max_size_bytes(4 * 1024 * 1024);
        let environment_factory = TEnvironmentFactory::new(&temp_dir_path);
        let environment = Arc::new(environment_factory.open_next_available(environment_dimensions).await?);

        let time_millis = time_provider.current_time_millis();

        let location_id_should_exist_metadata = {
            let (location_id, post_bundle_metadata, bytes) = get_random_post_bundle(1024);
            environment.put_post_bundle_metadata(time_millis, &location_id, &post_bundle_metadata, post_bundle_metadata.size)?;
            environment.put_post_bundle_bytes(time_millis, &location_id, &bytes)?;
            location_id
        };

        let location_id_should_exist_bytes = {
            let (location_id, post_bundle_metadata, bytes) = get_random_post_bundle(1024);
            environment.put_post_bundle_metadata(time_millis, &location_id, &post_bundle_metadata, post_bundle_metadata.size)?;
            environment.put_post_bundle_bytes(time_millis, &location_id, &bytes)?;
            location_id
        };

        let location_id_should_not_exist = {
            let (location_id, post_bundle_metadata, bytes) = get_random_post_bundle(1024);
            environment.put_post_bundle_metadata(time_millis, &location_id, &post_bundle_metadata, post_bundle_metadata.size)?;
            environment.put_post_bundle_bytes(time_millis, &location_id, &bytes)?;
            location_id
        };

        // Post to many locations
        let num_iterations = 128 * 1000;
        for i in 0..num_iterations {
            let time_millis = time_provider.current_time_millis();
            let (location_id, post_bundle_metadata, _bytes) = get_random_post_bundle(1024);

            environment.put_post_bundle_metadata(time_millis, &location_id, &post_bundle_metadata, post_bundle_metadata.size)?;
            // We dont actually have to write the bundles to disk for this test - we just want to confirm the eviction is taking place...
            //environment.put_post_bundle_bytes(time_millis, &location_id, &bytes)?;

            // Lets keep some posts active
            if 0 == i % 30 {
                let _ = environment.get_post_bundle_metadata(time_millis, &location_id_should_exist_metadata)?;
                let _ = environment.get_post_bundle_bytes(time_millis, &location_id_should_exist_bytes);
            }

            if 0 == i % 100 {
                environment.do_maintenance(&cancellation_token, time_millis).await?;
            }
        }

        {
            let time_millis = time_provider.current_time_millis();
            environment.do_maintenance(&cancellation_token, time_millis).await?;
            assert!(environment.post_bundle_current_size_bytes() < environment_dimensions.max_size_bytes * 11 / 10);
        }

        // Check the things that should still exist
        {
            // Existence by metadata queries
            {
                let result = environment.get_post_bundle_metadata(time_millis, &location_id_should_exist_metadata)?;
                assert!(result.is_some());
            }
            {
                let result = environment.get_post_bundle_bytes(time_millis, &location_id_should_exist_metadata)?;
                assert!(result.is_some());
            }
        }
        {
            // Existence by bytes queries
            {
                let result = environment.get_post_bundle_metadata(time_millis, &location_id_should_exist_bytes)?;
                assert!(result.is_some());
            }
            {
                let result = environment.get_post_bundle_bytes(time_millis, &location_id_should_exist_bytes)?;
                assert!(result.is_some());
            }
        }

        // Check the thing that should have died
        {
            {
                let result = environment.get_post_bundle_metadata(time_millis, &location_id_should_not_exist)?;
                assert!(result.is_none());
            }
            {
                let result = environment.get_post_bundle_bytes(time_millis, &location_id_should_not_exist)?;
                assert!(result.is_none());
            }
        }

        Ok(())
    }

    pub async fn decimation_feedback_deleted_test<TEnvironmentFactory: EnvironmentFactory>() -> anyhow::Result<()> {
        let time_provider = Arc::new(ScaledTimeProvider::new(60.0));
        let cancellation_token = CancellationToken::new();

        let (_temp_dir, temp_dir_path) = get_temp_dir()?;
        let environment_dimensions = EnvironmentDimensions::default().with_max_size_bytes(4 * 1024 * 1024);
        let environment_factory = TEnvironmentFactory::new(&temp_dir_path);
        let environment = Arc::new(environment_factory.open_next_available(environment_dimensions).await?);
        let store = &environment.environment_store;

        let time_millis = time_provider.current_time_millis();

        // Create a bundle and attach feedback to it
        let (location_id, post_bundle_metadata, bytes) = get_random_post_bundle(1024);
        environment.put_post_bundle_metadata(time_millis, &location_id, &post_bundle_metadata, post_bundle_metadata.size)?;
        environment.put_post_bundle_bytes(time_millis, &location_id, &bytes)?;

        let feedback = EncodedPostFeedbackV1::new(Id::random(), 1, Salt::random(), Pow(10));
        store.post_feedback_put_if_more_powerful(&location_id, &feedback)?;
        assert!(!store.post_bundle_feedbacks_bytes_get(&location_id)?.is_empty());

        // Flood with bundles (never re-access ours) to trigger decimation
        for i in 0..128 * 1000usize {
            let time_millis = time_provider.current_time_millis();
            let (flood_id, flood_meta, _) = get_random_post_bundle(1024);
            environment.put_post_bundle_metadata(time_millis, &flood_id, &flood_meta, flood_meta.size)?;
            if i % 100 == 0 {
                environment.do_maintenance(&cancellation_token, time_millis).await?;
            }
        }
        environment.do_maintenance(&cancellation_token, time_provider.current_time_millis()).await?;

        // The bundle must have been evicted
        assert!(environment.get_post_bundle_metadata(time_millis, &location_id)?.is_none(), "bundle should have been decimated");

        // Its feedback must be gone too
        assert!(store.post_bundle_feedbacks_bytes_get(&location_id)?.is_empty(), "feedback should have been deleted with the bundle");

        Ok(())
    }

    // ── feedback tests ────────────────────────────────────────────────────────

    use bytes::{Buf, Bytes};
    use hashiverse_lib::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1;
    use hashiverse_lib::tools::types::{Pow, Salt};

    fn decode_all_feedbacks(mut bytes: Bytes) -> anyhow::Result<Vec<EncodedPostFeedbackV1>> {
        let mut result = Vec::new();
        while bytes.has_remaining() {
            result.push(EncodedPostFeedbackV1::decode_from_bytes(&mut bytes)?);
        }
        Ok(result)
    }

    pub async fn feedback_bytes_get_test<TEnvironmentFactory: EnvironmentFactory>() -> anyhow::Result<()> {
        let (_temp_dir, temp_dir_path) = get_temp_dir()?;
        let factory = TEnvironmentFactory::new(&temp_dir_path);
        let env = factory.open_next_available(EnvironmentDimensions::default()).await?;
        let store = &env.environment_store;

        let location_id = Id::random();
        let other_location_id = Id::random();

        // Empty: unknown location returns empty bytes
        assert!(store.post_bundle_feedbacks_bytes_get(&location_id)?.is_empty());

        let f1 = EncodedPostFeedbackV1::new(Id::random(), 1, Salt::random(), Pow(10));
        let f2 = EncodedPostFeedbackV1::new(Id::random(), 2, Salt::random(), Pow(20));
        let f_other = EncodedPostFeedbackV1::new(Id::random(), 1, Salt::random(), Pow(5));

        store.post_feedback_put_if_more_powerful(&location_id, &f1)?;
        store.post_feedback_put_if_more_powerful(&location_id, &f2)?;
        store.post_feedback_put_if_more_powerful(&other_location_id, &f_other)?;

        // Correct entries returned for location_id
        let bytes = store.post_bundle_feedbacks_bytes_get(&location_id)?;
        let mut decoded = decode_all_feedbacks(bytes)?;
        decoded.sort_by_key(|f| f.feedback_type);
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0], f1);
        assert_eq!(decoded[1], f2);

        // Other location unaffected
        let other_bytes = store.post_bundle_feedbacks_bytes_get(&other_location_id)?;
        let other_decoded = decode_all_feedbacks(other_bytes)?;
        assert_eq!(other_decoded.len(), 1);
        assert_eq!(other_decoded[0], f_other);

        Ok(())
    }

    pub async fn feedback_put_if_more_powerful_test<TEnvironmentFactory: EnvironmentFactory>() -> anyhow::Result<()> {
        let (_temp_dir, temp_dir_path) = get_temp_dir()?;
        let factory = TEnvironmentFactory::new(&temp_dir_path);
        let env = factory.open_next_available(EnvironmentDimensions::default()).await?;
        let store = &env.environment_store;

        let location_id = Id::random();
        let post_id = Id::random();
        let feedback_type = 3u8;

        let get_single = |pow_expected: Pow| -> anyhow::Result<()> {
            let bytes = store.post_bundle_feedbacks_bytes_get(&location_id)?;
            let decoded = decode_all_feedbacks(bytes)?;
            assert_eq!(decoded.len(), 1);
            assert_eq!(decoded[0].post_id, post_id);
            assert_eq!(decoded[0].feedback_type, feedback_type);
            assert_eq!(decoded[0].pow, pow_expected);
            Ok(())
        };

        // First put: stored
        let f_low = EncodedPostFeedbackV1::new(post_id, feedback_type, Salt::random(), Pow(10));
        store.post_feedback_put_if_more_powerful(&location_id, &f_low)?;
        get_single(Pow(10))?;

        // Higher pow: replaces
        let f_high = EncodedPostFeedbackV1::new(post_id, feedback_type, Salt::random(), Pow(20));
        store.post_feedback_put_if_more_powerful(&location_id, &f_high)?;
        get_single(Pow(20))?;

        // Equal pow: existing kept (salt unchanged)
        let salt_before = decode_all_feedbacks(store.post_bundle_feedbacks_bytes_get(&location_id)?)?[0].salt;
        let f_equal = EncodedPostFeedbackV1::new(post_id, feedback_type, Salt::random(), Pow(20));
        store.post_feedback_put_if_more_powerful(&location_id, &f_equal)?;
        let salt_after = decode_all_feedbacks(store.post_bundle_feedbacks_bytes_get(&location_id)?)?[0].salt;
        assert_eq!(salt_before, salt_after);

        // Lower pow: existing kept
        let f_weaker = EncodedPostFeedbackV1::new(post_id, feedback_type, Salt::random(), Pow(5));
        store.post_feedback_put_if_more_powerful(&location_id, &f_weaker)?;
        get_single(Pow(20))?;

        // Different feedback_type for same post: stored as a separate entry
        let f_other_type = EncodedPostFeedbackV1::new(post_id, feedback_type + 1, Salt::random(), Pow(1));
        store.post_feedback_put_if_more_powerful(&location_id, &f_other_type)?;
        let all = decode_all_feedbacks(store.post_bundle_feedbacks_bytes_get(&location_id)?)?;
        assert_eq!(all.len(), 2);

        // Different post_id for same feedback_type: stored as a separate entry
        let f_other_post = EncodedPostFeedbackV1::new(Id::random(), feedback_type, Salt::random(), Pow(1));
        store.post_feedback_put_if_more_powerful(&location_id, &f_other_post)?;
        let all = decode_all_feedbacks(store.post_bundle_feedbacks_bytes_get(&location_id)?)?;
        assert_eq!(all.len(), 3);

        // Different location_id: does not affect original location
        let f_other_loc = EncodedPostFeedbackV1::new(post_id, feedback_type, Salt::random(), Pow(99));
        store.post_feedback_put_if_more_powerful(&Id::random(), &f_other_loc)?;
        let all_original = decode_all_feedbacks(store.post_bundle_feedbacks_bytes_get(&location_id)?)?;
        assert_eq!(all_original.len(), 3);

        Ok(())
    }
}
