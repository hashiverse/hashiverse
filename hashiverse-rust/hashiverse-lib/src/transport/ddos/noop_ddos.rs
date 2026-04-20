//! # No-op DDoS protection
//!
//! Implements [`crate::transport::ddos::ddos::DdosProtection`] as unconditional accept:
//! every connection is allowed, no score is tracked, no IP is ever banned. Used by
//! tests that exercise code paths downstream of the DDoS layer without also wanting
//! to reason about scoring dynamics.

use std::sync::Arc;
use crate::transport::ddos::ddos::DdosProtection;

/// No-op DDoS protection — allows all requests unconditionally.
/// Use this in general integration tests where DDoS behaviour is not under test.
pub struct NoopDdosProtection;

impl NoopDdosProtection {
    pub fn default() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl DdosProtection for NoopDdosProtection {
    fn allow_request(&self, _ip: &str) -> bool {
        true
    }

    fn report_bad_request(&self, _ip: &str) {}

    fn try_acquire_connection(&self, _ip: &str) -> bool {
        true
    }

    fn release_connection(&self, _ip: &str) {}
}
