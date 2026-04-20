//! # Seed peer address supply
//!
//! The `BootstrapProvider` trait hands a fresh node its first list of peer
//! addresses so it can break out of Kademlia's chicken-and-egg problem. A
//! DNSSEC-validated provider is used in production; a manual list-based provider
//! covers tests and private deployments.

pub mod bootstrap_provider;
#[cfg(not(target_arch = "wasm32"))]
pub mod dnssec_bootstrap_provider;
pub mod manual_bootstrap_provider;
