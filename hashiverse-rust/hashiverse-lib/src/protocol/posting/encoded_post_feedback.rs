//! # `EncodedPostFeedbackV1` — a single feedback vote
//!
//! Compact 35-byte entry representing one vote on one post: `post_id` (32) +
//! `feedback_type` (1) + `salt` (1) + `pow` (1). Entries are packed by the hundreds
//! into an [`crate::protocol::posting::encoded_post_bundle_feedback`] bundle; the tiny
//! fixed size is what makes that packing cheap to verify.
//!
//! The PoW is computed over `(post_id, feedback_type)`. Clients must search for a
//! `salt` that produces at least [`crate::tools::config::CLIENT_FEEDBACK_POW_NUMERAIRE`]
//! bits — this is what makes feedback Sybil-resistant: a thousand fake votes costs a
//! thousand PoWs, not one.
//!
//! [`EncodedPostFeedbackViewV1`] is a zero-copy view over the on-wire bytes for
//! iteration during bundle verification, avoiding the need to allocate one
//! `EncodedPostFeedbackV1` per entry just to read it.

use bytes::{Buf, BufMut, BytesMut};
use crate::{anyhow_assert_eq, anyhow_assert_ge};
use crate::tools::config::CLIENT_FEEDBACK_POW_NUMERAIRE;
use crate::tools::parallel_pow_generator::ParallelPowGenerator;
use crate::tools::pow;
use crate::tools::types::{Hash, Id, Pow, Salt, ID_BYTES, SALT_BYTES};

// Offsets within a single serialised entry
const OFF_POST_ID: usize = 1;
const OFF_FEEDBACK_TYPE: usize = OFF_POST_ID + ID_BYTES;
const OFF_SALT: usize = OFF_FEEDBACK_TYPE + 1;
const OFF_POW: usize = OFF_SALT + SALT_BYTES;
pub const ENTRY_SIZE: usize = OFF_POW + 1; // 1 + ID_BYTES + 1 + SALT_BYTES + 1

/// A zero-copy view into one serialised `EncodedPostFeedbackV1` entry.
/// Construction validates the slice length and version byte; all accessors
/// are then infallible and return references into the original buffer.
pub struct EncodedPostFeedbackViewV1<'a>(&'a [u8]);

impl<'a> EncodedPostFeedbackViewV1<'a> {
    pub fn from_slice(bytes: &'a [u8]) -> anyhow::Result<Self> {
        anyhow_assert_eq!(bytes.len(), ENTRY_SIZE, "wrong entry size for EncodedPostFeedbackViewV1");
        anyhow_assert_eq!(bytes[0], 1u8, "unsupported version in EncodedPostFeedbackViewV1");
        Ok(Self(bytes))
    }

