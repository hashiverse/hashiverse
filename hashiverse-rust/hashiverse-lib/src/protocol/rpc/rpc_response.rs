//! # RPC response packet encode / decode
//!
//! The outbound-server and inbound-client halves of a response, symmetric to
//! [`crate::protocol::rpc::rpc_request`]:
//!
//! - [`RpcResponsePacketTx`] — server side. `encode` signs the request's
//!   `pow_content_hash` with the server's private key and embeds the signature in the
//!   header. The signature binds the response to *this* specific request and *this*
//!   specific server identity, so clients can reject replays across servers or across
//!   requests. Payload is optionally compressed.
//! - [`RpcResponsePacketRx`] — client side. Parses the server-identity fields
//!   (verification key, PQ commitment, sponsor id, PoW timestamp, PoW hash, salt) from
//!   the header, re-derives the server id PoW to check it is sufficient, and verifies
//!   the signature against the request's content hash — all statelessly, so no peer
//!   database is required to validate the response of a freshly-discovered server.

use crate::protocol::payload::payload::PayloadResponseKind;
use crate::tools::server_id::ServerId;
use crate::tools::time::{TimeMillis, TimeMillisBytes, TIME_MILLIS_BYTES};
use crate::tools::types::{Hash, Id, PQCommitmentBytes, Pow, Salt, Signature, SignatureKey, VerificationKeyBytes, HASH_BYTES, ID_BYTES, PQ_COMMITMENT_BYTES, SALT_BYTES, SIGNATURE_BYTES, VERIFICATION_KEY_BYTES};
use crate::tools::{compression, config, signing, BytesGatherer};
use bitflags::bitflags;
use bytes::{Buf, Bytes};

bitflags! {
    pub struct RpcResponsePacketTxFlags: u8 {
        const COMPRESSED = 1 << 0;
    }
}

/// The encoder for an outbound RPC response.
///
/// `RpcResponsePacketTx` is a type-level tag — its sole associated function, `encode`,
/// assembles the wire response given the server's identity fields, the `pow_content_hash`
/// from the corresponding request, and a payload. The server signs the request's
/// `pow_content_hash` with its [`SignatureKey`] and emits the signature on the response so
/// the caller can prove the response was produced by the intended destination peer for
/// *this* specific request — a defence against both response substitution and replayed
/// responses.
///
/// Paired with [`RpcResponsePacketRx`] on the decode side.
pub struct RpcResponsePacketTx;

impl RpcResponsePacketTx {
    pub fn encode(
        server_id_signature_key: &SignatureKey,
        server_id_verification_key_bytes: &VerificationKeyBytes,
        server_id_pq_commitment_bytes: &PQCommitmentBytes,
        server_id_verification_sponsor_id: &Id,
        server_id_timestamp: &TimeMillis,
        server_id_hash: &Hash,
        server_id_salt: &Salt,
        pow_content_hash: &Hash,
        flags: RpcResponsePacketTxFlags,
        payload_response_kind: PayloadResponseKind,
        payload_uncompressed: BytesGatherer,
    ) -> anyhow::Result<BytesGatherer> {
        // Do we actually need to compress this?
        let payload_compressed: BytesGatherer = match flags.contains(RpcResponsePacketTxFlags::COMPRESSED) {
            true => compression::compress_for_speed(&payload_uncompressed.to_bytes())?,
            false => payload_uncompressed,
        };

        let payload_compressed_len = payload_compressed.len();

        // Check that it is not too large...
        if payload_compressed_len > config::PROTOCOL_MAX_BLOB_SIZE_RESPONSE {
            anyhow::bail!("response payload size exceeds maximum allowed size: {} > {}", payload_compressed_len, config::PROTOCOL_MAX_BLOB_SIZE_RESPONSE);
        }

        let pow_content_hash_signature = signing::sign(server_id_signature_key, pow_content_hash.as_ref());

    // All small header fields go into accumulator
    let mut result = BytesGatherer::default();
    result.put_u8(1); // Version = 1 (for now)
    result.put_u8(flags.bits());
    result.put_u16_le(payload_response_kind as u16);
    result.put_slice(server_id_verification_key_bytes.as_ref());
    result.put_slice(server_id_pq_commitment_bytes.as_ref());
    result.put_slice(server_id_verification_sponsor_id.as_ref());
    result.put_slice(server_id_timestamp.encode_be().as_ref());
    result.put_slice(server_id_hash.as_ref());
    result.put_slice(server_id_salt.as_ref());
    result.put_slice(pow_content_hash_signature.as_ref());
    result.put_u32_le(payload_compressed_len as u32);
    result.put_bytes_gatherer(payload_compressed);

        Ok(result)
    }
}

/// The client-side view of an inbound RPC response, after header parsing, PoW checks, and
/// signature verification.
///
/// By the time an `RpcResponsePacketRx` exists, the decoder has already proved that the
/// remote server really signed over the request's `pow_content_hash` and that the signing
/// identity matches the destination the caller intended. Callers see only the
/// [`PayloadResponseKind`] (so they can pick the right payload deserializer) and the still-
/// compressed body bytes.
///
/// Paired with [`RpcResponsePacketTx`] as the decode side of the same wire format.
pub struct RpcResponsePacketRx {
    pub response_request_kind: PayloadResponseKind,
    pub bytes: Bytes,
}

