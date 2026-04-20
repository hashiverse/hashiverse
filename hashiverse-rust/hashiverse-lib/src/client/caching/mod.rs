//! # Read-cache propagation helpers
//!
//! Client-side helpers that guide how cached post bundles spread across the DHT.
//! One piece remembers how far caching has propagated so future walks skip
//! already-cached peers; the other forwards freshly-written bundles to other
//! servers in the background under tokens returned by the write.

pub mod cache_radius_tracker;
pub mod post_bundle_cache_uploader;
