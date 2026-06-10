//! # Shared primitives
//!
//! The foundation layer the rest of the crate builds on: core identity and hash
//! newtypes, wire-format time and duration, the Blake3/Ed25519/ChaCha20 crypto
//! stack, the multi-algorithm proof-of-work chain and its parallel search engine,
//! hierarchical time buckets, compression, serde helpers, and the
//! [`runtime_services::RuntimeServices`] bundle that lets the same protocol code
//! run on native, WASM, and deterministic test environments.

pub mod types;
pub mod config;
pub mod keys;
pub mod keys_post_quantum;
pub mod time;
pub mod tools;
pub mod server_id;
pub mod client_id;
pub mod encryption;
pub mod compression;
pub mod signing;
pub mod hashing;
pub mod pow;
pub mod pow_required_estimator;
pub mod decaying_counter;
pub mod pow_generator;
pub mod runtime_services;
pub mod anyhow_asserts;
pub mod buckets;
pub mod json;
pub mod time_provider;
pub mod bytes_gatherer;
pub use bytes_gatherer::BytesGatherer;
pub mod url_preview;
pub mod hyper_log_log;
pub mod plain_text_post;
// Server-only: cert-chain validation for HTTPS transport-ownership-proof verification.
// Depends on rustls-webpki → ring, which needs a C toolchain to build for wasm. The wasm
// client never verifies ownership proofs (that path lives in hashiverse-server-lib), so the
// module and its deps are excluded from the wasm build. See the matching target table in Cargo.toml.
#[cfg(not(target_arch = "wasm32"))]
pub mod cert_validation;
// Browser-only helper: turns Result<T, JsValue> / IndexedDB errors into anyhow errors
// with context. Used by the relocated wasm trait impls (e.g. wasm_key_locker).
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub mod with_js_context;


