//! # `TransportOwnershipProof` — proving and verifying control of an announced address
//!
//! Hashiverse peers announce themselves by address; before today, nothing in the
//! announce wire format proved the announcer actually *controlled* that address. A
//! peer with broken TLS / no cert could still announce, get into Kademlia, and
//! pollute peer lists until every receiver independently RPCed it and pruned. The
//! [`crate::protocol::payload::payload::AnnounceV2`] variant fixes that by carrying an
//! opaque proof byte blob next to `peer_self`; the receiver decides whether to
//! admit the peer based on whether the proof verifies.
//!
//! The shape of "a proof" depends on the transport. This module defines the
//! interface; concrete impls live alongside each `TransportServer`:
//!
//! - HTTPS transport (`hashiverse-server-lib::HttpsTransportOwnershipProof`):
//!   serialises the server's current ACME-issued TLS chain plus a binding signature
//!   tying the chain to `peer_self.id`; verification is offline X.509 path validation
//!   against the bundled Mozilla NSS roots (see
//!   [`crate::tools::cert_validation::is_cert_valid`]) plus binding-signature check.
//! - Mem transport: an empty marker accepted by an empty-marker verifier; tests can
//!   exercise the V2 path end-to-end without real certs.
//!
//! Wrong-transport mismatches surface naturally: if peer X sends bytes that
//! transport Y's verifier can't deserialise, [`TransportOwnershipProof::prove`]
//! returns `false`. No discriminator field needed on the wire.
//!
//! [`TransportServer::get_transport_ownership_proof`][gtop] hands out one
//! `Arc<dyn TransportOwnershipProof>` per server. The default trait impl returns a
//! reject-all placeholder so old `TransportServer` impls keep compiling; real
//! transports override it.
//!
//! [gtop]: crate::transport::transport::TransportServer::get_transport_ownership_proof

use crate::protocol::peer::Peer;
use crate::tools::time::TimeMillis;
use bytes::Bytes;

/// A per-transport rule for producing our own proof bytes (announce-out) and verifying
/// other peers' proof bytes (announce-in). A single `Arc<dyn TransportOwnershipProof>`
/// lives on a `TransportServer` and is used for both directions.
pub trait TransportOwnershipProof: Send + Sync {
    /// Announce-out: produce the proof bytes for our own announce. Returns `None` if we
    /// can't currently prove ownership — e.g. an HTTPS server that hasn't completed its
    /// first ACME issuance yet. Callers (`maintain_kademlia`) treat `None` as "skip this
    /// announce tick, try again next interval".
    ///
    /// Returns `Bytes` rather than `Vec<u8>` so the produced payload elides cleanly into
    /// the project's `BytesGatherer`-based wire aggregation without an extra copy.
    fn make_ownership_proof_payload(&self) -> Option<Bytes>;

    /// Announce-in: validate `proof_payload` (bytes pulled out of an inbound `AnnounceV2`)
    /// against `peer`. Returns `false` if the bytes can't be deserialised by this impl
    /// (wrong transport / corrupt blob) or if the proof fails verification (expired cert,
    /// wrong-IP SAN, …).
    ///
    /// V2 only guarantees "someone managed to issue a public-CA cert for `peer.address`".
    /// A peer that *borrows* a stranger's chain (the chain is public — every TLS
    /// handshake leaks it) passes this gate, but any RPC the receiver then sends to
    /// `peer.address` reaches the real server with the real identity at that IP, and the
    /// response-side identity mismatch trips the existing prune path. V2 specifically
    /// blocks the original dodgy-peer case (peer has no valid cert at all).
    fn prove(&self, peer: &Peer, proof_payload: &[u8], now: TimeMillis) -> bool;
}

