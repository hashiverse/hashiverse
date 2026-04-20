//! # `EncodedPostBundleFeedbackV1` — server-signed aggregate feedback for a bundle
//!
//! Feedback (reactions, upvotes/downvotes, flags) on the posts in a single
//! `(location, time bucket)` is aggregated into its own bundle, parallel to — but
//! independent of — the [`crate::protocol::posting::encoded_post_bundle`]. The two are
//! split because feedback can change long after the post bundle itself has sealed, and
//! many consumers don't need the feedback layer at all.
//!
//! Wire format:
//!
//! - **Header** (server-signed) — lists every feedback entry's `(post_id,
//!   feedback_type, salt, pow)` along with a hash of the body. Signed by the
//!   publishing [`crate::protocol::peer::Peer`].
//! - **Body** — a packed array of fixed-size
//!   [`crate::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1`] entries,
//!   exactly `N * ENTRY_SIZE` bytes. Fixed sizing means iteration and verification are
//!   O(N) with no per-entry header overhead.
//!
//! `verify()` checks the signature, confirms body length is a multiple of `ENTRY_SIZE`,
//! re-hashes the body and compares against the header's hash, and checks the
//! proof-of-work on every entry against the numeraire — Sybil-resistant voting in one
//! pass.

use crate::protocol::peer::Peer;
use crate::protocol::posting::encoded_post_feedback::{EncodedPostFeedbackV1, EncodedPostFeedbackViewV1, ENTRY_SIZE};
use crate::tools::time::TimeMillis;
use crate::tools::types::{Hash, Id, Pow, Salt, Signature, SignatureKey, VerificationKey};
use std::collections::HashMap;
use crate::tools::{hashing, json, signing};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use std::fmt::{Debug, Display};
use crate::anyhow_assert_eq;

// Contains all the posts for a given location_id in a particular bucket
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct EncodedPostBundleFeedbackHeaderV1 {
    pub time_millis: TimeMillis, // When was this bundle last updated
    pub location_id: Id,
    pub feedbacks_bytes_hash: Hash, // Hash(feedbacks_bytes from the body)
    pub peer: Peer,

    pub signature: Signature, // Peer.sign(get_hash_for_signing())
}

impl EncodedPostBundleFeedbackHeaderV1 {
    pub fn get_hash_for_signing(&self) -> Hash {
        let time_millis_be = self.time_millis.encode_be();

        let mut hash_input: Vec<&[u8]> = vec![];

        hash_input.push(time_millis_be.as_ref());
        hash_input.push(self.location_id.as_ref());
        hash_input.push(self.feedbacks_bytes_hash.as_ref());
        hash_input.push(self.peer.signature.as_ref());

        hashing::hash_multiple(&hash_input)
    }

    pub fn signature_generate(&mut self, signature_key: &SignatureKey) {
        let hash = self.get_hash_for_signing();
        self.signature = signing::sign(signature_key, hash.as_ref());
    }

    pub fn signature_verify(&self) -> anyhow::Result<()> {
        let hash = self.get_hash_for_signing();
        let verification_key = VerificationKey::from_bytes(&self.peer.verification_key_bytes)?;
        signing::verify(&verification_key, &self.signature, hash.as_ref())
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        self.signature_verify()?;
        Ok(())
    }
}

impl Display for EncodedPostBundleFeedbackHeaderV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EncodedPostBundleFeedbackHeaderV1 [ location_id: {}, time_millis: {} ]", self.location_id, self.time_millis)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct EncodedPostBundleFeedbackV1 {
    pub header: EncodedPostBundleFeedbackHeaderV1,
    pub feedbacks_bytes: Bytes, // A concatenated array of EncodedPostFeedbackV1
}
impl Display for EncodedPostBundleFeedbackV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EncodedPostBundleV1 [ header: {}, length: {} ]", self.header, self.feedbacks_bytes.len())
    }
}

impl EncodedPostBundleFeedbackV1 {
    pub fn to_bytes(&self) -> anyhow::Result<Bytes> {
        let mut bytes = BytesMut::new();

        let header = json::struct_to_bytes(&self.header)?;
        bytes.put_u8(1u8); // Version
        bytes.put_u64(header.len() as u64);
        bytes.put_u64(self.feedbacks_bytes.len() as u64);
        bytes.put_slice(header.as_ref());
        bytes.put_slice(self.feedbacks_bytes.as_ref());

        Ok(bytes.freeze())
    }

