//! # Offline X.509 chain validation for `AnnounceV2` proofs
//!
//! [`is_cert_valid`] decides whether a leaf+intermediate chain (presented inline by
//! an announcing peer) is a real, current, public-CA-issued certificate for the
//! announced IP address. The whole point is to gate Kademlia admission *without
//! pinging the announcer*: a peer that doesn't control its announced IP can't
//! complete ACME's HTTP-01 / TLS-ALPN-01 challenge, so it can't put a chain in its
//! announce that satisfies all three checks here.
//!
//! Checks (all offline, no network):
//!
//! 1. **Path validation** against the bundled Mozilla NSS root store from
//!    `webpki-roots`. Enforces server-auth EKU and the cert's own validity window
//!    against the supplied `now`.
//! 2. **SAN match** against the announced IP — per [project invariant], hashiverse
//!    servers identify by raw IP, so we expect an IP SAN (not DNS).
//!
//! Out of scope: OCSP/CRL revocation (would need network), liveness of the listener
//! (the existing prune-on-RPC-failure path catches that).
//!
//! The function is wrapped by `HttpsTransportOwnershipProof::prove` in
//! `hashiverse-server-lib`; tests live alongside in this module.

use crate::tools::time::TimeMillis;
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;
use webpki::EndEntityCert;

/// Offline-validate a TLS chain against the bundled public-CA roots and the
/// announced IP address. Returns `true` only if every check passes.
pub fn is_cert_valid(chain_der: &[Vec<u8>], announced_address: &str, now: TimeMillis) -> bool {
    let Some((leaf_der_vec, intermediate_der_vecs)) = chain_der.split_first() else {
        return false;
    };

    let leaf_der: CertificateDer<'_> = CertificateDer::from(leaf_der_vec.as_slice());
    let leaf_cert: EndEntityCert<'_> = match EndEntityCert::try_from(&leaf_der) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let intermediate_ders: Vec<CertificateDer<'_>> = intermediate_der_vecs.iter().map(|d| CertificateDer::from(d.as_slice())).collect();

    let now_unix: UnixTime = match time_millis_to_unix_time(now) {
        Some(t) => t,
        None => return false,
    };

    let trust_anchors: &[rustls_pki_types::TrustAnchor<'_>] = webpki_roots::TLS_SERVER_ROOTS;

    if leaf_cert
        .verify_for_usage(
            webpki::ALL_VERIFICATION_ALGS,
            trust_anchors,
            &intermediate_ders,
            now_unix,
            webpki::KeyUsage::server_auth(),
            None,
            None,
        )
        .is_err()
    {
        return false;
    }

    let Some(ip_str) = strip_port_from_address(announced_address) else {
        return false;
    };

    let server_name: ServerName<'_> = match build_ip_server_name(&ip_str) {
        Some(name) => name,
        None => return false,
    };

    leaf_cert.verify_is_valid_for_subject_name(&server_name).is_ok()
}

fn time_millis_to_unix_time(time_millis: TimeMillis) -> Option<UnixTime> {
    let millis: i64 = time_millis.0;
    if millis < 0 {
        return None;
    }
    Some(UnixTime::since_unix_epoch(Duration::from_millis(millis as u64)))
}

/// Split a `<host>:<port>` or `[<ipv6>]:<port>` address into just the host part. Bare hosts
/// (no port) are returned unchanged. Returns `None` for empty input, unbalanced IPv6
/// brackets, junk after the bracketed host, or a non-numeric port — so a caller can't
/// quietly normalise a malformed announce into a plausible-looking host string.
fn strip_port_from_address(announced_address: &str) -> Option<String> {
    let trimmed: &str = announced_address.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(rest_after_open_bracket) = trimmed.strip_prefix('[') {
        let close_bracket_pos: usize = rest_after_open_bracket.find(']')?;
        let ipv6_body: &str = &rest_after_open_bracket[..close_bracket_pos];
        let suffix: &str = &rest_after_open_bracket[close_bracket_pos + 1..];
        // After the closing `]` the only legal suffixes are nothing (`[::1]`) or `:<digits>+`
        // (`[::1]:443`). Anything else (`[::1]garbage`, `[::1]:notaport`, `[::1]:`) is
        // malformed and must be rejected — otherwise the bracket body alone would be returned
        // as if it were a clean host.
        if !suffix.is_empty() {
            let port_str: &str = suffix.strip_prefix(':')?;
            if port_str.is_empty() || !port_str.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
        }
        return Some(ipv6_body.to_string());
    }

    match trimmed.rsplit_once(':') {
        Some((host_part, port_part)) if !host_part.contains(':') => {
            // Plain `host:port` form. The port must be present and all digits; `1.2.3.4:`
            // and `1.2.3.4:notaport` are malformed.
            if port_part.is_empty() || !port_part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(host_part.to_string())
        }
        // Either no colon at all (bare IPv4 host) or multiple colons with no port suffix
        // (bare IPv6 host). Either way, hand the trimmed input back; the downstream
        // `IpAddr::from_str` in `build_ip_server_name` is the final validator.
        _ => Some(trimmed.to_string()),
    }
}

