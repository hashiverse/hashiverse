//! # A single stateful timeline cursor
//!
//! `SingleTimeline` walks one feed (a specific user's posts, a specific hashtag, the
//! mentions of a given client, …) backwards through time, yielding pages of posts via
//! `get_more_posts()`. It owns:
//!
//! - a [`crate::client::timeline::recursive_bucket_visitor::RecursiveBucketVisitor`]
//!   that walks the hierarchical bucket ladder for the timeline's `(BucketType, base_id)`;
//! - a `post_ids_already_seen` set that deduplicates across overlapping buckets and
//!   across pages (important for Sequel / ReplyToPost timelines where posts can surface
//!   from multiple places);
//! - a [`crate::client::timeline::recent_posts_pen::RecentPostsPen`] reference so the
//!   user's own very recent posts appear instantly instead of waiting for the next
//!   DHT read.
//!
//! Timelines never reach an end — the walk just keeps stepping further back in time on
//! demand (a hard-earned invariant: manually triggered "fetch more" must always produce
//! *something*, even if it has to go a year further back to find it).

use crate::client::post_bundle::post_bundle_manager::PostBundleManager;
use crate::client::timeline::recent_posts_pen::RecentPostsPen;
use crate::client::timeline::recursive_bucket_visitor::{RecursiveBucketVisitor, RecursiveBucketVisitorCloseCallbackResult, RecursiveBucketVisitorOpenCallbackResult};
use crate::tools::buckets::{BucketLocation, BucketType};
use crate::tools::time::{DurationMillis, TimeMillis, MILLIS_IN_MONTH};
use crate::tools::types::Id;
use bytes::Bytes;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A cursor that walks one feed backward through time, yielding encoded posts page by page.
///
/// Every feed in hashiverse — a user's profile, a hashtag, a mention stream, a reply thread
/// — is represented on the wire as a sequence of time-bucketed post bundles keyed by a
/// single [`Id`] (the "base id" of the feed) and a [`BucketType`]. `SingleTimeline` is the
/// client-side state machine that reads such a feed: given a wall-clock `time_millis` and a
/// desired page size, each call to `get_more_posts` walks to the next-older bucket via
/// [`RecursiveBucketVisitor`], pulls the bundle through a [`PostBundleManager`], and emits
/// the posts in reverse chronological order while deduplicating against `post_ids_already_seen`.
///
/// It also reads from the shared [`RecentPostsPen`] so posts the local user has just
/// authored appear at the top of the feed before they have propagated through the network.
/// For aggregated feeds (following multiple people, multiple hashtags) see `MultipleTimeline`,
/// which composes several `SingleTimeline`s.
pub struct SingleTimeline {
    bucket_type: BucketType,
    base_id: Id,
    post_bundle_manager: Arc<dyn PostBundleManager>,
    recent_posts_pen: Arc<RwLock<RecentPostsPen>>,
    oldest_allowed_post_bundle_time_millis: TimeMillis,
    oldest_processed_post_bundle_time_millis: TimeMillis,
    post_ids_already_seen: HashSet<Id>,
}

impl SingleTimeline {
    pub fn new(bucket_type: BucketType, location_id: &Id, post_bundle_manager: Arc<dyn PostBundleManager>, recent_posts_pen: Arc<RwLock<RecentPostsPen>>) -> Self {
        Self {
            bucket_type,
            base_id: *location_id,
            post_bundle_manager,
            recent_posts_pen,
            oldest_allowed_post_bundle_time_millis: TimeMillis::MAX,
            oldest_processed_post_bundle_time_millis: TimeMillis::MAX,
            post_ids_already_seen: HashSet::new(),
        }
    }

    pub fn bucket_type(&self) -> BucketType {
        self.bucket_type
    }

    pub fn base_id(&self) -> Id {
        self.base_id
    }