    pub fn from_bytes(mut bytes: Bytes) -> anyhow::Result<Self> {
        if bytes.remaining() < 1 {
            anyhow::bail!("Invalid buffer: missing post_bundle version");
        }
        let version = bytes.get_u8();
        if 1 != version {
            anyhow::bail!("Invalid buffer: unknown post_bundle version");
        }

        if bytes.remaining() < 16 {
            anyhow::bail!("Invalid buffer: missing post_bundle lengths");
        }

        let header_len = bytes.get_u64() as usize;
        let body_len = bytes.get_u64() as usize;
        let total_length = header_len.checked_add(body_len).ok_or_else(|| anyhow::anyhow!("total_length overflow"))?;
        if bytes.remaining() < total_length {
            anyhow::bail!("Invalid buffer: post_bundle data truncated");
        }
        let header_bytes = bytes.copy_to_bytes(header_len);
        let body_bytes = bytes.copy_to_bytes(body_len);

        let header = json::bytes_to_struct(&header_bytes)?;

        Ok(EncodedPostBundleFeedbackV1 { header, feedbacks_bytes: body_bytes })
    }

    /// Verifies that this bundle is legitimate enough to cache.
    ///
    /// Checks:
    /// 1. The bundle header is valid and its signature verifies.
    /// 2. The feedback body is a valid multiple of `ENTRY_SIZE` (no partial entries).
    /// 3. The header's hash is the same as the body's
    /// 4. Each feedback entry's proof-of-work is correct.
    pub fn verify(&self) -> anyhow::Result<()> {
        // (1) Header signature
        self.header.verify()?;

        // (2) Body must be an exact multiple of the fixed entry size
        if self.feedbacks_bytes.len() % ENTRY_SIZE != 0 {
            anyhow::bail!(
                "feedbacks_bytes length ({}) is not a multiple of ENTRY_SIZE ({})",
                self.feedbacks_bytes.len(),
                ENTRY_SIZE
            );
        }

        // (3) Hash
        let feedbacks_bytes_hash = hashing::hash(self.feedbacks_bytes.as_ref());
        anyhow_assert_eq!(feedbacks_bytes_hash, self.header.feedbacks_bytes_hash, "feedbacks_bytes_hash mismatch");

        // (4) PoW check for every entry
        let num_entries = self.feedbacks_bytes.len() / ENTRY_SIZE;
        for i in 0..num_entries {
            let entry_bytes = &self.feedbacks_bytes[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE];
            let feedback = EncodedPostFeedbackV1::decode_from_bytes(&mut &entry_bytes[..])
                .map_err(|e| anyhow::anyhow!("feedback {}: invalid entry: {}", i, e))?;
            feedback.pow_verify()
                .map_err(|e| anyhow::anyhow!("feedback {}: pow verification failed: {}", i, e))?;
        }

        Ok(())
    }

    pub fn get_post_pow_for_feedback_type(&self, post_id: &Id, feedback_type: u8) -> Pow {
        for view in EncodedPostFeedbackViewV1::iter(&self.feedbacks_bytes) {
            if let Ok(view) = view {
                // Check feedback_type first as it short circuits and is therefore faster...
                if view.feedback_type() == feedback_type && view.post_id_bytes() == post_id.as_ref() {
                    return view.pow();
                }
            }
        }
        Pow(0)
    }

    /// Unions an array of bundles by taking the highest-pow entry for each
    /// `(post_id, feedback_type)` pair across all bundles.  The header is
    /// taken from the most-recently-timestamped bundle.  Returns `None` if
    /// `bundles` is empty.
    pub fn merge(bundles: &[Self]) -> Option<Self> {
        let header = bundles.iter().max_by_key(|b| b.header.time_millis)?.header.clone();

        let mut global_max: HashMap<(Id, u8), EncodedPostFeedbackV1> = HashMap::new();
        for bundle in bundles {
            for view in EncodedPostFeedbackViewV1::iter(&bundle.feedbacks_bytes) {
                let Ok(view) = view else { continue };
                let Ok(post_id) = Id::from_slice(view.post_id_bytes()) else { continue };
                let key = (post_id, view.feedback_type());
                let entry = global_max.entry(key).or_insert_with(|| EncodedPostFeedbackV1 {
                    post_id,
                    feedback_type: view.feedback_type(),
                    salt: Salt::from_slice(view.salt_bytes()).unwrap_or_else(|_| Salt::zero()),
                    pow: view.pow(),
                });
                if view.pow() > entry.pow {
                    entry.salt = Salt::from_slice(view.salt_bytes()).unwrap_or_else(|_| Salt::zero());
                    entry.pow = view.pow();
                }
            }
        }

        let mut feedbacks_bytes_mut = Vec::new();
        for feedback in global_max.values() {
            let _ = feedback.append_encode_to_bytes(&mut feedbacks_bytes_mut);
        }

        Some(Self { header, feedbacks_bytes: Bytes::from(feedbacks_bytes_mut) })
    }

