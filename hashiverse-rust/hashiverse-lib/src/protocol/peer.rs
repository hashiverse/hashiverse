//! # Peer records — identity, reputation, and PoW credentials for gossip
//!
//! Three types make up a peer's on-the-wire reputation:
//!
//! - [`Peer`] — the self-signed identity record gossiped through the Kademlia DHT. Carries
//!   the peer's Ed25519 public key, post-quantum commitment, network address, and
//!   **three** PoW tokens (see [`PeerPow`]): the initial "birth certificate", the best
//!   PoW observed during the current rolling day, and during the current rolling month.
//!   Rolling PoW means a peer that has continued to do work recently is weighted higher
//!   than one that submitted a good initial PoW and went quiet, without every peer
//!   having to redo heavy work on every gossip cycle.
//! - [`PeerPow`] — a PoW token bound to a specific `content_hash` and timestamp. Because
//!   the content_hash is part of the proof, these tokens can't be replayed to sign a
//!   *different* message.
//! - [`ClientPow`] — bundles a peer's public key + PQ commitment with its *hardest-ever*
//!   PoW, used to gate trust-sensitive operations (healing, feedback amplification).
//!
//! Every field uses compact `#[serde(rename)]` wire keys so gossiped peer blobs stay
//! small even at DHT scale.

use crate::tools::server_id::ServerId;
use crate::tools::time::{MILLIS_IN_DAY, MILLIS_IN_MONTH, TimeMillis, TimeMillisBytes};
use crate::tools::time_provider::time_provider::TimeProvider;
use crate::tools::types::{Hash, Id, PQCommitmentBytes, Pow, Salt, Signature, SignatureKey, VerificationKey, VerificationKeyBytes};
use crate::tools::{config, hashing, signing, tools};
use crate::{anyhow_assert_eq, anyhow_assert_ge};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A proof-of-work token sponsored by a specific peer and bound to a specific request body.
///
/// `PeerPow` is the standard "I did work, and I did it for *this* request" credential. It
/// fixes four things into the PoW input: the sponsor's peer [`Id`], the time the work was
/// performed, a `content_hash` over the request body, and a [`Salt`] that the worker varied
/// to find a solution. Pinning the content hash is what prevents an attacker from replaying
/// a valid PoW against a different request — re-using the same `salt` against a new body
/// would produce a different hash and fail verification.
///
/// `PeerPow` is worn by [`Peer`] itself (three of them: the initial identity PoW plus
/// rolling day/month "best-effort" PoWs) and by every authenticated RPC (see
/// [`crate::protocol::rpc`]). Because its inputs are canonical and verification is cheap,
/// receivers can re-derive and compare the PoW without keeping state.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct PeerPow {
    #[serde(rename = "sp")]
    pub sponsor_id: Id,
    #[serde(rename = "t")]
    pub timestamp: TimeMillis,
    #[serde(rename = "ch")]
    pub content_hash: Hash, // This is the hash of the content of the request that the pow is protecting.  Stops replay attacks with the same pow being used for different requests.
    #[serde(rename = "s")]
    pub salt: Salt,
    #[serde(rename = "z")]
    pub pow: Pow,
}

impl PeerPow {
    pub fn new(sponsor_id: Id, verification_key: &VerificationKeyBytes, pq_commitment_bytes: &PQCommitmentBytes, timestamp: TimeMillis, content_hash: Hash, salt: Salt) -> anyhow::Result<PeerPow> {
        let (pow, _) = Self::pow(&sponsor_id, verification_key, pq_commitment_bytes, &timestamp.encode_be(), &content_hash, &salt)?;
        Ok(Self {
            sponsor_id,
            timestamp,
            content_hash,
            salt,
            pow,
        })
    }

    pub fn zero() -> Self {
        PeerPow {
            sponsor_id: Id::zero(),
            timestamp: TimeMillis::zero(),
            content_hash: Hash::zero(),
            salt: Salt::zero(),
            pow: Pow(0),
        }
    }

    pub fn random() -> Self {
        Self {
            sponsor_id: Id::random(),
            timestamp: TimeMillis::random(),
            content_hash: Hash::random(),
            salt: Salt::random(),
            pow: Pow(tools::random_u8()),
        }
    }