    pub async fn get_more_posts(&mut self, time_millis: TimeMillis, max_posts: usize, bucket_durations: &[DurationMillis]) -> anyhow::Result<Vec<(BucketLocation, Bytes, bool)>> {
        let mut encoded_posts = Vec::new();

        if TimeMillis::MAX == self.oldest_allowed_post_bundle_time_millis {
            self.oldest_allowed_post_bundle_time_millis = time_millis;
        }
        if TimeMillis::MAX == self.oldest_processed_post_bundle_time_millis {
            self.oldest_processed_post_bundle_time_millis = time_millis;
        }

        let time_millis_max = time_millis;
        let time_millis_min = self.oldest_allowed_post_bundle_time_millis - MILLIS_IN_MONTH;

        RecursiveBucketVisitor::visit(
            time_millis_max,
            time_millis_min,
            bucket_durations,
            // on_bucket_open
            &mut async |bucket_time_millis: TimeMillis, bucket_duration_millis: DurationMillis| {
                // log::info!(
                //     "open {}: bucket_time_millis: {}, bucket_duration_millis: {}, time_millis_min: {}",
                //     self.base_id, bucket_time_millis, bucket_duration_millis, time_millis_min
                // );

                // Keep pushing back the oldest time we have queried - so that future calls are allowed to go back even further than that
                self.oldest_allowed_post_bundle_time_millis = self.oldest_allowed_post_bundle_time_millis.min(bucket_time_millis);

                // Get location's postbundle
                let bucket_location = BucketLocation::new(self.bucket_type, self.base_id, bucket_duration_millis, bucket_time_millis)?;
                let encoded_post_bundle = self.post_bundle_manager.get_post_bundle(&bucket_location, time_millis).await?;

                // Do we keep going deeper?
                match encoded_post_bundle.header.overflowed {
                    true => Ok(RecursiveBucketVisitorOpenCallbackResult::ContinueWithChildren),
                    false => Ok(RecursiveBucketVisitorOpenCallbackResult::ContinueWithoutChildren),
                }
            },
            // on_bucket_close
            &mut async |bucket_time_millis: TimeMillis, bucket_duration_millis: DurationMillis| {
                // log::info!("close {}: bucket_time_millis: {}, bucket_duration_millis: {}", self.base_id, bucket_time_millis, bucket_duration_millis);

                // Keep pushing back the oldest time we have processed
                self.oldest_processed_post_bundle_time_millis = self.oldest_processed_post_bundle_time_millis.min(bucket_time_millis);

                // Get location's postbundle
                let bucket_location = BucketLocation::new(self.bucket_type, self.base_id, bucket_duration_millis, bucket_time_millis)?;
                let encoded_post_bundle = self.post_bundle_manager.get_post_bundle(&bucket_location, time_millis).await?;

                // Extract all the posts we have not seen before (some post bundles may add posts over time if they are not yet sealed)
                let mut extraction_start_i = 0;
                for i in 0..(encoded_post_bundle.header.num_posts as usize) {
                    let extraction_end_i = extraction_start_i + encoded_post_bundle.header.encoded_post_lengths[i];
                    let encoded_post_id = encoded_post_bundle.header.encoded_post_ids[i];
                    if self.post_ids_already_seen.insert(encoded_post_id) {
                        let encoded_post = encoded_post_bundle.encoded_posts_bytes.slice(extraction_start_i..extraction_end_i);
                        let healed = encoded_post_bundle.header.encoded_post_healed.contains(&encoded_post_id);
                        encoded_posts.push((bucket_location.clone(), encoded_post, healed));
                    }
                    extraction_start_i = extraction_end_i;
                }

                // Do we have enough posts this round?
                match encoded_posts.len() < max_posts {
                    true => Ok(RecursiveBucketVisitorCloseCallbackResult::Continue),
                    false => Ok(RecursiveBucketVisitorCloseCallbackResult::Stop),
                }
            },
        )
        .await?;

        // Consult the recent posts pen for any posts we submitted that haven't yet appeared from the network
        let pen_posts = self.recent_posts_pen.write().await.get_matching_posts(self.bucket_type, &self.base_id, &self.post_ids_already_seen, time_millis);
        for (bucket_location, encoded_post_bytes, post_id) in pen_posts {
            if self.post_ids_already_seen.insert(post_id) {
                encoded_posts.push((bucket_location, encoded_post_bytes, false));
            }
        }

        Ok(encoded_posts)
    }

