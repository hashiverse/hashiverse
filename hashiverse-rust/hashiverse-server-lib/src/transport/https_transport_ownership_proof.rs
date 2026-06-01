//! # HTTPS-transport ownership proof
//!
//! Produces and verifies the proof bytes carried in an `AnnounceV2` from / to an
//! HTTPS server. The bytes are a postcard-encoded [`HttpsAcmeProof`] containing
//! the announcer's currently-serving ACME-issued TLS chain — nothing else.
//!
//! Verification is purely offline:
//! 1. postcard decode (wrong-transport / corrupt bytes fail here),
//! 2. [`hashiverse_lib::tools::cert_validation::is_cert_valid`] — X.509 path
//!    validation against the bundled Mozilla NSS roots plus IP-SAN match.
//!
//! ### What V2 guarantees, and what it doesn't
//!
//! V2's only claim is "someone managed to issue a public-CA cert for the announced
//! IP". That defeats the original target: a server that fails to acquire a cert
//! (no control of port 80/443) can't construct a valid proof and never enters
//! Kademlia. It does **not** defeat an attacker who borrows a stranger's chain —
//! the chain is public, served on every TLS handshake. But any RPC the receiver
//! then sends to the announced IP reaches the *real* server with the *real*
//! identity at that IP, and the response-side identity mismatch trips the existing
//! prune path (`HashiverseServer::add_potential_peer_to_kademlia` + RPC-failure
//! pruning in `maintain_kademlia`). So the chain-borrowing attack is bounded: an
//! attacker can briefly inject a wrong identity for an IP, and the network
//! self-corrects on the next round-trip.

use bytes::Bytes;
use hashiverse_lib::protocol::peer::Peer;
use hashiverse_lib::tools::cert_validation::is_cert_valid;
use hashiverse_lib::tools::time::TimeMillis;
use hashiverse_lib::transport::transport_ownership_proof::TransportOwnershipProof;
use parking_lot::{Mutex, RwLock};
use rustls::sign::CertifiedKey;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Wire-encoded HTTPS ownership proof — the payload bytes inside an `AnnounceV2`
/// when the announcer is an HTTPS-transport server. Serialised with postcard so
/// other transports' verifiers fail to deserialise it and reject the announce,
/// which is the cross-transport-mismatch path.
#[derive(Serialize, Deserialize)]
struct HttpsAcmeProof {
    /// DER-encoded leaf first, then intermediate(s). Exactly what
    /// `CertifiedKey::cert` carries server-side.
    chain_der: Vec<Vec<u8>>,
}

/// Cached payload: the encoded proof for a given `Arc<CertifiedKey>` (compared by
/// pointer equality — the cert refresher swaps in a new `Arc` on every reload).
/// Reused on every announce tick until ACME rotates the cert, which is hours/days
/// apart. The encoded bytes don't depend on the announcing peer (the chain is the
/// only signed-by-nothing payload), so the cache key is just the cert pointer.
struct CachedProofPayload {
    certified_key: Arc<CertifiedKey>,
    payload_bytes: Bytes,
}

/// Server-side proof object for the HTTPS transport. Holds one handle: a clone of
/// `HttpsTransportCertRefresher::base_cert`, so every call snapshots the freshest
/// loaded chain without coordinating with the cert refresher's own loop.
///
/// One instance per server, constructed in `FullHttpsTransportServer::new()` and
/// shared via `Arc` from `get_transport_ownership_proof()` — so the cache survives
/// across announce ticks.
pub struct HttpsTransportOwnershipProof {
    base_cert: Arc<RwLock<Option<Arc<CertifiedKey>>>>,
    cached_payload: Mutex<Option<CachedProofPayload>>,
}

impl HttpsTransportOwnershipProof {
    pub fn new(base_cert: Arc<RwLock<Option<Arc<CertifiedKey>>>>) -> Self {
        Self { base_cert, cached_payload: Mutex::new(None) }
    }
}

impl TransportOwnershipProof for HttpsTransportOwnershipProof {
    fn make_ownership_proof_payload(&self) -> Option<Bytes> {
        // Snapshot the current cert under the read lock and release it before doing
        // anything expensive. The refresher writes a brand-new Arc<CertifiedKey> on each
        // reload, so cache identity is settled by Arc::ptr_eq against this snapshot.
        let current_certified_key: Arc<CertifiedKey> = {
            let base_cert_guard = self.base_cert.read();
            base_cert_guard.as_ref()?.clone()
        };

        // Fast path: cache still matches the current cert → reuse the encoded payload
        // (a Bytes clone is just a pointer bump). 99%+ of calls hit this branch since
        // announces fire every ~30s and ACME rotates the cert every few days.
        {
            let cache_guard = self.cached_payload.lock();
            if let Some(cached) = cache_guard.as_ref() {
                if Arc::ptr_eq(&cached.certified_key, &current_certified_key) {
                    return Some(cached.payload_bytes.clone());
                }
            }
        }

        // Slow path: cert rotated (or first call). Re-encode.
        let chain_der: Vec<Vec<u8>> = current_certified_key.cert.iter().map(|der| der.to_vec()).collect();
        if chain_der.is_empty() {
            return None;
        }

        let proof: HttpsAcmeProof = HttpsAcmeProof { chain_der };
        let encoded: Vec<u8> = postcard::to_allocvec(&proof).ok()?;
        let payload_bytes: Bytes = Bytes::from(encoded);

        *self.cached_payload.lock() = Some(CachedProofPayload {
            certified_key: current_certified_key,
            payload_bytes: payload_bytes.clone(),
        });

        Some(payload_bytes)
    }

