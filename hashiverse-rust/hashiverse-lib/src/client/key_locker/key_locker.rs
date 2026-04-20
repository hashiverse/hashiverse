//! # Account identity and signing — pluggable storage
//!
//! A single device can have several accounts on it (work, personal, alt, …) and the
//! application must be able to switch between them, create new ones, and sign outgoing
//! messages under the currently-selected one. Two traits split that responsibility:
//!
//! - [`KeyLocker`] — the guarded holder of **one** account's private signing material.
//!   Exposes only `client_id()` and `sign()`; the private key never leaves the locker.
//!   This is deliberately the minimum surface so production implementations can delegate
//!   signing to a platform keychain without exposing raw key bytes.
//! - [`KeyLockerManager`] — the directory of lockers installed on this device:
//!   `list` / `create` / `switch` / `delete` / `reset`.
//!
//! The constant [`GUEST_CLIENT_ID`] is the deterministic `ClientId` produced from an
//! empty keyphrase. It is filtered out of `list()` so it never appears as a "stored
//! account" in the login UI, even though it's always present as a fallback identity.
//!
//! Implementations: [`crate::client::key_locker::mem_key_locker`] for tests,
//! [`crate::client::key_locker::disk_key_locker`] for native / desktop, and (in the WASM
//! crate) an IndexedDB-backed variant for browsers.

use crate::tools::client_id::ClientId;
use crate::tools::types::Signature;
use std::sync::Arc;

/// The client ID produced by `Keys::from_phrase("")` — deterministic and shared by all guests.
/// Filtered out of `list()` so it never appears as a stored account in the login UI.
pub const GUEST_CLIENT_ID: &str = "fe050cc21479a93d00fdb825c98c8489ea47dd6ce180b6f8b72665f284842e41";

/// The guarded holder of a single account's private signing material.
///
/// A `KeyLocker` binds together the public [`ClientId`] (what the rest of the protocol sees)
/// and the private [`crate::tools::types::SignatureKey`] (what produces signatures). It
/// exposes the minimum interface the client needs: "who am I" (`client_id`) and "sign this"
/// (`sign`). The private key itself never leaves the locker — that is the whole point of the
/// abstraction, which lets production implementations delegate signing to platform-native
/// key stores (WebCrypto in the browser, OS keyring elsewhere) while tests can use a
/// straightforward in-memory stub.
///
/// An account is identified externally by the hex string of its [`ClientId::id`]; the
/// special [`GUEST_CLIENT_ID`] constant marks the deterministic "empty keyphrase" guest
/// identity and is filtered out of [`KeyLockerManager::list`] so it never looks like a
/// stored account in the login UI.
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
pub trait KeyLocker: Send + Sync {
    fn client_id(&self) -> &ClientId;
    async fn sign(&self, data: &[u8]) -> anyhow::Result<Signature>;
}

/// The device-local account store that manages the set of [`KeyLocker`]s the user has set up
/// on this machine.
///
/// Where a single [`KeyLocker`] holds one account, a `KeyLockerManager` is the directory of
/// all of them: it can enumerate stored accounts (`list`), create a new one from a BIP-39-
/// style keyphrase (`create`), hand back the [`KeyLocker`] for an existing account
/// (`switch`), or remove one (`delete`). Implementations are backed by platform-specific
/// persistent storage — IndexedDB in the browser, sled on native, in-memory in tests — but
/// expose the same trait so the login flow in the UI never has to branch on target.
///
/// Generic over the concrete `KeyLocker` type so the WASM-friendly `?Send` bound can be
/// preserved where needed.
pub trait KeyLockerManager<TKeyLocker: KeyLocker> {
    async fn new() -> anyhow::Result<Arc<Self>>;
    async fn list(&self) -> anyhow::Result<Vec<String>>;
    async fn create(&self, key_phrase: String) -> anyhow::Result<Arc<TKeyLocker>>;
    async fn switch(&self, key_public: String) -> anyhow::Result<Arc<TKeyLocker>>;
    async fn delete(&self, key_public: String) -> anyhow::Result<()>;
    async fn reset(&self) -> anyhow::Result<()>;
}

#[cfg(any(test, feature = "generic-tests"))]
pub mod tests {
    use crate::client::key_locker::key_locker::{KeyLocker, KeyLockerManager, GUEST_CLIENT_ID};
    use crate::tools::types::VerificationKey;
    use crate::tools::{signing, tools};

