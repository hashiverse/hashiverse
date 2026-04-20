//! # Server-only helpers
//!
//! Utilities specific to running a public server node — public-IP discovery for
//! self-advertising the peer address, SSRF-safe IP validation for any
//! server-initiated outbound fetch, and the graceful-shutdown hook.

pub mod tools;
