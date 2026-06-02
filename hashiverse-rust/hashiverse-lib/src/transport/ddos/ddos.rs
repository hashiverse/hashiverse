//! # Per-IP DDoS scoring and connection-slot accounting
//!
//! Defines three collaborating pieces:
//!
//! - [`DdosScore`] — per-IP score with linear time-decay (drains
//!   [`crate::tools::config::SERVER_DDOS_DECAY_PER_SECOND`] points per second). Bad
//!   requests add points, good requests don't, and once the score crosses
//!   [`crate::tools::config::SERVER_DDOS_SCORE_THRESHOLD`] the IP is banned. Decay is
//!   lazy so there's no background timer — every `increment` or `current` call
//!   recomputes based on elapsed real time.
//! - [`DdosConnectionGuard`] — an RAII guard returned to the transport for every open
//!   connection slot. Its `Drop` decrements the per-IP connection count. Request
//!   processing code threads this guard through `IncomingRequest`, so the moment the
//!   request is finished the slot is freed, automatically.
//! - [`DdosProtection`] — the overall trait the transport calls into:
//!   `try_accept_connection` (returns `None` if over limit), `observe_bad_request`,
//!   `ban` (pokes `ipset`/`iptables` via the sibling server crate on native).
//!
//! Two implementations exist:
//! [`crate::transport::ddos::mem_ddos`] for accounting-only deployments and tests, and
//! the native ipset-backed production implementation lives in `hashiverse-server-lib`.

use std::sync::Arc;
use crate::tools::time::TimeMillis;

/// Per-IP score with linear time decay.
///
/// Score drains at `decay_per_second` points per second. The decay is applied
/// lazily on each `increment` or `current` call — no background timer needed —
/// against a caller-supplied `now` (drawn from the pluggable `TimeProvider`), so
/// it stays consistent with the rest of the system under a scaled test clock.
pub struct DdosScore {
    score: f64,
    /// `None` until the first `increment`, so a freshly-created score applies no decay.
    last_updated: Option<TimeMillis>,
}

impl Default for DdosScore {
    fn default() -> Self {
        Self::new()
    }
}

impl DdosScore {
    pub fn new() -> Self {
        Self { score: 0.0, last_updated: None }
    }

    fn elapsed_secs(&self, now: TimeMillis) -> f64 {
        match self.last_updated {
            Some(prev) => (now.0.saturating_sub(prev.0).max(0) as f64) / 1000.0,
            None => 0.0,
        }
    }

    /// Decay the score based on elapsed time, then add `points`. Returns the new score.
    pub fn increment(&mut self, points: f64, decay_per_second: f64, now: TimeMillis) -> f64 {
        self.score = (self.score - decay_per_second * self.elapsed_secs(now)).max(0.0) + points;
        self.last_updated = Some(now);
        self.score
    }

    /// Read the decayed score without modifying it.
    pub fn current(&self, decay_per_second: f64, now: TimeMillis) -> f64 {
        (self.score - decay_per_second * self.elapsed_secs(now)).max(0.0)
    }
}

/// RAII guard for a single IP's connection slot.
///
/// Created by `DdosConnectionGuard::try_new`; dropped when the connection ends.
/// While alive it holds a slot in the per-IP connection counter.
/// Exposes `allow_request` and `report_bad_request` so callers never need to
/// pass a raw `Arc<dyn DdosProtection>` through request handling code.
pub struct DdosConnectionGuard {
    ip: String,
    ddos: Arc<dyn DdosProtection>,
}

impl DdosConnectionGuard {
    /// Try to acquire a connection slot for `ip`.
    ///
    /// Returns `None` if the IP is over the per-IP connection cap or is already
    /// rate-limited.  Returns `Some(guard)` on success; the slot is released
    /// automatically when the guard is dropped.
    pub fn try_new(ddos: Arc<dyn DdosProtection>, ip: impl Into<String>) -> Option<Self> {
        let ip = ip.into();
        if ddos.try_acquire_connection(&ip) {
            Some(Self { ip, ddos })
        } else {
            None
        }
    }

