use hashiverse_lib::protocol::payload::payload::{PayloadRequestKind, PAYLOAD_REQUEST_KIND_COUNT};
use hashiverse_lib::tools::decaying_counter::DecayingCounter;
use hashiverse_lib::tools::time::{TimeMillis, MILLIS_IN_DAY, MILLIS_IN_HOUR, MILLIS_IN_MONTH};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-`PayloadRequestKind` decaying call-rate estimates over three trailing
/// windows. Each [`DecayingCounter`] settles at `rate · τ`, so the values read
/// directly as "estimated calls in the last hour / day / month". Pairs with the
/// lock-free all-time `request_counters` totals, which stay exact.
///
/// `MILLIS_IN_MONTH` is the codebase's 4-week (28-day) month.
pub struct RequestRateWindows {
    per_hour: DecayingCounter,
    per_day: DecayingCounter,
    per_month: DecayingCounter,
}

impl RequestRateWindows {
    pub fn new() -> Self {
        Self {
            per_hour: DecayingCounter::new(MILLIS_IN_HOUR),
            per_day: DecayingCounter::new(MILLIS_IN_DAY),
            per_month: DecayingCounter::new(MILLIS_IN_MONTH),
        }
    }

    /// Record one inbound call at `now` across all three windows.
    pub fn record(&mut self, now: TimeMillis) {
        self.per_hour.record(now, 1);
        self.per_day.record(now, 1);
        self.per_month.record(now, 1);
    }
}

impl Default for RequestRateWindows {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the `requests` subtree: one object per [`PayloadRequestKind`] variant,
/// keyed by its `Display` name, holding the all-time `total` plus the decaying
/// `per_hour` / `per_day` / `per_month` rate estimates evaluated at `now`.
///
/// Total reads use `Ordering::Relaxed` — counters are advisory metrics, not
/// synchronisation primitives, and the snapshot doesn't need to be coherent
/// across kinds.
pub fn request_counts_subtree(totals: &[AtomicU64; PAYLOAD_REQUEST_KIND_COUNT], windows: &[Mutex<RequestRateWindows>; PAYLOAD_REQUEST_KIND_COUNT], now: TimeMillis) -> serde_json::Value {
    let mut map = serde_json::Map::with_capacity(PAYLOAD_REQUEST_KIND_COUNT);
    for index in 0..PAYLOAD_REQUEST_KIND_COUNT {
        let kind = match PayloadRequestKind::from_u16(index as u16) {
            Ok(kind) => kind,
            Err(_) => continue,
        };

        let total = totals[index].load(Ordering::Relaxed);
        let (per_hour, per_day, per_month) = {
            let window = windows[index].lock();
            (window.per_hour.estimate(now), window.per_day.estimate(now), window.per_month.estimate(now))
        };

        let entry = serde_json::json!({
            "total":     total,
            "per_hour":  per_hour.round() as u64,
            "per_day":   per_day.round() as u64,
            "per_month": per_month.round() as u64,
        });
        map.insert(kind.to_string(), entry);
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hashiverse_lib::tools::time::MILLIS_IN_MINUTE;

    fn fresh_state() -> ([AtomicU64; PAYLOAD_REQUEST_KIND_COUNT], [Mutex<RequestRateWindows>; PAYLOAD_REQUEST_KIND_COUNT]) {
        (std::array::from_fn(|_| AtomicU64::new(0)), std::array::from_fn(|_| Mutex::new(RequestRateWindows::new())))
    }

    #[test]
    fn subtree_has_nested_shape_per_kind() {
        let (totals, windows) = fresh_state();
        let doc = request_counts_subtree(&totals, &windows, TimeMillis::zero());

        let ping = doc.get(&PayloadRequestKind::PingV1.to_string()).expect("PingV1 entry present");
        for key in ["total", "per_hour", "per_day", "per_month"] {
            assert!(ping.get(key).is_some(), "PingV1 entry should have {} key: {}", key, ping);
        }
    }

    #[test]
    fn records_reflect_in_total_and_windows() {
        let (totals, windows) = fresh_state();
        let ping_index = PayloadRequestKind::PingV1 as usize;

        // Drive one call per minute for an hour into PingV1.
        let mut now = TimeMillis::zero();
        for _ in 0..60 {
            now = TimeMillis(now.0 + MILLIS_IN_MINUTE.0);
            totals[ping_index].fetch_add(1, Ordering::Relaxed);
            windows[ping_index].lock().record(now);
        }

        let doc = request_counts_subtree(&totals, &windows, now);
        let ping = doc.get(&PayloadRequestKind::PingV1.to_string()).unwrap();

        assert_eq!(ping.get("total").unwrap().as_u64().unwrap(), 60);
        // ~one-per-minute into a 1h window settles in the tens (not the full 60 yet
        // after only one time constant of accumulation); just assert it registered.
        let per_hour = ping.get("per_hour").unwrap().as_u64().unwrap();
        assert!(per_hour > 0, "per_hour should be non-zero after 60 calls, got {}", per_hour);

        // An untouched kind stays all zero.
        let error = doc.get(&PayloadRequestKind::ErrorV1.to_string()).unwrap();
        assert_eq!(error.get("total").unwrap().as_u64().unwrap(), 0);
        assert_eq!(error.get("per_hour").unwrap().as_u64().unwrap(), 0);
    }
}
