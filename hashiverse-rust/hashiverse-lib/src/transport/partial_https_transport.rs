//! # Client-only HTTPS transport
//!
//! A [`crate::transport::transport::TransportFactory`] that knows how to make outbound
//! HTTPS POSTs and how to walk a
//! [`crate::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider`] for
//! seed addresses, but does **not** know how to bind and serve inbound requests. That
//! server-side half lives in `hashiverse-server-lib::FullHttpsTransportFactory` next to
//! the Let's Encrypt / rustls integration.
//!
//! Clients that only consume the network (browser WASM, CLI tools, the Python wrapper)
//! use this factory because they don't need the extra compile-time weight of the full
//! server stack.

use bytes::Bytes;
use std::sync::Arc;
use crate::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;
use crate::transport::transport::{TransportFactory, TransportServer};

/// Partial HTTPS transport factory for client-only use.
///
/// Provides `rpc()` (outbound HTTPS POST) and `get_bootstrap_addresses()`.
/// Does not support `create_server()` — use `FullHttpsTransportFactory` from
/// `hashiverse-server-lib` for that.
#[derive(Clone)]
pub struct PartialHttpsTransportFactory {
    bootstrap_provider: Arc<dyn BootstrapProvider>,
}

impl PartialHttpsTransportFactory {
    pub fn new(bootstrap_provider: Arc<dyn BootstrapProvider>) -> Self {
        Self { bootstrap_provider }
    }
}

#[async_trait::async_trait]
impl TransportFactory for PartialHttpsTransportFactory {
    async fn get_bootstrap_addresses(&self) -> Vec<String> {
        self.bootstrap_provider.get_bootstrap_addresses().await
    }

    async fn create_server(&self, _base_path: &str, _port: u16, _force_local_network: bool) -> anyhow::Result<Arc<dyn TransportServer>> {
        anyhow::bail!("HttpsTransportFactory is client-only and does not support create_server(). Use ServerHttpsTransportFactory from hashiverse-server-lib.")
    }

    async fn rpc(&self, address: &str, bytes: Bytes) -> anyhow::Result<Bytes> {
        let url = format!("https://{}/", address);
        // Build a fresh client per call to avoid stale pooled connections — the
        // servers have aggressive idle timeouts that kill kept-alive connections.
        let client = reqwest::ClientBuilder::new().danger_accept_invalid_certs(true).build()?;
        let response = client.post(url).body(bytes).send().await?;
        let response_bytes = response.bytes().await?;
        Ok(response_bytes)
    }
}
