//! # Stop-watch — elapsed duration against any `TimeProvider`
//!
//! Constructed with an `Arc<dyn TimeProvider>`, records "now" at creation, and
//! returns the delta since then on demand. Used wherever a piece of code needs
//! "how long did this take" in a way that plays nicely with both real and virtual
//! clocks — e.g. PoW progress reporting and cache decimation throttles.

use std::sync::Arc;
use crate::tools::time::{DurationMillis, TimeMillis};
use crate::tools::time_provider::time_provider::TimeProvider;

pub struct StopWatch {
    time_provider: Arc<dyn TimeProvider>,
    start_time_millis: TimeMillis,
}

impl StopWatch {
    pub fn new(time_provider: Arc<dyn TimeProvider>) -> Self {
        let start_time_millis = time_provider.current_time_millis();
        StopWatch { time_provider, start_time_millis }
    }
    pub fn elapsed_time_millis(&self) -> DurationMillis {
        self.time_provider.current_time_millis() - self.start_time_millis
    }
}
