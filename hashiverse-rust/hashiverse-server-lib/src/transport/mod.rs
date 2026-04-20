//! # Server-side transport implementations
//!
//! The inbound-accepting half of the transport stack: a full HTTPS transport with
//! TLS and automatic Let's Encrypt certificate rotation, a plain TCP transport
//! for testing and private LANs, and the kernel-level ipset-backed DDoS
//! protection that both can plug into.

pub mod ddos;
pub mod tcp_transport;
pub mod full_https_transport;
pub mod https_transport_cert_refresher;

#[cfg(test)]
mod transport_tests;
