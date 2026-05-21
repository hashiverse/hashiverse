//! # Production disk [`EnvironmentStore`]
//!
//! Backing store used by the real server binary. Persists to a `fjall` keyspace for
//! metadata and a two-level directory tree (256² slots, ≈16 M bundles before any
//! single directory grows past ~64 K entries) for the bundle bodies. Writes go
//! through a temp-file + rename dance so a mid-write crash never leaves a torn
//! bundle, and feedback-update batches are atomic at the `fjall` level.
//!
//! Library choice rationale:
//! - **fjall** over **redb** (redb compacts synchronously and blocks writes during
//!   it, which we can't afford on a single-node server) and over **sled** (stalled
//!   upstream; its v1.0 rewrite hasn't landed).
//! - **postcard** for metadata serialisation — compact, fast, stable format, no
//!   schema on disk.
//!
//! Feedback entries are stored under composite keys `(location_id, post_id,
//! feedback_type)` so range iteration can walk all feedback for a given bundle
//! location during decimation in a single sequential scan.

use bytes::Bytes;
use crate::environment::environment::{Environment, EnvironmentDimensions, EnvironmentFactory, PostBundleMetadata};
use crate::environment::environment_store::EnvironmentStore;
use anyhow::anyhow;
use async_trait::async_trait;
use fjall::{Database, Keyspace};
use fs2::FileExt;
use hashiverse_lib::tools::time::{TimeMillis, TimeMillisBytes};
use hashiverse_lib::tools::types::{ID_BYTES, Id, SALT_BYTES, Salt, Pow};
use log::{info, trace, warn};
use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Arc;
use hashiverse_lib::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1;

const MAX_ENVIRONMENTS_PER_NODE: usize = 256;

pub struct DiskEnvironmentFactory {
    base_path: String,
}

#[async_trait]
impl EnvironmentFactory for DiskEnvironmentFactory {
    fn new(base_path: &str) -> Self {
        Self { base_path: base_path.to_string() }
    }

    async fn open_next_available(&self, environment_dimensions: EnvironmentDimensions) -> anyhow::Result<Environment> {
        for env_id in 1..=MAX_ENVIRONMENTS_PER_NODE {
            let disk_environment_store = DiskEnvironmentStore::new(&self.base_path, env_id);
            match disk_environment_store {
                Ok(disk_environment_store) => return Environment::new(Arc::new(disk_environment_store), environment_dimensions).await,
                Err(_) => continue,
            }
        }

        anyhow::bail!("no environments available")
    }
}

/// DiskEnvironmentStore implements the `EnvironmentStore` trait for production use.  PostBundles are stored as files in a directory tree, while config, metadata, feedback, and post-expiry metainformation are stored in a key-value database.
///
/// A lot of time was spent comparing the different available databases.  Given the nature of hashiverse, it makes sense that some sort of key-value (KV) database is used.
/// For simplicity of building and maintenance, we decided to use a pure-Rust implementation.  This narrowed teh field down to redb, sled, and fjall.
/// We would have liked to use redb, because of its self-reported performance with individual writes and random range reads - both things that hashiverse does a lot of.
/// Unfortunately it requires occasional compaction of the ever-growing files on disk, something that needs downtime and 2x disk space to do.  This essentially killed it for us.
/// Next was sled, which autocompacts and seemed the next most performant, but there seems to be little improvement and support for it - it's own github repo says use this as beta software only, and work on its v1.0 release seems to have stalled.
/// That leaves fjall - seemingly third place in performance, but gauging by its github activity, is ever improving and is highly supported.  It also does autocompaction.
pub struct DiskEnvironmentStore {
    path: PathBuf,
    #[allow(dead_code)]
    lock_file: fs::File, // Blocks other processes from claiming this environment on disk
    database: Database,
    keyspace_config: Keyspace,
    keyspace_post_bundle_last_accessed: Keyspace, // location_id -> TimeMillis
    keyspace_post_bundle_metadata: Keyspace,      // location_id -> PostBundleMetadata
    keyspace_post_bundle_feedback: Keyspace,      // [location_id,post_id,feedback_type] -> [salt,pow]
}

