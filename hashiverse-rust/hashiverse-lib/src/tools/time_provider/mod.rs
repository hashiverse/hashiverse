//! # Clock abstraction and test utilities
//!
//! The `TimeProvider` trait lets every component read "now" through a common
//! interface, which in turn lets tests swap in a virtual clock that can jump
//! forward on demand. A stop-watch helper computes elapsed durations on top of
//! whichever clock is in use.

pub mod time_provider;

#[cfg(not(target_arch = "wasm32"))]
pub mod manual_time_provider;
pub mod moka_clock;
pub mod stop_watch;


