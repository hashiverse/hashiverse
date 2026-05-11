//! # Server-stats subtrees
//!
//! Each subsystem contributes a JSON subtree to the [`PeerStatsRequestV1`]
//! response via a free function named `<subsystem>_stats_subtree`. The
//! top-level handler in [`crate::server::handlers::dispatch`] composes the
//! full document by inserting each returned `serde_json::Value` at its own
//! root key. Subsystems don't know their own root path — placement is a
//! handler-side concern, kept here only for cheapness and consistency.

pub mod environment_stats;
pub mod kademlia_stats;
pub mod request_counts;
pub mod system_stats;

pub use environment_stats::environment_stats_subtree;
pub use kademlia_stats::kademlia_stats_subtree;
pub use request_counts::request_counts_subtree;
pub use system_stats::system_stats_subtree;
