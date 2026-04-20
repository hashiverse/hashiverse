//! # Pluggable client-side persistence
//!
//! `ClientStorage` is the abstract key-value store every higher-level client subsystem
//! writes through: peers ([`crate::client::peer_tracker`]), cached post bundles and
//! feedback, the meta-post / account config, and so on. The keyspace is partitioned into
//! named *buckets* (see [`BUCKETS`]), each with its own LRU cap enforced by
//! [`BUCKET_TRIMS`], so one subsystem cannot evict another's working set.
//!
//! Entries are Zstd-compressed and each carries a `last_accessed` timestamp so buckets
//! can evict their coldest entries on overflow. Passing [`TimeMillis::zero()`] on a read
//! is a sentinel meaning "don't touch LRU" — used by cache-warming paths so refreshing a
//! cold cache does not reshuffle the recency ordering.
//!
//! Typed convenience helpers on [`ClientStorage`] (`put_struct` / `get_struct` /
//! `put_str` / `get_str`) handle serde + compression in one step so callers don't
//! hand-roll the same three lines at each site.
//!
//! Three implementations ship with the crate:
//! - [`crate::client::client_storage::mem_client_storage`] — pure in-memory, tests and
//!   lightweight deployments.
//! - [`crate::client::client_storage::sqlite_client_storage`] — on-disk SQLite with WAL,
//!   native servers and desktop clients.
//! - (In the WASM client crate) an IndexedDB implementation lives alongside the web client.

use crate::tools::time::TimeMillis;
use serde::de::DeserializeOwned;
use serde::Serialize;
use crate::tools::{compression, json};
use crate::tools::types::Id;

pub const BUCKET_CONFIG: &str = "config";
pub const BUCKET_CONFIG_KEY_META_POST_V1_PUBLIC: &str = "meta_post_v1_public";
pub const BUCKET_CONFIG_KEY_META_POST_V1_PRIVATE: &str = "meta_post_v1_private";
pub const BUCKET_PEER: &str = "peer";
pub const BUCKET_META_POST_PUBLIC: &str = "meta_post_public";
pub const BUCKET_POST_BUNDLE: &str = "post_bundle";
pub const BUCKET_POST_BUNDLE_FEEDBACK: &str = "post_bundle_feedback";
pub const BUCKETS: &[&str] = &[BUCKET_CONFIG, BUCKET_PEER, BUCKET_POST_BUNDLE, BUCKET_POST_BUNDLE_FEEDBACK, BUCKET_META_POST_PUBLIC];
pub const BUCKET_TRIMS: &[usize] = &[0, 1024, 512, 512, 2048];

pub fn config_key_for_user(id: Id, key: &str) -> String {
    format!("{}.{}", id.to_hex_str(), key)
}

/// The persistence backend for a [`crate::client::hashiverse_client::HashiverseClient`].
///
/// `ClientStorage` is a bucketed, LRU-evictable key/value store: callers name a bucket
/// (`BUCKET_POST_BUNDLE`, `BUCKET_PEER`, `BUCKET_CONFIG`, …) and operate on byte values
/// under string keys. The per-entry `time_millis` argument controls LRU freshening; passing
/// `TimeMillis::zero()` reads an entry without touching its access time, which matters when
/// the client is warming caches and must not perturb eviction order.
///
/// Implementations are swappable per target: the server uses a sled-backed filesystem store,
/// the browser client uses IndexedDB, tests use an in-memory map. Typed convenience helpers
/// (`put_struct`, `get_struct`, `put_str`, `get_str`) live alongside the trait; they handle
/// JSON serialization and Zstd compression so trait implementors only ever see raw bytes.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait ClientStorage: Send + Sync {
    async fn count(&self, bucket: &str) -> anyhow::Result<usize>;
    async fn keys(&self, bucket: &str) -> anyhow::Result<Vec<String>>;

    /// Gets an item from the client storage
    ///
    /// Note that `time_millis` is used for LRU eviction.  If you pass in TimeMillis::zero(), the record will not be 'touched', and so will not be freshened for the LRU eviction.
    /// This is useful e.g. for loading all the records into a cache - we don't want to alter their last accessed time...
    async fn get(&self, bucket: &str, key: &str, time_millis: TimeMillis) -> anyhow::Result<Option<Vec<u8>>>;
    async fn put(&self, bucket: &str, key: &str, value: Vec<u8>, time_millis: TimeMillis) -> anyhow::Result<()>;
    async fn remove(&self, bucket: &str, key: &str) -> anyhow::Result<()>;
    async fn trim(&self, bucket: &str, max_count: usize) -> anyhow::Result<()>;
    async fn reset(&self) -> anyhow::Result<()>;
}

/// Extension methods for ClientStorage that provide typed access
pub async fn put_str(storage: &dyn ClientStorage, bucket: &str, key: &str, value: String, time_millis: TimeMillis) -> anyhow::Result<()> {
    storage.put(bucket, key, value.into_bytes(), time_millis).await
}

pub async fn get_str(storage: &dyn ClientStorage, bucket: &str, key: &str, time_millis: TimeMillis) -> anyhow::Result<Option<String>> {
    let result = storage.get(bucket, key, time_millis).await?;
    match result {
        Some(bytes) => Ok(Some(String::from_utf8(bytes)?)),
        None => Ok(None),
    }
}

pub async fn put_struct<T: Serialize>(storage: &dyn ClientStorage, bucket: &str, key: &str, value: &T, time_millis: TimeMillis) -> anyhow::Result<()> {
    let bytes = json::struct_to_bytes(value)?;
    let bytes_compressed = compression::compress_for_size(&bytes)?.to_bytes();
    storage.put(bucket, key, bytes_compressed.to_vec(), time_millis).await
}

