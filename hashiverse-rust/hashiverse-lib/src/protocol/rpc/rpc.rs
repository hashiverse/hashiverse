//! # High-level RPC client helpers
//!
//! The user-facing side of the RPC layer. Where
//! [`crate::protocol::rpc::rpc_request`] and
//! [`crate::protocol::rpc::rpc_response`] deal with packet bytes, this module exposes
//! end-to-end helpers that:
//!
//! 1. **Encode** the request with the right PoW difficulty (higher when the peer is
//!    unknown, lower when the peer is already in the tracker — see
//!    [`rpc_server_known_with_requisite_pow`]).
//! 2. **Dispatch** over the supplied transport.
//! 3. **Decode** the response, verifying the signature over the request's
//!    `pow_content_hash` (salted by the server) — so a reply can only have come from the
//!    specific server we addressed, not a replay from another peer.
//! 4. **Return** typed payloads or an `ErrorResponseV1`.
//!
//! Call sites either use [`rpc_server_unknown`] (first contact / bootstrap) or
//! [`rpc_server_known`] (everything else). Trust-sensitive handlers can opt into a
//! higher PoW floor via the `_with_requisite_pow` variant.

use bytes::Bytes;
use crate::protocol::payload::payload::{ErrorResponseV1, PayloadRequestKind, PayloadResponseKind};
use crate::protocol::peer::Peer;
use crate::protocol::rpc::rpc_request::{RpcRequestPacketTx, RpcRequestPacketTxFlags};
use crate::protocol::rpc::rpc_response::RpcResponsePacketRx;
use crate::tools::runtime_services::RuntimeServices;
use crate::tools::types::{Id, PQCommitmentBytes, Pow, VerificationKeyBytes};
use crate::tools::{config, json};

pub async fn rpc_server_unknown(
    runtime_services: &RuntimeServices,
    sponsor_id: &Id,
    destination_address: &str,
    payload_request_kind: PayloadRequestKind,
    payload: Bytes,
) -> anyhow::Result<RpcResponsePacketRx> {
    let destination_id = Id::zero();
    let destination_verification_key = VerificationKeyBytes::zero();
    let destination_pq_commitment_bytes = PQCommitmentBytes::zero();
    let flags = RpcRequestPacketTxFlags::COMPRESSED;
    rpc_server_xxx(
        runtime_services,
        config::POW_MINIMUM_PER_RPC_SERVER_UNKNOWN,
        flags,
        sponsor_id,
        destination_address,
        &destination_id,
        &destination_verification_key,
        &destination_pq_commitment_bytes,
        payload_request_kind,
        payload,
    )
    .await
}

pub async fn rpc_server_known(
    runtime_services: &RuntimeServices,
    sponsor_id: &Id,
    destination_peer: &Peer,
    payload_request_kind: PayloadRequestKind,
    payload: Bytes,
) -> anyhow::Result<RpcResponsePacketRx> {
    let flags = RpcRequestPacketTxFlags::COMPRESSED | RpcRequestPacketTxFlags::SERVER_KNOWN;
    rpc_server_xxx(
        runtime_services,
        config::POW_MINIMUM_PER_RPC_SERVER_KNOWN,
        flags,
        sponsor_id,
        &destination_peer.address,
        &destination_peer.id,
        &destination_peer.verification_key_bytes,
        &destination_peer.pq_commitment_bytes,
        payload_request_kind,
        payload,
    )
    .await
}

pub async fn rpc_server_known_with_no_compression(
    runtime_services: &RuntimeServices,
    sponsor_id: &Id,
    destination_peer: &Peer,
    payload_request_kind: PayloadRequestKind,
    payload: Bytes,
) -> anyhow::Result<RpcResponsePacketRx> {
    let flags = RpcRequestPacketTxFlags::SERVER_KNOWN;
    rpc_server_xxx(
        runtime_services,
        config::POW_MINIMUM_PER_RPC_SERVER_KNOWN,
        flags,
        sponsor_id,
        &destination_peer.address,
        &destination_peer.id,
        &destination_peer.verification_key_bytes,
        &destination_peer.pq_commitment_bytes,
        payload_request_kind,
        payload,
    )
    .await
}

