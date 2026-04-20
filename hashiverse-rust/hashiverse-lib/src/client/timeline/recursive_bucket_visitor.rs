//! # Depth-first walk over the hierarchical bucket ladder
//!
//! The pure, storage-free algorithm behind every timeline. Given a time range and the
//! bucket-duration sequence from [`crate::tools::buckets::BUCKET_DURATIONS`],
//! `RecursiveBucketVisitor` visits buckets in depth-first order — coarse granularity
//! first, drilling into finer granularity where the open callback asks for it.
//!
//! Two callbacks steer the walk:
//!
//! - **Open** — called before descending into a bucket. Returns
//!   `ContinueWithChildren`, `ContinueWithoutChildren` (skip this subtree — empty
//!   buckets), or `Stop` (abort the whole walk — enough posts collected).
//! - **Close** — called when leaving a bucket, so timelines can decide whether to
//!   back off to coarser granularity or continue stepping within the current level.
//!
//! The visitor has zero knowledge of posts, caches, networks, or deduplication — it
//! is a plain tree walker; everything else is implemented on top of it by
//! [`crate::client::timeline::single_timeline::SingleTimeline`].

use crate::tools::buckets::BucketLocation;
use crate::tools::time::{DurationMillis, TimeMillis};

pub enum RecursiveBucketVisitorOpenCallbackResult {
    ContinueWithChildren,
    ContinueWithoutChildren,
    Stop,
}

pub enum RecursiveBucketVisitorCloseCallbackResult {
    Continue,
    Stop,
}

pub struct RecursiveBucketVisitor {}

impl RecursiveBucketVisitor {
    pub async fn visit<FOpen, FClose>(time_millis_max_exclusive: TimeMillis, time_millis_min: TimeMillis, bucket_durations: &[DurationMillis], on_bucket_open: &mut FOpen, on_bucket_close: &mut FClose) -> anyhow::Result<bool>
    where
        FOpen: AsyncFnMut(TimeMillis, DurationMillis) -> anyhow::Result<RecursiveBucketVisitorOpenCallbackResult>,
        FClose: AsyncFnMut(TimeMillis, DurationMillis) -> anyhow::Result<RecursiveBucketVisitorCloseCallbackResult>,
    {
        struct FrameOpen {
            bucket_time_millis: TimeMillis,
            bucket_duration_i: usize,
            oldest_bucket_time_millis_allowed: TimeMillis,
        }

        struct FrameClose {
            bucket_time_millis: TimeMillis,
            bucket_duration_i: usize,
        }

        enum Frame {
            Open(FrameOpen),
            Close(FrameClose),
        }

        anyhow::ensure!(!bucket_durations.is_empty(), "bucket_durations must have at least one element");

        let mut frames = Vec::new();

        // Prime the stack
        {
            let next_bucket_duration_i = 0;
            let next_bucket_duration = bucket_durations[next_bucket_duration_i];

            let next_latest_bucket_time_millis = BucketLocation::round_down_to_bucket_start(time_millis_max_exclusive, next_bucket_duration);
            let next_oldest_bucket_time_millis_allowed = BucketLocation::round_down_to_bucket_start(time_millis_min, next_bucket_duration);

            frames.push(Frame::Open(FrameOpen {
                bucket_time_millis: next_latest_bucket_time_millis,
                bucket_duration_i: next_bucket_duration_i,
                oldest_bucket_time_millis_allowed: next_oldest_bucket_time_millis_allowed,
            }));
        }

        // Pump the stack
        while let Some(frame) = frames.pop() {
            match frame {
                Frame::Close(FrameClose { bucket_time_millis, bucket_duration_i }) => {
                    let bucket_duration = bucket_durations[bucket_duration_i];
                    let callback_result = on_bucket_close(bucket_time_millis, bucket_duration).await?;
                    if let RecursiveBucketVisitorCloseCallbackResult::Stop = callback_result {
                        return Ok(false);
                    };
                }

                Frame::Open(FrameOpen {
                    bucket_time_millis,
                    bucket_duration_i,
                    oldest_bucket_time_millis_allowed,
                }) => {
                    let bucket_duration = bucket_durations[bucket_duration_i];

                    // Push our next sibling onto the stack
                    {
                        let next_bucket_duration_i = bucket_duration_i;
                        let next_bucket_time_millis = bucket_time_millis - bucket_duration;
                        let next_oldest_bucket_time_millis_allowed = oldest_bucket_time_millis_allowed;

                        if next_bucket_time_millis >= oldest_bucket_time_millis_allowed {
                            frames.push(Frame::Open(FrameOpen {
                                bucket_time_millis: next_bucket_time_millis,
                                bucket_duration_i: next_bucket_duration_i,
                                oldest_bucket_time_millis_allowed: next_oldest_bucket_time_millis_allowed,
                            }));
                        }
                    }

                    // Is this bucket too recent to be allowed? (sometimes they request exactly the start of the next bucket - e.g. 1M - we dont want to include that month)
                    if bucket_time_millis >= time_millis_max_exclusive {
                        continue;
                    }

                    let callback_result = on_bucket_open(bucket_time_millis, bucket_duration).await?;

                    // If the user has elected to bail early
                    if let RecursiveBucketVisitorOpenCallbackResult::Stop = callback_result {
                        return Ok(false);
                    };

                    // Push our close onto the stack
                    frames.push(Frame::Close(FrameClose { bucket_time_millis, bucket_duration_i }));

                    // If the user wants to go more granular
                    if let RecursiveBucketVisitorOpenCallbackResult::ContinueWithChildren = callback_result {
                        let next_bucket_duration_i = bucket_duration_i + 1;
                        if next_bucket_duration_i < bucket_durations.len() {
                            let next_bucket_duration = bucket_durations[next_bucket_duration_i];
                            let time_millis_max_bucket = BucketLocation::round_down_to_bucket_start(time_millis_max_exclusive, next_bucket_duration);

                            let next_bucket_time_millis = time_millis_max_bucket.min(bucket_time_millis + bucket_duration - next_bucket_duration);
                            let next_oldest_bucket_time_millis_allowed = bucket_time_millis;

                            frames.push(Frame::Open(FrameOpen {
                                bucket_time_millis: next_bucket_time_millis,
                                bucket_duration_i: next_bucket_duration_i,
                                oldest_bucket_time_millis_allowed: next_oldest_bucket_time_millis_allowed,
                            }));
                        }
                    }
                }
            }
        }

        Ok(true)
    }
}

