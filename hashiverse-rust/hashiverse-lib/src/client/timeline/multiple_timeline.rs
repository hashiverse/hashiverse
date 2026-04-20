//! # Merging multiple timelines into one feed
//!
//! `MultipleTimeline` composes several
//! [`crate::client::timeline::single_timeline::SingleTimeline`] cursors into one
//! chronologically merged stream. Used for:
//!
//! - "following" feeds (one `SingleTimeline` per followed user),
//! - "trending hashtags" (one `SingleTimeline` per hashtag),
//! - any other feed that fans out over several `(BucketType, base_id)` sources.
//!
//! Each call to `get_more_posts` sorts the component timelines by age + post-count
//! penalty (so slow / empty sub-timelines don't hog quota), pulls a quota of posts from
//! each, and then merges the results by timestamp. Deduplication and the
//! recent-posts-pen integration are delegated to the sub-timelines.

use crate::client::post_bundle::post_bundle_manager::PostBundleManager;
use crate::client::timeline::recent_posts_pen::RecentPostsPen;
use crate::client::timeline::single_timeline::SingleTimeline;
use crate::tools::time::{DurationMillis, MILLIS_IN_HOUR, TimeMillis};
use crate::tools::types::Id;
use log::trace;
use std::sync::Arc;
use bytes::Bytes;
use tokio::sync::RwLock;
use crate::tools::buckets::{BucketLocation, BucketType};

pub struct MultipleTimeline {
    bucket_type: BucketType,
    base_ids: Vec<Id>,
    single_timelines: Vec<SingleTimeline>,
}

impl MultipleTimeline {
    pub fn new(bucket_type: BucketType, base_ids: Vec<Id>, post_bundle_manager: Arc<dyn PostBundleManager>, recent_posts_pen: Arc<RwLock<RecentPostsPen>>) -> Self {
        let single_timelines = base_ids.iter().map(|base_id| SingleTimeline::new(bucket_type, &base_id, post_bundle_manager.clone(), recent_posts_pen.clone())).collect();
        Self { bucket_type, base_ids, single_timelines }
    }

    pub fn base_ids(&self) -> &Vec<Id> {
        &self.base_ids
    }

    pub fn bucket_type(&self) -> BucketType {
        self.bucket_type
    }

    pub async fn get_more_posts(&mut self, time_millis: TimeMillis, max_posts: usize, max_posts_per_single_timeline: usize, bucket_durations: &[DurationMillis]) -> anyhow::Result<Vec<(BucketLocation, Bytes)>> {
        let mut encoded_posts_aggregated = Vec::new();

        // Work out the priority of the SingleTimelines to poll
        self.single_timelines.sort_by_key(|single_timeline| {
            let date_penalty = time_millis - single_timeline.oldest_processed_post_bundle_time_millis();
            let post_penalty = MILLIS_IN_HOUR.const_mul(single_timeline.post_count() as i64);
            date_penalty + post_penalty
        });

        for single_timeline in &mut self.single_timelines {
            trace!("Fetching posts from SingleTimeline with base_id={}", single_timeline.base_id());
            let posts = single_timeline.get_more_posts(time_millis, max_posts_per_single_timeline, bucket_durations).await?;

            encoded_posts_aggregated.extend(posts);

            if encoded_posts_aggregated.len() >= max_posts {
                break;
            }
        }

        Ok(encoded_posts_aggregated)
    }

    pub fn oldest_processed_post_bundle_time_millis(&self) -> TimeMillis {
        self.single_timelines
            .iter()
            .map(|single_timeline| single_timeline.oldest_processed_post_bundle_time_millis())
            .min()
            .unwrap_or(TimeMillis::MAX)
    }
}

#[cfg(test)]
pub mod tests {
    use crate::client::post_bundle::stub_post_bundle_manager::StubPostBundleManager;
    use crate::client::timeline::multiple_timeline::MultipleTimeline;
    use crate::client::timeline::recent_posts_pen::RecentPostsPen;
    use crate::tools::buckets::{BucketType, BUCKET_DURATIONS};
    use crate::tools::time::{MILLIS_IN_MONTH, MILLIS_IN_WEEK, TimeMillis};
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

        let ids = vec![Id::from_slice(&[0x11; 32])?, Id::from_slice(&[0x22; 32])?, Id::from_slice(&[0x33; 32])?];