impl RpcResponsePacketRx {
    pub fn decode(destination_id: &Id, pow_content_hash: &Hash, pow_min: Pow, mut response_bytes: Bytes) -> anyhow::Result<Self> {
        // Do we have enough bytes for the header?
        if response_bytes.len() < size_of::<u8>() + size_of::<u8>() + size_of::<u16>() + VERIFICATION_KEY_BYTES + PQ_COMMITMENT_BYTES + ID_BYTES + TIME_MILLIS_BYTES + HASH_BYTES + SALT_BYTES + SIGNATURE_BYTES + size_of::<u32>() {
            anyhow::bail!("RpcResponsePacket is too short for header");
        }

        let version = response_bytes.get_u8();
        if 1 != version {
            anyhow::bail!("Unsupported RpcRequestPacket version: {}", version);
        }

        let flags = RpcResponsePacketTxFlags::from_bits(response_bytes.get_u8()).ok_or_else(|| anyhow::anyhow!("Invalid RpcResponsePacket flags"))?;
        let response_request_kind = PayloadResponseKind::from_u16(response_bytes.get_u16_le())?;
        let server_id_verification_key = VerificationKeyBytes(response_bytes.slice(..VERIFICATION_KEY_BYTES).as_ref().try_into()?);
        response_bytes.advance(VERIFICATION_KEY_BYTES);
        let server_id_pq_commitment_bytes = PQCommitmentBytes(response_bytes.slice(..PQ_COMMITMENT_BYTES).as_ref().try_into()?);
        response_bytes.advance(PQ_COMMITMENT_BYTES);
        let server_id_verification_sponsor_id = Id(response_bytes.slice(..ID_BYTES).as_ref().try_into()?);
        response_bytes.advance(ID_BYTES);
        let server_id_verification_timestamp_bytes: TimeMillisBytes = TimeMillisBytes(response_bytes.slice(..TIME_MILLIS_BYTES).as_ref().try_into()?);
        response_bytes.advance(TIME_MILLIS_BYTES);
        let server_id_verification_hash = Hash(response_bytes.slice(..HASH_BYTES).as_ref().try_into()?);
        response_bytes.advance(HASH_BYTES);
        let server_id_verification_salt = Salt(response_bytes.slice(..SALT_BYTES).as_ref().try_into()?);
        response_bytes.advance(SALT_BYTES);

        let pow_content_hash_signature: Signature = Signature(response_bytes.slice(..SIGNATURE_BYTES).as_ref().try_into()?);
        response_bytes.advance(SIGNATURE_BYTES);

        let response_payload_len = response_bytes.get_u32_le() as usize;

        if response_payload_len > config::PROTOCOL_MAX_BLOB_SIZE_RESPONSE {
            anyhow::bail!("RpcResponsePacket payload too large: {} > {}", response_payload_len, config::PROTOCOL_MAX_BLOB_SIZE_RESPONSE);
        }

        // Do we have enough bytes for the payload?
        if response_bytes.len() < response_payload_len {
            anyhow::bail!("RpcResponsePacket is too short for payload");
        }

        let response_payload = response_bytes.slice(..response_payload_len);
        response_bytes.advance(response_payload_len);

        // Sanity check - did we use all the packet?
        if !response_bytes.is_empty() {
            anyhow::bail!("RpcResponsePacket is too long");
        }

        // Ensure that the server id is pow sufficient (server identity PoW uses Id::zero() as sponsor)
        let (pow, pow_hash) = ServerId::pow_measure(
            &server_id_verification_sponsor_id,
            &server_id_verification_key,
            &server_id_pq_commitment_bytes,
            &server_id_verification_timestamp_bytes,
            &server_id_verification_hash,
            &server_id_verification_salt,
        )?;
        if pow < pow_min {
            anyhow::bail!(format!("Server ID pow is not sufficient: {} < {}", pow, pow_min));
        }

        // Let's check that the server is who they say they are - unless of course we were querying Id::zero()
        let id = ServerId::server_pow_hash_to_id(pow_hash)?;
        if id != *destination_id {
            if !destination_id.is_zero() {
                anyhow::bail!("Server ID verification failed");
            }
        }

        // Check that the server has signed our initial content hash
        let verification_key = server_id_verification_key.to_verification_key()?;
        signing::verify(&verification_key, &pow_content_hash_signature, pow_content_hash.as_ref())?;

        // Do we need to decompress?
        let response_payload_decompressed = match flags.contains(RpcResponsePacketTxFlags::COMPRESSED) {
            true => compression::decompress(response_payload.as_ref())?.to_bytes(),
            false => response_payload,
        };

        Ok(Self {
            response_request_kind,
            bytes: response_payload_decompressed,
        })
    }
}