    pub fn post_count(&self) -> usize {
        self.post_ids_already_seen.len()
    }

    pub fn oldest_allowed_post_bundle_time_millis(&self) -> TimeMillis {
        self.oldest_allowed_post_bundle_time_millis
    }

    pub fn oldest_processed_post_bundle_time_millis(&self) -> TimeMillis {
        self.oldest_processed_post_bundle_time_millis
    }
}

#[cfg(test)]
pub mod tests {
    use crate::client::post_bundle::stub_post_bundle_manager::StubPostBundleManager;
    use crate::client::timeline::recent_posts_pen::RecentPostsPen;
    use crate::client::timeline::single_timeline::SingleTimeline;
    use crate::tools::buckets::{BucketType, BUCKET_DURATIONS};
    use crate::tools::time::{TimeMillis, MILLIS_IN_DAY, MILLIS_IN_MONTH, MILLIS_IN_WEEK};
    use crate::tools::types::Id;
    use log::info;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn empty_pen() -> Arc<RwLock<RecentPostsPen>> {
        Arc::new(RwLock::new(RecentPostsPen::new()))
    }

    #[tokio::test]
    async fn timeline_single_test() -> anyhow::Result<()> {
        // configure_logging();

        let id = Id::random();
        let stub_post_bundle_manager = StubPostBundleManager::default();
        {
            stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_MONTH, "1D", 23)?;
        }

