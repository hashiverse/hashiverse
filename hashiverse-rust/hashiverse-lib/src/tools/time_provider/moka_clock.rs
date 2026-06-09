//! Adapter exposing a [`TimeProvider`] as a [`moka::ExternalClock`].
//!
//! By default a moka cache measures its TTL/TTI/expiry against wall-clock
//! `std::time::Instant`, which ignores our pluggable `TimeProvider` — so under a
//! `ScaledTimeProvider` (integration tests run time fast) cache eviction would
//! fire on real time, out of step with the rest of the system. Building a cache
//! with `.external_clock(Arc::new(TimeProviderMokaClock::new(time_provider)))`
//! routes all of moka's time decisions through that provider instead, so the
//! same code behaves correctly under both real and scaled clocks.
//!
//! Relies on the `ExternalClock` seam added to our vendored moka fork
//! (see `3rdparty/moka/PATCH_NOTES.md`).

use std::sync::Arc;
use std::time::Duration;

use crate::tools::time_provider::time_provider::TimeProvider;

/// Bridges a [`TimeProvider`] to [`moka::ExternalClock`]. moka tracks time as a
/// `Duration` elapsed since the clock's origin, so we capture the provider's
/// reading at construction and report the difference on each call.
pub struct TimeProviderMokaClock {
    time_provider: Arc<dyn TimeProvider>,
    origin_millis: i64,
}

impl TimeProviderMokaClock {
    pub fn new(time_provider: Arc<dyn TimeProvider>) -> Self {
        let origin_millis = time_provider.current_time_millis().0;
        Self { time_provider, origin_millis }
    }
}

impl moka::ExternalClock for TimeProviderMokaClock {
    fn elapsed_since_origin(&self) -> Duration {
        let now = self.time_provider.current_time_millis().0;
        Duration::from_millis(now.saturating_sub(self.origin_millis).max(0) as u64)
    }
}
