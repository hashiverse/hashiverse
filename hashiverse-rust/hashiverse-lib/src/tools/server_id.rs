//! # Server identity with proof-of-work birth certificate
//!
//! [`ServerId`] is the stable identity of a single server node. Unlike [`crate::tools::client_id::ClientId`],
//! which costs nothing to produce, a `ServerId` is expensive: the server's 32-byte `Id`
//! is the *reversed* bytes of a PoW hash computed over the server's keys, a random
//! sponsor id, a timestamp, and a random content hash — with a minimum leading-zero-bit
//! count (`SERVER_KEY_POW_MIN`) configured in [`crate::tools::config`].
//!
//! Reversing the PoW hash means the id naturally has a large number of **trailing**
//! zero bits. The Kademlia DHT distributes responsibility by XOR distance, and the
//! trailing-zero structure spreads servers evenly across the keyspace while making
//! every id independently verifiable: anyone can recompute the PoW from the fields
//! embedded in the `ServerId` and confirm the id is real.
//!
//! The PoW therefore acts as a "birth certificate" that gates server identities —
//! you can't cheaply spin up thousands of sybil servers targeting a specific keyspace
//! region because each id costs real CPU time.
//!
//! Beyond identity:
//! - [`ServerId::to_peer`] wraps the identity into a [`crate::protocol::peer::Peer`]
//!   for gossip on the DHT, signing the result with the server's private key.
//! - [`ServerId::encode`] / [`ServerId::decode`] provide a compact fixed-length byte
//!   representation for on-disk persistence.
//! - [`ServerId::verify`] re-runs the PoW check and confirms the embedded id matches.

use crate::protocol::peer::{Peer, PeerPow};
use crate::tools::keys::Keys;
use crate::tools::parallel_pow_generator::ParallelPowGenerator;
use crate::tools::time::{TimeMillis, TimeMillisBytes, TIME_MILLIS_BYTES};
use crate::tools::time_provider::time_provider::TimeProvider;
use crate::tools::types::{Hash, Id, PQCommitmentBytes, Pow, Salt, Signature, SignatureKey, VerificationKey, VerificationKeyBytes, HASH_BYTES, ID_BYTES, PQ_COMMITMENT_BYTES, SALT_BYTES, SIGNATURE_KEY_BYTES, VERIFICATION_KEY_BYTES};
use crate::tools::{config, pow, tools, types};
use std::fmt;

#[derive(Clone)]
pub struct ServerId {
    pub keys: Keys,

    // Proof of initial work
    pub sponsor_id: Id,
    pub timestamp: TimeMillis,
    pub hash: Hash,
    pub salt: Salt,
    pub pow: Pow,

    pub id: Id,
}

impl ServerId {
    pub fn id_hex(&self) -> String {
        hex::encode(self.id)
    }

    pub fn server_pow_hash_to_id(hash: Hash) -> anyhow::Result<Id> {
        if hash.len() != ID_BYTES {
            anyhow::bail!("Invalid Hash length: expected {} bytes, got {} bytes", ID_BYTES, hash.len());
        }

        let id = Id(tools::reverse_bytes(hash.as_bytes()));
        Ok(id)
    }

    // Generates the initial pow hash for a server.
    pub async fn pow_generate(
        time_provider: &dyn TimeProvider,
        pow_min: Pow,
        sponsor_id: &Id,
        verification_key: &VerificationKeyBytes,
        pq_commitment_bytes: &PQCommitmentBytes,
        content_hash: &Hash,
        pow_generator: &dyn ParallelPowGenerator,
    ) -> anyhow::Result<(TimeMillis, Salt, Pow, Hash)> {
        let timestamp = time_provider.current_time_millis();
        let timestamp_be = timestamp.encode_be();
        let datas = [sponsor_id.as_ref(), verification_key.as_ref(), pq_commitment_bytes.as_ref(), timestamp_be.as_ref(), content_hash.as_ref()];
        let data_hash = pow::pow_compute_data_hash(&datas);
        let (salt, pow, pow_hash) = pow_generator.generate("server_id", pow_min, data_hash).await?;
        Ok((timestamp, salt, pow, pow_hash))
    }

