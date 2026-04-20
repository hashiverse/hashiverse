//! # Per-IP DDoS scoring and connection-slot accounting
//!
//! The `DdosProtection` trait plus its connection guard, time-decayed scoring,
//! and ban hooks. An in-memory implementation covers tests and accounting-only
//! deployments; a no-op implementation is available for test scenarios downstream
//! of the DDoS layer. The kernel-level ipset-backed production implementation
//! lives in `hashiverse-server-lib`.

pub mod ddos;
pub mod noop_ddos;
#[cfg(not(target_arch = "wasm32"))]
pub mod mem_ddos;
