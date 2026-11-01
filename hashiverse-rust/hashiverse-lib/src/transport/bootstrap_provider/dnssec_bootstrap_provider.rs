//! # DNSSEC-validated bootstrap provider
//!
//! Resolves the domains in [`crate::tools::config::BOOTSTRAP_DOMAINS`] via a
//! `hickory-resolver` configured to require DNSSEC authentication. Successfully-resolved
//! addresses are returned as the seed peer list.
//!
//! DNSSEC matters here because bootstrapping is the one moment a client hasn't yet
//! authenticated anyone on the network — a hijacked A record from an unauthenticated
//! resolver could redirect every new node to an attacker-controlled sybil cluster.
//! Requiring DNSSEC-validated lookups pushes that attack surface back to the root
//! trust anchor.

use crate::tools::{config, tools};
use crate::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;
use hickory_resolver::{
    config::{LookupIpStrategy, ResolverConfig, CLOUDFLARE, GOOGLE},
    net::runtime::TokioRuntimeProvider,
    Resolver, TokioResolver,
};
use log::warn;
use std::collections::HashSet;

pub struct DnssecBootstrapProvider {
    resolvers: Vec<TokioResolver>,
}

impl Default for DnssecBootstrapProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DnssecBootstrapProvider {
    pub fn new() -> Self {
        let resolver_configs = [
            ResolverConfig::https(&CLOUDFLARE),
            ResolverConfig::https(&GOOGLE),
            // ResolverConfig::https(&QUAD9),   // seems to be too busy - many errors / timeouts
        ];

        let resolvers = resolver_configs
            .into_iter()
            .filter_map(|resolver_config| {
                let mut builder = Resolver::builder_with_config(resolver_config, TokioRuntimeProvider::default());
                builder.options_mut().validate = true;
                builder.options_mut().ip_strategy = LookupIpStrategy::Ipv4Only;
                match builder.build() {
                    Ok(resolver) => Some(resolver),
                    Err(e) => {
                        warn!("DNSSEC bootstrap resolver construction failed: {}", e);
                        None
                    }
                }
            })
            .collect();

        Self { resolvers }
    }
}

#[async_trait::async_trait]
impl BootstrapProvider for DnssecBootstrapProvider {
    async fn get_bootstrap_addresses(&self) -> Vec<String> {
        let mut addresses = HashSet::new();
        for domain in config::BOOTSTRAP_DOMAINS {
            for resolver in &self.resolvers {
                match resolver.lookup_ip(*domain).await {
                    Ok(response) => {
                        for ip in response.iter() {
                            addresses.insert(format!("{}:443", ip));
                        }
                    }
                    Err(e) => {
                        warn!("DNSSEC bootstrap lookup failed for {}: {}", domain, e);
                    }
                }
            }
        }
        let mut addresses: Vec<String> = addresses.drain().collect();
        tools::shuffle(&mut addresses);
        addresses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hits the live Cloudflare/Google DoH resolvers, so it is opt-in:
    /// `cargo nextest run --cargo-profile profiling -p hashiverse-lib dnssec_bootstrap_resolves_live_domains --run-ignored only`
    #[tokio::test]
    #[ignore]
    async fn dnssec_bootstrap_resolves_live_domains() {
        let dnssec_bootstrap_provider = DnssecBootstrapProvider::new();
        assert_eq!(dnssec_bootstrap_provider.resolvers.len(), 2, "every configured resolver should build");
        let bootstrap_addresses = dnssec_bootstrap_provider.get_bootstrap_addresses().await;
        assert!(!bootstrap_addresses.is_empty(), "expected at least one DNSSEC-validated bootstrap address");
        assert!(bootstrap_addresses.iter().all(|bootstrap_address| bootstrap_address.ends_with(":443")));
    }
}