    pub fn pow_measure(sponsor_id: &Id, verification_key: &VerificationKeyBytes, pqcommitment_bytes: &PQCommitmentBytes, timestamp_be: &TimeMillisBytes, content_hash: &Hash, salt: &Salt) -> anyhow::Result<(Pow, Hash)> {
        pow::pow_measure(&[sponsor_id.as_ref(), verification_key.as_ref(), pqcommitment_bytes.as_ref(), timestamp_be.as_ref(), content_hash.as_ref()], salt)
    }

    pub async fn new(time_provider: &dyn TimeProvider, pow_min: Pow, skip_pq_commitment_bytes: bool, pow_generator: &dyn ParallelPowGenerator) -> anyhow::Result<Self> {
        let sponsor_id = Id::random();
        let keys = Keys::from_rnd(skip_pq_commitment_bytes)?;
        let hash = Hash::random();
        let (timestamp, salt, pow, pow_hash) = ServerId::pow_generate(time_provider, pow_min, &sponsor_id, &keys.verification_key_bytes, &keys.pq_commitment_bytes, &hash, pow_generator).await?;
        let id = ServerId::server_pow_hash_to_id(pow_hash)?;

        Ok(ServerId {
            keys,
            sponsor_id,
            timestamp,
            hash,
            salt,
            pow,
            id,
        })
    }

    pub fn to_peer(&self, time_provider: &dyn TimeProvider) -> anyhow::Result<Peer> {
        let mut peer = Peer {
            id: self.id,
            verification_key_bytes: self.keys.verification_key_bytes,
            pq_commitment_bytes: self.keys.pq_commitment_bytes,

            pow_initial: PeerPow {
                sponsor_id: self.sponsor_id,
                timestamp: self.timestamp,
                content_hash: self.hash,
                salt: self.salt,
                pow: self.pow,
            },

            pow_current_day: PeerPow {
                sponsor_id: self.sponsor_id,
                timestamp: self.timestamp,
                content_hash: self.hash,
                salt: self.salt,
                pow: self.pow,
            },

            pow_current_month: PeerPow {
                sponsor_id: self.sponsor_id,
                timestamp: self.timestamp,
                content_hash: self.hash,
                salt: self.salt,
                pow: self.pow,
            },

            address: "".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),

            timestamp: TimeMillis::zero(),
            signature: Signature::zero(),
        };

        // Sign the peer
        peer.sign(time_provider, &self.keys.signature_key)?;

        Ok(peer)
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        let (pow, pow_hash) = ServerId::pow_measure(&self.sponsor_id, &self.keys.verification_key_bytes, &self.keys.pq_commitment_bytes, &self.timestamp.encode_be(), &self.hash, &self.salt)?;
        if pow != self.pow {
            anyhow::bail!("ServerID pow does not verify");
        }

        if pow < config::SERVER_KEY_POW_MIN {
            anyhow::bail!("ServerID pow is not sufficient");
        }

        let id = ServerId::server_pow_hash_to_id(pow_hash)?;

        if id != self.id {
            anyhow::bail!("ServerID id does not verify");
        }