        let stub_post_bundle_manager = Arc::new(stub_post_bundle_manager);
        let mut single_timeline = SingleTimeline::new(BucketType::User, &id, stub_post_bundle_manager, empty_pen());
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("1M")?, 5, &BUCKET_DURATIONS[1..]).await?;
        assert_eq!(posts.len(), 23);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("0")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("0")?);

        Ok(())
    }

    #[tokio::test]
    async fn timeline_multiple_test() -> anyhow::Result<()> {
        // configure_logging();

        let id = Id::random();
        let stub_post_bundle_manager = StubPostBundleManager::default();
        {
            stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_MONTH, "1D", 27)?;
            stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_WEEK, "1W", 17)?;
            stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_WEEK, "2W", 6)?;
            stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_WEEK, "3W", 22)?;
        }

        let stub_post_bundle_manager = Arc::new(stub_post_bundle_manager);
        let mut single_timeline = SingleTimeline::new(BucketType::User, &id, stub_post_bundle_manager, empty_pen());
        info!("--- FETCH 1 -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("1M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 22);
        assert_eq!(single_timeline.post_count(), 22);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("0")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("3W")?);
        info!("--- FETCH 2 -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("1M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 23);
        assert_eq!(single_timeline.post_count(), 22 + 23);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("0")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("1W")?);
        info!("--- FETCH 3 -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("1M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 27);
        assert_eq!(single_timeline.post_count(), 22 + 23 + 27);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("0")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("0")?);
        info!("--- FETCH 4 -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("1M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 0);
        assert_eq!(single_timeline.post_count(), 22 + 23 + 27);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("-1M")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("-1M")?);

        Ok(())
    }

    #[tokio::test]
    async fn timeline_multiple_months_test() -> anyhow::Result<()> {
        // configure_logging();

        let id = Id::random();
        let stub_post_bundle_manager = StubPostBundleManager::default();
        {
            stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_MONTH, "1D", 23)?;
            stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_WEEK, "2W", 5)?;
            stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_WEEK, "3W", 16)?;
            stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_MONTH, "1M1D", 29)?;
            stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_WEEK, "1M2W", 21)?;
            stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_DAY, "1M2W", 7)?;
            stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_WEEK, "1M3W", 6)?;
        }

        let stub_post_bundle_manager = Arc::new(stub_post_bundle_manager);
        let mut single_timeline = SingleTimeline::new(BucketType::User, &id, stub_post_bundle_manager, empty_pen());
        info!("--- FETCH 1 -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("2M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 34);
        assert_eq!(single_timeline.post_count(), 34);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("1M")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("1M2W")?);
        info!("--- FETCH 2 -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("2M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 29);
        assert_eq!(single_timeline.post_count(), 34 + 29);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("1M")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("1M")?);
        info!("--- FETCH 3 -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("2M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 21);
        assert_eq!(single_timeline.post_count(), 34 + 29 + 21);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("0")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("2W")?);
        info!("--- FETCH 4 -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("2M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 23);
        assert_eq!(single_timeline.post_count(), 34 + 29 + 21 + 23);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("0")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("0")?);
        info!("--- FETCH 5 -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("2M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 0);
        assert_eq!(single_timeline.post_count(), 34 + 29 + 21 + 23);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("-1M")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("-1M")?);
        info!("--- FETCH 6 -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("2M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 0);
        assert_eq!(single_timeline.post_count(), 34 + 29 + 21 + 23);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("-2M")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("-2M")?);

        Ok(())
    }

    #[tokio::test]
    async fn timeline_bundle_update_test() -> anyhow::Result<()> {
        // configure_logging();

        // Note that each time we do the request, the "distance back" it has searched should be increasing by 1 month because we never actually get 20 posts...

        let id = Id::random();
        let stub_post_bundle_manager = Arc::new(StubPostBundleManager::default());
        let mut single_timeline = SingleTimeline::new(BucketType::User, &id, stub_post_bundle_manager.clone(), empty_pen());

        info!("--- FETCH -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("1M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 0);
        assert_eq!(single_timeline.post_count(), 0);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("0")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("0")?);

        let mut post_bundle = stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_MONTH, "1D", 12)?;

        info!("--- FETCH -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("1M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 12);
        assert_eq!(single_timeline.post_count(), 12);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("-1M")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("-1M")?);

        // Add a post to the old bundle - as if someone has posted since we last checked
        post_bundle.header.num_posts += 1;
        post_bundle.header.encoded_post_ids.push(Id::random());
        post_bundle.header.encoded_post_lengths.push(0);
        stub_post_bundle_manager.add_stub_post_bundle(&id, MILLIS_IN_MONTH, "1D", &post_bundle)?;

        info!("--- FETCH -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("1M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 1);
        assert_eq!(single_timeline.post_count(), 13);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("-2M")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("-2M")?);

        info!("--- FETCH -----------------------");
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("1M")?, 20, &BUCKET_DURATIONS[1..4]).await?;
        assert_eq!(posts.len(), 0);
        assert_eq!(single_timeline.post_count(), 13);
        assert_eq!(single_timeline.oldest_allowed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("-3M")?);
        assert_eq!(single_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::from_epoch_offset_str("-3M")?);

        Ok(())
    }

    #[tokio::test]
    async fn timeline_healed_flag_is_per_post() -> anyhow::Result<()> {
        // A bundle marks individual post_ids as healed via header.encoded_post_healed.
        // get_more_posts must surface that as the third tuple element per post.
        let id = Id::random();
        let stub_post_bundle_manager = Arc::new(StubPostBundleManager::default());

        let mut post_bundle = stub_post_bundle_manager.add_random_stub_post_bundle(&id, MILLIS_IN_MONTH, "1D", 5)?;
        let healed_post_id = post_bundle.header.encoded_post_ids[2];
        post_bundle.header.encoded_post_healed.insert(healed_post_id);
        stub_post_bundle_manager.add_stub_post_bundle(&id, MILLIS_IN_MONTH, "1D", &post_bundle)?;

        let mut single_timeline = SingleTimeline::new(BucketType::User, &id, stub_post_bundle_manager, empty_pen());
        let posts = single_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("1M")?, 20, &BUCKET_DURATIONS[1..]).await?;
        assert_eq!(posts.len(), 5);

        let healed_count = posts.iter().filter(|(_, _, healed)| *healed).count();
        assert_eq!(healed_count, 1, "exactly one post in this bundle is marked healed");

        // Pen-only posts (locally-submitted, no bundle yet) must be reported as not healed.
        // The existing bundle test already covers the bundle case; here just spot-check the
        // unhealed posts come through with `false`.
        let unhealed_count = posts.iter().filter(|(_, _, healed)| !*healed).count();
        assert_eq!(unhealed_count, 4);

        Ok(())
    }
}
