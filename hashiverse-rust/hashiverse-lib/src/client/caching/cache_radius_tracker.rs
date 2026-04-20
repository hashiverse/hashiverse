//! # Cache-radius tracker for DHT walk pruning
//!
//! Records, per-location and with a TTL, how far out in the DHT (measured in
//! [`crate::tools::tools::LeadingAgreementBits`]) we've already seen cached copies of a
//! given bundle. Subsequent DHT walks for the same location consult the tracker and skip
//! peers that are closer than the recorded radius — those peers are almost certainly
//! already cached — and instead try wider. This spreads cache load outward over time and
//! relieves the small set of "closest" peers from being hammered for every read.
//!
//! Uses `parking_lot::RwLock` because the tracker is read on every timeline walk but
//! written only when a new cached hit comes back.

use crate::tools::time::{DurationMillis, TimeMillis};
use crate::tools::tools::LeadingAgreementBits;
use crate::tools::types::Id;
use parking_lot::RwLock;
use std::collections::HashMap;

/// Per-location TTL store of the walk's cache radius.
///
/// The cache radius is the minimum `LeadingAgreementBits` of all servers that returned data
/// during the previous walk. It represents how far out caching has propagated: all servers
/// with LAB ≥ radius are likely to already hold cached data, so future walks can try a little further out, thereby alleviating load.
pub struct CacheRadiusTracker {
    radii: RwLock<HashMap<Id, (LeadingAgreementBits, TimeMillis)>>,
    ttl: DurationMillis,
}

impl CacheRadiusTracker {
    pub fn new(ttl: DurationMillis) -> Self {
        Self {
            radii: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    /// Returns the cached radius if it has not expired.
    pub fn get(&self, location_id: Id, now: TimeMillis) -> Option<LeadingAgreementBits> {
        let radii = self.radii.read();
        if let Some(&(radius, stored_at)) = radii.get(&location_id) {
            if (now - stored_at) < self.ttl {
                return Some(radius);
            }
        }
        None
    }

    /// Replaces the stored radius unconditionally (each walk produces a fresh observation).
    pub fn update(&self, location_id: Id, radius: LeadingAgreementBits, now: TimeMillis) {
        self.radii.write().insert(location_id, (radius, now));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::time::{DurationMillis, TimeMillis};
    use crate::tools::types::Id;

    #[test]
    fn test_get_returns_none_when_empty() {
        let tracker = CacheRadiusTracker::new(DurationMillis(60_000));
        let id = Id::random();
        let now = TimeMillis(1_000_000);
        assert_eq!(None, tracker.get(id, now));
    }

    #[test]
    fn test_update_and_get() {
        let tracker = CacheRadiusTracker::new(DurationMillis(60_000));
        let id = Id::random();
        let now = TimeMillis(1_000_000);
        tracker.update(id, 10, now);
        assert_eq!(Some(10), tracker.get(id, now));
        assert_eq!(Some(10), tracker.get(id, TimeMillis(1_000_000 + 59_999)));
    }

    #[test]
    fn test_get_returns_none_after_ttl() {
        let tracker = CacheRadiusTracker::new(DurationMillis(60_000));
        let id = Id::random();
        let now = TimeMillis(1_000_000);
        tracker.update(id, 10, now);
        assert_eq!(None, tracker.get(id, TimeMillis(1_000_000 + 60_000)));
        assert_eq!(None, tracker.get(id, TimeMillis(1_000_000 + 120_000)));
    }

    #[test]
    fn test_update_overwrites() {
        let tracker = CacheRadiusTracker::new(DurationMillis(60_000));
        let id = Id::random();
        let now = TimeMillis(1_000_000);
        tracker.update(id, 10, now);
        tracker.update(id, 20, now);
        assert_eq!(Some(20), tracker.get(id, now));
    }

    #[test]
    fn test_different_locations_independent() {
        let tracker = CacheRadiusTracker::new(DurationMillis(60_000));
        let id1 = Id::random();
        let id2 = Id::random();
        let now = TimeMillis(1_000_000);
        tracker.update(id1, 10, now);
        assert_eq!(Some(10), tracker.get(id1, now));
        assert_eq!(None, tracker.get(id2, now));
    }
}