    pub fn verify(&self, verification_key: &VerificationKeyBytes, pq_commitment_bytes: &PQCommitmentBytes) -> anyhow::Result<()> {
        let (pow, _) = Self::pow(&self.sponsor_id, verification_key, pq_commitment_bytes, &self.timestamp.encode_be(), &self.content_hash, &self.salt)?;
        match pow == self.pow {
            true => Ok(()),
            false => Err(anyhow::anyhow!("pow does not match inputs: {} != {}", self.pow, pow)),
        }
    }

    pub fn pow_decayed_day(&self, current_time_millis: TimeMillis) -> f64 {
        let pow_halflife_millis = MILLIS_IN_DAY;
        let elapsed_millis = current_time_millis - self.timestamp;
        (self.pow.0 as f64) / 2.0f64.powf(elapsed_millis.0 as f64 / pow_halflife_millis.0 as f64)
    }

    pub fn pow_decayed_month(&self, current_time_millis: TimeMillis) -> f64 {
        let pow_halflife_millis = MILLIS_IN_MONTH;
        let elapsed_millis = current_time_millis - self.timestamp;
        (self.pow.0 as f64) / 2.0f64.powf(elapsed_millis.0 as f64 / pow_halflife_millis.0 as f64)
    }

    pub fn pow(sponsor_id: &Id, verification_key: &VerificationKeyBytes, pq_commitment_bytes: &PQCommitmentBytes, timestamp: &TimeMillisBytes, hash: &Hash, salt: &Salt) -> anyhow::Result<(Pow, Hash)> {
        ServerId::pow_measure(sponsor_id, verification_key, pq_commitment_bytes, timestamp, hash, salt)
    }

    pub fn own_pow(&self, verification_key: &VerificationKeyBytes, pq_commitment_bytes: &PQCommitmentBytes) -> anyhow::Result<(Pow, Hash)> {
        Self::pow(&self.sponsor_id, verification_key, pq_commitment_bytes, &self.timestamp.encode_be(), &self.content_hash, &self.salt)
    }
}

/// A client-side best-ever proof-of-work token, used as a reputation credential in the
/// trust-sensitive parts of the protocol.
///
/// Unlike [`PeerPow`], which is scoped to a single request, `ClientPow` is the *hardest*
/// PoW a particular client has ever produced against a canonical sponsor target. It is
/// carried on feedback and healing requests — the operations that let peers influence what
/// content is propagated or suppressed — so the recipient can weight the sender's opinion
/// by how much work they have ever invested in an identity. This makes Sybil-style attacks
/// expensive: voting with many fresh clients means doing all that work many times.
///
/// The struct bundles the peer's verification key, PQ commitment, and the underlying
/// [`PeerPow`] so a verifier has everything it needs in one place.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct ClientPow {
    pub peer_verification_key: VerificationKeyBytes,
    pub peer_pq_commitment_bytes: PQCommitmentBytes,
    pub peer_pow: PeerPow,
}

impl ClientPow {
    pub fn measure(peer_verification_key: &VerificationKeyBytes, peer_pq_commitment_bytes: &PQCommitmentBytes, sponsor_id: &Id, timestamp: TimeMillis, content_hash: &Hash, salt: &Salt) -> anyhow::Result<Self> {
        let (pow, _) = ServerId::pow_measure(sponsor_id, peer_verification_key, peer_pq_commitment_bytes, &timestamp.encode_be(), content_hash, salt)?;
        Ok(Self {
            peer_verification_key: *peer_verification_key,
            peer_pq_commitment_bytes: *peer_pq_commitment_bytes,
            peer_pow: PeerPow {
                sponsor_id: *sponsor_id,
                timestamp,
                content_hash: *content_hash,
                salt: *salt,
                pow,
            },
        })
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        self.peer_pow.verify(&self.peer_verification_key, &self.peer_pq_commitment_bytes)
    }
}

impl fmt::Display for PeerPow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "timestamp={} hash={} salt={} pow={}", self.timestamp, hex::encode(self.content_hash), hex::encode(self.salt), self.pow,)
    }
}

