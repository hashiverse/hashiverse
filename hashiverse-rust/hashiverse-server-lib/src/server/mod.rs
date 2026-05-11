//! # Server orchestration, routing, handlers, and caches
//!
//! The top-level `HashiverseServer` struct that owns one node's identity, DHT,
//! caches, and inbound-request loop, together with the supporting pieces: command
//! line parsing, operator passphrase resolution, the in-RAM bundle/feedback read
//! caches, the Kademlia routing table, and the handler dispatch that routes each
//! decoded RPC to its per-op implementation.

pub mod kademlia;
pub mod handlers;
pub mod hashiverse_server;
pub mod passphrase;
pub mod post_bundle_caching_shared;
pub mod post_bundle_caching;
pub mod post_bundle_feedback_caching;
pub mod args;
pub mod stats;
