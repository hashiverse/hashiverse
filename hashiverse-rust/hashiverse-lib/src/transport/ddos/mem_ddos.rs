//! # In-memory DDoS accounting
//!
//! Implements [`crate::transport::ddos::ddos::DdosProtection`] purely in RAM: per-IP
//! `DdosScore`s live in a `moka` cache with time-based eviction so idle IPs get
//! collected automatically, and per-IP connection counts live in a `HashMap` guarded
//! by a `parking_lot::Mutex`.
//!
//! "Ban" here is just a flag in the cache — no kernel-level dropping. That makes this
//! implementation suitable for tests (the integration harness stresses the scoring
//! logic without wanting to touch host firewall state) and for platforms where
//! `ipset`/`iptables` aren't available. The production path in
//! `hashiverse-server-lib` wraps this with a real firewall-level ban via
//! [`crate::tools::config::SERVER_DDOS_IPSET_SET_NAME`].

use crate::transport::ddos::ddos::{DdosProtection, DdosScore};
use log::warn;
use moka::sync::Cache;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// In-memory DDoS protection with linearly decaying per-IP scores.
///
/// Each `allow_request` adds 1.0 point, each `report_bad_request` adds
/// `bad_request_penalty` points.  Between calls the score drains at
/// `decay_per_second` points/second, so sustained low-rate traffic stabilises
/// well below the threshold while bursts trigger quickly.
///
/// Scores are stored in a moka cache whose idle expiry is long enough for any
/// maxed-out score to fully decay, keeping memory bounded.
pub struct MemDdosProtection {
    score_threshold: f64,
    decay_per_second: f64,
    bad_request_penalty: f64,
    max_connections_per_ip: usize,
    scores: Cache<String, Arc<Mutex<DdosScore>>>,
    connections: Mutex<HashMap<String, usize>>,
}

impl MemDdosProtection {
    pub fn new(score_threshold: f64, decay_per_second: f64, bad_request_penalty: f64, max_connections_per_ip: usize) -> Self {
        // Idle expiry: time for a maxed-out score to fully decay, with 2x margin
        let idle_secs = if decay_per_second > 0.0 {
            (score_threshold / decay_per_second * 2.0).ceil() as u64
        } else {
            3600 // fallback: 1 hour if no decay
        };
        Self {
            score_threshold,
            decay_per_second,
            bad_request_penalty,
            max_connections_per_ip,
            scores: Cache::builder().time_to_idle(Duration::from_secs(idle_secs)).build(),
            connections: Mutex::new(HashMap::new()),
        }
    }

    fn increment_score(&self, ip: &str, points: f64) -> f64 {
        let entry = self.scores.get_with(ip.to_string(), || Arc::new(Mutex::new(DdosScore::new())));
        entry.lock().increment(points, self.decay_per_second)
    }

    fn is_score_banned(&self, ip: &str) -> bool {
        self.scores
            .get(ip)
            .map(|entry| entry.lock().current(self.decay_per_second) >= self.score_threshold)
            .unwrap_or(false)
    }
}

impl DdosProtection for MemDdosProtection {
    fn allow_request(&self, ip: &str) -> bool {
        self.increment_score(ip, 1.0) < self.score_threshold
    }

    fn report_bad_request(&self, ip: &str) {
        let score = self.increment_score(ip, self.bad_request_penalty);
        if score >= self.score_threshold {
            warn!("DDoS: {} blocked (score={:.1})", ip, score);
        }
    }

    fn try_acquire_connection(&self, ip: &str) -> bool {
        if self.is_score_banned(ip) {
            return false;
        }
        let mut connections = self.connections.lock();
        let count = connections.entry(ip.to_string()).or_insert(0);
        if *count >= self.max_connections_per_ip {
            return false;
        }
        *count += 1;
        true
    }

