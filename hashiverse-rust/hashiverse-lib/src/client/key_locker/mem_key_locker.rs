//! # In-memory key locker
//!
//! Reference implementation of [`crate::client::key_locker::key_locker::KeyLocker`] and
//! [`crate::client::key_locker::key_locker::KeyLockerManager`] that keeps every account's
//! plaintext [`crate::tools::keys::Keys`] in a `HashMap` for the lifetime of the process.
//!
//! Used by tests and short-lived tools that don't need on-disk persistence. In the
//! integration-test harness every simulated user gets a `MemKeyLocker`, so switching
//! accounts, signing, and the guest fallback all exercise the same code paths as a
//! production locker.

use crate::client::key_locker::key_locker::{KeyLocker, KeyLockerManager, GUEST_CLIENT_ID};
use crate::tools::client_id::ClientId;
use crate::tools::keys::Keys;
use crate::tools::signing;
use crate::tools::types::Signature;
use anyhow::anyhow;
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;

pub struct MemKeyLocker {
    keys: Keys,
    client_id: ClientId,
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl KeyLocker for MemKeyLocker {
    fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    async fn sign(&self, data: &[u8]) -> anyhow::Result<Signature> {
        Ok(signing::sign(&self.keys.signature_key, data))
    }
}

pub struct MemKeyLockerManager {
    buckets: Arc<RwLock<HashMap<String, Arc<MemKeyLocker>>>>,
}

impl KeyLockerManager<MemKeyLocker> for MemKeyLockerManager {
    async fn new() -> anyhow::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            buckets: Arc::new(RwLock::new(HashMap::new())),
        }))
    }

    async fn list(&self) -> anyhow::Result<Vec<String>> {
        let buckets = self.buckets.read();
        let keys = buckets.keys().filter(|k| k.as_str() != GUEST_CLIENT_ID).cloned().collect::<Vec<String>>();
        Ok(keys)
    }

    async fn create(&self, key_phrase: String) -> anyhow::Result<Arc<MemKeyLocker>> {
        let keys = Keys::from_phrase(&key_phrase)?;
        let client_id = ClientId::new(keys.verification_key_bytes, keys.pq_commitment_bytes)?;
        let client_id_hex = client_id.id_hex();

        let kml = Arc::new(MemKeyLocker { keys, client_id });

        let mut buckets = self.buckets.write();
        buckets.insert(client_id_hex, kml.clone());

        Ok(kml)
    }

    async fn switch(&self, client_id_hex: String) -> anyhow::Result<Arc<MemKeyLocker>> {
        let buckets = self.buckets.read();
        let kml = buckets.get(&client_id_hex);
        match kml {
            Some(kml) => Ok(kml.clone()),
            None => Err(anyhow!("Unknown key_public {}", client_id_hex)),
        }
    }

    async fn delete(&self, client_id_hex: String) -> anyhow::Result<()> {
        let mut buckets = self.buckets.write();
        let kml = buckets.remove(&client_id_hex);
        match kml {
            Some(_kml) => Ok(()),
            None => Err(anyhow!("Unknown key_public {}", client_id_hex)),
        }
    }

    async fn reset(&self) -> anyhow::Result<()> {
        let mut buckets = self.buckets.write();
        buckets.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::client::key_locker::key_locker;
    use crate::client::key_locker::mem_key_locker::{MemKeyLocker, MemKeyLockerManager};

    #[tokio::test]
    async fn add_test() {
        key_locker::tests::add_test::<MemKeyLocker, MemKeyLockerManager>().await;
    }
    #[tokio::test]
    async fn sign_test() {
        key_locker::tests::sign_test::<MemKeyLocker, MemKeyLockerManager>().await;
    }
    #[tokio::test]
    async fn guest_client_id_excluded_from_list_test() {
        key_locker::tests::guest_client_id_excluded_from_list_test::<MemKeyLocker, MemKeyLockerManager>().await;
    }
}
