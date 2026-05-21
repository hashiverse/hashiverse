//! # `EncodedPostBundleV1` — the server-signed index of posts at a location
//!
//! A "post bundle" is a server's durable record of every post accepted into a given
//! `(location_id, time bucket)` pair. This is the unit of replication, caching, and
//! healing on the network — timelines fetch bundles, caches store bundles, and the
//! two-phase heal protocol reconciles divergent bundles.
//!
//! The encoding is two parts glued together:
//!
//! - [`EncodedPostBundleHeaderV1`] — JSON-encoded, server-signed, enumerating the
//!   `post_id`s and per-post body lengths the bundle contains, plus
//!   `sealed`/`overflowed` flags, any `healed_ids`, and the publishing
//!   [`crate::protocol::peer::Peer`] record. This is the bit that proves "server X
//!   attests that these posts lived here at time T".
//! - A concatenation of each [`crate::protocol::posting::encoded_post::EncodedPostV1`]
//!   body, in the same order as the header's post id list.
//!
//! `verify()` is exhaustive: it checks the header signature, confirms the sum of
//! header-declared lengths exactly spans the body, and decrypts each post to verify
//! its own signature and that its plaintext `post_id` matches the header's claim.

use std::collections::HashSet;
use crate::{anyhow_assert_eq, anyhow_assert_ge};
use crate::protocol::peer::Peer;
use crate::protocol::posting::encoded_post::EncodedPostV1;
use crate::tools::time::TimeMillis;
use crate::tools::types::{Hash, Id, ID_BYTES, Signature, SignatureKey, VerificationKey};
use crate::tools::{hashing, json, signing};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use std::fmt::{Debug, Display};

/// The server-signed index for all posts accumulated under one (location, time-bucket) key.
///
/// Posts on hashiverse are not stored one-by-one on the DHT: they are grouped into "bundles"
/// keyed by a [`crate::tools::buckets::BucketLocation`] (a location_id plus a time bucket).
/// An `EncodedPostBundleHeaderV1` is the header that a server publishes describing the
/// bundle's current state — the list of post IDs it holds, their lengths, which ones have
/// been healed (re-uploaded after loss), whether the bundle has overflowed (too many posts,
/// go more granular), whether it is sealed (old enough that new posts are unlikely), and the
/// [`Peer`] record of the server publishing it.
///
/// The whole header is signed by the publishing peer, which lets any client reading the
/// bundle verify both the contents list and the identity of the hosting server before
/// deciding to fetch individual posts.
// Contains all the posts for a given location_id in a particular bucket
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct EncodedPostBundleHeaderV1 {
    pub time_millis: TimeMillis, // When was this bundle last updated
    pub location_id: Id,
    pub overflowed: bool, // This bucket has overflowed, so go more granular
    pub sealed: bool,    // Enough time has passed that it is unlikely that new posts will be added to this cluster
    pub num_posts: u8,
    pub encoded_post_ids: Vec<Id>, 
    pub encoded_post_lengths: Vec<usize>,
    pub encoded_post_healed: HashSet<Id>,
    pub peer: Peer,

    pub signature: Signature, // Peer.sign(get_hash_for_signing())
}

impl EncodedPostBundleHeaderV1 {
    pub fn get_hash_for_signing(&self) -> anyhow::Result<Hash> {
        let time_millis_be = self.time_millis.encode_be();
        let overflowed_bytes = [self.overflowed as u8];
        let sealed_bytes = [self.sealed as u8];
        let num_posts_bytes = [self.num_posts];
        let encoded_post_lengths_be: Vec<[u8; 8]> = self.encoded_post_lengths.iter().map(|&l| (l as u64).to_be_bytes()).collect();
        let peer_hash = self.peer.signature_hash_generate()?;

        let mut hash_input: Vec<&[u8]> = vec![
            time_millis_be.as_ref(),
            self.location_id.as_ref(),
            &overflowed_bytes,
            &sealed_bytes,
            &num_posts_bytes,
        ];

        for encoded_post_id in &self.encoded_post_ids {
            hash_input.push(encoded_post_id.as_ref());
        }
        for length_be in &encoded_post_lengths_be {
            hash_input.push(length_be.as_ref());
        }
        let mut healed_ids_sorted: Vec<Id> = self.encoded_post_healed.iter().copied().collect();
        healed_ids_sorted.sort();
        for healed_id in &healed_ids_sorted {
            hash_input.push(healed_id.as_ref());
        }
        hash_input.push(peer_hash.as_ref());

        Ok(hashing::hash_multiple(&hash_input))
    }