/// Reject-all placeholder used as the default for `TransportServer` impls that
/// haven't been taught about ownership proofs yet (notably the wasm / TCP transports
/// during the V2 rollout). It cannot produce a proof and rejects every inbound proof
/// — safe by default: peers using this transport won't be admitted to Kademlia via
/// V2, and we never accidentally accept somebody else's proof we don't understand.
pub struct RejectAllTransportOwnershipProof;

impl TransportOwnershipProof for RejectAllTransportOwnershipProof {
    fn make_ownership_proof_payload(&self) -> Option<Bytes> {
        None
    }

    fn prove(&self, _peer: &Peer, _proof_payload: &[u8], _now: TimeMillis) -> bool {
        false
    }
}

/// Empty-marker ownership proof, shared by transports that don't (and can't) cryptographically
/// model address ownership: the in-memory test transport and the plain-TCP transport used for
/// local-LAN / private-network deployments. Both ends agree on the empty byte string as the
/// "I'm a trusted-network peer" token. An HTTPS-shaped proof byte blob sent to one of these
/// servers fails the `is_empty()` check; conversely an empty-marker proof sent to an HTTPS
/// server fails its postcard decode (or `is_cert_valid` on an empty chain). Cross-transport
/// announces fall through naturally without a discriminator field on the wire.
pub struct EmptyMarkerOwnershipProof;

impl TransportOwnershipProof for EmptyMarkerOwnershipProof {
    fn make_ownership_proof_payload(&self) -> Option<Bytes> {
        Some(Bytes::new())
    }

    fn prove(&self, _peer: &Peer, proof_payload: &[u8], _now: TimeMillis) -> bool {
        proof_payload.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_peer_for_proof_tests() -> Peer {
        // Proof methods on RejectAllTransportOwnershipProof don't touch the peer; an
        // unsigned skeleton is enough to satisfy the type system.
        Peer::zero()
    }

    #[test]
    fn reject_all_make_returns_none() {
        let proof: RejectAllTransportOwnershipProof = RejectAllTransportOwnershipProof;
        let _peer: Peer = fake_peer_for_proof_tests();
        assert!(proof.make_ownership_proof_payload().is_none());
    }

    #[test]
    fn reject_all_prove_rejects_empty() {
        let proof: RejectAllTransportOwnershipProof = RejectAllTransportOwnershipProof;
        let peer: Peer = fake_peer_for_proof_tests();
        assert!(!proof.prove(&peer, &[], TimeMillis(1_700_000_000_000)));
    }

    #[test]
    fn reject_all_prove_rejects_arbitrary_bytes() {
        let proof: RejectAllTransportOwnershipProof = RejectAllTransportOwnershipProof;
        let peer: Peer = fake_peer_for_proof_tests();
        assert!(!proof.prove(&peer, &[1, 2, 3, 4, 5], TimeMillis(1_700_000_000_000)));
    }

    #[test]
    fn empty_marker_make_returns_empty() {
        let proof: EmptyMarkerOwnershipProof = EmptyMarkerOwnershipProof;
        let payload: Bytes = proof.make_ownership_proof_payload().expect("empty-marker proof is always producible");
        assert!(payload.is_empty());
    }

    #[test]
    fn empty_marker_prove_accepts_empty() {
        let proof: EmptyMarkerOwnershipProof = EmptyMarkerOwnershipProof;
        let peer: Peer = fake_peer_for_proof_tests();
        assert!(proof.prove(&peer, &[], TimeMillis(1_700_000_000_000)));
    }

    #[test]
    fn empty_marker_prove_rejects_nonempty() {
        // An HTTPS-shaped (non-empty) byte blob sent to a trusted-network (mem / TCP) server
        // is rejected — this is how cross-transport mismatch surfaces without an on-wire
        // discriminator.
        let proof: EmptyMarkerOwnershipProof = EmptyMarkerOwnershipProof;
        let peer: Peer = fake_peer_for_proof_tests();
        assert!(!proof.prove(&peer, &[1, 2, 3], TimeMillis(1_700_000_000_000)));
    }
}