    fn prove(&self, peer: &Peer, proof_payload: &[u8], now: TimeMillis) -> bool {
        let proof: HttpsAcmeProof = match postcard::from_bytes(proof_payload) {
            Ok(p) => p,
            Err(_) => return false,
        };

        is_cert_valid(&proof.chain_der, &peer.address, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hashiverse_lib::tools::types::Id;

    fn peer_at(address: &str) -> Peer {
        let mut peer: Peer = Peer::zero();
        peer.address = address.to_string();
        peer.id = Id([42u8; 32]);
        peer
    }

    fn empty_base_cert_lock() -> Arc<RwLock<Option<Arc<CertifiedKey>>>> {
        Arc::new(RwLock::new(None))
    }

    #[test]
    fn make_returns_none_when_no_cert_loaded_yet() {
        let proof: HttpsTransportOwnershipProof = HttpsTransportOwnershipProof::new(empty_base_cert_lock());
        assert!(proof.make_ownership_proof_payload().is_none());
    }

    #[test]
    fn prove_rejects_garbage_bytes() {
        let proof: HttpsTransportOwnershipProof = HttpsTransportOwnershipProof::new(empty_base_cert_lock());
        let peer: Peer = peer_at("1.2.3.4:443");
        let now: TimeMillis = TimeMillis(1_700_000_000_000);
        assert!(!proof.prove(&peer, &[0xff, 0xfe, 0xfd], now));
    }

    #[test]
    fn prove_rejects_empty_bytes() {
        let proof: HttpsTransportOwnershipProof = HttpsTransportOwnershipProof::new(empty_base_cert_lock());
        let peer: Peer = peer_at("1.2.3.4:443");
        let now: TimeMillis = TimeMillis(1_700_000_000_000);
        // Empty bytes = the mem-transport marker. The HTTPS verifier must reject it
        // (cross-transport mismatch): postcard either decodes an empty slice into
        // HttpsAcmeProof { chain_der: vec![] }, in which case is_cert_valid then fails
        // on the empty chain — or postcard rejects the bytes outright. Either way: false.
        assert!(!proof.prove(&peer, &[], now));
    }

    /// Build a `CertifiedKey` carrying placeholder DER bytes. The payload won't survive
    /// real X.509 verification (that's `is_cert_valid`'s job, exercised separately), but
    /// it's sufficient to drive cache-hit / cache-miss bookkeeping in the proof object.
    fn placeholder_certified_key(der_bytes: Vec<u8>) -> Arc<CertifiedKey> {
        use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert_chain: Vec<CertificateDer<'static>> = vec![CertificateDer::from(der_bytes)];
        // rcgen ships a key generator; we only need a structurally-valid PKCS#8 envelope
        // so rustls' any_supported_type accepts it. The cert above is bogus DER so the
        // CertifiedKey can never sign anything real — fine, the cache tests don't sign.
        let key_pair: rcgen::KeyPair = rcgen::KeyPair::generate().expect("rcgen key gen never fails on supported algorithms");
        let pkcs8: Vec<u8> = key_pair.serialize_der();
        let key_der: PrivateKeyDer<'static> = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8));
        let signing: Arc<dyn rustls::sign::SigningKey> = rustls::crypto::ring::sign::any_supported_type(&key_der).expect("rcgen key is a supported type");
        Arc::new(CertifiedKey { cert: cert_chain, key: signing, ocsp: None })
    }

    #[test]
    fn cache_hit_returns_identical_bytes_when_cert_unchanged() {
        let base_cert: Arc<RwLock<Option<Arc<CertifiedKey>>>> = Arc::new(RwLock::new(Some(placeholder_certified_key(vec![0x30, 0x82, 0x01, 0x00, 0xAA]))));
        let proof: HttpsTransportOwnershipProof = HttpsTransportOwnershipProof::new(base_cert);

        let first_bytes: Bytes = proof.make_ownership_proof_payload().expect("placeholder cert is non-empty");
        let second_bytes: Bytes = proof.make_ownership_proof_payload().expect("placeholder cert is non-empty");

        // Pointer equality on the underlying buffer: the cache returned the cached `Bytes`,
        // not a freshly-encoded one.
        assert_eq!(first_bytes.as_ptr(), second_bytes.as_ptr());
        assert_eq!(first_bytes, second_bytes);
    }

    #[test]
    fn cache_miss_when_cert_rotates() {
        let base_cert: Arc<RwLock<Option<Arc<CertifiedKey>>>> = Arc::new(RwLock::new(Some(placeholder_certified_key(vec![0x01, 0x02, 0x03]))));
        let proof: HttpsTransportOwnershipProof = HttpsTransportOwnershipProof::new(base_cert.clone());

        let first_bytes: Bytes = proof.make_ownership_proof_payload().expect("placeholder cert is non-empty");

        // Simulate ACME rotation: refresher writes a new Arc<CertifiedKey> into the lock.
        *base_cert.write() = Some(placeholder_certified_key(vec![0x04, 0x05, 0x06]));

        let second_bytes: Bytes = proof.make_ownership_proof_payload().expect("placeholder cert is non-empty");

        assert_ne!(first_bytes.as_ptr(), second_bytes.as_ptr());
        assert_ne!(first_bytes, second_bytes);
    }
}
