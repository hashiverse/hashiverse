//! # In-memory [`ClientStorage`] implementation
//!
//! A `HashMap<String, HashMap<String, Entry>>` that implements the
//! [`crate::client::client_storage::client_storage::ClientStorage`] trait. Each inner map is
//! one bucket; each `Entry` carries the compressed value and a `last_accessed` timestamp.
//! When a bucket exceeds its [`crate::client::client_storage::client_storage::BUCKET_TRIMS`]
//! cap, the oldest-accessed entries are dropped.
//!
//! Used by tests (including the integration-test harness) and by any deployment that
//! explicitly opts out of on-disk persistence.

use crate::client::client_storage::client_storage::{ClientStorage, BUCKETS};
use crate::tools::time::TimeMillis;
use anyhow::Context;
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;

struct Entry {
    value: Vec<u8>,
    last_accessed: TimeMillis,
}

pub struct MemClientStorage {
    buckets: Arc<RwLock<HashMap<String, HashMap<String, Entry>>>>,
}

impl MemClientStorage {
    pub async fn new() -> anyhow::Result<Arc<Self>> {
        let mut buckets = HashMap::new();
        for bucket in BUCKETS {
            buckets.insert(bucket.to_string(), HashMap::new());
        }

        Ok(Arc::new(Self { buckets: Arc::new(RwLock::new(buckets)) }))
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl ClientStorage for MemClientStorage {
    async fn count(&self, bucket: &str) -> anyhow::Result<usize> {
        let buckets = self.buckets.read();
        let bucket = buckets.get(bucket).context(format!("Bucket not found: {}", bucket))?;
        Ok(bucket.len())
    }

    async fn keys(&self, bucket: &str) -> anyhow::Result<Vec<String>> {
        let buckets = self.buckets.read();
        let bucket = buckets.get(bucket).context(format!("Bucket not found: {}", bucket))?;
        Ok(bucket.keys().cloned().collect())
    }

    async fn get(&self, bucket: &str, key: &str, time_millis: TimeMillis) -> anyhow::Result<Option<Vec<u8>>> {
        let mut buckets = self.buckets.write();
        let bucket = buckets.get_mut(bucket).context(format!("Bucket not found: {}", bucket))?;
        match bucket.get_mut(key) {
            Some(entry) => {
                if time_millis > TimeMillis::zero() {
                    entry.last_accessed = time_millis;
                }
                Ok(Some(entry.value.clone()))
            }
            None => Ok(None),
        }
    }

    async fn put(&self, bucket: &str, key: &str, value: Vec<u8>, time_millis: TimeMillis) -> anyhow::Result<()> {
        let mut buckets = self.buckets.write();
        let bucket = buckets.get_mut(bucket).context(format!("Bucket not found: {}", bucket))?;
        bucket.insert(key.to_string(), Entry { value, last_accessed: time_millis });
        Ok(())
    }

    async fn remove(&self, bucket: &str, key: &str) -> anyhow::Result<()> {
        let mut buckets = self.buckets.write();
        let bucket = buckets.get_mut(bucket).context(format!("Bucket not found: {}", bucket))?;
        bucket.remove(key);
        Ok(())
    }

    async fn trim(&self, bucket: &str, max_count: usize) -> anyhow::Result<()> {
        let mut buckets = self.buckets.write();
        let bucket = buckets.get_mut(bucket).context(format!("Bucket not found: {}", bucket))?;
        if bucket.len() > max_count {
            let num_to_delete = bucket.len() - max_count;
            let mut entries: Vec<(&String, TimeMillis)> = bucket.iter().map(|(k, e)| (k, e.last_accessed)).collect();
            entries.sort_by_key(|(_, t)| *t);
            let keys_to_delete: Vec<String> = entries.into_iter().take(num_to_delete).map(|(k, _)| k.clone()).collect();
            for key in keys_to_delete {
                bucket.remove(&key);
            }
        }
        Ok(())
    }

    async fn reset(&self) -> anyhow::Result<()> {
        let mut buckets = self.buckets.write();
        for bucket in BUCKETS {
            let bucket = buckets.get_mut(*bucket).context(format!("Bucket not found: {}", bucket))?;
            bucket.clear();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::client::client_storage::client_storage;
    use crate::client::client_storage::mem_client_storage::MemClientStorage;

    #[tokio::test]
    async fn add_test() {
        client_storage::tests::add_test(MemClientStorage::new().await.unwrap()).await;
    }

    #[tokio::test]
    async fn trim_test() {
        client_storage::tests::trim_test(MemClientStorage::new().await.unwrap()).await;
    }
}
