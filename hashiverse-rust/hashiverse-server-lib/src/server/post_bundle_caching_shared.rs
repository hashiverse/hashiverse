//! # Shared types and tuning constants for the bundle caches
//!
//! The common pieces used by both
//! [`crate::server::post_bundle_caching`] and
//! [`crate::server::post_bundle_feedback_caching`]:
//!
//! - [`CachedBundle`] — bytes + optional `expires_at`. *Live* (still-mutating)
//!   bundles carry an expiry; *sealed* (immutable past-tense) bundles do not and stay
//!   cached until evicted by LRU pressure.
//! - [`GetCacheResult`] — lookup outcome: `Miss`, `Hit`, `HitAndIssueToken` (the
//!   latter triggering [`hashiverse_lib::protocol::payload::payload::CacheRequestTokenV1`]
//!   emission from the caller).
//! - `CACHE_LOCATION_TTI` (60 s) and `CACHE_HIT_THRESHOLD` (10) — the tuning that
//!   decides when a bundle is "hot enough" to justify asking callers to help replicate
//!   it further.

use bytes::Bytes;
use hashiverse_lib::protocol::payload::payload::CacheRequestTokenV1;
use hashiverse_lib::tools::time::{DurationMillis, TimeMillis, MILLIS_IN_SECOND};
use std::time::Duration;

/// TTI for all cache entries: if a location_id hasn't been queried for a while, evict it entirely.
pub const CACHE_LOCATION_TTI: Duration = Duration::from_secs(60);

/// Number of hits on a placeholder entry before the server issues a CacheRequestToken.
/// Essentially: have we had this many requests in the last `CACHE_LOCATION_TTI`? If so, start caching.
pub const CACHE_HIT_THRESHOLD: u32 = 10;

/// How long a CacheRequestToken is valid for (client must upload within this window).
pub const CACHE_REQUEST_TOKEN_TTL_DURATION_MILLIS: DurationMillis = MILLIS_IN_SECOND.const_mul(30);
pub const CACHE_REQUEST_TOKEN_TTL_DURATION: Duration = Duration::from_millis(CACHE_REQUEST_TOKEN_TTL_DURATION_MILLIS.0 as u64);

/// One bundle (from one originator peer) held in the cache.
pub struct CachedBundle {
    pub bytes: Bytes,
    /// `None` for sealed bundles (never individually stale; only location-level TTI applies).
    /// `Some(server_time + 5min)` for live bundles (checked manually on read).
    pub expires_at: Option<TimeMillis>,
}

impl CachedBundle {
    pub fn is_stale(&self, now: TimeMillis) -> bool {
        match self.expires_at {
            None => false,
            Some(exp) => now >= exp,
        }
    }
}

/// Result returned to the dispatch handler by either cache's `on_get`.
pub struct GetCacheResult {
    /// Fresh cached bundles filtered by `already_retrieved_peer_ids`.
    pub cached_items: Vec<Bytes>,
    /// Issued when the server wants the client to upload after its walk.
    pub cache_request_token: Option<CacheRequestTokenV1>,
}