#[cfg(test)]
pub mod tests {
    use crate::client::timeline::recursive_bucket_visitor::{RecursiveBucketVisitor, RecursiveBucketVisitorCloseCallbackResult, RecursiveBucketVisitorOpenCallbackResult};
    use crate::tools::buckets::BUCKET_DURATIONS;
    use crate::tools::time::{DurationMillis, MILLIS_IN_DAY, MILLIS_IN_MILLISECOND, MILLIS_IN_MONTH, MILLIS_IN_WEEK, TimeMillis};
    use log::info;

    #[tokio::test]
    async fn visitor_test() -> anyhow::Result<()> {
        // configure_logging();

        {
            info!("Just at a new month (so only one month in view)");
            let mut total_buckets = 0;

            RecursiveBucketVisitor::visit(
                TimeMillis::zero() + MILLIS_IN_MONTH,
                TimeMillis::zero(),
                &BUCKET_DURATIONS[1..3],
                &mut async |time_millis, duration_millis| {
                    info!("time_millis: {}, duration_millis: {}", time_millis, duration_millis);
                    total_buckets += 1;
                    Ok(RecursiveBucketVisitorOpenCallbackResult::ContinueWithChildren)
                },
                &mut async |_time_millis: TimeMillis, _duration_millis: DurationMillis| Ok(RecursiveBucketVisitorCloseCallbackResult::Continue),
            )
            .await?;

            assert_eq!(total_buckets, 5);
        }

        {
            info!("Just after a new month (one entire month and the beginning of one month in view)");
            let mut total_buckets = 0;

            RecursiveBucketVisitor::visit(
                TimeMillis::zero() + MILLIS_IN_MONTH + MILLIS_IN_MILLISECOND,
                TimeMillis::zero(),
                &BUCKET_DURATIONS[1..3],
                &mut async |time_millis: TimeMillis, duration_millis: DurationMillis| {
                    info!("time_millis: {}, duration_millis: {}", time_millis, duration_millis);
                    total_buckets += 1;
                    Ok(RecursiveBucketVisitorOpenCallbackResult::ContinueWithChildren)
                },
                &mut async |_time_millis: TimeMillis, _duration_millis: DurationMillis| Ok(RecursiveBucketVisitorCloseCallbackResult::Continue),
            )
            .await?;

            assert_eq!(total_buckets, 7);
        }

        {
            info!("Just at a new month plus a 2 weeks plus 3 days");
            let mut total_buckets = 0;

            RecursiveBucketVisitor::visit(
                TimeMillis::zero() + MILLIS_IN_MONTH + MILLIS_IN_WEEK.const_mul(2) + MILLIS_IN_DAY.const_mul(3),
                TimeMillis::zero(),
                &BUCKET_DURATIONS[1..4],
                &mut async |time_millis, duration_millis| {
                    info!("time_millis: {}, duration_millis: {}", time_millis, duration_millis);
                    total_buckets += 1;
                    Ok(RecursiveBucketVisitorOpenCallbackResult::ContinueWithChildren)
                },
                &mut async |_time_millis: TimeMillis, _duration_millis: DurationMillis| Ok(RecursiveBucketVisitorCloseCallbackResult::Continue),
            )
            .await?;

            assert_eq!(total_buckets, 54);
        }

        Ok(())
    }

