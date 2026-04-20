//! # Transport — the seam between protocol and network
//!
//! Abstract traits for moving RPC bytes between peers, with concrete
//! implementations for tests (in-memory) and for native clients (HTTPS). Server-side
//! HTTPS binding lives in `hashiverse-server-lib`; browser HTTPS lives in
//! `hashiverse-client-wasm`. DDoS scoring and the bootstrap-address supply live here
//! too because every transport implementation needs them.

pub mod bootstrap_provider;
pub mod ddos;
#[cfg(not(target_arch = "wasm32"))]
pub mod partial_https_transport;
pub mod transport;
pub mod mem_transport;