fn build_ip_server_name(host_str: &str) -> Option<ServerName<'static>> {
    let ip_addr: IpAddr = IpAddr::from_str(host_str).ok()?;
    let ip_addr_typed: rustls_pki_types::IpAddr = match ip_addr {
        IpAddr::V4(v4) => rustls_pki_types::IpAddr::V4(rustls_pki_types::Ipv4Addr::from(v4)),
        IpAddr::V6(v6) => rustls_pki_types::IpAddr::V6(rustls_pki_types::Ipv6Addr::from(v6)),
    };
    Some(ServerName::IpAddress(ip_addr_typed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_chain_is_invalid() {
        let now: TimeMillis = TimeMillis(1_700_000_000_000);
        assert!(!is_cert_valid(&[], "127.0.0.1:8080", now));
    }

    #[test]
    fn garbage_der_in_leaf_is_invalid() {
        let chain_der: Vec<Vec<u8>> = vec![vec![0xff, 0xfe, 0xfd, 0xfc]];
        let now: TimeMillis = TimeMillis(1_700_000_000_000);
        assert!(!is_cert_valid(&chain_der, "127.0.0.1:8080", now));
    }

    #[test]
    fn empty_address_is_invalid() {
        let chain_der: Vec<Vec<u8>> = vec![vec![0xff]];
        let now: TimeMillis = TimeMillis(1_700_000_000_000);
        assert!(!is_cert_valid(&chain_der, "", now));
    }

    #[test]
    fn negative_time_is_invalid() {
        let chain_der: Vec<Vec<u8>> = vec![vec![0xff]];
        let now: TimeMillis = TimeMillis(-1);
        assert!(!is_cert_valid(&chain_der, "127.0.0.1:8080", now));
    }

    #[test]
    fn strip_port_ipv4_with_port() {
        assert_eq!(strip_port_from_address("1.2.3.4:8080"), Some("1.2.3.4".to_string()));
    }

    #[test]
    fn strip_port_ipv4_without_port() {
        assert_eq!(strip_port_from_address("1.2.3.4"), Some("1.2.3.4".to_string()));
    }

    #[test]
    fn strip_port_ipv6_bracketed_with_port() {
        assert_eq!(strip_port_from_address("[::1]:8080"), Some("::1".to_string()));
    }

    #[test]
    fn strip_port_ipv6_bracketed_without_port() {
        assert_eq!(strip_port_from_address("[2001:db8::1]"), Some("2001:db8::1".to_string()));
    }

    #[test]
    fn strip_port_bare_ipv6_no_port() {
        assert_eq!(strip_port_from_address("2001:db8::1"), Some("2001:db8::1".to_string()));
    }

    #[test]
    fn strip_port_empty() {
        assert_eq!(strip_port_from_address(""), None);
    }

    #[test]
    fn strip_port_whitespace_trimmed() {
        assert_eq!(strip_port_from_address("  1.2.3.4:8080  "), Some("1.2.3.4".to_string()));
    }

    #[test]
    fn strip_port_rejects_non_numeric_port_on_ipv4() {
        assert_eq!(strip_port_from_address("1.2.3.4:notaport"), None);
    }

    #[test]
    fn strip_port_rejects_empty_port_on_ipv4() {
        assert_eq!(strip_port_from_address("1.2.3.4:"), None);
    }

    #[test]
    fn strip_port_rejects_bracketed_with_junk_suffix() {
        assert_eq!(strip_port_from_address("[::1]garbage"), None);
    }

    #[test]
    fn strip_port_rejects_bracketed_with_non_numeric_port() {
        assert_eq!(strip_port_from_address("[::1]:notaport"), None);
    }

    #[test]
    fn strip_port_rejects_bracketed_with_empty_port() {
        assert_eq!(strip_port_from_address("[::1]:"), None);
    }

    #[test]
    fn strip_port_rejects_unbalanced_bracket() {
        assert_eq!(strip_port_from_address("[::1"), None);
    }

    #[test]
    fn build_server_name_for_ipv4() {
        let server_name: Option<ServerName<'static>> = build_ip_server_name("1.2.3.4");
        assert!(server_name.is_some());
        assert!(matches!(server_name.unwrap(), ServerName::IpAddress(_)));
    }

    #[test]
    fn build_server_name_for_ipv6() {
        let server_name: Option<ServerName<'static>> = build_ip_server_name("::1");
        assert!(server_name.is_some());
        assert!(matches!(server_name.unwrap(), ServerName::IpAddress(_)));
    }

    #[test]
    fn build_server_name_rejects_dns() {
        let server_name: Option<ServerName<'static>> = build_ip_server_name("example.com");
        assert!(server_name.is_none());
    }

    /// Per project preference ([[feedback_fuzzer_choice]]) bolero is the fuzzer of choice.
    /// `is_cert_valid` is exposed to bytes that arrive over the wire from untrusted peers,
    /// so the invariant under test is "never panics, always returns a bool". In `cargo
    /// nextest run` this exercises a small deterministic sample (bolero's default property-
    /// test mode); under `cargo bolero test` it runs as a coverage-guided fuzzer.
    #[test]
    fn fuzz_is_cert_valid_never_panics() {
        bolero::check!()
            .with_type::<(Vec<Vec<u8>>, String, i64)>()
            .for_each(|(chain_der, announced_address, now_millis)| {
                let _ = is_cert_valid(chain_der, announced_address, TimeMillis(*now_millis));
            });
    }

    #[test]
    fn fuzz_strip_port_from_address_never_panics() {
        bolero::check!()
            .with_type::<String>()
            .for_each(|input| {
                let _ = strip_port_from_address(input);
            });
    }
}
