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
    config::{LookupIpStrategy, ResolverConfig},
    name_server::TokioConnectionProvider,
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
            ResolverConfig::cloudflare_https(),
            ResolverConfig::google_https(),
            // ResolverConfig::quad9_https(),   // seems to be too busy - many errors / timeouts
        ];

        let resolvers = resolver_configs
            .into_iter()
            .map(|resolver_config| {
                let mut builder = Resolver::builder_with_config(resolver_config, TokioConnectionProvider::default());
                builder.options_mut().validate = true;
                builder.options_mut().ip_strategy = LookupIpStrategy::Ipv4Only;
                builder.build()
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
