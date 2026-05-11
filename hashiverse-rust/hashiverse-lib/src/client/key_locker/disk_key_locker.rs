//! # File-backed key locker
//!
//! Persists each account's encrypted `Keys` to `{data_dir}/key_locker/{public_key_hex}.key`
//! using [`crate::tools::keys::Keys::to_persistence`] (passphrase-protected ChaCha20Poly1305
//! over an Argon2-derived key). On load, the stored passphrase unlocks the file back into an
//! in-memory [`crate::client::key_locker::key_locker::KeyLocker`].
//!
//! A `TempDirHandle` fallback is used when no persistent data directory is configured,
//! so headless tools and tests still get a functional locker with correctly-typed paths.
//! Used by the native server binary and desktop client.

use crate::client::key_locker::key_locker::{KeyLocker, KeyLockerManager, GUEST_CLIENT_ID};
use crate::tools::client_id::ClientId;
use crate::tools::keys::Keys;
use crate::tools::signing;
use crate::tools::types::Signature;
use crate::tools::tools::TempDirHandle;
use anyhow::anyhow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use parking_lot::RwLock;

/// A key locker that holds the signing key in memory, with persistence to disk.
pub struct DiskKeyLocker {
    keys: Keys,
    client_id: ClientId,
}

#[async_trait::async_trait]
impl KeyLocker for DiskKeyLocker {
    fn client_id(&self) -> &ClientId {
        &self.client_id
    }

    async fn sign(&self, data: &[u8]) -> anyhow::Result<Signature> {
        Ok(signing::sign(&self.keys.signature_key, data))
    }
}

/// File-backed key locker manager. Keys are stored as encrypted files in
/// `{data_dir}/key_locker/{public_key_hex}.key`, encrypted via `Keys::to_persistence()`.
///
/// The `passphrase` is used to encrypt/decrypt key files at rest. Use an empty
/// string for bots that don't need key encryption.
pub struct DiskKeyLockerManager {
    key_locker_dir: PathBuf,
    passphrase: String,
    loaded_keys: Arc<RwLock<HashMap<String, Arc<DiskKeyLocker>>>>,
    /// Holds the temp dir handle to prevent cleanup when created via `new()`.
    _temp_dir_handle: Option<TempDirHandle>,
}

impl DiskKeyLockerManager {
    pub fn with_data_dir(data_dir: PathBuf, passphrase: String) -> anyhow::Result<Arc<Self>> {
        let key_locker_dir = data_dir.join("key_locker");
        std::fs::create_dir_all(&key_locker_dir)?;
        let manager = Arc::new(Self {
            key_locker_dir,
            passphrase,
            loaded_keys: Arc::new(RwLock::new(HashMap::new())),
            _temp_dir_handle: None,
        });
        Ok(manager)
    }

    fn key_file_path(&self, key_public_hex: &str) -> PathBuf {
        self.key_locker_dir.join(format!("{}.key", key_public_hex))
    }
}

impl KeyLockerManager<DiskKeyLocker> for DiskKeyLockerManager {
    /// Warning: stores keys in a temporary folder that will be cleaned up when this manager is dropped.
    /// Use `with_data_dir()` for persistent storage.
    async fn new() -> anyhow::Result<Arc<Self>> {
        let (temp_dir_handle, temp_dir_path) = crate::tools::tools::get_temp_dir()?;
        let key_locker_dir = PathBuf::from(&temp_dir_path).join("key_locker");
        std::fs::create_dir_all(&key_locker_dir)?;
        Ok(Arc::new(Self {
            key_locker_dir,
            passphrase: String::new(),
            loaded_keys: Arc::new(RwLock::new(HashMap::new())),
            _temp_dir_handle: Some(temp_dir_handle),
        }))
    }

    async fn list(&self) -> anyhow::Result<Vec<String>> {
        let mut key_ids = Vec::new();
        for entry in std::fs::read_dir(&self.key_locker_dir)? {
            let entry = entry?;
            let file_name = entry.file_name().to_string_lossy().to_string();
            if let Some(key_public_hex) = file_name.strip_suffix(".key") {
                if key_public_hex != GUEST_CLIENT_ID {
                    key_ids.push(key_public_hex.to_string());
                }
            }
        }
        Ok(key_ids)
    }