pub async fn rpc_server_known_with_requisite_pow(
    runtime_services: &RuntimeServices,
    sponsor_id: &Id,
    destination_peer: &Peer,
    payload_request_kind: PayloadRequestKind,
    payload: Bytes,
    requisite_pow: Pow,
) -> anyhow::Result<RpcResponsePacketRx> {
    let flags = RpcRequestPacketTxFlags::COMPRESSED | RpcRequestPacketTxFlags::SERVER_KNOWN;
    rpc_server_xxx(
        runtime_services,
        requisite_pow,
        flags,
        sponsor_id,
        &destination_peer.address,
        &destination_peer.id,
        &destination_peer.verification_key_bytes,
        &destination_peer.pq_commitment_bytes,
        payload_request_kind,
        payload,
    )
    .await
}

pub async fn rpc_server_known_with_requisite_pow_and_no_compression(
    runtime_services: &RuntimeServices,
    sponsor_id: &Id,
    destination_peer: &Peer,
    payload_request_kind: PayloadRequestKind,
    payload: Bytes,
    requisite_pow: Pow,
) -> anyhow::Result<RpcResponsePacketRx> {
    let flags = RpcRequestPacketTxFlags::SERVER_KNOWN;
    rpc_server_xxx(
        runtime_services,
        requisite_pow,
        flags,
        sponsor_id,
        &destination_peer.address,
        &destination_peer.id,
        &destination_peer.verification_key_bytes,
        &destination_peer.pq_commitment_bytes,
        payload_request_kind,
        payload,
    )
    .await
}