pub async fn get_struct<T: DeserializeOwned>(storage: &dyn ClientStorage, bucket: &str, key: &str, time_millis: TimeMillis) -> anyhow::Result<Option<T>> {
    let result = storage.get(bucket, key, time_millis).await?;
    match result {
        Some(bytes_compressed) => {
            let bytes = compression::decompress(&bytes_compressed)?.to_bytes();
            let value = json::bytes_to_struct::<T>(&bytes)?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

#[cfg(any(test, feature = "generic-tests"))]
pub mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn buckets_and_bucket_trims_have_equal_length() {
        assert_eq!(BUCKETS.len(), BUCKET_TRIMS.len(), "BUCKETS has {} entries but BUCKET_TRIMS has {} entries", BUCKETS.len(), BUCKET_TRIMS.len());
    }

    pub async fn trim_test(cs: Arc<dyn ClientStorage>) {
        let result: anyhow::Result<()> = try {
            cs.reset().await?;

            // Put 5 entries with ascending timestamps (= ascending last_accessed)
            put_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key1", "val1".to_string(), TimeMillis(10)).await?;
            put_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key2", "val2".to_string(), TimeMillis(20)).await?;
            put_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key3", "val3".to_string(), TimeMillis(30)).await?;
            put_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key4", "val4".to_string(), TimeMillis(40)).await?;
            put_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key5", "val5".to_string(), TimeMillis(50)).await?;
            assert_eq!(5, cs.count(BUCKET_POST_BUNDLE).await?);

            // Trim to 3 — key1 (ts=10) and key2 (ts=20) are the oldest, should be removed
            cs.trim(BUCKET_POST_BUNDLE, 3).await?;
            assert_eq!(3, cs.count(BUCKET_POST_BUNDLE).await?);
            assert_eq!(None, get_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key1", TimeMillis::zero()).await?);
            assert_eq!(None, get_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key2", TimeMillis::zero()).await?);
            assert_eq!(Some("val3".to_string()), get_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key3", TimeMillis::zero()).await?);
            assert_eq!(Some("val4".to_string()), get_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key4", TimeMillis::zero()).await?);
            assert_eq!(Some("val5".to_string()), get_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key5", TimeMillis::zero()).await?);

            // Get key3 with a fresh timestamp — it is now the newest
            get_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key3", TimeMillis(60)).await?;

            // Trim to 2 — order is now key4 (ts=40), key5 (ts=50), key3 (ts=60); key4 should be removed
            cs.trim(BUCKET_POST_BUNDLE, 2).await?;
            assert_eq!(2, cs.count(BUCKET_POST_BUNDLE).await?);
            assert_eq!(None, get_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key4", TimeMillis::zero()).await?);
            assert_eq!(Some("val5".to_string()), get_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key5", TimeMillis::zero()).await?);
            assert_eq!(Some("val3".to_string()), get_str(cs.as_ref(), BUCKET_POST_BUNDLE, "key3", TimeMillis::zero()).await?);

            // Trim to more than count — no-op
            cs.trim(BUCKET_POST_BUNDLE, 10).await?;
            assert_eq!(2, cs.count(BUCKET_POST_BUNDLE).await?);

            // Trim to zero
            cs.trim(BUCKET_POST_BUNDLE, 0).await?;
            assert_eq!(0, cs.count(BUCKET_POST_BUNDLE).await?);

            cs.reset().await?;
        };

        if let Err(e) = result {
            panic!("trim_test failed: {}", e);
        }
    }

    pub async fn add_test(client_storage: Arc<dyn ClientStorage>) {
        let result = try {
            // Start clean
            client_storage.reset().await?;
            assert_eq!(0, client_storage.count(BUCKET_POST_BUNDLE).await?);

            // Test a put
            {
                put_str(client_storage.as_ref(), BUCKET_POST_BUNDLE, "key1", "val1".to_string(), TimeMillis(0)).await?;
                put_str(client_storage.as_ref(), BUCKET_POST_BUNDLE, "key2", "val2".to_string(), TimeMillis(1)).await?;
                put_str(client_storage.as_ref(), BUCKET_POST_BUNDLE, "key3", "val3".to_string(), TimeMillis(2)).await?;
            }

            // Test a count
            assert_eq!(3, client_storage.count(BUCKET_POST_BUNDLE).await?);

            // Test a get
            {
                let x = get_str(client_storage.as_ref(), BUCKET_POST_BUNDLE, "key1", TimeMillis(3)).await?;
                assert_eq!(Some("val1".to_string()), x);
            }

            // Test an update
            put_str(client_storage.as_ref(), BUCKET_POST_BUNDLE, "key1", "val4".to_string(), TimeMillis(4)).await?;
            assert_eq!(3, client_storage.count(BUCKET_POST_BUNDLE).await?);
            {
                let x = get_str(client_storage.as_ref(), BUCKET_POST_BUNDLE, "key1", TimeMillis(5)).await?;
                assert_eq!(Some("val4".to_string()), x);
            }

            // Test a missing
            {
                let x = get_str(client_storage.as_ref(), BUCKET_POST_BUNDLE, "key_none", TimeMillis(6)).await?;
                assert_eq!(None, x);
            }

            // Test a delete
            {
                client_storage.remove(BUCKET_POST_BUNDLE, "key1").await?;
                assert_eq!(2, client_storage.count(BUCKET_POST_BUNDLE).await?);
            }

            // End clean
            client_storage.reset().await?;
            assert_eq!(0, client_storage.count(BUCKET_POST_BUNDLE).await?);
        };

        if let Err(e) = result {
            panic!("Test failed: {}", e);
        }
    }
}
