//! # Virtual clock for deterministic tests
//!
//! Implements [`crate::tools::time_provider::time_provider::TimeProvider`] as a
//! manually-advanced clock. Tests set the "current" time explicitly, and every
//! `sleep` returns immediately — so a simulated network that covers hours or days
//! of activity runs in a few hundred milliseconds of wall time, deterministically.

use crate::tools::time::{TimeMillis};
use parking_lot::RwLock;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use crate::tools::time_provider::time_provider::TimeProvider;

/// A wake time entry for the manual time provider
#[derive(Debug)]
struct ManualTimeProviderWakeTime {
    time: TimeMillis,
    waker: Waker,
}

impl PartialEq for ManualTimeProviderWakeTime {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time
    }
}

impl Eq for ManualTimeProviderWakeTime {}

impl PartialOrd for ManualTimeProviderWakeTime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ManualTimeProviderWakeTime {
    fn cmp(&self, other: &Self) -> Ordering {
        // We want a min-heap, so reverse the comparison
        other.time.cmp(&self.time)
    }
}

/// Implementation of TimeProvider that allows manual control of time
///
/// This provider allows you to explicitly set the current time for testing
/// time-dependent code without relying on the system clock.
#[derive(Clone)]
pub struct ManualTimeProvider {
    current_time: Arc<RwLock<TimeMillis>>,
    wake_times: Arc<RwLock<BinaryHeap<ManualTimeProviderWakeTime>>>,
    new_sleepers_notify: Arc<Notify>,
}

impl Default for ManualTimeProvider {
    fn default() -> Self {
        Self::new(TimeMillis::zero())
    }
}

impl ManualTimeProvider {
    /// Create a new ManualTimeProvider with a specified starting time
    pub fn new(start_time_millis: TimeMillis) -> Self {
        Self {
            current_time: Arc::new(RwLock::new(start_time_millis)),
            wake_times: Arc::new(RwLock::new(BinaryHeap::new())),
            new_sleepers_notify: Arc::new(Notify::new()),
        }
    }

    /// Advance time until there are no more registered sleepers.
    ///
    /// It jumps time forward to the next wakeup repeatedly, waking all due tasks,
    /// and yields to the executor so those tasks can make progress and potentially
    /// register more sleeps.
    pub async fn run_all_sleepers_till_done(&self, cancellation_token: &CancellationToken) {
        while !cancellation_token.is_cancelled() {
            if self.wake_times.read().is_empty() {
                tokio::select! {
                   _ = self.new_sleepers_notify.notified() => {},
                    _ = cancellation_token.cancelled() => {},
                }
            }

            tokio::task::yield_now().await;
            self.advance_time_until_next_sleeper().await;
        }
    }

    /// Advance time to the earlier of: the next scheduled wake time or the current time plus max_advance_ms
    ///
    /// Returns the amount of time (in milliseconds) that was actually advanced and the remaining number of sleeping taks
    pub async fn advance_time_until_next_sleeper(&self) {
        let mut current = self.current_time.write();
        let mut wake_times = self.wake_times.write();

        let new_time = match wake_times.peek() {
            Some(wake_time) => wake_time.time,
            None => *current,
        };

        // Set the new current time
        *current = new_time;

        // Collect wakers that need to be awakened
        let mut wakers_to_wake = Vec::new();

        while let Some(wake_time) = wake_times.peek() {
            if wake_time.time <= new_time {
                if let Some(entry) = wake_times.pop() {
                    wakers_to_wake.push(entry.waker);
                }
            }
            else {
                break;
            }
        }

        // Drop the locks before waking to avoid potential deadlocks
        drop(current);
        drop(wake_times);

        // Now wake all the sleepers
        for waker in wakers_to_wake {
            waker.wake();
        }
    }

    /// Jump the clock to `time`, waking any sleepers now due. Unlike
    /// `advance_time_until_next_sleeper`, this sets an explicit time, letting a
    /// test exercise time-dependent logic (e.g. score decay) deterministically
    /// without an active time-driver task.
    pub fn set_time(&self, time: TimeMillis) {
        *self.current_time.write() = time;

        let mut wake_times = self.wake_times.write();
        let mut wakers_to_wake = Vec::new();
        while let Some(wake_time) = wake_times.peek() {
            if wake_time.time <= time {
                if let Some(entry) = wake_times.pop() {
                    wakers_to_wake.push(entry.waker);
                }
            } else {
                break;
            }
        }
        drop(wake_times);
        for waker in wakers_to_wake {
            waker.wake();
        }
    }

    /// Register a waker to be notified when time reaches a certain point
    fn register_wake_time(&self, wake_time: TimeMillis, waker: Waker) {
        let mut wake_times = self.wake_times.write();
        wake_times.push(ManualTimeProviderWakeTime { time: wake_time, waker });

        // Wake the time driver (if it is waiting for more sleepers)
        self.new_sleepers_notify.notify_one();
    }
}

/// Future returned by ManualTimeProvider::sleep
pub struct ManualTimeProviderSleep {
    provider: ManualTimeProvider,
    wake_time: TimeMillis,
}

impl Future for ManualTimeProviderSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Check if the current time has reached or passed the wake time
        let current_time = self.provider.current_time_millis();
        if current_time >= self.wake_time {
            return Poll::Ready(());
        }

        // Register waker so we're notified when time advances.
        let waker = cx.waker().clone();
        self.provider.register_wake_time(self.wake_time, waker);

        Poll::Pending
    }
}

impl TimeProvider for ManualTimeProvider {
    fn current_time_millis(&self) -> TimeMillis {
        *self.current_time.read()
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let current_time = self.current_time_millis();
        let wake_time = current_time + duration;

        Box::pin(ManualTimeProviderSleep {
            provider: self.clone(), // Note that the provider may be a different copy, but the state inside is shared...
            wake_time,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::time::{MILLIS_IN_SECOND, TimeMillis};
    use log::info;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;
    use crate::tools::time_provider::manual_time_provider::ManualTimeProvider;
    use crate::tools::time_provider::time_provider::TimeProvider;

    #[tokio::test]
    async fn generic_test() {
        let time_provider = Arc::new(ManualTimeProvider::new(TimeMillis::zero()));
        // configure_logging_with_time_provider("trace", time_provider.clone());

        let cancellation_token = CancellationToken::new();

        tokio::join!(
            async {
                info!("Thread 1 start");
                for _ in 0..10 {
                    info!("Thread 1 tick");
                    time_provider.sleep_millis(MILLIS_IN_SECOND.const_mul(1)).await;
                }
                info!("Thread 1 end");
            },
            async {
                info!("Thread 2 start");
                for _ in 0..10 {
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                    info!("Thread 2 tick");
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                    time_provider.sleep_millis(MILLIS_IN_SECOND.const_mul(1)).await;
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                    tokio::task::yield_now().await;
                }
                cancellation_token.cancel();
                info!("Thread 2 end");
            },
            async {
                info!("Time driver start");
                time_provider.run_all_sleepers_till_done(&cancellation_token).await;
                info!("Time driver end");
            },
        );
    }
}
