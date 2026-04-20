//! # Test stub [`PostBundleFeedbackManager`]
//!
//! A do-nothing implementation of
//! [`crate::client::post_bundle::post_bundle_feedback_manager::PostBundleFeedbackManager`]
//! that always returns a "not implemented" error. Used by test scenarios that exercise
//! timeline or posting logic without caring about feedback, so those tests don't need to
//! stand up a full live feedback stack.

use crate::client::post_bundle::post_bundle_feedback_manager::PostBundleFeedbackManager;
use crate::protocol::posting::encoded_post_bundle_feedback::EncodedPostBundleFeedbackV1;
use crate::tools::buckets::BucketLocation;
use crate::tools::time::TimeMillis;

pub struct StubPostBundleFeedbackManager {
}

impl Default for StubPostBundleFeedbackManager {
    fn default() -> Self { Self { }}
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl PostBundleFeedbackManager for StubPostBundleFeedbackManager {
    async fn get_post_bundle_feedback(&self, _bucket_location: BucketLocation, _time_millis: TimeMillis) -> anyhow::Result<EncodedPostBundleFeedbackV1> {
        anyhow::bail!("Not implemented");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::{Bytes, BytesMut};
    use crate::protocol::posting::encoded_post_feedback::{EncodedPostFeedbackV1, EncodedPostFeedbackViewV1};
    use crate::tools::buckets::{BucketLocation, BucketType, BUCKET_DURATIONS};
    use crate::tools::time::TimeMillis;
    use crate::tools::types::{Id, Pow, Salt};

    fn make_bucket_location() -> BucketLocation {
        BucketLocation::new(BucketType::User, Id::random(), BUCKET_DURATIONS[0], TimeMillis(1_000_000)).unwrap()
    }

    #[tokio::test]
    async fn stub_always_returns_error() {
        let stub = StubPostBundleFeedbackManager::default();
        let result = stub.get_post_bundle_feedback(make_bucket_location(), TimeMillis(1_000_000)).await;
        assert!(result.is_err(), "stub should always return an error");
        assert!(result.unwrap_err().to_string().contains("Not implemented"));
    }

    #[test]
    fn feedback_roundtrip_encoding() {
        let post_id = Id::random();
        let feedback = EncodedPostFeedbackV1 {
            post_id,
            feedback_type: 1,
            salt: Salt::random(),
            pow: Pow(42),
        };

        let mut buf = BytesMut::new();
        feedback.append_encode_to_bytes(&mut buf).unwrap();
        let bytes = buf.freeze();

        let view = EncodedPostFeedbackViewV1::iter(&bytes)
            .next()
            .expect("should yield one entry")
            .expect("entry should decode without error");

        assert_eq!(view.post_id_bytes(), post_id.as_ref());
        assert_eq!(view.feedback_type(), 1);
        assert_eq!(view.pow(), Pow(42));
    }

    #[test]
    fn feedback_view_iter_multiple_entries() {
        let post_ids: Vec<Id> = (0..3).map(|_| Id::random()).collect();
        let mut combined = BytesMut::new();
        for (i, &post_id) in post_ids.iter().enumerate() {
            let feedback = EncodedPostFeedbackV1 {
                post_id,
                feedback_type: (i as u8) + 1,
                salt: Salt::random(),
                pow: Pow((i as u8) * 10),
            };
            feedback.append_encode_to_bytes(&mut combined).unwrap();
        }
        let bytes: Bytes = combined.freeze();

        let views: Vec<_> = EncodedPostFeedbackViewV1::iter(&bytes)
            .collect::<Result<Vec<_>, _>>()
            .expect("all entries should decode");

        assert_eq!(views.len(), 3);
        for (i, view) in views.iter().enumerate() {
            assert_eq!(view.post_id_bytes(), post_ids[i].as_ref());
            assert_eq!(view.feedback_type(), (i as u8) + 1);
            assert_eq!(view.pow(), Pow((i as u8) * 10));
        }
    }

    #[test]
    fn feedback_pow_comparison() {
        // Feedback merge rule: for a given (post_id, feedback_type), keep the highest PoW.
        // Verify the Pow type supports the comparisons needed for that rule.
        assert!(Pow(10) > Pow(5));
        assert!(Pow(0) < Pow(1));
        assert_eq!(Pow(7), Pow(7));

        // Verify that the "keep max" logic works with standard iterator helpers
        let pows = vec![Pow(3), Pow(15), Pow(7)];
        let strongest = pows.into_iter().max().unwrap();
        assert_eq!(strongest, Pow(15));
    }
}