    fn release_connection(&self, ip: &str) {
        let mut connections = self.connections.lock();
        if let Some(count) = connections.get_mut(ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                connections.remove(ip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::config;
    use crate::transport::ddos::ddos::DdosConnectionGuard;

    fn make_ddos() -> Arc<MemDdosProtection> {
        Arc::new(MemDdosProtection::new(
            config::SERVER_DDOS_SCORE_THRESHOLD,
            config::SERVER_DDOS_DECAY_PER_SECOND,
            config::SERVER_DDOS_BAD_REQUEST_PENALTY,
            config::SERVER_DDOS_MAX_CONNECTIONS_PER_IP,
        ))
    }

    #[test]
    fn connection_guard_limits_per_ip() {
        let ddos = make_ddos();
        let ip = "1.2.3.4";

        let mut guards = vec![];
        for _ in 0..config::SERVER_DDOS_MAX_CONNECTIONS_PER_IP {
            let guard = DdosConnectionGuard::try_new(ddos.clone(), ip);
            assert!(guard.is_some(), "should acquire slot within limit");
            guards.push(guard.unwrap());
        }

        let over_limit = DdosConnectionGuard::try_new(ddos.clone(), ip);
        assert!(over_limit.is_none(), "should be blocked at per-IP cap");

        // Release one slot — should unblock
        drop(guards.pop().unwrap());
        let recovered = DdosConnectionGuard::try_new(ddos.clone(), ip);
        assert!(recovered.is_some(), "should acquire after release");
    }

    #[test]
    fn connection_guard_independent_ips() {
        let ddos = make_ddos();

        let guard_a = DdosConnectionGuard::try_new(ddos.clone(), "1.1.1.1");
        let guard_b = DdosConnectionGuard::try_new(ddos.clone(), "2.2.2.2");

        assert!(guard_a.is_some());
        assert!(guard_b.is_some());
    }

    #[test]
    fn banned_ip_cannot_acquire_connection() {
        let ddos = Arc::new(MemDdosProtection::new(3.0, 0.0, 3.0, 8));
        let ip = "1.2.3.4";

        // Exhaust the score to trigger a ban
        while ddos.allow_request(ip) {}

        let guard = DdosConnectionGuard::try_new(ddos.clone(), ip);
        assert!(guard.is_none(), "banned IP should not acquire a connection slot");
    }

    #[test]
    fn guard_report_bad_request_delegates() {
        let ddos = Arc::new(MemDdosProtection::new(100.0, 0.0, 5.0, 8));
        let ip = "5.6.7.8";
        let guard = DdosConnectionGuard::try_new(ddos.clone(), ip).unwrap();

        guard.report_bad_request();
        // After one bad-request penalty the score is ~5 — still under 100, so allow_request works
        assert!(guard.allow_request());
    }

    #[test]
    fn connection_count_drops_to_zero_after_all_guards_released() {
        let ddos = make_ddos();
        let ip = "9.9.9.9";

        let guard = DdosConnectionGuard::try_new(ddos.clone(), ip).unwrap();
        drop(guard);

        let mut guards = vec![];
        for _ in 0..config::SERVER_DDOS_MAX_CONNECTIONS_PER_IP {
            guards.push(DdosConnectionGuard::try_new(ddos.clone(), ip).unwrap());
        }
        assert!(DdosConnectionGuard::try_new(ddos.clone(), ip).is_none());
    }

    #[test]
    fn allow_request_returns_false_at_threshold() {
        // Use zero decay so timing doesn't affect the test
        let threshold = 5.0;
        let ddos = Arc::new(MemDdosProtection::new(threshold, 0.0, 1.0, 8));
        let ip = "3.3.3.3";

        for i in 0..4 {
            assert!(ddos.allow_request(ip), "request {} of 5 should be allowed", i + 1);
        }
        // 5th call reaches the limit
        assert!(!ddos.allow_request(ip), "request at threshold should be blocked");
        assert!(!ddos.allow_request(ip), "subsequent requests must also be blocked");
    }

    #[test]
    fn bad_request_penalty_causes_ban_faster_than_normal_requests() {
        let threshold = 20.0;
        let penalty = 10.0;
        let ddos = Arc::new(MemDdosProtection::new(threshold, 0.0, penalty, 8));
        let ip = "4.4.4.4";

        ddos.report_bad_request(ip); // score = 10
        ddos.report_bad_request(ip); // score = 20 — at threshold

        assert!(!ddos.allow_request(ip), "IP should be banned after two penalty-weight bad requests");
        assert!(DdosConnectionGuard::try_new(ddos.clone(), ip).is_none(), "banned IP must not acquire a connection");
    }

    #[test]
    fn score_is_independent_per_ip() {
        let threshold = 3.0;
        let ddos = Arc::new(MemDdosProtection::new(threshold, 0.0, 1.0, 8));
        let ip_a = "10.0.0.1";
        let ip_b = "10.0.0.2";

        while ddos.allow_request(ip_a) {}
        assert!(!ddos.allow_request(ip_a), "ip_a should be blocked");

        assert!(ddos.allow_request(ip_b), "ip_b should be unaffected by ip_a's exhaustion");
        let guard_b = DdosConnectionGuard::try_new(ddos.clone(), ip_b);
        assert!(guard_b.is_some(), "ip_b should still acquire a connection after ip_a is banned");
    }

    #[test]
    fn score_decays_over_time() {
        // High threshold, fast decay: score should drop below threshold quickly
        let ddos = Arc::new(MemDdosProtection::new(5.0, 1000.0, 1.0, 8));
        let ip = "7.7.7.7";

        // Add 4 points (just under threshold)
        for _ in 0..4 {
            ddos.allow_request(ip);
        }

        // With decay_per_second=1000, even a microsecond decays significantly
        // Next request should be allowed because the score has decayed
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(ddos.allow_request(ip), "score should have decayed, allowing the request");
    }
}