/// A single node's self-signed identity record, gossiped through the Kademlia DHT.
///
/// `Peer` is the unit of peer discovery. Each server publishes one: its [`Id`], its Ed25519
/// verification key, its post-quantum commitment, its current network address, its build
/// version, and three [`PeerPow`] tokens — one fixed "initial" PoW that was produced at
/// startup and never changes (so it acts as a long-term identity credential), plus rolling
/// "best seen this day" and "best seen this month" PoWs that keep the record fresh and let
/// other peers weight trust by recent work. The whole thing is covered by a `signature`
/// so tampering is detectable.
///
/// Other peers store, forward, and compare `Peer` records to decide who to talk to, which
/// is why the PoW fields have time decay built in (`pow_decayed_day`, `pow_decayed_month`)
/// — stale records age out naturally.
#[derive(Serialize, Deserialize, PartialEq, Clone)]
pub struct Peer {
    pub id: Id,

    #[serde(rename = "vkb")]
    pub verification_key_bytes: VerificationKeyBytes,
    #[serde(rename = "pqcb")]
    pub pq_commitment_bytes: PQCommitmentBytes,

    #[serde(rename = "pi")]
    pub pow_initial: PeerPow, // The pow generated when the server first started up.  Never changes.
    #[serde(rename = "pcd")]
    pub pow_current_day: PeerPow, // The best pow seen this day (approximately)
    #[serde(rename = "pcm")]
    pub pow_current_month: PeerPow, // The best pow seen this month (approximately)

    #[serde(rename = "ver")]
    pub version: String, // The compile version of the Peer
    #[serde(rename = "addr")]
    pub address: String, // The last known public address at which this server knows itself.

    #[serde(rename = "ts")]
    pub timestamp: TimeMillis, // The last time this record was updated - used for other Peers to overwrite stale records.

    #[serde(rename = "sig")]
    pub signature: Signature, // sign( hash ( id, pow_initial.pow(signature_pub), pow_current.pow(signature_pub), address, signature_timestamp ) )
}

impl fmt::Debug for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[ id={} version={} address={} pows=[ {} {} {} ] timestamp={} ]",
            self.id, self.version, self.address, self.pow_initial.pow, self.pow_current_day.pow, self.pow_current_month.pow, self.timestamp
        )

        // write!(
        //     f,
        //     "[ id={} signature_pub={} encryption_pub={} pow_initial={} pow_current={} address={} signature_timestamp={} signature={} ]",
        //     hex::encode(self.id),
        //     hex::encode(self.signature_pub),
        //     hex::encode(self.encryption_pub),
        //     self.pow_initial,
        //     self.pow_current,
        //     self.address,
        //     self.signature_timestamp,
        //     hex::encode(self.signature),
        // )
    }
}

impl Peer {
    pub fn zero() -> Peer {
        Peer {
            id: Id::zero(),
            verification_key_bytes: VerificationKeyBytes::zero(),
            pq_commitment_bytes: PQCommitmentBytes::zero(),

            pow_initial: PeerPow::zero(),
            pow_current_day: PeerPow::zero(),
            pow_current_month: PeerPow::zero(),

            version: env!("CARGO_PKG_VERSION").to_string(),
            address: String::new(),

            timestamp: TimeMillis::zero(),

            signature: Signature::zero(),
        }
    }

    pub fn signature_hash_generate(&self) -> anyhow::Result<Hash> {
        Ok(hashing::hash_multiple(&[
            self.id.as_ref(),
            self.verification_key_bytes.as_ref(),
            self.pq_commitment_bytes.as_ref(),
            self.pow_initial.own_pow(&self.verification_key_bytes, &self.pq_commitment_bytes)?.1.as_ref(),
            self.pow_current_day.own_pow(&self.verification_key_bytes, &self.pq_commitment_bytes)?.1.as_ref(),
            self.pow_current_month.own_pow(&self.verification_key_bytes, &self.pq_commitment_bytes)?.1.as_ref(),
            self.address.as_bytes(),
            self.timestamp.encode_be().as_ref(),
        ]))
    }

