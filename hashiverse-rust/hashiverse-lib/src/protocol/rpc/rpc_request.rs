//! # RPC request packet encode / decode
//!
//! The wire format of every outgoing and incoming RPC request. Split into a strict
//! Tx / Rx pair so the compiler enforces the asymmetry between "I'm building a request"
//! and "I'm parsing one":
//!
//! - [`RpcRequestPacketTx`] — build a request. `encode` optionally compresses the
//!   payload, runs the PoW search (via a
//!   [`crate::tools::pow_generator::pow_generator::PowGenerator`]) targeting the
//!   destination server's identity, and produces a header containing the discriminator,
//!   sponsor id, PoW timestamp, content hash, salt, and payload length.
//! - [`RpcRequestPacketRx`] — parse a request on the receive side. Extracts the header,
//!   verifies PoW against the receiving server's own identity, rejects under-powered
//!   or stale requests (via [`crate::tools::config::POW_MAX_CLOCK_DRIFT_MILLIS`]) before
//!   any payload work happens.
//!
//! The PoW sits in the *header* and binds to the server's identity, so a request valid
//! for server A can't be forwarded to server B and still authenticate.

use crate::protocol::payload::payload::PayloadRequestKind;
use crate::tools::pow_generator::pow_generator::PowGenerator;
use crate::tools::server_id::ServerId;
use crate::tools::time::{TimeMillis, TimeMillisBytes, TIME_MILLIS_BYTES};
use crate::tools::time_provider::time_provider::TimeProvider;
use crate::tools::types::{Hash, Id, PQCommitmentBytes, Pow, Salt, VerificationKeyBytes, HASH_BYTES, ID_BYTES, SALT_BYTES};
use crate::tools::{compression, config, hashing};
use bitflags::bitflags;
use bytes::{Buf, BufMut, Bytes, BytesMut};

bitflags! {
    pub struct RpcRequestPacketTxFlags: u8 {
        const COMPRESSED = 1 << 0;
        const SERVER_KNOWN = 1 << 1;
    }
}

/// An outbound RPC request, freshly encoded and ready to be sent over a [`crate::transport::TransportFactory`].
///
/// The "Tx" (transmit) side owns the encode path. Constructing one is expensive: in addition
/// to compressing the payload and prefixing the header, `encode` performs a proof-of-work
/// search against the destination server's identity via [`PowGenerator`]. The `pow`
/// sits in the wire header so the server can reject under-powered requests cheaply before
/// doing any payload work. `pow_content_hash` is retained on the struct so the caller can
/// later verify that the response was produced for *this* specific request (see
/// [`RpcResponsePacketRx`]).
///
/// This type and its "Rx" counterpart [`RpcRequestPacketRx`] are intentionally separate so
/// the encode side and the decode side cannot be accidentally mixed up.
pub struct RpcRequestPacketTx {
    pub pow_content_hash: Hash,
    pub bytes: Bytes,
}
impl RpcRequestPacketTx {
    pub async fn encode(
        time_provider: &dyn TimeProvider,
        pow_minimum_per_rpc: Pow,
        flags: RpcRequestPacketTxFlags,
        payload_request_kind: PayloadRequestKind,
        pow_sponsor_id: &Id,
        destination_verification_key_bytes: &VerificationKeyBytes,
        destination_pq_commitment_bytes: &PQCommitmentBytes,
        payload_uncompressed: Bytes,
        pow_generator: &dyn PowGenerator,
    ) -> anyhow::Result<Self> {
        // Do we actually need to compress this?
        let payload_compressed = match flags.contains(RpcRequestPacketTxFlags::COMPRESSED) {
            true => compression::compress_for_speed(payload_uncompressed.as_ref())?.to_bytes(),
            false => payload_uncompressed,
        };

        // Check that it is not too large...
        if payload_compressed.len() > config::PROTOCOL_MAX_BLOB_SIZE_REQUEST {
            anyhow::bail!("request payload size exceeds maximum allowed size: {} > {}", payload_compressed.len(), config::PROTOCOL_MAX_BLOB_SIZE_REQUEST);
        }

        // This will be needed when we verify that the server processed our request
        let pow_content_hash: Hash = hashing::hash(payload_compressed.as_ref());

        // Do some proof of work on behalf of the destination server, sponsored by the caller
        let pow_label = format!("rpc:{}", payload_request_kind);
        let (pow_timestamp, pow_salt, _, _) = ServerId::pow_generate(&pow_label, time_provider, pow_minimum_per_rpc, pow_sponsor_id, destination_verification_key_bytes, destination_pq_commitment_bytes, &pow_content_hash, pow_generator).await?;

        // Write the header
        let mut bytes_mut = BytesMut::with_capacity(size_of::<u8>() + size_of::<u8>() + size_of::<u16>() + ID_BYTES + TIME_MILLIS_BYTES + HASH_BYTES + SALT_BYTES + size_of::<u32>() + payload_compressed.len());
        bytes_mut.put_u8(1);
        bytes_mut.put_u8(flags.bits());
        bytes_mut.put_u16_le(payload_request_kind as u16);
        bytes_mut.put_slice(pow_sponsor_id.as_ref());
        bytes_mut.put_slice(pow_timestamp.encode_be().as_ref());
        bytes_mut.put_slice(pow_content_hash.as_ref());
        bytes_mut.put_slice(pow_salt.as_ref());
        bytes_mut.put_u32_le(payload_compressed.len() as u32);
        bytes_mut.put_slice(&payload_compressed);

        let bytes = bytes_mut.freeze();

        Ok(Self { pow_content_hash, bytes })
    }
}