    pub fn post_id_bytes(&self) -> &'a [u8] {
        &self.0[OFF_POST_ID..OFF_FEEDBACK_TYPE]
    }

    pub fn feedback_type(&self) -> u8 {
        self.0[OFF_FEEDBACK_TYPE]
    }

    pub fn salt_bytes(&self) -> &'a [u8] {
        &self.0[OFF_SALT..OFF_POW]
    }

    pub fn pow(&self) -> Pow {
        Pow(self.0[OFF_POW])
    }

    /// Iterate views over a concatenated feedback byte slice.  Entries that
    /// fail validation are surfaced as `Err`; iteration always advances by
    /// `ENTRY_SIZE` so a bad entry does not stall the rest.
    pub fn iter(bytes: &'a [u8]) -> impl Iterator<Item = anyhow::Result<Self>> + 'a {
        bytes.chunks_exact(ENTRY_SIZE).map(Self::from_slice)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct EncodedPostFeedbackV1 {
    pub post_id: Id,
    pub feedback_type: u8,
    pub salt: Salt,
    pub pow: Pow,
}
impl EncodedPostFeedbackV1 {
    pub fn new(post_id: Id, feedback_type: u8, salt: Salt, pow: Pow) -> Self {
        Self {
            post_id,
            feedback_type,
            salt,
            pow,
        }
    }

    pub async fn pow_generate(post_id: &Id, feedback_type: u8, pow_generator: &dyn ParallelPowGenerator) -> anyhow::Result<(Salt, Pow, Hash)> {
        let data_hash = pow::pow_compute_data_hash(&[post_id.as_bytes(), &[feedback_type]]);
        pow_generator.generate_best_effort("feedback", CLIENT_FEEDBACK_POW_NUMERAIRE, Pow(255), data_hash).await
    }

    pub fn pow_verify(&self) -> anyhow::Result<()> {
        let (pow, _hash) = pow::pow_measure(&[self.post_id.as_bytes(), &[self.feedback_type]], &self.salt)?;
        anyhow_assert_eq!(pow, self.pow);
        Ok(())
    }

    pub async fn encode_to_bytes(&mut self) -> anyhow::Result<Vec<u8>> {
        let mut bytes = BytesMut::new();
        bytes.put_u8(1); // Version
        bytes.put_slice(self.post_id.as_ref());
        bytes.put_u8(self.feedback_type);
        bytes.put_slice(self.salt.as_ref());
        bytes.put_u8(self.pow.0);
        let bytes = bytes.to_vec();
        Ok(bytes)
    }

    pub fn append_encode_direct_to_bytes<B: BufMut>(bytes: &mut B, post_id: &[u8], feedback_type: u8, salt: &[u8], pow: Pow) -> anyhow::Result<()> {
        anyhow_assert_eq!(post_id.len(), ID_BYTES);
        anyhow_assert_eq!(salt.len(), SALT_BYTES);

        bytes.put_u8(1); // Version
        bytes.put_slice(post_id.as_ref());
        bytes.put_u8(feedback_type);
        bytes.put_slice(salt.as_ref());
        bytes.put_u8(pow.0);

        Ok(())
    }

    pub fn append_encode_to_bytes<B: BufMut>(&self, bytes: &mut B) -> anyhow::Result<()> {
        Self::append_encode_direct_to_bytes(bytes, self.post_id.as_ref(), self.feedback_type, self.salt.as_ref(), self.pow)
    }

    pub fn decode_from_bytes(mut bytes: impl Buf) -> anyhow::Result<Self> {
        anyhow_assert_ge!(bytes.remaining(), 1, "Missing version");
        let version = bytes.get_u8();
        anyhow_assert_eq!(1, version);

        let post_id = Id::from_buf(&mut bytes, "post_id")?;

        anyhow_assert_ge!(bytes.remaining(), 1, "Missing feedback_type");
        let feedback_type = bytes.get_u8();

        let salt = Salt::from_buf(&mut bytes, "salt")?;

        anyhow_assert_ge!(bytes.remaining(), 1, "Missing pow");
        let pow = Pow(bytes.get_u8());

        // We don't check that the remeinig bytes are zero, as this method it used to decode a sequence of Self in a buffer...

        Ok(Self {
            post_id,
            feedback_type,
            salt,
            pow,
        })
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use super::*;
    use crate::tools::types::{Id, Salt};

    const ENTRY_SIZE: usize = 1 + ID_BYTES + 1 + SALT_BYTES + 1;

    // ── encode_to_bytes / decode_from_bytes ──────────────────────────────────

    #[tokio::test]
    async fn roundtrip_encode_decode() -> anyhow::Result<()> {
        let original = EncodedPostFeedbackV1::new(Id::random(), 3, Salt::random(), Pow(42));

        let bytes = original.clone().encode_to_bytes().await?;
        assert_eq!(bytes.len(), ENTRY_SIZE);

        let decoded = EncodedPostFeedbackV1::decode_from_bytes(&mut Bytes::from(bytes.clone()))?;
        assert_eq!(original, decoded);

        // Re-encoding the decoded value must produce identical bytes (stability)
        let bytes2 = decoded.clone().encode_to_bytes().await?;
        assert_eq!(bytes, bytes2);

        Ok(())
    }

    #[tokio::test]
    async fn roundtrip_all_feedback_types() -> anyhow::Result<()> {
        for feedback_type in [0u8, 1, 2, 3, 127, 255] {
            let original = EncodedPostFeedbackV1::new(Id::random(), feedback_type, Salt::random(), Pow(0));
            let bytes = original.clone().encode_to_bytes().await?;
            let decoded = EncodedPostFeedbackV1::decode_from_bytes(&mut Bytes::from(bytes))?;
            assert_eq!(original, decoded, "failed for feedback_type={feedback_type}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn roundtrip_extreme_pow_values() -> anyhow::Result<()> {
        for pow in [0u8, 1, 127, 255] {
            let original = EncodedPostFeedbackV1::new(Id::random(), 1, Salt::random(), Pow(pow));
            let bytes = original.clone().encode_to_bytes().await?;
            let decoded = EncodedPostFeedbackV1::decode_from_bytes(&mut Bytes::from(bytes))?;
            assert_eq!(original, decoded, "failed for pow={pow}");
        }
        Ok(())
    }

    // ── append_encode_to_bytes ───────────────────────────────────────────────

    #[test]
    fn append_single_roundtrip() -> anyhow::Result<()> {
        let original = EncodedPostFeedbackV1::new(Id::random(), 5, Salt::random(), Pow(99));

        let mut buf = Vec::new();
        EncodedPostFeedbackV1::append_encode_direct_to_bytes(&mut buf, original.post_id.as_ref(), original.feedback_type, original.salt.as_ref(), original.pow)?;

        assert_eq!(buf.len(), ENTRY_SIZE);
        let decoded = EncodedPostFeedbackV1::decode_from_bytes(&mut Bytes::from(buf))?;
        assert_eq!(original, decoded);
        Ok(())
    }

    #[test]
    fn append_multiple_roundtrip() -> anyhow::Result<()> {
        let originals = [
            EncodedPostFeedbackV1::new(Id::random(), 1, Salt::random(), Pow(10)),
            EncodedPostFeedbackV1::new(Id::random(), 2, Salt::random(), Pow(20)),
            EncodedPostFeedbackV1::new(Id::random(), 255, Salt::zero(), Pow(0)),
        ];

        let mut buf = Vec::new();
        for f in &originals {
            EncodedPostFeedbackV1::append_encode_direct_to_bytes(&mut buf, f.post_id.as_ref(), f.feedback_type, f.salt.as_ref(), f.pow)?;
        }

        assert_eq!(buf.len(), originals.len() * ENTRY_SIZE);

        for (i, expected) in originals.iter().enumerate() {
            let start = i * ENTRY_SIZE;
            let mut chunk = Bytes::copy_from_slice(&buf[start..start + ENTRY_SIZE]);
            let decoded = EncodedPostFeedbackV1::decode_from_bytes(&mut chunk)?;
            assert_eq!(*expected, decoded, "mismatch at entry {i}");
        }

        Ok(())
    }

    #[test]
    fn append_rejects_wrong_post_id_length() {
        let mut buf = Vec::new();
        let short_id = vec![0u8; ID_BYTES - 1];
        let salt = Salt::zero();
        assert!(EncodedPostFeedbackV1::append_encode_direct_to_bytes(&mut buf, &short_id, 1, salt.as_ref(), Pow(0)).is_err());
    }

    #[test]
    fn append_rejects_wrong_salt_length() {
        let mut buf = Vec::new();
        let id = Id::random();
        let short_salt = vec![0u8; SALT_BYTES - 1];
        assert!(EncodedPostFeedbackV1::append_encode_direct_to_bytes(&mut buf, id.as_ref(), 1, &short_salt, Pow(0)).is_err());
    }

    // ── decode_from_bytes error cases ────────────────────────────────────────

    #[test]
    fn decode_rejects_empty_input() {
        assert!(EncodedPostFeedbackV1::decode_from_bytes(&mut Bytes::new()).is_err());
    }

    #[test]
    fn decode_rejects_wrong_version() {
        let mut buf = vec![0u8; ENTRY_SIZE];
        buf[0] = 99; // bad version
        assert!(EncodedPostFeedbackV1::decode_from_bytes(&mut Bytes::from(buf)).is_err());
    }

    #[test]
    fn decode_rejects_truncated_at_each_boundary() {
        // Build a valid buffer, then truncate at every byte before the last
        let valid = {
            let mut b = BytesMut::new();
            b.put_u8(1);
            b.put_slice(Id::random().as_ref());
            b.put_u8(3);
            b.put_slice(Salt::random().as_ref());
            b.put_u8(7);
            b.freeze()
        };
        assert_eq!(valid.len(), ENTRY_SIZE);

        for truncate_at in 0..ENTRY_SIZE {
            let mut truncated = valid.slice(0..truncate_at);
            assert!(
                EncodedPostFeedbackV1::decode_from_bytes(&mut truncated).is_err(),
                "expected error when truncated to {truncate_at} bytes"
            );
        }
    }

    // ── EncodedPostFeedbackViewV1 ────────────────────────────────────────────

    /// Encode a struct, then confirm the view's accessors agree with both
    /// the original field values (encoder) and a full decode (decoder).
    #[tokio::test]
    async fn view_matches_encoder_and_decoder() -> anyhow::Result<()> {
        let original = EncodedPostFeedbackV1::new(Id::random(), 7, Salt::random(), Pow(42));
        let bytes_for_view = original.clone().encode_to_bytes().await?;
        let bytes_for_decode = bytes_for_view.clone();

        let view = EncodedPostFeedbackViewV1::from_slice(&bytes_for_view)?;
        assert_eq!(view.post_id_bytes(), original.post_id.as_ref());
        assert_eq!(view.feedback_type(), original.feedback_type);
        assert_eq!(view.salt_bytes(), original.salt.as_ref());
        assert_eq!(view.pow(), original.pow);

        // Decoder must read back the same values the view exposes
        let decoded = EncodedPostFeedbackV1::decode_from_bytes(&mut Bytes::from(bytes_for_decode))?;
        assert_eq!(view.post_id_bytes(), decoded.post_id.as_ref());
        assert_eq!(view.feedback_type(), decoded.feedback_type);
        assert_eq!(view.salt_bytes(), decoded.salt.as_ref());
        assert_eq!(view.pow(), decoded.pow);

        Ok(())
    }

    /// Iter over a concatenated buffer — each view must agree with the
    /// original struct that produced its bytes.
    #[test]
    fn view_iter_matches_originals() -> anyhow::Result<()> {
        let originals = [
            EncodedPostFeedbackV1::new(Id::random(), 1, Salt::random(), Pow(10)),
            EncodedPostFeedbackV1::new(Id::random(), 2, Salt::random(), Pow(20)),
            EncodedPostFeedbackV1::new(Id::random(), 255, Salt::zero(), Pow(0)),
        ];

        let mut buf = Vec::new();
        for f in &originals {
            f.append_encode_to_bytes(&mut buf)?;
        }

        let views: Vec<_> = EncodedPostFeedbackViewV1::iter(&buf)
            .collect::<anyhow::Result<_>>()?;

        assert_eq!(views.len(), originals.len());
        for (view, original) in views.iter().zip(originals.iter()) {
            assert_eq!(view.post_id_bytes(), original.post_id.as_ref());
            assert_eq!(view.feedback_type(), original.feedback_type);
            assert_eq!(view.salt_bytes(), original.salt.as_ref());
            assert_eq!(view.pow(), original.pow);
        }

        Ok(())
    }

    /// A partial trailing entry must be silently ignored by iter
    /// (chunks_exact semantics), not cause a panic or error.
    #[test]
    fn view_iter_ignores_partial_tail() -> anyhow::Result<()> {
        let f = EncodedPostFeedbackV1::new(Id::random(), 3, Salt::random(), Pow(5));
        let mut buf = Vec::new();
        f.append_encode_to_bytes(&mut buf)?;
        buf.push(0xFF); // one extra byte — partial second entry

        let views: Vec<_> = EncodedPostFeedbackViewV1::iter(&buf)
            .collect::<anyhow::Result<_>>()?;
        assert_eq!(views.len(), 1);

        Ok(())
    }

    #[test]
    fn view_rejects_wrong_length() {
        assert!(EncodedPostFeedbackViewV1::from_slice(&[]).is_err());
        assert!(EncodedPostFeedbackViewV1::from_slice(&vec![1u8; ENTRY_SIZE - 1]).is_err());
        assert!(EncodedPostFeedbackViewV1::from_slice(&vec![1u8; ENTRY_SIZE + 1]).is_err());
    }

    #[test]
    fn view_rejects_wrong_version() {
        let mut buf = vec![1u8; ENTRY_SIZE];
        buf[0] = 99;
        assert!(EncodedPostFeedbackViewV1::from_slice(&buf).is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod bolero_fuzz {
        use bytes::Bytes;
        use crate::protocol::posting::encoded_post_feedback::EncodedPostFeedbackV1;

        #[test]
        fn fuzz_decode_from_bytes() {
            bolero::check!().for_each(|data: &[u8]| {
                let mut bytes = Bytes::copy_from_slice(data);
                let _ = EncodedPostFeedbackV1::decode_from_bytes(&mut bytes);
            });
        }
    }
}