impl DiskEnvironmentStore {
    fn new(base_path: &str, env_id: usize) -> anyhow::Result<Self> {
        let path = PathBuf::from(base_path).join(env_id.to_string());

        // Check that the path exists
        fs::create_dir_all(&path)?;

        // Attempt to grab the lock
        let lock_path = path.join("lock");
        let lock_file = OpenOptions::new().create(true).truncate(true).read(true).write(true).open(&lock_path)?;
        lock_file.try_lock_exclusive()?;

        // Make sure the various stores exist
        let database = Database::builder(path.join("database")).open()?;
        let keyspace_config = database.keyspace("config", fjall::KeyspaceCreateOptions::default)?;
        let keyspace_post_bundle_last_accessed = database.keyspace("post_bundle_last_accessed", fjall::KeyspaceCreateOptions::default)?;
        let keyspace_post_bundle_metadata = database.keyspace("keyspace_post_bundle_metadata", fjall::KeyspaceCreateOptions::default)?;
        let keyspace_post_bundle_feedback = database.keyspace("keyspace_post_bundle_feedback", fjall::KeyspaceCreateOptions::default)?;

        info!("using environment {} at {}", env_id, path.to_str().unwrap());

        Ok(Self {
            path,
            lock_file,
            database,
            keyspace_config,
            keyspace_post_bundle_last_accessed,
            keyspace_post_bundle_metadata,
            keyspace_post_bundle_feedback,
        })
    }

    fn path_for_location_id(&self, location_id: &Id) -> (PathBuf, PathBuf) {
        // Two indirections of with 256 files in the folder allows 256^3 = 16million postbundles
        // Assuming each is at tiniest 1kb, that implies about 16Gb of disk space, which is what we are aiming for.
        // If we wanted to substantially increase the disk size supported by a hashiverse node, we might have to intro another level of indirection.
        // Note that it is likely ext4 will also start running out of inodes if posts are too small - inodes on ext4 assume a 16kb file on average.
        // Perhaps XFS is mandated?  Though the tradeoff is XFS is slower than ext4 when handling little files...
        let b0 = format!("{:02x}", location_id.0[0]);
        let b1 = format!("{:02x}", location_id.0[1]);

        let directory = self.path.join("post_bundles").join(b0).join(b1);
        let filename = directory.join(location_id.to_hex_str());

        (directory, filename)
    }
}

impl EnvironmentStore for DiskEnvironmentStore {
    fn post_bundle_count(&self) -> anyhow::Result<usize> {
        let len = self.keyspace_post_bundle_last_accessed.len()?;
        Ok(len)
    }

    fn post_bundle_feedback_count(&self) -> anyhow::Result<usize> {
        let len = self.keyspace_post_bundle_feedback.len()?;
        Ok(len)
    }
    fn post_bundle_metadata_get(&self, location_id: &Id) -> anyhow::Result<Option<PostBundleMetadata>> {
        let guard = self.keyspace_post_bundle_metadata.get(location_id)?;
        match guard {
            Some(guard) => Ok(Some(postcard::from_bytes(&guard)?)),
            None => Ok(None),
        }
    }

    fn post_bundle_metadata_put(&self, location_id: &Id, post_bundle_metadata: &PostBundleMetadata) -> anyhow::Result<()> {
        // Much faster than allocating a Vec each time...
        let mut scratch = [0u8; 64];
        let scratch_used = postcard::to_slice(post_bundle_metadata, &mut scratch)?;
        self.keyspace_post_bundle_metadata.insert(location_id.0, scratch_used.as_ref())?;
        Ok(())
    }

    fn post_bundle_bytes_get(&self, location_id: &Id) -> anyhow::Result<Option<Bytes>> {
        let (_directory, filename) = self.path_for_location_id(location_id);
        let result = fs::read(filename).ok().map(Bytes::from);
        Ok(result)
    }

    fn post_bundle_bytes_put(&self, location_id: &Id, bytes: &[u8]) -> anyhow::Result<()> {
        let (directory, filename) = self.path_for_location_id(location_id);
        let filename_temp = filename.with_added_extension("tmp");
        fs::create_dir_all(&directory)?;
        fs::write(&filename_temp, bytes)?;
        fs::rename(&filename_temp, &filename)?;
        Ok(())
    }

    fn post_bundle_feedbacks_bytes_get(&self, post_bundle_location_id: &Id) -> anyhow::Result<Bytes> {
        let mut bytes = Vec::new();

        self.keyspace_post_bundle_feedback.prefix(post_bundle_location_id).for_each(|guard| {
            let try_result = try {
                let (key, value) = guard.into_inner().map_err(|e| anyhow!("{}", e))?;
                let post_bundle_feedback_key = PostBundleFeedbackKey::from_slice(&key)?;
                let post_bundle_feedback_value = PostBundleFeedbackValue::from_slice(&value)?;
                EncodedPostFeedbackV1::append_encode_direct_to_bytes(
                    &mut bytes,
                    post_bundle_feedback_key.post_id_bytes(),
                    post_bundle_feedback_key.feedback_type(),
                    post_bundle_feedback_value.salt_bytes(),
                    post_bundle_feedback_value.pow(),
                )?;
            };

            if let Err(e) = try_result {
                warn!("unexpectedly unable to encode post bundle feedback: {}", e);
            }
        });

        Ok(Bytes::from(bytes))
    }