    pub fn ip(&self) -> &str {
        &self.ip
    }

    /// Returns `true` if the next request from this connection should be processed.
    pub fn allow_request(&self) -> bool {
        self.ddos.allow_request(&self.ip)
    }

    /// Report that a request from this connection was malformed or malicious.
    pub fn report_bad_request(&self) {
        self.ddos.report_bad_request(&self.ip)
    }
}

impl Drop for DdosConnectionGuard {
    fn drop(&mut self) {
        self.ddos.release_connection(&self.ip);
    }
}

/// The server-side throttle and abuse-mitigation policy for inbound connections.
///
/// Every inbound request is gated through a `DdosProtection` implementation: the server asks
/// `try_acquire_connection` when a new connection lands, `allow_request` before processing
/// each request, and calls `report_bad_request` when a packet fails validation (bad PoW,
/// malformed RPC framing, signature mismatch, …). Implementations accumulate a per-IP score
/// from reported bad requests and refuse further connections above a threshold.
///
/// Pairing with [`DdosConnectionGuard`] means call sites never have to pass a raw
/// `Arc<dyn DdosProtection>` through every layer of the request handler — the guard carries
/// a handle to this trait through `IncomingRequest` and releases its slot automatically
/// on drop. A `NoopDdosProtection` exists for tests that do not want to exercise rate limiting.
pub trait DdosProtection: Send + Sync {
    /// Returns `true` if the request from `ip` should be processed, `false` if it should be
    /// dropped immediately.
    fn allow_request(&self, ip: &str) -> bool;

    /// Notify the implementation that a request from `ip` was rejected.  Implementations
    /// should use this to accumulate evidence and eventually ban repeat offenders.
    fn report_bad_request(&self, ip: &str);

    /// Try to acquire a connection slot for `ip`, checking both the ban score and the
    /// per-IP connection cap.  Returns `true` and increments the connection count on
    /// success.  Returns `false` if the IP is blocked or over the per-IP cap.
    ///
    /// Must be paired with `release_connection` — this is handled automatically by
    /// `DdosConnectionGuard::drop`.
    fn try_acquire_connection(&self, ip: &str) -> bool;

    /// Release a connection slot previously acquired by `try_acquire_connection`.
    /// Called automatically by `DdosConnectionGuard::drop`.
    fn release_connection(&self, ip: &str);
}

#[cfg(test)]
mod tests {
    use super::DdosScore;
    use crate::tools::time::TimeMillis;

    #[test]
    fn fresh_score_has_no_decay_on_first_increment() {
        // last_updated is None initially, so the first increment applies no decay
        // regardless of decay_per_second or the supplied `now`.
        let mut score = DdosScore::new();
        assert_eq!(score.increment(5.0, 1000.0, TimeMillis(1_000_000)), 5.0);
    }

    #[test]
    fn score_decays_with_elapsed_time() {
        let decay_per_second = 2.0;
        let mut score = DdosScore::new();

        // t=0: +10 -> 10.0 (no decay on first increment)
        assert_eq!(score.increment(10.0, decay_per_second, TimeMillis(0)), 10.0);

        // 3s later: read decays 2.0 * 3 = 6.0 -> 4.0, without mutating.
        assert_eq!(score.current(decay_per_second, TimeMillis(3_000)), 4.0);
        // current() must not have mutated the stored score.
        assert_eq!(score.current(decay_per_second, TimeMillis(0)), 10.0);

        // 10s later: 2.0 * 10 = 20 fully drains it (floored at 0), then +1 -> 1.0
        assert_eq!(score.increment(1.0, decay_per_second, TimeMillis(10_000)), 1.0);
    }

    #[test]
    fn zero_decay_never_drains() {
        let mut score = DdosScore::new();
        score.increment(3.0, 0.0, TimeMillis(0));
        assert_eq!(score.current(0.0, TimeMillis(1_000_000_000)), 3.0);
    }
}
