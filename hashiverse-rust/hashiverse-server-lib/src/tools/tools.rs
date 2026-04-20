//! # Server-only helpers
//!
//! A small grab bag of utilities the server needs that don't belong in
//! `hashiverse-lib`:
//!
//! - [`get_public_ipv4`] — asks ipify.org for the node's externally-visible IPv4 so
//!   the node's advertised peer address matches reality (behind NAT / firewalls).
//!   Falls back to loopback when explicitly running in a local-only test network.
//! - [`is_ssrf_protected_ip`] — the deny-list for every server-initiated outbound
//!   fetch (URL previews, Let's Encrypt challenges, ipify). Rejects loopback,
//!   RFC-1918 private ranges, link-local, CGNAT, cloud-metadata endpoints
//!   (169.254.169.254 friends), multicast, broadcast, the unspecified address,
//!   IPv6 ULA/link-local, and IPv4-mapped IPv6. This is the primary defence against
//!   SSRF — a malicious URL in a post must not let a client coerce our server into
//!   probing its own internal network.
//! - [`spawn_ctrl_c_handler`] — wires `ctrl-c` into a `CancellationToken` so the
//!   server can shut down gracefully.

use log::warn;
use tokio_util::sync::CancellationToken;

pub async fn get_public_ipv4(force_local_network: bool) -> anyhow::Result<String> {
    match force_local_network {
        true => Ok("127.0.0.1".to_string()),
        false => {
            let client = reqwest::ClientBuilder::new().danger_accept_invalid_certs(true).build()?; // we are willing to accept invalid certs as we do application level signature checking
            let response = client.get("https://api4.ipify.org").send().await?.text().await?;
            Ok(response)
        }
    }
}

pub fn spawn_ctrl_c_handler(cancellation_token: CancellationToken) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            warn!("Ctrl+C received, cancelling...");
            cancellation_token.cancel();
        }
    });
}

/// Returns true for any IP that must never be the target of a server-initiated fetch.
/// Covers: loopback, RFC-1918 private, link-local (incl. cloud metadata 169.254.169.254),
/// CGNAT (100.64.0.0/10), multicast, unspecified, broadcast, IPv6 ULA (fc00::/7),
/// IPv6 link-local (fe80::/10), and IPv4-mapped IPv6 addresses whose embedded v4 is protected.
pub fn is_ssrf_protected_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()                               // 127.0.0.0/8
                || v4.is_private()                         // 10/8, 172.16/12, 192.168/16
                || v4.is_link_local()                      // 169.254.0.0/16 — cloud metadata
                || v4.is_multicast()                       // 224.0.0.0/4
                || v4.is_unspecified()                     // 0.0.0.0
                || v4.is_broadcast()                       // 255.255.255.255
                || (o[0] == 100 && (o[1] & 0xC0) == 64)   // CGNAT 100.64.0.0/10
        }
        std::net::IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback()                               // ::1
                || v6.is_multicast()                       // ff00::/8
                || v6.is_unspecified()                     // ::
                || (s[0] & 0xFFC0) == 0xFE80              // link-local fe80::/10
                || (s[0] & 0xFE00) == 0xFC00              // unique local fc00::/7
                // IPv4-mapped (::ffff:0:0/96) — recurse on the embedded v4 address
                || matches!(v6.to_ipv4_mapped(), Some(v4) if is_ssrf_protected_ip(std::net::IpAddr::V4(v4)))
        }
    }
}
