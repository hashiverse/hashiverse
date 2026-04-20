//! # Kernel-level DDoS protection backed by Linux `ipset` + `iptables`
//!
//! The production implementation of
//! [`hashiverse_lib::transport::ddos::ddos::DdosProtection`] used by the real server.
//! Layered on top of the in-RAM scoring logic from `hashiverse-lib`:
//!
//! 1. Per-IP `DdosScore` accumulates penalties for bad requests (e.g. invalid PoW,
//!    malformed packets) with linear time decay from
//!    [`hashiverse_lib::tools::config::SERVER_DDOS_DECAY_PER_SECOND`].
//! 2. When a score crosses
//!    [`hashiverse_lib::tools::config::SERVER_DDOS_SCORE_THRESHOLD`], the IP is
//!    shelled out to `ipset add` against the set named by
//!    [`hashiverse_lib::tools::config::SERVER_DDOS_IPSET_SET_NAME`], which an
//!    operator-configured `iptables` rule then drops at the kernel.
//! 3. A short (≥10 s) throttle around the `ipset` call prevents hammering the
//!    subprocess in edge cases.
//!
//! Per-IP concurrent-connection caps are enforced via a `HashMap<String, usize>`
//! guarded by a `parking_lot::Mutex`, cutting off a single IP from monopolising all
//! [`hashiverse_lib::tools::config::SERVER_DDOS_MAX_CONNECTIONS_PER_IP`] slots. The
//! `NET_ADMIN` capability is required on the container — see the operator docs.

use hashiverse_lib::transport::ddos::ddos::{DdosProtection, DdosScore};
use parking_lot::Mutex;
use log::{info, warn};
use moka::sync::Cache;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

/// Production DDoS protection backed by Linux `ipset`.
///
/// Per-IP scores use linear time decay: each `allow_request` adds 1.0 point,
/// each `report_bad_request` adds `bad_request_penalty` points, and the score
/// drains at `decay_per_second` points/second.  This means sustained low-rate
/// traffic stabilises below the threshold while bursts trigger quickly.
///
/// When a score first crosses `score_threshold`, the IP is added to the named
/// ipset via `ipset add <set_name> <ip> --exist`.  A second 10-second moka cache
/// (`ipset_throttle`) prevents hammering the ipset command.
///
/// `try_acquire_connection` additionally enforces a per-IP connection cap
/// (`max_connections_per_ip`).
pub struct IpsetDdosProtection {
    set_name: String,
    score_threshold: f64,
    decay_per_second: f64,
    bad_request_penalty: f64,
    max_connections_per_ip: usize,
    scores: Cache<String, Arc<Mutex<DdosScore>>>,
    ipset_throttle: Cache<String, ()>,
    connections: Mutex<HashMap<String, usize>>,
}

impl IpsetDdosProtection {
    pub fn new(set_name: impl Into<String>, score_threshold: f64, decay_per_second: f64, bad_request_penalty: f64, max_connections_per_ip: usize) -> Self {
        let set_name = set_name.into();

        // Idle expiry: time for a maxed-out score to fully decay, with 2x margin
        let idle_secs = if decay_per_second > 0.0 {
            (score_threshold / decay_per_second * 2.0).ceil() as u64
        } else {
            3600
        };

        let result = Command::new("ipset").args(["create", &set_name, "hash:ip", "timeout", &idle_secs.to_string(), "--exist"]).status();
        match result {
            Ok(status) if status.success() => info!("DDoS: ipset set '{}' ready", set_name),
            Ok(status) => warn!("DDoS: ipset create '{}' failed with status {}", set_name, status),
            Err(e) => warn!("DDoS: failed to run ipset create '{}': {}", set_name, e),
        }

        // Set up our iptables rules for both docker (FORWARD) and metal (INPUT) servers.
        for chain in ["INPUT", "FORWARD"] {
            // -D silently fails if the rule doesn't exist, so delete then re-insert is safe
            let _ = Command::new("iptables").args(["-D", chain, "-m", "set", "--match-set", &set_name, "src", "-j", "DROP"]).status();
            let result = Command::new("iptables").args(["-I", chain, "-m", "set", "--match-set", &set_name, "src", "-j", "DROP"]).status();
            match result {
                Ok(status) if status.success() => info!("DDoS: iptables {} rule for '{}' installed", chain, set_name),
                Ok(status) => warn!("DDoS: iptables -I {} for '{}' failed with status {}", chain, set_name, status),
                Err(e) => warn!("DDoS: failed to run iptables {} for '{}': {}", chain, set_name, e),
            }
        }

        Self {
            set_name,
            score_threshold,
            decay_per_second,
            bad_request_penalty,
            max_connections_per_ip,
            scores: Cache::builder().time_to_idle(Duration::from_secs(idle_secs)).build(),
            ipset_throttle: Cache::builder().time_to_live(Duration::from_secs(10)).build(),
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

    fn maybe_call_ipset(&self, ip: &str) {
        // Have we already spawned this IP address?
        {
            if self.ipset_throttle.contains_key(ip) {
                return;
            }
            self.ipset_throttle.insert(ip.to_string(), ());
        }

        // Add to ipset asynchronously
        {
            let set_name = self.set_name.clone();
            let ip = ip.to_string();

            tokio::spawn(async move {
                info!("Banning DDoS ip: {}", ip);
                match tokio::process::Command::new("ipset").args(["add", &set_name, &ip, "--exist"]).status().await {
                    Ok(status) if status.success() => info!("DDoS: banned {} via ipset set '{}'", ip, set_name),
                    Ok(status) => warn!("DDoS: ipset add {} failed with status {}", ip, status),
                    Err(e) => warn!("DDoS: failed to run ipset for {}: {}", ip, e),
                }
            });
        }
    }
}

impl DdosProtection for IpsetDdosProtection {
    fn allow_request(&self, ip: &str) -> bool {
        let score = self.increment_score(ip, 1.0);
        if score >= self.score_threshold {
            self.maybe_call_ipset(ip);
            false
        }
        else {
            true
        }
    }

    fn report_bad_request(&self, ip: &str) {
        let score = self.increment_score(ip, self.bad_request_penalty);
        if score >= self.score_threshold {
            self.maybe_call_ipset(ip);
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