        let stub_post_bundle_manager = Arc::new(StubPostBundleManager::default());
        {
            // Fetch 1
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[0], MILLIS_IN_MONTH, "5M", 29)?; // Fetch 1.1
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[1], MILLIS_IN_MONTH, "5M", 23)?; // Fetch 1.2
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[2], MILLIS_IN_MONTH, "5M", 27)?; // Fetch 1.3

            // Fetch 2
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[0], MILLIS_IN_MONTH, "4M", 25)?; // Fetch 2.3 - Accumulative 29
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[1], MILLIS_IN_MONTH, "4M", 21)?; // Fetch 2.1 - Accumulative 23
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[2], MILLIS_IN_MONTH, "4M", 28)?; // Fetch 2.2 - Accumulative 27

            // Fetch 3
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[0], MILLIS_IN_MONTH, "3M", 6)?; // Fetch 3.2 - Accumulative 54
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[1], MILLIS_IN_MONTH, "3M", 9)?; // Fetch 3.1 - Accumulative 44
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[2], MILLIS_IN_MONTH, "3M", 7)?; // Fetch 3.3 - Accumulative 55

            // Fetch 4
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[0], MILLIS_IN_MONTH, "2M", 35)?; // Fetch 5.1 - Accumulative 68
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[1], MILLIS_IN_MONTH, "2M", 9)?; // Fetch 4.1 - Accumulative 54
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[2], MILLIS_IN_MONTH, "2M", 11)?; // Fetch 4.3 - Accumulative 62
            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[0], MILLIS_IN_WEEK, "2M1W", 8)?; // Fetch 4.2 - Accumulative 60

            stub_post_bundle_manager.add_random_stub_post_bundle(&ids[2], MILLIS_IN_MONTH, "1M", 3)?; // Fetch 6
        }

        let mut multiple_timeline = MultipleTimeline::new(BucketType::User, ids, stub_post_bundle_manager, empty_pen());

        // The stubs have been set up around buckets that have months as the most granular...
        let bucket_durations_starting_at_monthly = &BUCKET_DURATIONS[1..];

        // Check the ordering of the initial ties
        {
            info!("1)--- FETCH -----------------------");
            let posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("6M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
            assert_eq!(29, posts.len());
            info!("--- FETCH -----------------------");
            let posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("6M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
            assert_eq!(23, posts.len());
            info!("--- FETCH -----------------------");
            let posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("6M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
            assert_eq!(27, posts.len());
        }

        // Check the ordering of the "num posts" penalty
        {
            info!("2)--- FETCH -----------------------");
            let posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("6M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
            assert_eq!(21, posts.len());
            info!("--- FETCH -----------------------");
            let posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("6M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
            assert_eq!(28, posts.len());
            info!("--- FETCH -----------------------");
            let posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("6M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
            assert_eq!(25, posts.len());
        }

        // Check the next "batch" that will need to be aggregated to reach enough posts for the batch
        {
            info!("3)--- FETCH -----------------------");
            let posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("6M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
            assert_eq!(9 + 6 + 7, posts.len());
        }

        {
            info!("4)--- FETCH -----------------------");
            let posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("6M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
            assert_eq!(28, posts.len());
        }

        // Then the rump posts
        {
            info!("5) --- FETCH -----------------------");
            let posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("6M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
            assert_eq!(35, posts.len());

            info!("6) --- FETCH -----------------------");
            let posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("6M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
            assert_eq!(3, posts.len());

            info!("--- FETCH -----------------------");
            let posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("6M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
            assert_eq!(0, posts.len());
        }

        Ok(())
    }

    #[tokio::test]
    async fn oldest_processed_post_bundle_time_millis_is_min_of_single_timelines() -> anyhow::Result<()> {
        let ids = vec![Id::from_slice(&[0x11; 32])?, Id::from_slice(&[0x22; 32])?];

        let stub_post_bundle_manager = Arc::new(StubPostBundleManager::default());
        // id[0] has posts at 3M, id[1] has posts at 2M — so after fetching, id[1] will have the older processed time
        stub_post_bundle_manager.add_random_stub_post_bundle(&ids[0], MILLIS_IN_MONTH, "3M", 5)?;
        stub_post_bundle_manager.add_random_stub_post_bundle(&ids[1], MILLIS_IN_MONTH, "2M", 5)?;

        let mut multiple_timeline = MultipleTimeline::new(BucketType::User, ids, stub_post_bundle_manager, empty_pen());
        let bucket_durations_starting_at_monthly = &BUCKET_DURATIONS[1..];

        // Before any fetches, should be TimeMillis::MAX
        assert_eq!(multiple_timeline.oldest_processed_post_bundle_time_millis(), TimeMillis::MAX);

        // First fetch — processes id[0] at 3M
        let _posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("4M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
        let oldest_after_first_fetch = multiple_timeline.oldest_processed_post_bundle_time_millis();

        // Second fetch — processes id[1] at 2M (older)
        let _posts = multiple_timeline.get_more_posts(TimeMillis::from_epoch_offset_str("4M")?, 20, 5, &bucket_durations_starting_at_monthly).await?;
        let oldest_after_second_fetch = multiple_timeline.oldest_processed_post_bundle_time_millis();

        // The oldest should have moved further back after processing id[1]
        assert!(oldest_after_second_fetch <= oldest_after_first_fetch);

        Ok(())
    }
}