    pub async fn add_test<TKeyLocker: KeyLocker, TKeyLockerManager: KeyLockerManager<TKeyLocker>>() {
        let result = try {
            let key_locker_manager = TKeyLockerManager::new().await?;

            // Start clean
            key_locker_manager.reset().await?;
            assert_eq!(0, key_locker_manager.list().await?.len());

            // Add a key
            let key_locker_1 = key_locker_manager.create("key_phrase_1".to_string()).await?;
            let key_locker_2 = key_locker_manager.create("key_phrase_2".to_string()).await?;
            assert_eq!(2, key_locker_manager.list().await?.len());

            // Locate key
            let public_key_1 = key_locker_1.client_id().id.to_hex_str();
            let public_key_2 = key_locker_2.client_id().id.to_hex_str();
            assert_eq!(key_locker_1.client_id(), key_locker_manager.switch(public_key_1).await?.client_id());
            assert_eq!(key_locker_2.client_id(), key_locker_manager.switch(public_key_2).await?.client_id());

            // End clean
            key_locker_manager.reset().await?;
            assert_eq!(0, key_locker_manager.list().await?.len());
        };

        if let Err(e) = result {
            panic!("Test failed: {}", e);
        }
    }
    /// Confirms that `list()` never returns the guest client ID.
    pub async fn guest_client_id_excluded_from_list_test<TKeyLocker: KeyLocker, TKeyLockerManager: KeyLockerManager<TKeyLocker>>() {
        let result: anyhow::Result<()> = try {
            let key_locker_manager = TKeyLockerManager::new().await?;
            key_locker_manager.reset().await?;

            // Create a guest key (empty keyphrase) and a real key
            let _guest_key_locker = key_locker_manager.create("".to_string()).await?;
            let real_key_locker = key_locker_manager.create("real_keyphrase".to_string()).await?;
            let real_public_key = real_key_locker.client_id().id.to_hex_str();

            // list() should only return the real key, not the guest
            let listed_keys = key_locker_manager.list().await?;
            assert_eq!(listed_keys.len(), 1, "list() should return exactly 1 key, got {}", listed_keys.len());
            assert_eq!(listed_keys[0], real_public_key);
            assert!(!listed_keys.contains(&GUEST_CLIENT_ID.to_string()), "list() must not return the guest client ID");

            key_locker_manager.reset().await?;
        };

        if let Err(e) = result {
            panic!("Test failed: {}", e);
        }
    }

    pub async fn sign_test<TKeyLocker: KeyLocker, TKeyLockerManager: KeyLockerManager<TKeyLocker>>() {
        let result = try {
            let key_locker_manager = TKeyLockerManager::new().await?;

            // Start clean
            key_locker_manager.reset().await?;
            assert_eq!(0, key_locker_manager.list().await?.len());

            // Add a key
            let key_locker_1 = key_locker_manager.create("key_phrase_1".to_string()).await?;

            // Test a signature
            {
                let mut message_to_sign = [0u8; 1024];
                tools::random_fill_bytes(&mut message_to_sign);
                let signature = key_locker_1.sign(&message_to_sign).await?;
                let verification_key = VerificationKey::from_bytes(&key_locker_1.client_id().verification_key_bytes)?;
                let result = signing::verify(&verification_key, &signature, &message_to_sign);
                assert!(result.is_ok())
            }
        };

        if let Err(e) = result {
            panic!("Test failed: {}", e);
        }
    }

    /// Confirms that `GUEST_CLIENT_ID` matches the client ID derived from an empty keyphrase.
    /// If the key derivation changes, this test will fail and the constant must be updated.
    #[test]
    fn guest_client_id_constant_test() {
        use crate::tools::client_id::ClientId;
        use crate::tools::keys::Keys;
        let keys = Keys::from_phrase("").expect("empty keyphrase should always work");
        let client_id = ClientId::new(keys.verification_key_bytes, keys.pq_commitment_bytes).expect("client id creation should always work");
        assert_eq!(client_id.id.to_hex_str(), GUEST_CLIENT_ID, "GUEST_CLIENT_ID constant is stale — update it to match the current empty-keyphrase derivation");
    }
}
