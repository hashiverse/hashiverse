//! # `TimeProvider` trait — the clock seam
//!
//! A small trait every component takes as an `Arc<dyn TimeProvider>` instead of
//! calling `SystemTime::now()` or `tokio::time::sleep` directly. `RealTimeProvider`
//! delegates to wall-clock time; [`crate::tools::time_provider::manual_time_provider`]
//! lets tests advance time by hand and makes every `sleep` return immediately, so
//! deterministic integration tests can span simulated days in milliseconds of
//! real wall time.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use chrono::{DateTime, TimeZone, Utc};
use crate::tools::time::{DurationMillis, TimeMillis};

/// A stubbable abstraction over wall-clock time and asynchronous sleeping.
///
/// Every component in the crate that needs to know "what time is it?" or "wake me in N
/// milliseconds?" goes through a `TimeProvider` rather than touching `std::time` or
/// `tokio::time` directly. That lets tests swap in a virtual clock that can be advanced
/// programmatically — critical for exercising time-dependent logic (bucket rotation, PoW
/// retry budgets, peer freshness, LRU eviction) without actually sleeping the test harness.
///
/// The production implementation is [`RealTimeProvider`]. In-memory integration tests use a
/// controllable stub whose `sleep` resolves only when the virtual clock is advanced past the
/// wake time. `TimeProvider` is carried on [`crate::tools::runtime_services::RuntimeServices`]
/// so every layer — transport, client, server — shares a single clock.
pub trait TimeProvider: Send + Sync {
    /// Returns the current time in milliseconds since the UNIX epoch
    fn current_time_millis(&self) -> TimeMillis;

    fn current_time_str(&self) -> String {
        self.current_time_millis().to_string()
    }

    /// Convert the current time to a `DateTime<Utc>`
    fn current_datetime(&self) -> DateTime<Utc> {
        let millis = self.current_time_millis();
        Utc.timestamp_opt(millis.as_secs(), millis.part_nanos() as u32).unwrap()
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>>;

    fn sleep_millis(&self, millis: DurationMillis) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.sleep(Duration::from(millis))
    }
}

/// Implementation of TimeProvider that uses the system clock
#[derive(Default, Clone)]
pub struct RealTimeProvider;

impl TimeProvider for RealTimeProvider {
    fn current_time_millis(&self) -> TimeMillis {
        let now: DateTime<Utc> = Utc::now();
        TimeMillis(now.timestamp_millis())
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// Implementation of TimeProvider that scales time by a given factor
///
/// This provider allows you to make time pass faster or slower than real time.
/// For example, a scale factor of 2.0 means time passes twice as fast,
/// and a scale factor of 0.5 means time passes at half speed.
#[derive(Clone)]
pub struct ScaledTimeProvider {
    scale_factor: f64,
    start_real_time: i64,
}

impl ScaledTimeProvider {
    /// Create a new ScaledTimeProvider with the specified scale factor
    ///
    /// # Arguments
    /// * `scale_factor` - How much faster (>1.0) or slower (<1.0) time should pass compared to real time
    pub fn new(scale_factor: f64) -> Self {
        Self {
            scale_factor,
            start_real_time: Utc::now().timestamp_millis(),
        }
    }
}

impl TimeProvider for ScaledTimeProvider {
    fn current_time_millis(&self) -> TimeMillis {
        // Calculate how much real time has passed since we started
        let real_now = Utc::now().timestamp_millis();
        let real_elapsed = real_now.saturating_sub(self.start_real_time);

        // Scale the elapsed time and add it to our starting point
        let scaled_elapsed = (real_elapsed as f64 * self.scale_factor) as i64;
        TimeMillis(scaled_elapsed)
    }

    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        // Calculate the scaled duration and create the sleep future once
        let scaled_duration = Duration::from_secs_f64(duration.as_secs_f64() / self.scale_factor);
        Box::pin(tokio::time::sleep(scaled_duration))
    }
}


