//! # Server-side data environment
//!
//! The server's persistence facade plus the pluggable store trait behind it.
//! The facade layers striped locking, disk-quota enforcement, and LRU decimation
//! over the store. An in-memory backend covers tests; a `fjall`-backed disk
//! backend powers the production server.

pub mod environment;
pub mod environment_store;
pub mod mem_environment_store;
pub mod disk_environment_store;