        Ok(())
    }

    pub fn encode(&self) -> anyhow::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        {
            bytes.extend_from_slice(self.keys.signature_key.as_ref());
            bytes.extend_from_slice(self.keys.verification_key.as_ref());
            bytes.extend_from_slice(self.keys.pq_commitment_bytes.as_ref());
            bytes.extend_from_slice(self.sponsor_id.as_ref());
            bytes.extend_from_slice(self.timestamp.encode_be().as_ref());
            bytes.extend_from_slice(self.hash.as_ref());
            bytes.extend_from_slice(self.salt.as_ref());
            bytes.push(self.pow.0);
            bytes.extend_from_slice(self.id.as_ref());
        }

        // Sanity check
        let expected_len = SIGNATURE_KEY_BYTES + VERIFICATION_KEY_BYTES + PQ_COMMITMENT_BYTES + ID_BYTES + TIME_MILLIS_BYTES + HASH_BYTES + SALT_BYTES + 1 + types::ID_BYTES;
        if bytes.len() != expected_len {
            anyhow::bail!("incorrect byte count: expected {}, got {}", expected_len, bytes.len());
        }

        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> anyhow::Result<Self> {
        // Sanity check
        let expected_len = SIGNATURE_KEY_BYTES + VERIFICATION_KEY_BYTES + PQ_COMMITMENT_BYTES + ID_BYTES + TIME_MILLIS_BYTES + HASH_BYTES + SALT_BYTES + 1 + types::ID_BYTES;
        if bytes.len() != expected_len {
            anyhow::bail!("incorrect byte count: expected {}, got {}", expected_len, bytes.len());
        }

        let mut pos = 0;

        let signature_key_bytes = &bytes[pos..pos + SIGNATURE_KEY_BYTES];
        pos += SIGNATURE_KEY_BYTES;
        let verification_key_bytes = &bytes[pos..pos + VERIFICATION_KEY_BYTES];
        pos += VERIFICATION_KEY_BYTES;
        let pq_commitment_bytes = &bytes[pos..pos + PQ_COMMITMENT_BYTES];
        pos += PQ_COMMITMENT_BYTES;
        let sponsor_id = Id::from_slice(bytes[pos..pos + ID_BYTES].try_into()?)?;
        pos += ID_BYTES;
        let timestamp = TimeMillis::timestamp_decode_be(&TimeMillisBytes::from_bytes(&bytes[pos..pos + 8])?);
        pos += TIME_MILLIS_BYTES;
        let hash = Hash::from_slice(bytes[pos..pos + HASH_BYTES].try_into()?)?;
        pos += HASH_BYTES;
        let salt = Salt::from_slice(bytes[pos..pos + SALT_BYTES].try_into()?)?;
        pos += SALT_BYTES;
        let pow = Pow(bytes[pos]);
        pos += 1;
        let id_bytes = &bytes[pos..pos + types::ID_BYTES];

        // Recreate keys
        let signature_key_arr: &[u8; 32] = signature_key_bytes.try_into()?;
        let verification_key_arr: &[u8; 32] = verification_key_bytes.try_into()?;
        let signature_key = SignatureKey::from_bytes(signature_key_arr)?;
        let verification_key = VerificationKey::from_bytes_raw(verification_key_arr)?;
        let verification_key_bytes = verification_key.to_verification_key_bytes();
        let pq_commitment = PQCommitmentBytes::from_slice(pq_commitment_bytes)?;

        let keys = Keys {
            signature_key,
            verification_key,
            verification_key_bytes,
            pq_commitment_bytes: pq_commitment,
        };

        let id = Id::from_slice(id_bytes)?;

        Ok(ServerId {
            keys,
            sponsor_id,
            timestamp,
            hash,
            salt,
            pow,
            id,
        })
    }
}

impl fmt::Display for ServerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[ id={} pow={} hash={} salt={} keys={} ]", self.id,  self.pow, self.hash, self.salt, self.keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::parallel_pow_generator::StubParallelPowGenerator;
    use crate::tools::time_provider::time_provider::RealTimeProvider;

    #[tokio::test]
    async fn pow_test() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        const POW_MAX: u8 = 2 * 8;
        for pow_min in 0..POW_MAX {
            let server_id = ServerId::new(&time_provider, Pow(pow_min), true, &pow_generator).await?;
            assert!(server_id.pow >= Pow(pow_min));
        }

        Ok(())
    }

    #[tokio::test]
    async fn server_id_encode_decode_verify() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new(&time_provider, Pow(8), false, &pow_generator).await?;
        let encoded = server_id.encode()?;
        let decoded = ServerId::decode(&encoded)?;
        decoded.verify()?;
        Ok(())
    }

    #[tokio::test]
    async fn server_id_encode_decode_reversibility() -> anyhow::Result<()> {
        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new(&time_provider, Pow(8), false, &pow_generator).await?;

        let server_id_encoded = server_id.encode()?;
        let server_id2 = ServerId::decode(&server_id_encoded)?;

        // Thoroughly compare all fields
        // If Keys/Id derive PartialEq, this is enough:
        assert_eq!(server_id.keys.signature_key, server_id2.keys.signature_key, "Keys do not match after decode");
        assert_eq!(server_id.keys.verification_key, server_id2.keys.verification_key, "Keys do not match after decode");
        assert_eq!(server_id.keys.pq_commitment_bytes, server_id2.keys.pq_commitment_bytes, "Keys do not match after decode");
        assert_eq!(server_id.timestamp, server_id2.timestamp, "Timestamps do not match");
        assert_eq!(server_id.salt, server_id2.salt, "Salts do not match");
        assert_eq!(server_id.pow, server_id2.pow, "PoW bits do not match");
        assert_eq!(server_id.id, server_id2.id, "IDs do not match");

        Ok(())
    }
}