    fn post_feedback_put_if_more_powerful(&self, location_id: &Id, encoded_post_feedback: &EncodedPostFeedbackV1) -> anyhow::Result<()> {
        let post_bundle_feedback_key = PostBundleFeedbackKey::new(location_id, &encoded_post_feedback.post_id, encoded_post_feedback.feedback_type);

        // Check that we don't already have much better feedback
        let post_bundle_feedback_value = self.keyspace_post_bundle_feedback.get(&post_bundle_feedback_key)?;
        if let Some(post_bundle_feedback_value) = post_bundle_feedback_value {
            let post_bundle_feedback_value = PostBundleFeedbackValue::from_slice(&post_bundle_feedback_value)?;
            if post_bundle_feedback_value.pow() >= encoded_post_feedback.pow {
                trace!("Not storing lesser feedback for location_id={} with existing pow={}: feedback={:?}", location_id, post_bundle_feedback_value.pow(), encoded_post_feedback);
                return Ok(());
            }
        }

        // Commit the improved feedback
        {
            let post_bundle_feedback_value = PostBundleFeedbackValue::new(encoded_post_feedback.salt, encoded_post_feedback.pow);
            self.keyspace_post_bundle_feedback.insert(post_bundle_feedback_key.0, post_bundle_feedback_value.0)?;
        }

        Ok(())
    }

    fn post_bundles_delete(&self, location_ids: &[Id]) -> anyhow::Result<()> {
        let post_bundle_bytes_delete = |location_id: &Id| -> anyhow::Result<()> {
            let (_directory, filename) = self.path_for_location_id(location_id);
            let _result = fs::remove_file(filename);
            Ok(())
        };

        // Collect feedback keys before opening the batch to avoid borrow conflicts
        let mut feedback_keys_to_delete: Vec<Vec<u8>> = Vec::new();
        for location_id in location_ids {
            self.keyspace_post_bundle_feedback.prefix(location_id).for_each(|guard| {
                let try_result: anyhow::Result<()> = try {
                    let (key, _) = guard.into_inner().map_err(|e| anyhow!("{}", e))?;
                    feedback_keys_to_delete.push(key.to_vec());
                };
                if let Err(e) = try_result {
                    warn!("failed to collect feedback key for deletion: {}", e);
                }
            });
        }

        let mut batch = self.database.batch();
        for location_id in location_ids {
            batch.remove(&self.keyspace_post_bundle_metadata, location_id.0);
            batch.remove(&self.keyspace_post_bundle_last_accessed, location_id.0);
            post_bundle_bytes_delete(location_id)?;
        }
        for key in &feedback_keys_to_delete {
            batch.remove(&self.keyspace_post_bundle_feedback, key.as_slice());
        }
        batch.commit()?;

        Ok(())
    }

    fn post_bundles_last_accessed_flush(&self, post_bundles_last_accessed: &HashMap<Id, TimeMillis>) -> anyhow::Result<()> {
        let mut batch = self.database.batch();
        for (location_id, time_millis) in post_bundles_last_accessed.iter() {
            let time_millis_bytes = time_millis.encode_be();
            batch.insert(&self.keyspace_post_bundle_last_accessed, location_id.0, time_millis_bytes.0);
        }
        batch.commit()?;
        Ok(())
    }

    fn post_bundles_last_accessed_iter(&self, location_id: &Id) -> Box<dyn Iterator<Item = Result<(Id, TimeMillisBytes), anyhow::Error>> + '_> {
        let it = self
            .keyspace_post_bundle_last_accessed
            .range(location_id.to_string()..)
            .chain(self.keyspace_post_bundle_last_accessed.range(..location_id.to_string()))
            .map(|guard| {
                let (location_id, time_millis_bytes) = guard.into_inner().map_err(|e| anyhow!("{}", e))?;
                let location_id = Id::from_slice(&location_id)?;
                let time_millis_bytes = TimeMillisBytes::from_bytes(&time_millis_bytes)?;
                Ok((location_id, time_millis_bytes))
            });