    async fn create(&self, key_phrase: String) -> anyhow::Result<Arc<DiskKeyLocker>> {
        let keys = Keys::from_phrase(&key_phrase)?;
        let client_id = ClientId::new(keys.verification_key_bytes, keys.pq_commitment_bytes)?;
        let client_id_hex = client_id.id_hex();

        // Persist the key to disk
        let persistence_data = keys.to_persistence(&self.passphrase)?;
        let key_file_path = self.key_file_path(&client_id_hex);
        std::fs::write(&key_file_path, persistence_data)?;

        let native_key_locker = Arc::new(DiskKeyLocker { keys, client_id });

        let mut loaded_keys = self.loaded_keys.write();
        loaded_keys.insert(client_id_hex, native_key_locker.clone());

        Ok(native_key_locker)
    }

    async fn switch(&self, client_id_hex: String) -> anyhow::Result<Arc<DiskKeyLocker>> {
        // Check if already loaded in memory
        {
            let loaded_keys = self.loaded_keys.read();
            if let Some(native_key_locker) = loaded_keys.get(&client_id_hex) {
                return Ok(native_key_locker.clone());
            }
        }

        // Load from disk
        let key_file_path = self.key_file_path(&client_id_hex);
        let persistence_data = std::fs::read_to_string(&key_file_path)
            .map_err(|_| anyhow!("Key file not found for {}", client_id_hex))?;
        let keys = Keys::from_persistence(&self.passphrase, &persistence_data)?;
        let client_id = ClientId::new(keys.verification_key_bytes, keys.pq_commitment_bytes)?;

        let native_key_locker = Arc::new(DiskKeyLocker { keys, client_id });

        let mut loaded_keys = self.loaded_keys.write();
        loaded_keys.insert(client_id_hex, native_key_locker.clone());

        Ok(native_key_locker)
    }

    async fn delete(&self, client_id_hex: String) -> anyhow::Result<()> {
        // Remove from memory
        {
            let mut loaded_keys = self.loaded_keys.write();
            loaded_keys.remove(&client_id_hex);
        }

        // Remove from disk
        let key_file_path = self.key_file_path(&client_id_hex);
        if key_file_path.exists() {
            std::fs::remove_file(&key_file_path)?;
        }

        Ok(())
    }

    async fn reset(&self) -> anyhow::Result<()> {
        // Clear memory
        {
            let mut loaded_keys = self.loaded_keys.write();
            loaded_keys.clear();
        }

        // Remove all .key files from the data directory
        for entry in std::fs::read_dir(&self.key_locker_dir)? {
            let entry = entry?;
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name.ends_with(".key") {
                std::fs::remove_file(entry.path())?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::client::key_locker::disk_key_locker::{DiskKeyLocker, DiskKeyLockerManager};
    use crate::tools::tools::get_temp_dir;
    use std::sync::Arc;
    use crate::client::key_locker::key_locker;

    #[tokio::test]
    async fn add_test() {
        key_locker::tests::add_test::<DiskKeyLocker, DiskKeyLockerManager>().await;
    }
    #[tokio::test]
    async fn sign_test() {
        key_locker::tests::sign_test::<DiskKeyLocker, DiskKeyLockerManager>().await;
    }
    #[tokio::test]
    async fn guest_client_id_excluded_from_list_test() {
        key_locker::tests::guest_client_id_excluded_from_list_test::<DiskKeyLocker, DiskKeyLockerManager>().await;
    }

    #[tokio::test]
    async fn persistence_roundtrip_test() {
        use crate::client::key_locker::key_locker::KeyLockerManager;

        let (_temp_dir, temp_dir_path) = get_temp_dir().unwrap();
        let data_dir = std::path::PathBuf::from(temp_dir_path);

        // Create a key with one manager instance
        let manager_1 = DiskKeyLockerManager::with_data_dir(data_dir.clone(), "test_passphrase".to_string()).unwrap();
        manager_1.reset().await.unwrap();
        let key_locker: Arc<DiskKeyLocker> = manager_1.create("my_key_phrase".to_string()).await.unwrap();

        use crate::client::key_locker::key_locker::KeyLocker;
        let original_client_id = key_locker.client_id().clone();
        let key_public_hex = original_client_id.id.to_hex_str();

        // Create a fresh manager instance (simulating process restart) and switch to the stored key
        let manager_2 = DiskKeyLockerManager::with_data_dir(data_dir, "test_passphrase".to_string()).unwrap();
        let restored_key_locker = manager_2.switch(key_public_hex).await.unwrap();

        assert_eq!(&original_client_id, restored_key_locker.client_id());
    }
}