    pub fn signature_generate(&mut self, signature_key: &SignatureKey) -> anyhow::Result<()> {
        let hash = self.get_hash_for_signing()?;
        self.signature = signing::sign(signature_key, hash.as_ref());
        Ok(())
    }

    pub fn signature_verify(&self) -> anyhow::Result<()> {
        let hash = self.get_hash_for_signing()?;
        let verification_key = VerificationKey::from_bytes(&self.peer.verification_key_bytes)?;
        signing::verify(&verification_key, &self.signature, hash.as_ref())
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        anyhow_assert_eq!(self.num_posts, self.encoded_post_lengths.len() as u8);
        anyhow_assert_eq!(self.num_posts, self.encoded_post_ids.len() as u8);
        for healed_id in &self.encoded_post_healed {
            if !self.encoded_post_ids.contains(healed_id) {
                anyhow::bail!("encoded_post_healed contains id not in encoded_post_ids: {}", healed_id);
            }
        }
        self.signature_verify()?;
        Ok(())
    }
}

impl Display for EncodedPostBundleHeaderV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EncodedPostBundleHeaderV1 [ location_id: {}, time_millis: {}, num_posts: {}, overflowed: {}, sealed: {} ]", self.location_id, self.time_millis, self.num_posts, self.overflowed, self.sealed)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct EncodedPostBundleV1 {
    pub header: EncodedPostBundleHeaderV1,
    pub encoded_posts_bytes: Bytes,
}

impl Display for EncodedPostBundleV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EncodedPostBundleV1 [ header: {}, length: {} ]", self.header, self.encoded_posts_bytes.len())
    }
}

impl EncodedPostBundleV1 {
    pub fn to_bytes(&self) -> anyhow::Result<Bytes> {
        let mut bytes = BytesMut::new();

        let json_post_bundle_header = json::struct_to_bytes(&self.header)?;
        bytes.put_u8(1u8); // Version
        bytes.put_u64(json_post_bundle_header.len() as u64);
        bytes.put_u64(self.encoded_posts_bytes.len() as u64);
        bytes.put_slice(json_post_bundle_header.as_ref());
        bytes.put_slice(self.encoded_posts_bytes.as_ref());

        Ok(bytes.freeze())
    }

    pub fn from_bytes(mut bytes: Bytes, decode_body: bool) -> anyhow::Result<Self> {
        anyhow_assert_ge!(bytes.remaining(), 1, "Missing version");
        let version = bytes.get_u8();
        anyhow_assert_eq!(1, version, "Invalid version");

        anyhow_assert_ge!(bytes.remaining(), 8, "Missing header length");
        let header_len = bytes.get_u64() as usize;
        anyhow_assert_ge!(bytes.remaining(), 8, "Missing body length");
        let body_len = bytes.get_u64() as usize;

        let total_length = header_len.checked_add(body_len).ok_or_else(|| anyhow::anyhow!("header_len + body_len overflow"))?;
        anyhow_assert_ge!(bytes.remaining(), total_length, "Truncated post bundle data");

        let header_bytes = bytes.copy_to_bytes(header_len);
        let header = json::bytes_to_struct(&header_bytes)?;

        let body = match decode_body {
            true => {
                let body_bytes = bytes.copy_to_bytes(body_len);
                anyhow_assert_eq!(bytes.remaining(), 0, "Excess data");
                body_bytes
            },
            false => Bytes::new(),
        };

        Ok(EncodedPostBundleV1 {
            header,
            encoded_posts_bytes: body,
        })
    }