    pub fn get_post_pows(&self, post_id: &Id) -> [Pow; 256] {
        let mut result = [Pow(0); 256];
        for view in EncodedPostFeedbackViewV1::iter(&self.feedbacks_bytes) {
            if let Ok(view) = view {
                if  view.post_id_bytes() == post_id.as_ref() {
                    result[view.feedback_type() as usize] = view.pow();
                }
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::server_id::ServerId;
    use crate::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
    use crate::tools::pow;
    use crate::tools::parallel_pow_generator::StubParallelPowGenerator;

    /// Builds a valid single-entry feedback bundle.
    async fn make_valid_bundle() -> anyhow::Result<EncodedPostBundleFeedbackV1> {
        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new(&time_provider, Pow(0), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;

        let post_id = Id::random();
        let feedback_type = 1u8;
        // One iteration with Pow(0) always succeeds immediately — cheap in tests
        let data_hash = pow::pow_compute_data_hash(&[post_id.as_bytes(), &[feedback_type]]);
        let (salt, achieved_pow, _) = pow::pow_generate_with_iteration_limit(1, Pow(0), &data_hash).await?;
        let feedback = EncodedPostFeedbackV1::new(post_id, feedback_type, salt, achieved_pow);

        let mut feedbacks_bytes_mut = Vec::new();
        feedback.append_encode_to_bytes(&mut feedbacks_bytes_mut)?;
        let feedbacks_bytes = Bytes::from(feedbacks_bytes_mut);

        let mut header = EncodedPostBundleFeedbackHeaderV1 {
            time_millis: time_provider.current_time_millis(),
            location_id: Id::random(),
            feedbacks_bytes_hash: hashing::hash(feedbacks_bytes.as_ref()),
            peer,
            signature: Signature::zero(),
        };
        header.signature_generate(&server_id.keys.signature_key);

        Ok(EncodedPostBundleFeedbackV1 { header, feedbacks_bytes })
    }

    #[tokio::test]
    async fn test_verify_valid_bundle() -> anyhow::Result<()> {
        let bundle = make_valid_bundle().await?;
        bundle.verify()
    }

    #[tokio::test]
    async fn test_verify_bad_header_signature() -> anyhow::Result<()> {
        let mut bundle = make_valid_bundle().await?;
        bundle.header.signature = Signature::zero();
        assert!(bundle.verify().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_wrong_feedbacks_hash() -> anyhow::Result<()> {
        let mut bundle = make_valid_bundle().await?;
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new(&RealTimeProvider::default(), Pow(0), true, &pow_generator).await?;
        bundle.header.feedbacks_bytes_hash = hashing::hash(b"wrong");
        bundle.header.signature_generate(&server_id.keys.signature_key); // re-sign so header sig itself is valid
        assert!(bundle.verify().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_partial_entry() -> anyhow::Result<()> {
        let mut bundle = make_valid_bundle().await?;
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new(&RealTimeProvider::default(), Pow(0), true, &pow_generator).await?;
        // Append one extra byte to make the length not a multiple of ENTRY_SIZE
        let mut bytes = bundle.feedbacks_bytes.to_vec();
        bytes.push(0u8);
        bundle.feedbacks_bytes = Bytes::from(bytes);
        bundle.header.feedbacks_bytes_hash = hashing::hash(bundle.feedbacks_bytes.as_ref());
        bundle.header.signature_generate(&server_id.keys.signature_key);
        assert!(bundle.verify().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_wrong_pow() -> anyhow::Result<()> {
        let mut bundle = make_valid_bundle().await?;
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new(&RealTimeProvider::default(), Pow(0), true, &pow_generator).await?;
        // Flip the last byte of the entry (the pow byte) to an incorrect value
        let mut bytes = bundle.feedbacks_bytes.to_vec();
        let last = bytes.last_mut().unwrap();
        *last = last.wrapping_add(1);
        bundle.feedbacks_bytes = Bytes::from(bytes);
        bundle.header.feedbacks_bytes_hash = hashing::hash(bundle.feedbacks_bytes.as_ref());
        bundle.header.signature_generate(&server_id.keys.signature_key);
        assert!(bundle.verify().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn encoded_post_bundle_header_v1_to_from_bytes_roundtrip() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new(&time_provider, Pow(0), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;
        let feedbacks_bytes =  Bytes::new();

        let mut header = EncodedPostBundleFeedbackHeaderV1 {
            time_millis: TimeMillis::random(),
            location_id: Id::random(),
            feedbacks_bytes_hash: hashing::hash(feedbacks_bytes.as_ref()),
            peer,
            signature: Signature::zero(),
        };

        header.signature_generate(&server_id.keys.signature_key);
        header.verify()?;

        let bundle = EncodedPostBundleFeedbackV1 { header, feedbacks_bytes };

        let bytes1 = bundle.to_bytes()?;
        let decoded = EncodedPostBundleFeedbackV1::from_bytes(bytes1.clone())?;

        assert_eq!(bundle, decoded);

        // Optional extra sanity check: encoding the decoded struct should be stable.
        let bytes2 = decoded.to_bytes()?;
        assert_eq!(bytes1, bytes2);

        Ok(())
    }
}
