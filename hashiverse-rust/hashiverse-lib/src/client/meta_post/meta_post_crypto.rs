//! # Encryption of the private meta-post section
//!
//! The private half of a [`crate::client::meta_post::meta_post::MetaPostV1`]
//! (feedback thresholds, warning toggles, anything the account holder doesn't want
//! public) is symmetric-encrypted with a key derived from the account's own private
//! signing key: `sig = sign(signature_key, constant)` then `key = blake3(sig)`.
//!
//! Because signing requires the private key, only the account holder can recover the
//! symmetric key; because the key is deterministic, every device that unlocks the same
//! [`crate::tools::keys::Keys`] derives the same key and can read/write the same
//! private section. No key exchange, no on-network secrets.
//!
//! The `encrypt_private_section` / `decrypt_private_section` pair here is the only
//! place this derivation happens.

use crate::client::key_locker::key_locker::KeyLocker;
use crate::client::meta_post::meta_post::MetaPostPrivateV1;
use crate::tools::types::Salt;
use crate::tools::{compression, encryption, json};

const META_POST_ENCRYPTION_CONTEXT: &[u8] = b"hashiverse-meta-post-encryption";

/// Derive a 32-byte symmetric encryption key by signing a well-known
/// constant concatenated with the provided salt, then hashing the
/// signature with blake3.
///
/// Only the holder of the private signing key can reproduce this key,
/// making it suitable for encrypting data that only the user should read.
pub async fn derive_meta_post_encryption_key(key_locker: &dyn KeyLocker, salt: &Salt) -> anyhow::Result<Vec<u8>> {
    let mut message = Vec::with_capacity(META_POST_ENCRYPTION_CONTEXT.len() + salt.as_ref().len());
    message.extend_from_slice(META_POST_ENCRYPTION_CONTEXT);
    message.extend_from_slice(salt.as_ref());

    let signature = key_locker.sign(&message).await?;
    let key_hash = blake3::hash(signature.as_ref());
    Ok(key_hash.as_bytes().to_vec())
}

/// Encrypt a `MetaPostPrivateV1` into a hex-encoded string.
pub async fn encrypt_private_section(key_locker: &dyn KeyLocker, salt: &Salt, private: &MetaPostPrivateV1) -> anyhow::Result<String> {
    let symmetric_key = derive_meta_post_encryption_key(key_locker, salt).await?;
    let plaintext = json::struct_to_bytes(private)?;
    let compressed = compression::compress_for_size(&plaintext)?.to_bytes();
    let encrypted = encryption::encrypt_strong(&compressed, &vec![symmetric_key])?;
    Ok(hex::encode(encrypted))
}

/// Decrypt a hex-encoded string back into a `MetaPostPrivateV1`.
pub async fn decrypt_private_section(key_locker: &dyn KeyLocker, salt: &Salt, encrypted_hex: &str) -> anyhow::Result<MetaPostPrivateV1> {
    let symmetric_key = derive_meta_post_encryption_key(key_locker, salt).await?;
    let encrypted = hex::decode(encrypted_hex)?;
    let compressed = encryption::decrypt(&encrypted, &symmetric_key)?;
    let plaintext = compression::decompress(&compressed)?.to_bytes();
    let private: MetaPostPrivateV1 = json::bytes_to_struct(&plaintext)?;
    Ok(private)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::key_locker::mem_key_locker::MemKeyLockerManager;
    use crate::client::key_locker::key_locker::KeyLockerManager;
    use crate::client::meta_post::meta_post::VersionedField;
    use crate::tools::time::TimeMillis;

    fn t(millis: i64) -> TimeMillis { TimeMillis(millis) }

    #[tokio::test]
    async fn encrypt_decrypt_private_section_roundtrip() -> anyhow::Result<()> {
        let key_locker_manager = MemKeyLockerManager::new().await?;
        let key_locker = key_locker_manager.create("test_phrase".to_string()).await?;
        let salt = Salt::random();

        let mut private = MetaPostPrivateV1::empty();
        private.followed_client_ids.insert("client_abc".to_string(), VersionedField::new(true, t(100)));
        private.followed_hashtags.insert("rust".to_string(), VersionedField::new(true, t(200)));
        private.content_thresholds.insert(101, VersionedField::new(100, t(300)));
        private.skip_warnings_for_followed = VersionedField::new(true, t(400));

        let encrypted_hex = encrypt_private_section(key_locker.as_ref(), &salt, &private).await?;
        let decrypted = decrypt_private_section(key_locker.as_ref(), &salt, &encrypted_hex).await?;

        assert_eq!(private, decrypted);
        Ok(())
    }

    #[tokio::test]
    async fn different_salt_produces_different_key() -> anyhow::Result<()> {
        let key_locker_manager = MemKeyLockerManager::new().await?;
        let key_locker = key_locker_manager.create("test_phrase".to_string()).await?;

        let salt_a = Salt::random();
        let salt_b = Salt::random();

        let key_a = derive_meta_post_encryption_key(key_locker.as_ref(), &salt_a).await?;
        let key_b = derive_meta_post_encryption_key(key_locker.as_ref(), &salt_b).await?;

        assert_ne!(key_a, key_b);
        Ok(())
    }

    #[tokio::test]
    async fn same_salt_produces_same_key() -> anyhow::Result<()> {
        let key_locker_manager = MemKeyLockerManager::new().await?;
        let key_locker = key_locker_manager.create("test_phrase".to_string()).await?;

        let salt = Salt::random();

        let key_1 = derive_meta_post_encryption_key(key_locker.as_ref(), &salt).await?;
        let key_2 = derive_meta_post_encryption_key(key_locker.as_ref(), &salt).await?;

        assert_eq!(key_1, key_2);
        Ok(())
    }

    #[tokio::test]
    async fn different_user_cannot_decrypt() -> anyhow::Result<()> {
        let key_locker_manager = MemKeyLockerManager::new().await?;
        let key_locker_a = key_locker_manager.create("client_a_phrase".to_string()).await?;
        let key_locker_b = key_locker_manager.create("client_b_phrase".to_string()).await?;

        let salt = Salt::random();
        let private = MetaPostPrivateV1::empty();

        let encrypted_hex = encrypt_private_section(key_locker_a.as_ref(), &salt, &private).await?;
        let decrypt_result = decrypt_private_section(key_locker_b.as_ref(), &salt, &encrypted_hex).await;

        assert!(decrypt_result.is_err(), "Different user should not be able to decrypt");
        Ok(())
    }

    #[tokio::test]
    async fn encrypt_decrypt_with_tombstones() -> anyhow::Result<()> {
        let key_locker_manager = MemKeyLockerManager::new().await?;
        let key_locker = key_locker_manager.create("test_phrase".to_string()).await?;
        let salt = Salt::random();

        let mut private = MetaPostPrivateV1::empty();
        private.followed_client_ids.insert("deleted_client".to_string(), VersionedField::tombstone(t(999)));
        private.skip_warnings_for_followed = VersionedField::tombstone(t(888));

        let encrypted_hex = encrypt_private_section(key_locker.as_ref(), &salt, &private).await?;
        let decrypted = decrypt_private_section(key_locker.as_ref(), &salt, &encrypted_hex).await?;

        assert_eq!(private, decrypted);
        assert!(decrypted.followed_client_ids["deleted_client"].is_tombstone());
        assert!(decrypted.skip_warnings_for_followed.is_tombstone());
        Ok(())
    }
}
