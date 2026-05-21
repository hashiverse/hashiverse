//! # `EncodedPostV1` — individual post wire format
//!
//! The canonical on-the-wire representation of a single user post.
//! [`EncodedPostV1`] pairs:
//!
//! - an [`EncodedPostHeaderV1`] carrying the author's keys, timestamp, linked base IDs,
//!   and signing-authority choice,
//! - the post body bytes (compressed + encrypted),
//! - an Ed25519 signature (or delegated-key signature per `PostSigningAuthorityV1`).
//!
//! The `post_id` is the hash of the signature, which gives every post a deterministic,
//! collision-free identifier derivable from the bytes alone. On the wire both header
//! and body are compressed and encrypted under passwords derived from the client id
//! and the set of linked base IDs — so that recipients who already know those keys
//! can decrypt without an extra key exchange.
//!
//! [`EncodedPostV1::bytes_without_body`] returns just the header bytes, so that
//! previews and timeline skeletons can be delivered before the full body is paid for
//! in the reader's bandwidth budget.

use crate::client::key_locker::key_locker::KeyLocker;
use crate::tools::client_id::ClientId;
use crate::tools::time::TimeMillis;
use crate::tools::types::{Hash, Id, PQCommitmentBytes, Signature, VerificationKey, VerificationKeyBytes, HASH_BYTES, ID_BYTES, SIGNATURE_BYTES};
use crate::tools::{compression, encryption, hashing, json, signing};
use crate::{anyhow_assert_eq, anyhow_assert_ge};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use std::fmt::Debug;
use std::sync::Arc;

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct EncodedPostHeaderSignatureDirectV1 {}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct EncodedPostHeaderSignatureEphemeralV1 {
    // Not yet implemented
    // ephemeral_salt: Salt,
    // ephemeral_signature: Signature,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct EncodedPostHeaderSignatureDelegationV1 {
    // Not yet implemented
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct EncodedPostHeaderSignatureMechanismV1 {
    direct: Option<EncodedPostHeaderSignatureDirectV1>,
    ephemeral: Option<EncodedPostHeaderSignatureEphemeralV1>,
    delegation: Option<EncodedPostHeaderSignatureDelegationV1>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct EncodedPostHeaderV1 {
    pub verification_key_bytes: VerificationKeyBytes,
    pub pq_commitment_bytes: PQCommitmentBytes,
    pub time_millis: TimeMillis,
    pub post_length: usize,
    pub linked_base_ids: Vec<Id>,
    pub signature_mechanism: EncodedPostHeaderSignatureMechanismV1,
}

impl EncodedPostHeaderV1 {
    pub fn client_id(&self) -> anyhow::Result<ClientId> {
        ClientId::new(self.verification_key_bytes, self.pq_commitment_bytes)
    }
}

/// A single user post in its canonical, verifiable form as it travels across the network.
///
/// `EncodedPostV1` bundles the post body together with everything a recipient needs to
/// authenticate it: the author's [`EncodedPostHeaderV1`] (verification key, PQ commitment,
/// timestamp, linked base IDs, and which signature mechanism is in use), the author's
/// [`Signature`] over the canonical bytes, and a `post_id` which is simply the hash of the
/// signature (so every post has a deterministic, collision-free identifier).
///
/// On the wire the post body is compressed and optionally encrypted; this struct is the
/// decoded in-memory form. [`EncodedPostV1::bytes_without_body`] exists for callers who only
/// need the header portion — e.g. a timeline that is showing post previews without having
/// yet fetched the full body.
#[derive(Debug, PartialEq, Clone)]
pub struct EncodedPostV1 {
    pub post_id: Id, // Simply the hash of the signature
    pub signature: Signature,
    pub header: EncodedPostHeaderV1,
    pub post: String,
}

impl EncodedPostV1 {
    /// Returns the header portion of encoded post bytes (post_id + signature + version +
    /// hashes + lengths + encrypted_header), excluding the encrypted body.
    /// The result is suitable for passing to `decode_from_bytes(bytes, password, false, false)`.
    pub fn bytes_without_body(bytes: Bytes) -> anyhow::Result<Bytes> {
        let fixed_prefix = ID_BYTES + SIGNATURE_BYTES + 1 + HASH_BYTES * 2;
        anyhow_assert_ge!(bytes.len(), fixed_prefix + 4 + 4, "Bytes too short for header");
        let header_encrypted_length = u32::from_be_bytes(bytes[fixed_prefix..fixed_prefix + 4].try_into()?) as usize;
        let length_without_body = fixed_prefix + 4 + 4 + header_encrypted_length;
        anyhow_assert_ge!(bytes.len(), length_without_body, "Bytes too short for encrypted header");
        Ok(bytes.slice(..length_without_body))
    }

    pub fn new(client_id: &ClientId, timestamp: TimeMillis, linked_base_ids: Vec<Id>, post: &str) -> Self {
        let post = post.to_string();
        let post_length = post.len();

        Self {
            post_id: Id::zero(),
            signature: Signature::zero(),
            header: EncodedPostHeaderV1 {
                verification_key_bytes: client_id.verification_key_bytes,
                pq_commitment_bytes: client_id.pq_commitment_bytes,
                time_millis: timestamp,
                post_length,
                linked_base_ids,
                signature_mechanism: EncodedPostHeaderSignatureMechanismV1 {
                    direct: None,
                    ephemeral: None,
                    delegation: None,
                },
            },

            post,
        }
    }

    pub async fn encode_to_bytes_direct(&mut self, key_locker: &Arc<dyn KeyLocker>) -> anyhow::Result<EncodedPostBytesV1> {
        self.header.signature_mechanism.direct = Some(EncodedPostHeaderSignatureDirectV1 {});

        let mut passwords = Vec::new();
        {
            let client_id = ClientId::id_from_parts(&self.header.verification_key_bytes, &self.header.pq_commitment_bytes)?;
            passwords.push(client_id.as_bytes().to_vec());
            let mut linked_base_ids = self.header.linked_base_ids.iter().map(|id| id.as_bytes().to_vec()).collect();
            passwords.append(&mut linked_base_ids);
        }

        let header = json::struct_to_bytes(&self.header)?;
        let header_compressed = compression::compress_for_size(&header)?.to_bytes();
        let header_encrypted = encryption::encrypt_weak(&header_compressed, &passwords)?;
        let header_encrypted_hash = hashing::hash(&header_encrypted);

        let post_compressed = compression::compress_for_size(self.post.as_bytes())?.to_bytes();
        let post_encrypted = encryption::encrypt_weak(&post_compressed, &passwords)?;
        let post_encrypted_hash = hashing::hash(&post_encrypted);

        let hash = hashing::hash_multiple(&[header_encrypted_hash.as_ref(), post_encrypted_hash.as_ref()]);
        let signature = key_locker.sign(hash.as_ref()).await?;
        let post_id = Id::from_hash(hashing::hash(signature.as_ref()))?;

        self.signature = signature;
        self.post_id = post_id;

        let mut bytes = BytesMut::new();
        bytes.put_slice(post_id.as_ref());
        bytes.put_slice(signature.as_ref());
        bytes.put_u8(1u8); // Version
        bytes.put_slice(header_encrypted_hash.as_ref());
        bytes.put_slice(post_encrypted_hash.as_ref());
        bytes.put_u32(header_encrypted.len() as u32);
        bytes.put_u32(post_encrypted.len() as u32);
        bytes.put_slice(header_encrypted.as_ref());
        bytes.put_slice(post_encrypted.as_ref());

        let bytes = bytes.freeze();

        Ok(EncodedPostBytesV1 {
            length_without_body: bytes.len() - post_encrypted.len(),
            bytes,
        })
    }

    pub fn decode_signature_from_bytes(bytes: &[u8]) -> anyhow::Result<Signature> {
        anyhow::ensure!(bytes.len() >= SIGNATURE_BYTES, "decode_signature_from_bytes: need {} bytes, got {}", SIGNATURE_BYTES, bytes.len());
        Signature::from_slice(&bytes[0..SIGNATURE_BYTES])
    }

    pub fn decode_from_bytes(mut bytes: Bytes, password_base_id: &Id, expect_body: bool, decode_body: bool) -> anyhow::Result<Self> {
        let password = password_base_id.as_ref();

        anyhow_assert_ge!(bytes.remaining(), ID_BYTES, "Missing post_id");
        let post_id = Id::from_slice(&bytes.split_to(ID_BYTES))?;

        anyhow_assert_ge!(bytes.remaining(), SIGNATURE_BYTES, "Missing signature");
        let signature = Signature::from_slice(&bytes.split_to(SIGNATURE_BYTES))?;

        anyhow_assert_ge!(bytes.remaining(), 1, "Missing version");
        let version = bytes.get_u8();
        if 1 != version {
            anyhow::bail!("Invalid buffer: unknown version");
        }

        anyhow_assert_ge!(bytes.remaining(), HASH_BYTES, "Missing encrypted hashes");
        let header_encrypted_hash = Hash::from_slice(&bytes.split_to(HASH_BYTES))?;
        anyhow_assert_ge!(bytes.remaining(), HASH_BYTES, "Missing encrypted hashes");
        let post_encrypted_hash = Hash::from_slice(&bytes.split_to(HASH_BYTES))?;

        anyhow_assert_ge!(bytes.remaining(), size_of::<u32>(), "Missing encrypted lengths");
        let header_encrypted_length = bytes.get_u32() as usize;
        anyhow_assert_ge!(bytes.remaining(), size_of::<u32>(), "Missing encrypted lengths");
        let post_encrypted_length = bytes.get_u32() as usize;

        anyhow_assert_ge!(bytes.remaining(), header_encrypted_length, "Missing encrypted header");
        let header_encrypted = bytes.split_to(header_encrypted_length);

        let post_encrypted = match expect_body {
            true => {
                anyhow_assert_ge!(bytes.remaining(), post_encrypted_length, "Missing encrypted post");
                bytes.split_to(post_encrypted_length)
            }
            false => Bytes::new(),
        };

        anyhow_assert_eq!(bytes.remaining(), 0, "Unexpected remaining data");

        // Recreate the header
        // log::trace!("Decrypting header length {}", header_encrypted.len());
        let header_compressed = encryption::decrypt(&header_encrypted, password)?;
        // log::trace!("Decompressing header length {}", header_compressed.len());
        let header_bytes = compression::decompress(&header_compressed)?.to_bytes();
        // log::trace!("Done header length {}", header_bytes.len());
        let header = json::bytes_to_struct::<EncodedPostHeaderV1>(&header_bytes)?;

        // Verify the hashes
        anyhow_assert_eq!(header_encrypted_hash, hashing::hash(&header_encrypted));
        if expect_body {
            anyhow_assert_eq!(post_encrypted_hash, hashing::hash(&post_encrypted));
        }

        // Verify the signature
        {
            let hash = hashing::hash_multiple(&[header_encrypted_hash.as_ref(), post_encrypted_hash.as_ref()]);

            // Direct
            if header.signature_mechanism.direct.is_some() {
                let verification_key = VerificationKey::from_bytes(&header.verification_key_bytes)?;
                signing::verify(&verification_key, &signature, hash.as_ref())?;
            }
            // Ephemeral
            else if header.signature_mechanism.ephemeral.is_some() {
                anyhow::bail!("Signature verification ephemeral not implemented")
            }
            // Delegation
            else if header.signature_mechanism.delegation.is_some() {
                anyhow::bail!("Signature verification delegation not implemented")
            }
            // None!
            else {
                anyhow::bail!("No signature verification mechanisms")
            }
        }

        // Verify the post_id
        {
            anyhow_assert_eq!(post_id, Id::from_hash(hashing::hash(signature.as_ref()))?, "post_id is not the has of signature");
        }

        // Recreate the body
        let post = match expect_body && decode_body {
            true => {
                // log::trace!("Decrypting post length {}", post_encrypted.len());
                let post_compressed = encryption::decrypt(&post_encrypted, password)?;
                // log::trace!("Decompressing post length {}", post_compressed.len());
                let post_bytes = compression::decompress(&post_compressed)?.to_bytes();
                // log::trace!("Done post length {}", post_bytes.len());
                let post = std::str::from_utf8(&post_bytes)?;
                if post.len() != header.post_length {
                    anyhow::bail!("Post length mismatch: header.post_length={}, actual={}", header.post_length, post.len())
                }
                post.to_string()
            }
            false => String::new(),
        };

        // Woohoo!
        Ok(Self { post_id, signature, header, post })
    }
}

pub struct EncodedPostBytesV1 {
    length_without_body: usize,
    bytes: Bytes,
}

impl EncodedPostBytesV1 {
    pub fn bytes_without_body(&self) -> &[u8] {
        &self.bytes[..self.length_without_body]
    }
    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::key_locker::key_locker::{KeyLocker, KeyLockerManager};
    use crate::client::key_locker::mem_key_locker::MemKeyLockerManager;
    use crate::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};

    #[tokio::test]
    async fn test_post_v1_verification() -> anyhow::Result<()> {
        let key_locker_manager = MemKeyLockerManager::new().await?;
        let key_locker: Arc<dyn KeyLocker> = key_locker_manager.create("this is a random keyphrase".to_string()).await?;
        let time_provider = RealTimeProvider;
        let client_id = key_locker.client_id();
        let timestamp = time_provider.current_time_millis();
        let linked_base_ids = vec![client_id.id, Id::random(), Id::random(), Id::random()];

        let password1 = linked_base_ids[0];
        let password2 = linked_base_ids[1];

        let mut encoded_post = EncodedPostV1::new(client_id, timestamp, linked_base_ids, "this is a test post");
        let bytes = encoded_post.encode_to_bytes_direct(&key_locker).await?;

        // Test decoding with multiple passwords
        {
            {
                let decoded_post = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(bytes.bytes()), &password1, true, true)?;
                assert_eq!(encoded_post, decoded_post);
            }

            {
                let decoded_post = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(bytes.bytes()), &password2, true, true)?;
                assert_eq!(encoded_post, decoded_post);
            }
        }

        // Test decoding without body
        {
            let decoded_post = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(bytes.bytes()), &password2, true, false)?;
            assert_eq!("", decoded_post.post);
        }

        // Incorrect password
        {
            let wrong_password = Id::random();
            let decoded_post_attempt = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(bytes.bytes()), &wrong_password, true, true);
            if decoded_post_attempt.is_ok() {
                anyhow::bail!("Decoding with wrong password should fail")
            }
        }

        // Tampering with bytes
        {
            let mut tampered_bytes = Vec::from(bytes.bytes());
            tampered_bytes[100] = 0u8;
            let decoded_post_attempt = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(&tampered_bytes), &password1, true, true);
            if decoded_post_attempt.is_ok() {
                anyhow::bail!("Decoding tampered bytes should fail")
            }
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_header_only_verification() -> anyhow::Result<()> {
        let key_locker_manager = MemKeyLockerManager::new().await?;
        let key_locker: Arc<dyn KeyLocker> = key_locker_manager.create("this is a random keyphrase".to_string()).await?;
        let time_provider = RealTimeProvider;
        let client_id = key_locker.client_id();
        let timestamp = time_provider.current_time_millis();
        let linked_base_ids = vec![client_id.id, Id::random(), Id::random(), Id::random()];

        let password1 = linked_base_ids[0];
        let password2 = linked_base_ids[1];

        let mut encoded_post = EncodedPostV1::new(client_id, timestamp, linked_base_ids, "this is a test post");
        let bytes = encoded_post.encode_to_bytes_direct(&key_locker).await?;

        {
            let decoded_post = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(bytes.bytes_without_body()), &password1, false, false)?;
            assert_eq!(encoded_post.signature, decoded_post.signature);
            assert_eq!(encoded_post.header, decoded_post.header);
            assert_eq!(String::new(), decoded_post.post);
        }

        {
            let decoded_post = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(bytes.bytes_without_body()), &password2, false, false)?;
            assert_eq!(encoded_post.signature, decoded_post.signature);
            assert_eq!(encoded_post.header, decoded_post.header);
            assert_eq!(String::new(), decoded_post.post);
        }

        // Provide the body when not expected
        {
            let decoded_post_attempt = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(bytes.bytes()), &password2, false, false);
            if decoded_post_attempt.is_ok() {
                anyhow::bail!("Decoding with wrong too many bytes should fail")
            }
        }

        {
            let wrong_password = Id::random();
            let decoded_post_attempt = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(bytes.bytes_without_body()), &wrong_password, false, false);
            if decoded_post_attempt.is_ok() {
                anyhow::bail!("Decoding with wrong password should fail")
            }
        }

        {
            let mut tampered_bytes = Vec::from(bytes.bytes_without_body());
            tampered_bytes[100] = 0u8;
            let decoded_post_attempt = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(&tampered_bytes), &password1, false, false);
            if decoded_post_attempt.is_ok() {
                anyhow::bail!("Decoding tampered bytes should fail")
            }
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_post_with_no_linked_base_ids() -> anyhow::Result<()> {
        let key_locker_manager = MemKeyLockerManager::new().await?;
        let key_locker: Arc<dyn KeyLocker> = key_locker_manager.create("this is a random keyphrase".to_string()).await?;
        let time_provider = RealTimeProvider;
        let client_id = key_locker.client_id();
        let timestamp = time_provider.current_time_millis();

        // No linked_base_ids — only the client_id itself is used as password
        let password = client_id.id;
        let mut encoded_post = EncodedPostV1::new(client_id, timestamp, vec![], "post with no linked ids");
        let bytes = encoded_post.encode_to_bytes_direct(&key_locker).await?;

        // Decrypt with the client_id password succeeds
        let decoded_post = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(bytes.bytes()), &password, true, true)?;
        assert_eq!(encoded_post, decoded_post);

        // Wrong password fails
        let wrong_password = Id::random();
        let attempt = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(bytes.bytes()), &wrong_password, true, true);
        if attempt.is_ok() {
            anyhow::bail!("Decoding with wrong password should fail");
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_bytes_without_body_round_trip() -> anyhow::Result<()> {
        let key_locker_manager = MemKeyLockerManager::new().await?;
        let key_locker: Arc<dyn KeyLocker> = key_locker_manager.create("test keyphrase for bytes_without_body".to_string()).await?;
        let time_provider = RealTimeProvider;
        let client_id = key_locker.client_id();
        let timestamp = time_provider.current_time_millis();
        let linked_base_ids = vec![client_id.id, Id::random(), Id::random()];

        let password = linked_base_ids[0];

        let mut encoded_post = EncodedPostV1::new(client_id, timestamp, linked_base_ids, "test post for bytes_without_body");
        let bytes = encoded_post.encode_to_bytes_direct(&key_locker).await?;

        // bytes_without_body from the static method should match the EncodedPostBytesV1 method
        let full_bytes = Bytes::copy_from_slice(bytes.bytes());
        let header_bytes_static = EncodedPostV1::bytes_without_body(full_bytes.clone())?;
        let header_bytes_original = Bytes::copy_from_slice(bytes.bytes_without_body());
        assert_eq!(header_bytes_static, header_bytes_original);

        // The header bytes should be decodable with expect_body=false
        let decoded_header_only = EncodedPostV1::decode_from_bytes(header_bytes_static.clone(), &password, false, false)?;
        assert_eq!(encoded_post.header, decoded_header_only.header);
        assert_eq!(encoded_post.signature, decoded_header_only.signature);
        assert_eq!(encoded_post.post_id, decoded_header_only.post_id);
        assert_eq!("", decoded_header_only.post);

        // Hex round-trip (simulating the WASM bridge flow)
        let hex_encoded = hex::encode(&header_bytes_static);
        let hex_decoded = Bytes::from(hex::decode(&hex_encoded)?);
        let decoded_from_hex = EncodedPostV1::decode_from_bytes(hex_decoded, &password, false, false)?;
        assert_eq!(encoded_post.header, decoded_from_hex.header);

        Ok(())
    }

    // ── Robustness tests: decode_signature_from_bytes ──

    #[test]
    fn test_decode_signature_from_bytes_empty() {
        assert!(EncodedPostV1::decode_signature_from_bytes(&[]).is_err());
    }

    #[test]
    fn test_decode_signature_from_bytes_too_short() {
        assert!(EncodedPostV1::decode_signature_from_bytes(&[0u8; SIGNATURE_BYTES - 1]).is_err());
    }

    #[test]
    fn test_decode_signature_from_bytes_exact_length() {
        // Should succeed (signature is all zeros, but structurally valid)
        assert!(EncodedPostV1::decode_signature_from_bytes(&[0u8; SIGNATURE_BYTES]).is_ok());
    }

    // ── Robustness tests: decode_from_bytes ──

    #[test]
    fn test_decode_from_bytes_empty() {
        let password = Id::random();
        assert!(EncodedPostV1::decode_from_bytes(Bytes::new(), &password, true, true).is_err());
    }

    #[test]
    fn test_decode_from_bytes_too_short_for_post_id() {
        let password = Id::random();
        let bytes = Bytes::from_static(&[0u8; ID_BYTES - 1]);
        assert!(EncodedPostV1::decode_from_bytes(bytes, &password, true, true).is_err());
    }

    #[test]
    fn test_decode_from_bytes_garbage() {
        let password = Id::random();
        let bytes = Bytes::from_static(&[0xff; 256]);
        assert!(EncodedPostV1::decode_from_bytes(bytes, &password, true, true).is_err());
    }

    // ── Robustness tests: bytes_without_body ──

    #[test]
    fn test_bytes_without_body_empty() {
        assert!(EncodedPostV1::bytes_without_body(Bytes::new()).is_err());
    }

    #[test]
    fn test_bytes_without_body_too_short() {
        let bytes = Bytes::from_static(&[0u8; 10]);
        assert!(EncodedPostV1::bytes_without_body(bytes).is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod bolero_fuzz {
        use crate::protocol::posting::encoded_post::EncodedPostV1;
        use crate::tools::types::Id;
        use bytes::Bytes;

        #[test]
        fn fuzz_decode_from_bytes() {
            bolero::check!().for_each(|data: &[u8]| {
                let password = Id::zero();
                let _ = EncodedPostV1::decode_from_bytes(Bytes::copy_from_slice(data), &password, true, true);
            });
        }

        #[test]
        fn fuzz_decode_signature_from_bytes() {
            bolero::check!().for_each(|data: &[u8]| {
                let _ = EncodedPostV1::decode_signature_from_bytes(data);
            });
        }

        #[test]
        fn fuzz_bytes_without_body() {
            bolero::check!().for_each(|data: &[u8]| {
                let _ = EncodedPostV1::bytes_without_body(Bytes::copy_from_slice(data));
            });
        }
    }
}