        Box::new(it)
    }

    fn config_get_bytes(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.keyspace_config.get(key)?.map(|v| v.to_vec()))
    }

    fn config_put_bytes(&self, key: &str, v: Vec<u8>) -> anyhow::Result<()> {
        self.keyspace_config.insert(key, v)?;
        Ok(())
    }
}

const POST_BUNDLE_FEEDBACK_KEY_SIZE: usize = ID_BYTES + ID_BYTES + 1;
pub struct PostBundleFeedbackKey(pub [u8; POST_BUNDLE_FEEDBACK_KEY_SIZE]);

impl PostBundleFeedbackKey {
    pub fn new(location_id: &Id, post_id: &Id, feedback_type: u8) -> Self {
        let mut bytes = [0u8; POST_BUNDLE_FEEDBACK_KEY_SIZE];
        bytes[0..ID_BYTES].copy_from_slice(location_id.as_bytes());
        bytes[ID_BYTES..2*ID_BYTES].copy_from_slice(post_id.as_bytes());
        bytes[2*ID_BYTES] = feedback_type;
        Self(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        let bytes: [u8; POST_BUNDLE_FEEDBACK_KEY_SIZE] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid PostBundleFeedbackKey length: expected {}, got {}", POST_BUNDLE_FEEDBACK_KEY_SIZE, bytes.len()))?;
        Ok(Self(bytes))
    }

    pub fn post_bundle_location_id_bytes(&self) -> &[u8] {
        &self.0[0..ID_BYTES]
    }
    pub fn post_id_bytes(&self) -> &[u8] {
        &self.0[ID_BYTES..2*ID_BYTES]
    }

    pub fn feedback_type(&self) -> u8 {
        self.0[2*ID_BYTES]
    }
}

impl AsRef<[u8]> for PostBundleFeedbackKey {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

const POST_BUNDLE_FEEDBACK_VALUE_SIZE: usize = SALT_BYTES + 1;
pub struct PostBundleFeedbackValue(pub [u8; POST_BUNDLE_FEEDBACK_VALUE_SIZE]);

impl PostBundleFeedbackValue {
    pub fn new(salt: Salt, pow: Pow) -> Self {
        let mut bytes = [0u8; POST_BUNDLE_FEEDBACK_VALUE_SIZE];
        bytes[0..SALT_BYTES].copy_from_slice(salt.as_slice());
        bytes[SALT_BYTES] = pow.0;
        Self(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        let bytes: [u8; POST_BUNDLE_FEEDBACK_VALUE_SIZE] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Invalid PostBundleFeedbackValue length: expected {}, got {}", POST_BUNDLE_FEEDBACK_VALUE_SIZE, bytes.len()))?;
        Ok(Self(bytes))
    }
    pub fn salt_bytes(&self) -> &[u8] {
        &self.0[0..SALT_BYTES]
    }
    pub fn pow(&self) -> Pow {
        Pow(self.0[SALT_BYTES])
    }
}

#[cfg(test)]
mod tests {
    use crate::environment;
    use crate::environment::disk_environment_store::DiskEnvironmentFactory;

    #[tokio::test]
    async fn basics_test() -> anyhow::Result<()> {
        environment::environment::tests::basics_test::<DiskEnvironmentFactory>().await
    }


    #[tokio::test]
    async fn feedback_bytes_get_test() -> anyhow::Result<()> {
        environment::environment::tests::feedback_bytes_get_test::<DiskEnvironmentFactory>().await
    }

    #[tokio::test]
    async fn feedback_put_if_more_powerful_test() -> anyhow::Result<()> {
        environment::environment::tests::feedback_put_if_more_powerful_test::<DiskEnvironmentFactory>().await
    }

    // These are brutally expensive on disk - we rely on the memory stub for the decimation tests
    //
    // #[tokio::test]
    // async fn decimation_feedback_deleted_test() -> anyhow::Result<()> {
    //     environment::environment::tests::decimation_feedback_deleted_test::<DiskEnvironmentFactory>().await
    // }
    // #[tokio::test]
    // async fn decimation_test() -> anyhow::Result<()> {
    //     environment::environment::tests::decimation_test::<DiskEnvironmentFactory>().await
    // }
    // #[tokio::test]
    // async fn decimation_convergence_test() -> anyhow::Result<()> {
    //     environment::environment::tests::decimation_convergence_test::<DiskEnvironmentFactory>(2 * 1000).await
    // }
    // #[tokio::test]
    // async fn decimation_existence_test() -> anyhow::Result<()> { environment::environment::tests::decimation_existence_test::<DiskEnvironmentFactory>().await }
}