    /// Verifies that this bundle is legitimate enough to cache.
    ///
    /// Checks:
    /// 1. The bundle header is structurally valid and its signature verifies.
    /// 2. The sum of `encoded_post_lengths` exactly spans `encoded_posts_bytes`, and
    ///    the plaintext `post_id` prefix of each slice matches the corresponding entry
    ///    in `encoded_post_ids`.
    /// 3. Each post slice can be decrypted using `base_id`, which also verifies the
    ///    per-post signature and hash integrity.
    pub fn verify(&self, base_id: &Id) -> anyhow::Result<()> {
        // (1) Header structure and signature
        self.header.verify()?;

        // (2) Lengths must exactly span the body
        let total_length: usize = self.header.encoded_post_lengths.iter().sum();
        if total_length != self.encoded_posts_bytes.len() {
            anyhow::bail!(
                "sum of encoded_post_lengths ({}) != encoded_posts_bytes length ({})",
                total_length,
                self.encoded_posts_bytes.len()
            );
        }

        // (2) + (3) Per-post checks
        let mut offset = 0usize;
        for (i, (&length, expected_post_id)) in self.header.encoded_post_lengths.iter().zip(self.header.encoded_post_ids.iter()).enumerate() {
            let post_bytes = self.encoded_posts_bytes.slice(offset..offset + length);

            // Plaintext post_id prefix must match the header's claim before we do any crypto
            if post_bytes.len() < ID_BYTES {
                anyhow::bail!("post {}: bytes too short to contain post_id", i);
            }
            let actual_post_id = Id::from_slice(&post_bytes[..ID_BYTES])?;
            if actual_post_id != *expected_post_id {
                anyhow::bail!("post {}: id mismatch — header claims {} but bytes contain {}", i, expected_post_id, actual_post_id);
            }

            // Decrypt header (expect_body=true, decode_body=false): verifies per-post signature and hashes
            EncodedPostV1::decode_from_bytes(post_bytes, base_id, true, false)
                .map_err(|e| anyhow::anyhow!("post {}: failed to verify with base_id: {}", i, e))?;

            offset += length;
        }

        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::key_locker::key_locker::{KeyLocker, KeyLockerManager};
    use crate::client::key_locker::mem_key_locker::MemKeyLockerManager;
    use std::sync::Arc;
    use crate::protocol::posting::encoded_post::EncodedPostV1;
    use crate::tools::server_id::ServerId;
    use crate::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
    use crate::tools::tools;
    use crate::tools::types::Pow;
    use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;

    /// Builds a valid single-post bundle encrypted under `base_id`.
    async fn make_valid_bundle(base_id: Id) -> anyhow::Result<EncodedPostBundleV1> {
        let time_provider = RealTimeProvider;
        let pow_generator = SingleThreadedPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(0), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;

        let key_locker_manager = MemKeyLockerManager::new().await?;
        let key_locker: Arc<dyn KeyLocker> = key_locker_manager.create("test keyphrase".to_string()).await?;
        let client_id = key_locker.client_id();
        let timestamp = time_provider.current_time_millis();

        let mut encoded_post = EncodedPostV1::new(client_id, timestamp, vec![base_id], "test post content");
        let post_bytes_obj = encoded_post.encode_to_bytes_direct(&key_locker).await?;
        let post_bytes = Bytes::copy_from_slice(post_bytes_obj.bytes());

        let mut header = EncodedPostBundleHeaderV1 {
            time_millis: timestamp,
            location_id: Id::random(),
            overflowed: false,
            sealed: false,
            num_posts: 1,
            encoded_post_ids: vec![encoded_post.post_id],
            encoded_post_lengths: vec![post_bytes.len()],
            encoded_post_healed: HashSet::new(),
            peer,
            signature: Signature::zero(),
        };
        header.signature_generate(&server_id.keys.signature_key)?;

        Ok(EncodedPostBundleV1 { header, encoded_posts_bytes: post_bytes })
    }

    #[tokio::test]
    async fn test_verify_valid_bundle() -> anyhow::Result<()> {
        let base_id = Id::random();
        let bundle = make_valid_bundle(base_id).await?;
        bundle.verify(&base_id)
    }

    #[tokio::test]
    async fn test_verify_wrong_base_id() -> anyhow::Result<()> {
        let base_id = Id::random();
        let bundle = make_valid_bundle(base_id).await?;
        let wrong_base_id = Id::random();
        assert!(bundle.verify(&wrong_base_id).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_tampered_post_bytes() -> anyhow::Result<()> {
        let base_id = Id::random();
        let bundle = make_valid_bundle(base_id).await?;
        let mut tampered_posts = bundle.encoded_posts_bytes.to_vec();
        tampered_posts[ID_BYTES + 10] ^= 0xff; // flip a byte inside the encrypted header
        let tampered_bundle = EncodedPostBundleV1 {
            header: bundle.header,
            encoded_posts_bytes: Bytes::from(tampered_posts),
        };
        assert!(tampered_bundle.verify(&base_id).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_wrong_post_id_in_header() -> anyhow::Result<()> {
        let base_id = Id::random();
        let mut bundle = make_valid_bundle(base_id).await?;
        let pow_generator = SingleThreadedPowGenerator::new();
        let server_id = ServerId::new("own_pow", &RealTimeProvider, Pow(0), true, &pow_generator).await?;
        bundle.header.encoded_post_ids[0] = Id::random(); // wrong post_id
        bundle.header.signature_generate(&server_id.keys.signature_key)?;
        assert!(bundle.verify(&base_id).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_wrong_length_sum() -> anyhow::Result<()> {
        let base_id = Id::random();
        let mut bundle = make_valid_bundle(base_id).await?;
        let pow_generator = SingleThreadedPowGenerator::new();
        let server_id = ServerId::new("own_pow", &RealTimeProvider, Pow(0), true, &pow_generator).await?;
        bundle.header.encoded_post_lengths[0] += 1; // length doesn't match bytes
        bundle.header.signature_generate(&server_id.keys.signature_key)?;
        assert!(bundle.verify(&base_id).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_tampered_post_length() -> anyhow::Result<()> {
        // Changing encoded_post_lengths must invalidate the signature even when the
        // structural checks (sum == body length, num_posts == lengths.len()) still pass.
        let base_id = Id::random();
        let bundle = make_valid_bundle(base_id).await?;
        let original_length = bundle.header.encoded_post_lengths[0];
        // Pad the body so the sum still matches, then bump the recorded length.
        let mut tampered_posts = bundle.encoded_posts_bytes.to_vec();
        tampered_posts.push(0u8); // one extra byte on the body
        let mut tampered_header = bundle.header.clone();
        tampered_header.encoded_post_lengths[0] = original_length + 1;
        // Do NOT re-sign — the signature now covers a different lengths list.
        let tampered_bundle = EncodedPostBundleV1 {
            header: tampered_header,
            encoded_posts_bytes: Bytes::from(tampered_posts),
        };
        assert!(tampered_bundle.verify(&base_id).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_bad_header_signature() -> anyhow::Result<()> {
        let base_id = Id::random();
        let mut bundle = make_valid_bundle(base_id).await?;
        bundle.header.signature = Signature::zero(); // corrupt the bundle header signature
        assert!(bundle.verify(&base_id).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn encoded_post_bundle_v1_to_from_bytes_roundtrip() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider;
        let pow_generator = SingleThreadedPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(0), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;

        let num_posts: u8 = 3;

        let mut header = EncodedPostBundleHeaderV1 {
            time_millis: TimeMillis::random(),
            location_id: Id::random(),
            overflowed: true,
            sealed: false,
            num_posts,
            encoded_post_ids: (0..num_posts).map(|_| Id::random()).collect(),
            encoded_post_lengths: (0..num_posts).map(|_| tools::random_usize_bounded(1024)).collect(),
            encoded_post_healed: HashSet::new(),
            peer,
            signature: Signature::zero(),
        };

        header.signature_generate(&server_id.keys.signature_key)?;
        header.verify()?;

        let total_bytes = header.encoded_post_lengths.iter().sum::<usize>();
        let encoded_posts_bytes = Bytes::from(tools::random_bytes(total_bytes));

        let bundle = EncodedPostBundleV1 {
            header,
            encoded_posts_bytes,
        };

        let bytes1 = bundle.to_bytes()?;
        let decoded = EncodedPostBundleV1::from_bytes(bytes1.clone(), true)?;

        assert_eq!(bundle, decoded);

        // Optional extra sanity check: encoding the decoded struct should be stable.
        let bytes2 = decoded.to_bytes()?;
        assert_eq!(bytes1, bytes2);

        Ok(())
    }

    #[tokio::test]
    async fn encoded_post_bundle_v1_to_from_bytes_roundtrip_without_body() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider;
        let pow_generator = SingleThreadedPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(0), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;

        let num_posts: u8 = 3;

        let mut header = EncodedPostBundleHeaderV1 {
            time_millis: TimeMillis::random(),
            location_id: Id::random(),
            overflowed: true,
            sealed: false,
            num_posts,
            encoded_post_ids: (0..num_posts).map(|_| Id::random()).collect(),
            encoded_post_lengths: (0..num_posts).map(|_| tools::random_usize_bounded(1024)).collect(),
            encoded_post_healed: HashSet::new(),
            peer,
            signature: Signature::zero(),
        };

        header.signature_generate(&server_id.keys.signature_key)?;
        header.verify()?;

        let total_bytes = header.encoded_post_lengths.iter().sum::<usize>();
        let encoded_posts_bytes = Bytes::from(tools::random_bytes(total_bytes));

        let bundle = EncodedPostBundleV1 {
            header,
            encoded_posts_bytes,
        };

        let bytes1 = bundle.to_bytes()?;
        let decoded = EncodedPostBundleV1::from_bytes(bytes1.clone(), false)?;

        assert_eq!(bundle.header, decoded.header);
        assert!(decoded.encoded_posts_bytes.is_empty());

        Ok(())
    }

    // ── Robustness tests: EncodedPostBundleV1::from_bytes ──

    #[test]
    fn test_from_bytes_empty() {
        assert!(EncodedPostBundleV1::from_bytes(Bytes::new(), true).is_err());
    }

    #[test]
    fn test_from_bytes_wrong_version() {
        assert!(EncodedPostBundleV1::from_bytes(Bytes::from_static(&[99u8]), true).is_err());
    }

    #[test]
    fn test_from_bytes_truncated_at_header_length() {
        // Version byte only, no header length u64
        assert!(EncodedPostBundleV1::from_bytes(Bytes::from_static(&[1u8]), true).is_err());
    }

    #[test]
    fn test_from_bytes_truncated_at_body_length() {
        // Version + header_len=0, but no body_len u64
        let mut bytes = BytesMut::new();
        bytes.put_u8(1); // version
        bytes.put_u64(0); // header_len
        assert!(EncodedPostBundleV1::from_bytes(bytes.freeze(), true).is_err());
    }

    #[test]
    fn test_from_bytes_header_len_exceeds_remaining() {
        let mut bytes = BytesMut::new();
        bytes.put_u8(1); // version
        bytes.put_u64(99999); // header_len way too large
        bytes.put_u64(0); // body_len
        assert!(EncodedPostBundleV1::from_bytes(bytes.freeze(), true).is_err());
    }

    #[test]
    fn test_from_bytes_overflow_lengths() {
        let mut bytes = BytesMut::new();
        bytes.put_u8(1); // version
        bytes.put_u64(u64::MAX); // header_len
        bytes.put_u64(1); // body_len — header_len + body_len overflows usize
        assert!(EncodedPostBundleV1::from_bytes(bytes.freeze(), true).is_err());
    }

    #[test]
    fn test_from_bytes_garbage() {
        assert!(EncodedPostBundleV1::from_bytes(Bytes::from_static(&[0xff; 128]), true).is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod bolero_fuzz {
        use bytes::Bytes;
        use crate::protocol::posting::encoded_post_bundle::EncodedPostBundleV1;

        #[test]
        fn fuzz_from_bytes() {
            bolero::check!().for_each(|data: &[u8]| {
                let _ = EncodedPostBundleV1::from_bytes(Bytes::copy_from_slice(data), true);
            });
        }

        #[test]
        fn fuzz_from_bytes_no_body() {
            bolero::check!().for_each(|data: &[u8]| {
                let _ = EncodedPostBundleV1::from_bytes(Bytes::copy_from_slice(data), false);
            });
        }
    }
}