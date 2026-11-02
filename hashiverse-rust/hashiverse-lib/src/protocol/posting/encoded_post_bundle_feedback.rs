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

        let hash_input: Vec<&[u8]> = vec![
            time_millis_be.as_ref(),
            self.location_id.as_ref(),
            self.feedbacks_bytes_hash.as_ref(),
            self.peer.signature.as_ref(),
        ];

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
        if !self.feedbacks_bytes.len().is_multiple_of(ENTRY_SIZE) {
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

    /// Verifies that this bundle is a legitimate feedback bundle for `location_id`, as received from the network.
    ///
    /// On top of [`EncodedPostBundleFeedbackV1::verify`]:
    /// 1. The publishing peer is a genuine server identity (server-id PoW, id derived from its key, self-signed).
    ///    [`EncodedPostBundleFeedbackHeaderV1::verify`] alone only checks the header against the key the header itself supplies.
    /// 2. The bundle is for this location_id.
    pub fn verify_for_location(&self, location_id: &Id) -> anyhow::Result<()> {
        self.header.peer.verify().map_err(|e| anyhow::anyhow!("feedback bundle peer {} failed verification: {}", self.header.peer.id, e))?;
        if self.header.location_id != *location_id {
            anyhow::bail!("feedback bundle location_id {} does not match location_id {}", self.header.location_id, location_id);
        }
        self.verify()
    }

    pub fn get_post_pow_for_feedback_type(&self, post_id: &Id, feedback_type: u8) -> Pow {
        for view in EncodedPostFeedbackViewV1::iter(&self.feedbacks_bytes).flatten() {
            // Check feedback_type first as it short circuits and is therefore faster...
            if view.feedback_type() == feedback_type && view.post_id_bytes() == post_id.as_ref() {
                return view.pow();
            }
        }
        Pow(0)
    }

    /// Unions an array of bundles by taking the highest-pow entry for each
    /// `(post_id, feedback_type)` pair across all bundles.  The header is
    /// taken from the most-recently-timestamped bundle.  Returns `None` if
    /// `bundles` is empty.
    ///
    /// Entries whose claimed pow does not match what their salt achieves are
    /// dropped individually.  The bundle header signature only proves which
    /// server produced the bundle, not that its entries are honest, so a
    /// dishonest (or poisoned) server must not be able to plant pow=255 flags.
    /// Dropping per-entry rather than rejecting the whole bundle means one bad
    /// entry cannot erase all other feedback for the bucket.
    pub fn merge(bundles: &[Self]) -> Option<Self> {
        let header = bundles.iter().max_by_key(|b| b.header.time_millis)?.header.clone();

        let mut global_max: HashMap<(Id, u8), EncodedPostFeedbackV1> = HashMap::new();
        for bundle in bundles {
            for view in EncodedPostFeedbackViewV1::iter(&bundle.feedbacks_bytes) {
                let Ok(view) = view else { continue };
                let Ok(post_id) = Id::from_slice(view.post_id_bytes()) else { continue };
                let Ok(salt) = Salt::from_slice(view.salt_bytes()) else { continue };
                let encoded_post_feedback = EncodedPostFeedbackV1::new(post_id, view.feedback_type(), salt, view.pow());
                if encoded_post_feedback.pow_verify().is_err() {
                    continue;
                }

                let key = (post_id, encoded_post_feedback.feedback_type);
                match global_max.entry(key) {
                    std::collections::hash_map::Entry::Vacant(vacant) => {
                        vacant.insert(encoded_post_feedback);
                    }
                    std::collections::hash_map::Entry::Occupied(mut occupied) => {
                        if encoded_post_feedback.pow > occupied.get().pow {
                            occupied.insert(encoded_post_feedback);
                        }
                    }
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
        for view in EncodedPostFeedbackViewV1::iter(&self.feedbacks_bytes).flatten() {
            if  view.post_id_bytes() == post_id.as_ref() {
                result[view.feedback_type() as usize] = view.pow();
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
    use crate::tools::pow_generator::pow_generator::PowGenerator;
    use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;

    /// Builds a valid single-entry feedback bundle.
    async fn make_valid_bundle() -> anyhow::Result<EncodedPostBundleFeedbackV1> {
        let time_provider = RealTimeProvider;
        let pow_generator = SingleThreadedPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(0), true, &pow_generator).await?;
        let peer = server_id.to_peer(&time_provider)?;

        let post_id = Id::random();
        let feedback_type = 1u8;
        // One iteration with Pow(0) always succeeds immediately — cheap in tests
        let data_hash = pow::pow_compute_data_hash(&[post_id.as_bytes(), &[feedback_type]]);
        let (salt, achieved_pow, _) = pow_generator.generate_best_effort("make_valid_bundle_feedback", 1, Pow(0), data_hash).await?;
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
        let pow_generator = SingleThreadedPowGenerator::new();
        let server_id = ServerId::new("own_pow", &RealTimeProvider, Pow(0), true, &pow_generator).await?;
        bundle.header.feedbacks_bytes_hash = hashing::hash(b"wrong");
        bundle.header.signature_generate(&server_id.keys.signature_key); // re-sign so header sig itself is valid
        assert!(bundle.verify().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_verify_partial_entry() -> anyhow::Result<()> {
        let mut bundle = make_valid_bundle().await?;
        let pow_generator = SingleThreadedPowGenerator::new();
        let server_id = ServerId::new("own_pow", &RealTimeProvider, Pow(0), true, &pow_generator).await?;
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
        let pow_generator = SingleThreadedPowGenerator::new();
        let server_id = ServerId::new("own_pow", &RealTimeProvider, Pow(0), true, &pow_generator).await?;
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

    /// Builds a feedback bundle from explicit `(post_id, feedback_type, pow)` entries with random salts.
    /// The entries are NOT valid PoW: fine for `get_post_pows` / `get_post_pow_for_feedback_type`,
    /// which read entries directly, but `merge` drops them — use `make_bundle_with_encoded_post_feedbacks`
    /// with `make_valid_feedback` entries for merge tests.
    async fn make_bundle_with_feedbacks(time_millis: TimeMillis, feedbacks: &[(Id, u8, Pow)]) -> anyhow::Result<EncodedPostBundleFeedbackV1> {
        let encoded_post_feedbacks: Vec<EncodedPostFeedbackV1> = feedbacks.iter().map(|(post_id, feedback_type, pow)| EncodedPostFeedbackV1::new(*post_id, *feedback_type, Salt::random(), *pow)).collect();
        make_bundle_with_encoded_post_feedbacks(time_millis, &encoded_post_feedbacks).await
    }

    /// A feedback entry whose pow is exactly what its random salt achieves, so it passes `pow_verify`.
    fn make_valid_feedback(post_id: Id, feedback_type: u8) -> anyhow::Result<EncodedPostFeedbackV1> {
        let salt = Salt::random();
        let (measured_pow, _) = pow::pow_measure(&[post_id.as_bytes(), &[feedback_type]], &salt)?;
        let encoded_post_feedback = EncodedPostFeedbackV1::new(post_id, feedback_type, salt, measured_pow);
        encoded_post_feedback.pow_verify()?;
        Ok(encoded_post_feedback)
    }

    /// A feedback entry claiming `Pow(255)` with a random salt — what an attacker would plant.
    fn make_forged_feedback(post_id: Id, feedback_type: u8) -> EncodedPostFeedbackV1 {
        let encoded_post_feedback = EncodedPostFeedbackV1::new(post_id, feedback_type, Salt::random(), Pow(255));
        assert!(encoded_post_feedback.pow_verify().is_err());
        encoded_post_feedback
    }

    async fn make_bundle_with_encoded_post_feedbacks(time_millis: TimeMillis, encoded_post_feedbacks: &[EncodedPostFeedbackV1]) -> anyhow::Result<EncodedPostBundleFeedbackV1> {
        let server_id = ServerId::new("own_pow", &RealTimeProvider, Pow(0), true, &SingleThreadedPowGenerator::new()).await?;
        make_bundle_signed_by(&server_id, Id::random(), time_millis, encoded_post_feedbacks)
    }

    fn make_bundle_signed_by(server_id: &ServerId, location_id: Id, time_millis: TimeMillis, encoded_post_feedbacks: &[EncodedPostFeedbackV1]) -> anyhow::Result<EncodedPostBundleFeedbackV1> {
        let peer = server_id.to_peer(&RealTimeProvider)?;

        let mut feedbacks_bytes_mut = Vec::new();
        for encoded_post_feedback in encoded_post_feedbacks {
            encoded_post_feedback.append_encode_to_bytes(&mut feedbacks_bytes_mut)?;
        }
        let feedbacks_bytes = Bytes::from(feedbacks_bytes_mut);

        let mut header = EncodedPostBundleFeedbackHeaderV1 {
            time_millis,
            location_id,
            feedbacks_bytes_hash: hashing::hash(feedbacks_bytes.as_ref()),
            peer,
            signature: Signature::zero(),
        };
        header.signature_generate(&server_id.keys.signature_key);
        Ok(EncodedPostBundleFeedbackV1 { header, feedbacks_bytes })
    }

    #[tokio::test]
    async fn get_post_pows_reports_pow_per_feedback_type() -> anyhow::Result<()> {
        let post_a = Id::random();
        let post_b = Id::random();
        let bundle = make_bundle_with_feedbacks(TimeMillis(1000), &[
            (post_a, 1, Pow(10)),
            (post_a, 2, Pow(20)),
            (post_b, 1, Pow(30)),
        ]).await?;

        // Per-type lookups.
        assert_eq!(Pow(10), bundle.get_post_pow_for_feedback_type(&post_a, 1));
        assert_eq!(Pow(20), bundle.get_post_pow_for_feedback_type(&post_a, 2));
        assert_eq!(Pow(0), bundle.get_post_pow_for_feedback_type(&post_a, 3), "absent type => Pow(0)");
        assert_eq!(Pow(0), bundle.get_post_pow_for_feedback_type(&post_b, 2), "post_b has no type-2 feedback");

        // Full per-post arrays, indexed by feedback_type.
        let pows_a = bundle.get_post_pows(&post_a);
        assert_eq!(Pow(10), pows_a[1]);
        assert_eq!(Pow(20), pows_a[2]);
        assert_eq!(Pow(0), pows_a[0]);
        assert_eq!(Pow(0), pows_a[3]);

        let pows_b = bundle.get_post_pows(&post_b);
        assert_eq!(Pow(30), pows_b[1]);
        assert_eq!(Pow(0), pows_b[2]);
        Ok(())
    }

    #[tokio::test]
    async fn merge_keeps_highest_pow_per_post_and_type() -> anyhow::Result<()> {
        let post_a = Id::random();

        // Several valid candidates for (post_a, 1), spread over two bundles; random salts give a spread of measured pows.
        let candidates_for_type_1: Vec<EncodedPostFeedbackV1> = (0..16).map(|_| make_valid_feedback(post_a, 1)).collect::<anyhow::Result<_>>()?;
        let only_for_type_2 = make_valid_feedback(post_a, 2)?;
        let expected_type_1_pow = candidates_for_type_1.iter().map(|encoded_post_feedback| encoded_post_feedback.pow).max().unwrap();

        let (first_half, second_half) = candidates_for_type_1.split_at(8);
        let mut first_bundle_feedbacks = first_half.to_vec();
        first_bundle_feedbacks.push(only_for_type_2.clone());
        let bundle_first = make_bundle_with_encoded_post_feedbacks(TimeMillis(1000), &first_bundle_feedbacks).await?;
        let bundle_second = make_bundle_with_encoded_post_feedbacks(TimeMillis(2000), second_half).await?;

        let merged = EncodedPostBundleFeedbackV1::merge(&[bundle_first, bundle_second]).expect("non-empty input");

        assert_eq!(expected_type_1_pow, merged.get_post_pow_for_feedback_type(&post_a, 1), "the highest pow for (post_a, type 1) must win");
        assert_eq!(only_for_type_2.pow, merged.get_post_pow_for_feedback_type(&post_a, 2), "the only pow for (post_a, type 2) must be preserved");
        merged_entries_all_verify(&merged)?;
        Ok(())
    }

    // Regression for C3: a forged pow=255 entry (from a dishonest server, or planted via an unchecked heal)
    // must not win the merge, must not appear for keys that only have forged entries, and must not
    // take the honest entries of the same bundle down with it.
    #[tokio::test]
    async fn merge_drops_entries_with_invalid_pow() -> anyhow::Result<()> {
        let post_a = Id::random();
        let post_b = Id::random();
        let valid_for_post_a = make_valid_feedback(post_a, 1)?;

        let poisoned_bundle = make_bundle_with_encoded_post_feedbacks(TimeMillis(1000), &[make_forged_feedback(post_a, 1), valid_for_post_a.clone(), make_forged_feedback(post_b, 2)]).await?;
        let honest_bundle = make_bundle_with_encoded_post_feedbacks(TimeMillis(2000), &[]).await?;

        let merged = EncodedPostBundleFeedbackV1::merge(&[poisoned_bundle, honest_bundle]).expect("non-empty input");

        assert_eq!(valid_for_post_a.pow, merged.get_post_pow_for_feedback_type(&post_a, 1), "the valid entry must survive alongside a forged one");
        assert_eq!(Pow(0), merged.get_post_pow_for_feedback_type(&post_b, 2), "a key with only a forged entry must be absent");
        assert_eq!(merged.feedbacks_bytes.len(), ENTRY_SIZE, "only the one valid entry should remain");
        merged_entries_all_verify(&merged)?;
        Ok(())
    }

    // merge() output is a client-side view: its header (taken from the newest bundle) no longer covers its
    // feedbacks_bytes, so it must never be forwarded to caches - only intact originals that pass verify() may be.
    #[tokio::test]
    async fn merged_bundle_is_a_client_view_not_a_signed_bundle() -> anyhow::Result<()> {
        let post_a = Id::random();
        let valid_for_type_1 = make_valid_feedback(post_a, 1)?;
        let valid_for_type_2 = make_valid_feedback(post_a, 2)?;

        let bundle_first = make_bundle_with_encoded_post_feedbacks(TimeMillis(1000), &[valid_for_type_1.clone()]).await?;
        let bundle_second = make_bundle_with_encoded_post_feedbacks(TimeMillis(2000), &[valid_for_type_2.clone()]).await?;
        bundle_first.verify()?;
        bundle_second.verify()?;

        let merged = EncodedPostBundleFeedbackV1::merge(&[bundle_first, bundle_second]).expect("non-empty input");
        assert_eq!(valid_for_type_1.pow, merged.get_post_pow_for_feedback_type(&post_a, 1));
        assert_eq!(valid_for_type_2.pow, merged.get_post_pow_for_feedback_type(&post_a, 2));
        assert!(merged.verify().is_err(), "the merged union is not covered by any originator's signature");

        // A signed bundle carrying a forged entry is not uploadable, yet merge still salvages its honest entry for the client view.
        let poisoned_bundle = make_bundle_with_encoded_post_feedbacks(TimeMillis(3000), &[make_forged_feedback(post_a, 3), valid_for_type_1.clone()]).await?;
        poisoned_bundle.header.verify()?;
        assert!(poisoned_bundle.verify().is_err(), "a bundle with a forged entry must fail full verification");
        let merged_from_poisoned = EncodedPostBundleFeedbackV1::merge(&[poisoned_bundle]).expect("non-empty input");
        assert_eq!(valid_for_type_1.pow, merged_from_poisoned.get_post_pow_for_feedback_type(&post_a, 1));
        assert_eq!(Pow(0), merged_from_poisoned.get_post_pow_for_feedback_type(&post_a, 3));
        Ok(())
    }

    // H4: a feedback bundle uploaded into a location's cache must come from a genuine (PoW-backed) server and be for that location.
    #[tokio::test]
    async fn verify_for_location_rejects_wrong_location_and_unworked_peer() -> anyhow::Result<()> {
        let location_id = Id::random();
        let encoded_post_feedbacks = [make_valid_feedback(Id::random(), 1)?];

        let genuine_server_id = ServerId::new("genuine", &RealTimeProvider, crate::tools::config::SERVER_KEY_POW_MIN, true, &SingleThreadedPowGenerator::new()).await?;
        let genuine_bundle = make_bundle_signed_by(&genuine_server_id, location_id, TimeMillis(1000), &encoded_post_feedbacks)?;
        genuine_bundle.verify_for_location(&location_id)?;
        assert!(genuine_bundle.verify_for_location(&Id::random()).is_err(), "a bundle for another location must be rejected");

        let unworked_server_id = loop {
            let server_id = ServerId::new("unworked", &RealTimeProvider, Pow(0), true, &SingleThreadedPowGenerator::new()).await?;
            if server_id.pow < crate::tools::config::SERVER_KEY_POW_MIN {
                break server_id;
            }
        };
        let unworked_bundle = make_bundle_signed_by(&unworked_server_id, location_id, TimeMillis(1000), &encoded_post_feedbacks)?;
        unworked_bundle.verify()?;
        assert!(unworked_bundle.verify_for_location(&location_id).is_err(), "a self-signed bundle from a pow-less identity must be rejected");
        Ok(())
    }

    fn merged_entries_all_verify(merged: &EncodedPostBundleFeedbackV1) -> anyhow::Result<()> {
        for entry_bytes in merged.feedbacks_bytes.chunks_exact(ENTRY_SIZE) {
            EncodedPostFeedbackV1::decode_from_bytes(entry_bytes)?.pow_verify()?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn encoded_post_bundle_header_v1_to_from_bytes_roundtrip() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider;
        let pow_generator = SingleThreadedPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(0), true, &pow_generator).await?;
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
