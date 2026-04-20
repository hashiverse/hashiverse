//! # Local peer set and XOR-distance walk
//!
//! The client's working memory of the network — a Kademlia-style deduplicated
//! list of peers persisted across restarts, plus the stateful cursor that walks
//! those peers in order of closeness to a target id. Every DHT operation the
//! client performs ultimately goes through this pair.

pub mod peer_tracker;
pub mod peer_iterator;