    pub fn sign(&mut self, time_provider: &dyn TimeProvider, signature_key: &SignatureKey) -> anyhow::Result<()> {
        self.timestamp = time_provider.current_time_millis();

        let signature_hash = self.signature_hash_generate()?;
        self.signature = signing::sign(signature_key, signature_hash.as_ref());
        Ok(())
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        self.verify_signature()?;
        self.verify_server_id()?;
        self.verify_pows()?;
        Ok(())
    }

    fn verify_signature(&self) -> anyhow::Result<()> {
        let signature_hash = self.signature_hash_generate()?;
        signing::verify(&VerificationKey::from_bytes(&self.verification_key_bytes)?, &self.signature, signature_hash.as_ref())?;

        Ok(())
    }

    fn verify_server_id(&self) -> anyhow::Result<()> {
        let (pow, pow_hash) = ServerId::pow_measure(
            &self.pow_initial.sponsor_id,
            &self.verification_key_bytes,
            &self.pq_commitment_bytes,
            &self.pow_initial.timestamp.encode_be(),
            &self.pow_initial.content_hash,
            &self.pow_initial.salt,
        )?;
        anyhow_assert_ge!(&pow, &config::SERVER_KEY_POW_MIN, "insufficient server_id pow");

        let id = ServerId::server_pow_hash_to_id(pow_hash)?;

        anyhow_assert_eq!(&self.id, &id, "served_id mismatch");

        Ok(())
    }

    fn verify_pows(&self) -> anyhow::Result<()> {
        self.pow_initial.verify(&self.verification_key_bytes, &self.pq_commitment_bytes)?;
        self.pow_current_day.verify(&self.verification_key_bytes, &self.pq_commitment_bytes)?;
        self.pow_current_month.verify(&self.verification_key_bytes, &self.pq_commitment_bytes)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::peer::{Peer, PeerPow};
    use crate::tools::server_id::ServerId;
    use crate::tools::time::TimeMillis;
    use crate::tools::time_provider::time_provider::RealTimeProvider;
    use crate::tools::parallel_pow_generator::StubParallelPowGenerator;
    use crate::tools::tools;
    use crate::tools::types::{Pow, VerificationKeyBytes};

    async fn get_random_ingredients() -> anyhow::Result<(ServerId, Peer)> {
        let time_provider = RealTimeProvider::default();
        let pow_generator = StubParallelPowGenerator::new();
        let server_id = ServerId::new("own_pow", &time_provider, Pow(0), true, &pow_generator).await?;

        let mut peer = server_id.to_peer(&time_provider)?;
        peer.pow_current_day = PeerPow::random();
        peer.pow_current_month = PeerPow::random();
        peer.address = tools::random_base64(16);

        // Sign again now that we have changed some of the data
        peer.sign(&time_provider, &server_id.keys.signature_key)?;

        Ok((server_id, peer))
    }

    #[tokio::test]
    async fn signing_test() -> anyhow::Result<()> {
        let (_, peer) = get_random_ingredients().await?;

        // Now verify the signature
        peer.verify_signature()?;
        Ok(())
    }

    #[tokio::test]
    async fn signing_fail_test() -> anyhow::Result<()> {
        {
            let (_, mut peer) = get_random_ingredients().await?;
            peer.verification_key_bytes = VerificationKeyBytes::zero();
            peer.verify_signature().unwrap_err();
        }

        {
            let (_, mut peer) = get_random_ingredients().await?;
            peer.pow_initial = PeerPow::random();
            peer.verify_signature().unwrap_err();
        }

        {
            let (_, mut peer) = get_random_ingredients().await?;
            peer.pow_current_day = PeerPow::random();
            peer.verify_signature().unwrap_err();
        }

        {
            let (_, mut peer) = get_random_ingredients().await?;
            peer.pow_current_month = PeerPow::random();
            peer.verify_signature().unwrap_err();
        }

        {
            let (_, mut peer) = get_random_ingredients().await?;
            peer.address = tools::random_base64(16);
            peer.verify_signature().unwrap_err();
        }

        {
            let (_, mut peer) = get_random_ingredients().await?;
            peer.timestamp = TimeMillis::random();
            peer.verify_signature().unwrap_err();
        }

        Ok(())
    }
}
