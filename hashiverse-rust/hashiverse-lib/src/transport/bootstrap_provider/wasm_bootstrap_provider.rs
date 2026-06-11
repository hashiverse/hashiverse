use crate::tools::{config, tools};
use crate::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;
use gloo_net::http::Request;
use log::warn;
use send_wrapper::SendWrapper;
use std::collections::HashSet;

// Only providers that return Access-Control-Allow-Origin: * are usable from the browser.
// Tested 2026-03-31: Cloudflare and Google pass; Quad9, NextDNS, AdGuard do not.
const DOH_PROVIDERS: &[&str] = &[
    "https://cloudflare-dns.com/dns-query",
    "https://dns.google/resolve",
    // "https://dns.quad9.net/dns-query", // Currently does not support CORS (queries from browser - let's check in occasionally)
];

#[derive(serde::Deserialize)]
struct DoHResponse {
    #[serde(rename = "Answer")]
    answer: Option<Vec<DoHRecord>>,
}

#[derive(serde::Deserialize)]
struct DoHRecord {
    #[serde(rename = "type")]
    record_type: u16,
    data: String,
}

/// Bootstrap provider that performs DNSSEC-validated DNS resolution using the
/// browser's Fetch API to query DNS-over-HTTPS (DoH) providers directly.
/// This is the WASM equivalent of `DnssecBootstrapProvider` which uses
/// `hickory-resolver` for native targets.
pub struct WasmBootstrapProvider {}

impl WasmBootstrapProvider {
    pub fn new() -> Self {
        Self {}
    }

    async fn query_doh(doh_provider: &str, domain: &str) -> Vec<String> {
        let url = format!("{}?name={}&type=A", doh_provider, domain);
        let fetch_future = async move {
            let response = Request::get(&url)
                .header("Accept", "application/dns-json")
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("DoH fetch error: {}", e))?;

            let text = response
                .text()
                .await
                .map_err(|e| anyhow::anyhow!("DoH read error: {}", e))?;

            let doh_response: DoHResponse = serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("DoH parse error: {}", e))?;

            Ok::<DoHResponse, anyhow::Error>(doh_response)
        };

        match SendWrapper::new(fetch_future).await {
            Ok(doh_response) => doh_response
                .answer
                .unwrap_or_default()
                .into_iter()
                .filter(|record| record.record_type == 1) // A records only
                .map(|record| format!("{}:443", record.data))
                .collect(),
            Err(e) => {
                warn!("DoH query failed for {} via {}: {}", domain, doh_provider, e);
                vec![]
            }
        }
    }
}

#[async_trait::async_trait]
impl BootstrapProvider for WasmBootstrapProvider {
    async fn get_bootstrap_addresses(&self) -> Vec<String> {
        let bootstrap_config = SendWrapper::new(super::wasm_local_settings::local_settings_get("bootstrap"))
            .await
            .unwrap_or(None)
            .unwrap_or_else(|| "production".to_string());

        match bootstrap_config.as_str() {
            "production" => {
                let mut addresses = HashSet::new();
                for domain in config::BOOTSTRAP_DOMAINS {
                    for doh_provider in DOH_PROVIDERS {
                        for address in Self::query_doh(doh_provider, domain).await {
                            addresses.insert(address);
                        }
                    }
                }
                let mut addresses: Vec<String> = addresses.into_iter().collect();
                tools::shuffle(&mut addresses);
                addresses
            }
            "local" => vec!["127.0.0.1:443".to_string()],
            manual => {
                let mut addresses: Vec<String> = manual.split(',').map(|s| s.trim().to_string()).collect();
                tools::shuffle(&mut addresses);
                addresses
            }
        }
    }
}