#[allow(clippy::too_many_arguments)] // protocol layer — each arg is part of the RPC envelope; bundling into a struct would just rename them
async fn rpc_server_xxx(
    runtime_services: &RuntimeServices,
    pow_minimum_per_rpc: Pow,
    flags: RpcRequestPacketTxFlags,
    sponsor_id: &Id,
    destination_address: &str,
    destination_id: &Id,
    destination_verification_key_bytes: &VerificationKeyBytes,
    destination_pq_commitment_bytes: &PQCommitmentBytes,
    payload_request_kind: PayloadRequestKind,
    payload: Bytes,
) -> anyhow::Result<RpcResponsePacketRx> {
    let rpc_request_packet = RpcRequestPacketTx::encode(runtime_services.time_provider.as_ref(), pow_minimum_per_rpc, flags, payload_request_kind, sponsor_id, destination_verification_key_bytes, destination_pq_commitment_bytes, payload, runtime_services.pow_generator.as_ref()).await?;

    let response_bytes = runtime_services.transport_factory.rpc(destination_address, rpc_request_packet.bytes).await?;

    let rpc_response_packet = RpcResponsePacketRx::decode(destination_id, &rpc_request_packet.pow_content_hash, config::SERVER_KEY_POW_MIN, response_bytes)?;

    // Check if there was a server error
    if rpc_response_packet.response_request_kind == PayloadResponseKind::ErrorResponseV1 {
        let response = json::bytes_to_struct::<ErrorResponseV1>(&rpc_response_packet.bytes)?;
        return Err(anyhow::anyhow!("server error {}: {}", response.code, response.message));
    }

    Ok(rpc_response_packet)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use bytes::Bytes;
    use crate::protocol::payload::payload::{PayloadRequestKind, PayloadResponseKind};
    use crate::protocol::rpc::rpc_request::{RpcRequestPacketRx, RpcRequestPacketTx, RpcRequestPacketTxFlags};
    use crate::protocol::rpc::rpc_response::{RpcResponsePacketRx, RpcResponsePacketTx, RpcResponsePacketTxFlags};
    use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
    use crate::tools::server_id::ServerId;
    use crate::tools::time_provider::time_provider::RealTimeProvider;
    use crate::tools::tools;
    use crate::tools::{types::{Id, PQCommitmentBytes, Pow, VerificationKeyBytes}, BytesGatherer};
    use log::trace;
    use crate::tools::runtime_services::RuntimeServices;

    #[tokio::test]
    async fn rpc_request_packet_txrx() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();

        let pow_min = Pow(12);
        let pow_minimum_per_rpc = Pow(12);

        let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), pow_min, true, runtime_services.pow_generator.as_ref()).await?;
        trace!("server_id.pow={}", server_id.pow);
        let payload_request_kind = PayloadRequestKind::PingV1;
        let mut payload_request = [0u8; 1024];
        tools::random_fill_bytes(&mut payload_request);
        let request_flags = RpcRequestPacketTxFlags::COMPRESSED | RpcRequestPacketTxFlags::SERVER_KNOWN;

        let rpc_request_packet_tx = RpcRequestPacketTx::encode(
            runtime_services.time_provider.as_ref(),
            pow_minimum_per_rpc,
            request_flags,
            payload_request_kind.clone(),
            &server_id.id,
            &server_id.keys.verification_key_bytes,
            &server_id.keys.pq_commitment_bytes,
            Bytes::copy_from_slice(&payload_request),
            runtime_services.pow_generator.as_ref(),
        )
        .await?;

        let rpc_request_packet_rx = RpcRequestPacketRx::decode(&server_id.timestamp, &server_id.keys.verification_key_bytes, &server_id.keys.pq_commitment_bytes, rpc_request_packet_tx.bytes)?;
        assert_eq!(payload_request_kind, rpc_request_packet_rx.payload_request_kind);
        assert_eq!(payload_request, rpc_request_packet_rx.bytes.as_ref());
        assert!(rpc_request_packet_rx.pow_server_known);

        let mut payload_response = [0u8; 1024];
        tools::random_fill_bytes(&mut payload_response);
        let response_flags = RpcResponsePacketTxFlags::COMPRESSED;

        let gatherer = RpcResponsePacketTx::encode(
            &server_id.keys.signature_key,
            &server_id.keys.verification_key_bytes,
            &server_id.keys.pq_commitment_bytes,
            &server_id.sponsor_id,
            &server_id.timestamp,
            &server_id.hash,
            &server_id.salt,
            &rpc_request_packet_rx.pow_content_hash,
            response_flags,
            PayloadResponseKind::PingResponseV1,
            BytesGatherer::from_bytes(Bytes::copy_from_slice(&payload_response)),
        )?;

        let rpc_response_packet_rx = RpcResponsePacketRx::decode(&server_id.id, &rpc_request_packet_rx.pow_content_hash, pow_min, gatherer.to_bytes())?;

        assert_eq!(rpc_response_packet_rx.response_request_kind, PayloadResponseKind::PingResponseV1);
        assert_eq!(rpc_response_packet_rx.bytes.as_ref(), payload_response);

        Ok(())
    }

    #[tokio::test]
    async fn rpc_request_packet_txrx_server_unknown() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider;
        let pow_generator: Arc<dyn crate::tools::pow_generator::pow_generator::PowGenerator> = Arc::new(SingleThreadedPowGenerator::new());

        let pow_min_for_server_id = Pow(12);
        let pow_min_for_rpc = Pow(12);

        let server_id = ServerId::new("own_pow", &time_provider, pow_min_for_server_id, true, pow_generator.as_ref()).await?;
        let payload_request_kind = PayloadRequestKind::PingV1;
        let mut payload_request = [0u8; 1024];
        tools::random_fill_bytes(&mut payload_request);
        let flags = RpcRequestPacketTxFlags::COMPRESSED;

        let rpc_request_packet_tx = RpcRequestPacketTx::encode(&time_provider, pow_min_for_rpc, flags, payload_request_kind.clone(), &Id::zero(), &VerificationKeyBytes::zero(), &PQCommitmentBytes::zero(), Bytes::copy_from_slice(&payload_request), pow_generator.as_ref()).await?;

        let rpc_request_packet_rx = RpcRequestPacketRx::decode(&server_id.timestamp, &server_id.keys.verification_key_bytes, &server_id.keys.pq_commitment_bytes, rpc_request_packet_tx.bytes)?;
        assert_eq!(payload_request_kind, rpc_request_packet_rx.payload_request_kind);
        assert_eq!(payload_request, rpc_request_packet_rx.bytes.as_ref());
        assert!(!rpc_request_packet_rx.pow_server_known);

        let mut payload_response = [0u8; 1024];
        tools::random_fill_bytes(&mut payload_response);
        let response_flags = RpcResponsePacketTxFlags::COMPRESSED;
        let gatherer = RpcResponsePacketTx::encode(
            &server_id.keys.signature_key,
            &server_id.keys.verification_key_bytes,
            &server_id.keys.pq_commitment_bytes,
            &server_id.sponsor_id,
            &server_id.timestamp,
            &server_id.hash,
            &server_id.salt,
            &rpc_request_packet_rx.pow_content_hash,
            response_flags,
            PayloadResponseKind::PingResponseV1,
            BytesGatherer::from_bytes(Bytes::copy_from_slice(&payload_response)),
        )?;

        let rpc_response_packet_rx = RpcResponsePacketRx::decode(&Id::zero(), &rpc_request_packet_rx.pow_content_hash, pow_min_for_server_id, gatherer.to_bytes())?;

        assert_eq!(rpc_response_packet_rx.response_request_kind, PayloadResponseKind::PingResponseV1);
        assert_eq!(rpc_response_packet_rx.bytes.as_ref(), payload_response);

        Ok(())
    }

    // ── Salted response signature (C1: servers must not be a signing oracle) ──

    use crate::protocol::rpc::rpc_response::RPC_RESPONSE_PACKET_VERSION;
    use crate::tools::types::{Hash, Signature, Salt, HASH_BYTES, ID_BYTES, PQ_COMMITMENT_BYTES, SALT_BYTES, SIGNATURE_BYTES, VERIFICATION_KEY_BYTES};
    use crate::tools::time::TIME_MILLIS_BYTES;

    const RESPONSE_SIGNATURE_OFFSET: usize = 1 + 1 + 2 + VERIFICATION_KEY_BYTES + PQ_COMMITMENT_BYTES + ID_BYTES + TIME_MILLIS_BYTES + HASH_BYTES + SALT_BYTES;
    const RESPONSE_SALT_OFFSET: usize = RESPONSE_SIGNATURE_OFFSET + SIGNATURE_BYTES;

    fn encode_test_response(server_id: &ServerId, pow_content_hash: &Hash) -> anyhow::Result<Bytes> {
        let bytes_gatherer = RpcResponsePacketTx::encode(
            &server_id.keys.signature_key,
            &server_id.keys.verification_key_bytes,
            &server_id.keys.pq_commitment_bytes,
            &server_id.sponsor_id,
            &server_id.timestamp,
            &server_id.hash,
            &server_id.salt,
            pow_content_hash,
            RpcResponsePacketTxFlags::COMPRESSED,
            PayloadResponseKind::ErrorResponseV1,
            BytesGatherer::from_bytes(Bytes::from_static(b"{}")),
        )?;
        Ok(bytes_gatherer.to_bytes())
    }

    fn extract_response_signature(response_bytes: &Bytes) -> anyhow::Result<Signature> {
        Ok(Signature(response_bytes[RESPONSE_SIGNATURE_OFFSET..RESPONSE_SIGNATURE_OFFSET + SIGNATURE_BYTES].try_into()?))
    }

    fn extract_response_salt(response_bytes: &Bytes) -> anyhow::Result<Salt> {
        Ok(Salt(response_bytes[RESPONSE_SALT_OFFSET..RESPONSE_SALT_OFFSET + SALT_BYTES].try_into()?))
    }

    async fn test_server_id(runtime_services: &RuntimeServices) -> anyhow::Result<ServerId> {
        ServerId::new("own_pow", runtime_services.time_provider.as_ref(), Pow(8), true, runtime_services.pow_generator.as_ref()).await
    }

    #[tokio::test]
    async fn rpc_response_signature_is_not_a_signing_oracle_for_peer() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let server_id = test_server_id(&runtime_services).await?;

        // Attacker forges a Peer record for the server: their own address, far-future timestamp,
        // and the server's genuine pows copied from its real record.
        let mut forged_peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
        forged_peer.address = "6.6.6.6:666".to_string();
        forged_peer.timestamp = TimeMillis(i64::MAX / 2);
        let forged_peer_digest = forged_peer.signature_hash_generate()?;

        // Sanity: a signature over the raw digest (the old, unsalted behaviour) would forge it.
        {
            let mut unsalted_forged_peer = forged_peer.clone();
            unsalted_forged_peer.signature = crate::tools::signing::sign(&server_id.keys.signature_key, forged_peer_digest.as_ref());
            assert!(unsalted_forged_peer.verify().is_ok(), "test setup: raw-digest signature should forge the peer");
        }

        // The attacker submits the forged digest as pow_content_hash and harvests the response signature.
        let response_bytes = encode_test_response(&server_id, &forged_peer_digest)?;
        forged_peer.signature = extract_response_signature(&response_bytes)?;
        assert!(forged_peer.verify().is_err(), "response signature must not be usable as a Peer signature");

        // ...while the response itself still verifies for the honest caller.
        RpcResponsePacketRx::decode(&server_id.id, &forged_peer_digest, Pow(8), response_bytes)?;
        Ok(())
    }

    #[tokio::test]
    async fn rpc_response_signature_is_not_a_signing_oracle_for_submit_post_claim_token() -> anyhow::Result<()> {
        use crate::protocol::payload::payload::SubmitPostClaimTokenV1;
        use crate::tools::buckets::{BucketLocation, BucketType, BUCKET_DURATIONS};

        let runtime_services = RuntimeServices::default_for_testing();
        let server_id = test_server_id(&runtime_services).await?;
        let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
        let bucket_location = BucketLocation::new(BucketType::User, Id::random(), BUCKET_DURATIONS[0], TimeMillis(1_000_000))?;
        let post_id = Id::random();

        // Mirrors SubmitPostClaimTokenV1::get_hash_for_signing.
        let claim_token_digest = hashing::hash_multiple(&[peer.id.as_ref(), bucket_location.get_hash_for_signing().as_ref(), post_id.as_ref(), "SubmitPostClaimTokenV1".as_bytes()]);

        // Sanity: the genuine token's signature is exactly sign(claim_token_digest), so the mirror above is correct.
        let genuine_claim_token = SubmitPostClaimTokenV1::new(peer.clone(), bucket_location.clone(), post_id, &server_id.keys.signature_key);
        assert_eq!(genuine_claim_token.token_signature, crate::tools::signing::sign(&server_id.keys.signature_key, claim_token_digest.as_ref()));

        let response_bytes = encode_test_response(&server_id, &claim_token_digest)?;
        let forged_claim_token = SubmitPostClaimTokenV1 { peer, bucket_location, post_id, token_signature: extract_response_signature(&response_bytes)? };
        assert!(forged_claim_token.verify().is_err(), "response signature must not be usable as a claim token signature");
        Ok(())
    }

    #[tokio::test]
    async fn rpc_response_salt_or_signature_tamper_is_rejected() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let server_id = test_server_id(&runtime_services).await?;
        let pow_content_hash = hashing::hash(b"some request");
        let response_bytes = encode_test_response(&server_id, &pow_content_hash)?;

        RpcResponsePacketRx::decode(&server_id.id, &pow_content_hash, Pow(8), response_bytes.clone())?;

        for tamper_offset in [RESPONSE_SALT_OFFSET, RESPONSE_SALT_OFFSET + SALT_BYTES - 1, RESPONSE_SIGNATURE_OFFSET, RESPONSE_SALT_OFFSET - 1] {
            let mut tampered_response_bytes = response_bytes.to_vec();
            tampered_response_bytes[tamper_offset] ^= 0x01;
            assert!(RpcResponsePacketRx::decode(&server_id.id, &pow_content_hash, Pow(8), Bytes::from(tampered_response_bytes)).is_err(), "tamper at offset {} must be rejected", tamper_offset);
        }
        Ok(())
    }

    #[tokio::test]
    async fn rpc_response_old_version_is_rejected() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let server_id = test_server_id(&runtime_services).await?;
        let pow_content_hash = hashing::hash(b"some request");
        let response_bytes = encode_test_response(&server_id, &pow_content_hash)?;
        assert_eq!(response_bytes[0], RPC_RESPONSE_PACKET_VERSION);

        let mut old_version_response_bytes = response_bytes.to_vec();
        old_version_response_bytes[0] = 1;
        let error = RpcResponsePacketRx::decode(&server_id.id, &pow_content_hash, Pow(8), Bytes::from(old_version_response_bytes)).err().expect("version 1 must be rejected");
        assert!(error.to_string().contains("Unsupported RpcResponsePacket version: 1"), "unexpected error: {}", error);
        Ok(())
    }

    #[tokio::test]
    async fn rpc_response_salt_is_fresh_per_response() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let server_id = test_server_id(&runtime_services).await?;
        let pow_content_hash = hashing::hash(b"some request");

        let response_bytes_1 = encode_test_response(&server_id, &pow_content_hash)?;
        let response_bytes_2 = encode_test_response(&server_id, &pow_content_hash)?;
        assert_ne!(extract_response_salt(&response_bytes_1)?, extract_response_salt(&response_bytes_2)?);
        assert_ne!(extract_response_signature(&response_bytes_1)?, extract_response_signature(&response_bytes_2)?);

        RpcResponsePacketRx::decode(&server_id.id, &pow_content_hash, Pow(8), response_bytes_1)?;
        RpcResponsePacketRx::decode(&server_id.id, &pow_content_hash, Pow(8), response_bytes_2)?;
        Ok(())
    }

    // ── Robustness tests: RpcRequestPacketRx::decode ──

    use crate::tools::time::TimeMillis;
    use crate::tools::hashing;

    #[test]
    fn rpc_request_decode_empty_input() {
        let timestamp = TimeMillis::zero();
        let verification_key_bytes = VerificationKeyBytes::zero();
        let pq_commitment_bytes = PQCommitmentBytes::zero();
        assert!(RpcRequestPacketRx::decode(&timestamp, &verification_key_bytes, &pq_commitment_bytes, Bytes::new()).is_err());
    }

    #[test]
    fn rpc_request_decode_single_byte() {
        let timestamp = TimeMillis::zero();
        let verification_key_bytes = VerificationKeyBytes::zero();
        let pq_commitment_bytes = PQCommitmentBytes::zero();
        assert!(RpcRequestPacketRx::decode(&timestamp, &verification_key_bytes, &pq_commitment_bytes, Bytes::from_static(&[1u8])).is_err());
    }

    #[test]
    fn rpc_request_decode_garbage() {
        let timestamp = TimeMillis::zero();
        let verification_key_bytes = VerificationKeyBytes::zero();
        let pq_commitment_bytes = PQCommitmentBytes::zero();
        assert!(RpcRequestPacketRx::decode(&timestamp, &verification_key_bytes, &pq_commitment_bytes, Bytes::from_static(&[0xff; 256])).is_err());
    }

    // ── Robustness tests: RpcResponsePacketRx::decode ──

    #[test]
    fn rpc_response_decode_empty_input() {
        let destination_id = Id::zero();
        let pow_content_hash = hashing::hash(&[0u8]);
        assert!(RpcResponsePacketRx::decode(&destination_id, &pow_content_hash, Pow(0), Bytes::new()).is_err());
    }

    #[test]
    fn rpc_response_decode_single_byte() {
        let destination_id = Id::zero();
        let pow_content_hash = hashing::hash(&[0u8]);
        assert!(RpcResponsePacketRx::decode(&destination_id, &pow_content_hash, Pow(0), Bytes::from_static(&[1u8])).is_err());
    }

    #[test]
    fn rpc_response_decode_garbage() {
        let destination_id = Id::zero();
        let pow_content_hash = hashing::hash(&[0u8]);
        assert!(RpcResponsePacketRx::decode(&destination_id, &pow_content_hash, Pow(0), Bytes::from_static(&[0xff; 256])).is_err());
    }

    #[test]
    fn rpc_response_decode_header_too_short_for_payload_len() {
        // The header up to (but not including) the u32 payload_len field is 220 bytes
        // (212 + 8 bytes of pow_content_hash_salt). Providing header + 2 bytes used to pass
        // the old `+ 2` minimum-length check but panic on get_u32_le() which needs 4 bytes.
        // Now correctly rejected.
        let mut data = vec![0u8; 222];
        data[0] = crate::protocol::rpc::rpc_response::RPC_RESPONSE_PACKET_VERSION;
        let destination_id = Id::zero();
        let pow_content_hash = hashing::hash(&[0u8]);
        assert!(RpcResponsePacketRx::decode(&destination_id, &pow_content_hash, Pow(0), Bytes::from(data)).is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod bolero_fuzz {
        use bytes::Bytes;
        use crate::protocol::rpc::rpc_request::RpcRequestPacketRx;
        use crate::protocol::rpc::rpc_response::RpcResponsePacketRx;
        use crate::tools::types::{Id, PQCommitmentBytes, Pow, VerificationKeyBytes};
        use crate::tools::time::TimeMillis;
        use crate::tools::hashing;

        #[test]
        fn fuzz_rpc_request_decode() {
            bolero::check!().for_each(|data: &[u8]| {
                let timestamp = TimeMillis::zero();
                let verification_key_bytes = VerificationKeyBytes::zero();
                let pq_commitment_bytes = PQCommitmentBytes::zero();
                let _ = RpcRequestPacketRx::decode(&timestamp, &verification_key_bytes, &pq_commitment_bytes, Bytes::copy_from_slice(data));
            });
        }

        #[test]
        fn fuzz_rpc_response_decode() {
            bolero::check!().for_each(|data: &[u8]| {
                let destination_id = Id::zero();
                let pow_content_hash = hashing::hash(&[0u8]);
                let _ = RpcResponsePacketRx::decode(&destination_id, &pow_content_hash, Pow(0), Bytes::copy_from_slice(data));
            });
        }
    }
}
