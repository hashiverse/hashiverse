//! # Hand-configured bootstrap provider
//!
//! Trivial implementation of
//! [`crate::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider`] that
//! just hands back a `Vec<String>` provided at construction time. Used by tests
//! (the integration-test harness feeds every client the addresses of the in-memory
//! servers it spawned) and by private deployments that prefer to pin seed nodes in
//! config rather than rely on public DNSSEC.

use crate::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;
use std::sync::Arc;

pub struct ManualBootstrapProvider {
    addresses: Vec<String>,
}

impl ManualBootstrapProvider {
    #[allow(clippy::should_implement_trait)] // wraps Arc<Self>, can't satisfy the Default trait
    pub fn default() -> Arc<Self> {
        Self::new(vec![])
    }
    pub fn new(addresses: Vec<String>) -> Arc<Self> {
        Arc::new(Self { addresses })
    }
    pub fn new_tcp_localhost() -> Arc<Self> {
        Arc::new(Self { addresses: vec!["127.0.0.1:443".to_string()] })
    }
    pub fn new_mem_multiple() -> Arc<Self> {
        Arc::new(Self {
            addresses: vec![
                "443".to_string(),
                "10000".to_string(),
                "20000".to_string(),
                "20001".to_string(),
                "20002".to_string(),
                "20003".to_string(),
                "20004".to_string(),
                "20005".to_string(),
                "20006".to_string(),
                "20007".to_string(),
                "20008".to_string(),
                "20009".to_string(),
                "20010".to_string(),
                "20011".to_string(),
                "20012".to_string(),
                "20013".to_string(),
                "20014".to_string(),
                "20015".to_string(),
                "20016".to_string(),
                "20017".to_string(),
                "20018".to_string(),
                "20019".to_string(),
            ],
        })
    }
}

#[async_trait::async_trait]
impl BootstrapProvider for ManualBootstrapProvider {
    async fn get_bootstrap_addresses(&self) -> Vec<String> {
        self.addresses.clone()
    }
}
