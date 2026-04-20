//! # Server-side Kademlia routing table
//!
//! The DHT data structure the server uses to answer `BootstrapV1` and
//! `AnnounceV1` queries and to pick candidate peers for post-bundle lookups —
//! per-bit buckets, freshness-based eviction, and XOR-distance iteration.

pub mod kademlia;
