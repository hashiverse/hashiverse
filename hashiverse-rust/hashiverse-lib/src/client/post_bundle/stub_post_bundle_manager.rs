//! # Test stub [`PostBundleManager`]
//!
//! An in-memory implementation of
//! [`crate::client::post_bundle::post_bundle_manager::PostBundleManager`] for tests that
//! need to exercise timeline logic without any network. Lookups return pre-configured
//! bundles from a `HashMap`, and the stub can synthesise random bundles of a requested
//! post count on demand. `sealed` / `overflowed` flags are set based on the bucket's
//! position in time relative to the test clock so edge-case transitions (just-sealed,
//! just-overflowed) can be reproduced deterministically.

use crate::client::post_bundle::post_bundle_manager::PostBundleManager;
use crate::protocol::peer::Peer;
use crate::protocol::posting::encoded_post_bundle::{EncodedPostBundleHeaderV1, EncodedPostBundleV1};
use crate::tools::buckets::{BucketLocation, BucketType};
use crate::tools::config;
use crate::tools::time::{DurationMillis, TimeMillis};
use crate::tools::types::{Id, Signature};
use log::{trace, warn};
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use bytes::Bytes;

pub struct StubPostBundleManager {
    post_bundles: RwLock<HashMap<Id, EncodedPostBundleV1>>,
}

impl StubPostBundleManager {
    pub fn add_random_stub_post_bundle(&self, id: &Id, granularity: DurationMillis, epoch_offset_str: &str, num_posts: u8) -> anyhow::Result<EncodedPostBundleV1> {
        let bucket_location = BucketLocation::new(BucketType::User, *id, granularity, TimeMillis::from_epoch_offset_str(epoch_offset_str)?)?;

        let mut post_bundle = EncodedPostBundleV1::stub();
        post_bundle.header.location_id = bucket_location.location_id;
        post_bundle.header.num_posts = num_posts;
        for _ in 0..num_posts {
            post_bundle.header.encoded_post_ids.push(Id::random());
            post_bundle.header.encoded_post_lengths.push(0);
        }
        let result = self.post_bundles.write().insert(bucket_location.location_id, post_bundle.clone());
        if result.is_some() {
            warn!("Replaced stub post bundle for location_id: {}", bucket_location.location_id);
        }

        Ok(post_bundle)
    }

    pub fn add_stub_post_bundle(&self, id: &Id, granularity: DurationMillis, epoch_offset_str: &str, post_bundle: &EncodedPostBundleV1) -> anyhow::Result<()> {
        let bucket_location = BucketLocation::new(BucketType::User, *id, granularity, TimeMillis::from_epoch_offset_str(epoch_offset_str)?)?;

        let result = self.post_bundles.write().insert(bucket_location.location_id, post_bundle.clone());
        if result.is_some() {
            warn!("Replaced stub post bundle for location_id: {}", bucket_location.location_id);
        }

        Ok(())
    }
}

impl Default for StubPostBundleManager {
    fn default() -> Self {
        Self { post_bundles: RwLock::new(HashMap::new()) }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
impl PostBundleManager for StubPostBundleManager {
    async fn get_post_bundle(&self, bucket_location: &BucketLocation, time_millis: TimeMillis) -> anyhow::Result<EncodedPostBundleV1> {
        let post_bundles = self.post_bundles.read();
        let post_bundle = post_bundles.get(&bucket_location.location_id);
        let post_bundle = match post_bundle {
            Some(post_bundle) => {
                let mut post_bundle = post_bundle.clone();
                post_bundle.header.time_millis = time_millis;
                post_bundle.header.overflowed = post_bundle.header.num_posts > config::ENCODED_POST_BUNDLE_V1_OVERFLOWED_NUM_POSTS;
                post_bundle.header.sealed = post_bundle.header.overflowed || time_millis > bucket_location.bucket_time_millis + bucket_location.duration + config::ENCODED_POST_BUNDLE_V1_ELAPSED_THRESHOLD_MILLIS;
                trace!("Returning stub postbundle: {}", post_bundle);
                post_bundle
            }
            None => {
                let sealed = time_millis > bucket_location.bucket_time_millis + bucket_location.duration + config::ENCODED_POST_BUNDLE_V1_ELAPSED_THRESHOLD_MILLIS;
                let mut post_bundle = EncodedPostBundleV1::stub();
                post_bundle.header.time_millis = time_millis;
                post_bundle.header.location_id = bucket_location.location_id;
                post_bundle.header.sealed = sealed;
                trace!("Returning phantom postbundle: {}", post_bundle);
                post_bundle
            }
        };

        Ok(post_bundle)
    }
}

impl EncodedPostBundleV1 {
    pub fn stub() -> Self {
        let header = EncodedPostBundleHeaderV1 {
            time_millis: TimeMillis::zero(),
            location_id: Id::zero(),
            overflowed: false,
            sealed: false,
            num_posts: 0,
            encoded_post_ids: vec![],
            encoded_post_lengths: vec![],
            encoded_post_healed: HashSet::new(),
            peer: Peer::zero(),
            signature: Signature::zero(),
        };

        EncodedPostBundleV1 { header, encoded_posts_bytes: Bytes::new() }
    }
}