/// The server-side view of an inbound RPC request after header parsing and PoW verification.
///
/// `RpcRequestPacketRx` is what falls out of `decode` on the receiving end. It exposes the
/// routing discriminator ([`PayloadRequestKind`]) so the dispatcher knows which handler to
/// invoke, plus all the PoW fields needed to decide how much trust to assign to the call.
/// The `bytes` field still contains the compressed payload — payload decoding happens later,
/// after the dispatcher has picked the right handler.
///
/// Paired with [`RpcRequestPacketTx`] as the decode side of the same wire format.
#[derive(Debug)]
pub struct RpcRequestPacketRx {
    pub payload_request_kind: PayloadRequestKind,
    pub pow_sponsor_id: Id,
    pub pow_server_known: bool,
    pub pow: Pow,
    pub pow_timestamp: TimeMillis,
    pub pow_content_hash: Hash,
    pub pow_salt: Salt,
    pub bytes: Bytes,
}
impl RpcRequestPacketRx {
    pub fn decode(current_time_millis: &TimeMillis, verification_key_bytes: &VerificationKeyBytes, pq_commitment_bytes: &PQCommitmentBytes, mut response_bytes: Bytes) -> anyhow::Result<Self> {
        // Do we have enough bytes for the header?
        if response_bytes.len() < size_of::<u8>() + size_of::<u8>() + size_of::<u16>() + ID_BYTES + TIME_MILLIS_BYTES + HASH_BYTES + SALT_BYTES + size_of::<u32>() {
            anyhow::bail!("RpcRequestPacket is too short for header");
        }

        let version = response_bytes.get_u8();
        if 1 != version {
            anyhow::bail!("Unsupported RpcRequestPacket version: {}", version);
        }

        let flags = RpcRequestPacketTxFlags::from_bits(response_bytes.get_u8()).ok_or_else(|| anyhow::anyhow!("Invalid RpcRequestPacket flags"))?;
        let payload_request_kind = PayloadRequestKind::from_u16(response_bytes.get_u16_le())?;
        let pow_sponsor_id = Id::from_slice(response_bytes.slice(..ID_BYTES).as_ref())?;
        response_bytes.advance(ID_BYTES);
        let pow_timestamp_bytes: TimeMillisBytes = TimeMillisBytes::from_bytes(response_bytes.slice(..TIME_MILLIS_BYTES).as_ref())?;
        response_bytes.advance(TIME_MILLIS_BYTES);
        let pow_content_hash: Hash = Hash::from_slice(response_bytes.slice(..HASH_BYTES).as_ref())?;
        response_bytes.advance(HASH_BYTES);
        let pow_salt = Salt::from_slice(response_bytes.slice(..SALT_BYTES).as_ref())?;
        response_bytes.advance(SALT_BYTES);

        // Is the packet timestamp close enough to ours?
        let pow_timestamp = TimeMillis::timestamp_decode_be(&pow_timestamp_bytes);
        {
            let delta_millis = (*current_time_millis - pow_timestamp).abs();
            if delta_millis.0 > config::POW_MAX_CLOCK_DRIFT_MILLIS.0 {
                anyhow::bail!("Client pow clock drift too large: us={} them={}", current_time_millis, pow_timestamp);
            }
        }

        // Has the client done enough pow for us?
        let (pow, pow_server_known) = match flags.contains(RpcRequestPacketTxFlags::SERVER_KNOWN) {
            true => {
                let (pow, _) = ServerId::pow_measure(&pow_sponsor_id, verification_key_bytes, pq_commitment_bytes, &pow_timestamp_bytes, &pow_content_hash, &pow_salt)?;
                if pow < config::POW_MINIMUM_PER_RPC_SERVER_KNOWN {
                    anyhow::bail!("Client has not done enough known pow: {} < {}", pow, config::POW_MINIMUM_PER_RPC_SERVER_KNOWN);
                }
                (pow, true)
            }
            false => {
                let (pow, _) = ServerId::pow_measure(&pow_sponsor_id, &VerificationKeyBytes::zero(), &PQCommitmentBytes::zero(), &pow_timestamp_bytes, &pow_content_hash, &pow_salt)?;
                if pow < config::POW_MINIMUM_PER_RPC_SERVER_UNKNOWN {
                    anyhow::bail!("Client has not done enough unknown pow: {} < {}", pow, config::POW_MINIMUM_PER_RPC_SERVER_UNKNOWN);
                }
                (pow, false)
            }
        };

        let request_payload_len = response_bytes.get_u32_le() as usize;

        if request_payload_len > config::PROTOCOL_MAX_BLOB_SIZE_REQUEST {
            anyhow::bail!("RpcRequestPacket payload too large: {} > {}", request_payload_len, config::PROTOCOL_MAX_BLOB_SIZE_REQUEST);
        }

        // Do we have enough bytes for the payload?
        if response_bytes.len() < request_payload_len {
            anyhow::bail!("RpcRequestPacket is too short for payload");
        }

        let response_payload = response_bytes.slice(..request_payload_len);
        response_bytes.advance(request_payload_len);

        // Sanity check - are we done?
        if !response_bytes.is_empty() {
            anyhow::bail!("RpcRequestPacket is too long");
        }

        // Do we need to decompress?
        let response_payload_decompressed = match flags.contains(RpcRequestPacketTxFlags::COMPRESSED) {
            true => compression::decompress(response_payload.as_ref())?.to_bytes(),
            false => response_payload,
        };

        let selfie = Self {
            payload_request_kind,
            pow_sponsor_id,
            pow_server_known,
            pow,
            pow_timestamp,
            pow_content_hash,
            pow_salt,
            bytes: response_payload_decompressed,
        };
        // trace!("Decoded RpcRequestPacketRx={:?}", selfie);

        Ok(selfie)
    }
}

