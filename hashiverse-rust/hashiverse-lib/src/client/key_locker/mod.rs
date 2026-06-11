//! # Per-account signing identity with pluggable persistence
//!
//! The `KeyLocker` trait guards one account's private signing material; the
//! `KeyLockerManager` trait manages the set of accounts on a device (list, create,
//! switch, delete). Backed in memory for tests and on disk for native deployments;
//! the WASM crate supplies an IndexedDB variant.

pub mod key_locker;
pub mod mem_key_locker;
#[cfg(not(target_arch = "wasm32"))]
pub mod disk_key_locker;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub mod wasm_key_locker;