    #[tokio::test]
    async fn visitor_granular_test() -> anyhow::Result<()> {
        // configure_logging();

        {
            info!("Just before the beginning of a new month (so only one month in view) - but mega granular");
            let mut total_buckets = 0;

            RecursiveBucketVisitor::visit(
                TimeMillis::zero() + MILLIS_IN_MONTH,
                TimeMillis::zero(),
                &BUCKET_DURATIONS[1..],
                &mut async |time_millis: TimeMillis, duration_millis: DurationMillis| {
                    info!("time_millis: {}, duration_millis: {}", time_millis, duration_millis);
                    total_buckets += 1;
                    Ok(RecursiveBucketVisitorOpenCallbackResult::ContinueWithChildren)
                },
                &mut async |_time_millis: TimeMillis, _duration_millis: DurationMillis| Ok(RecursiveBucketVisitorCloseCallbackResult::Continue),
            )
            .await?;

            let total_expected = 1 + 4 * (1 + 7 * (1 + 4 * (1 + 6 * (1 + 4 * (1 + 3 * (1 + 5))))));
            assert_eq!(total_buckets, total_expected);
        }

        Ok(())
    }

    #[tokio::test]
    async fn visitor_granular_with_recurse_sibling_test() -> anyhow::Result<()> {
        // configure_logging();

        {
            info!("Limit the depth we want to go based on a condition");
            let mut total_buckets = 0;

            RecursiveBucketVisitor::visit(
                TimeMillis::zero() + MILLIS_IN_MONTH,
                TimeMillis::zero(),
                &BUCKET_DURATIONS[1..],
                &mut async |time_millis: TimeMillis, duration_millis: DurationMillis| {
                    info!("time_millis: {}, duration_millis: {}", time_millis, duration_millis);
                    total_buckets += 1;

                    if duration_millis > MILLIS_IN_DAY {
                        Ok(RecursiveBucketVisitorOpenCallbackResult::ContinueWithChildren)
                    }
                    else {
                        Ok(RecursiveBucketVisitorOpenCallbackResult::ContinueWithoutChildren)
                    }
                },
                &mut async |_time_millis: TimeMillis, _duration_millis: DurationMillis| Ok(RecursiveBucketVisitorCloseCallbackResult::Continue),
            )
            .await?;

            let total_expected = 1 + 4 * (1 + 7);
            assert_eq!(total_buckets, total_expected);
        }

        Ok(())
    }

    #[tokio::test]
    async fn visitor_granular_with_abort_test() -> anyhow::Result<()> {
        // configure_logging();

        {
            info!("Abort after a few visits");
            let mut total_buckets = 0;

            RecursiveBucketVisitor::visit(
                TimeMillis::zero() + MILLIS_IN_MONTH,
                TimeMillis::zero(),
                &BUCKET_DURATIONS[1..],
                &mut async |bucket_time_millis: TimeMillis, bucket_duration_millis: DurationMillis| {
                    info!("bucket_time_millis: {}, duration_millis: {}", bucket_time_millis, bucket_duration_millis);
                    total_buckets += 1;

                    if total_buckets >= 9 {
                        Ok(RecursiveBucketVisitorOpenCallbackResult::Stop)
                    }
                    else {
                        Ok(RecursiveBucketVisitorOpenCallbackResult::ContinueWithChildren)
                    }
                },
                &mut async |_time_millis: TimeMillis, _duration_millis: DurationMillis| Ok(RecursiveBucketVisitorCloseCallbackResult::Continue),
            )
            .await?;

            assert_eq!(total_buckets, 9);
        }

        Ok(())
    }
}
