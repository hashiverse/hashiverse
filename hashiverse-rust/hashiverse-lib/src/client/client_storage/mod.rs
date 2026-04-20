//! # Pluggable client-side key-value store
//!
//! The `ClientStorage` trait and its bundled backends — an in-memory store for
//! tests and a SQLite store for native deployments. The WASM client crate
//! supplies an IndexedDB implementation on top of the same trait.

pub mod client_storage;
pub mod mem_client_storage;
#[cfg(not(target_arch = "wasm32"))]
pub mod sqlite_client_storage;
