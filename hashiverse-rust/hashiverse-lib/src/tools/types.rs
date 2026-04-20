//! # Core 32-byte newtypes and cryptographic primitive wrappers
//!
//! The "nouns" of the hashiverse protocol. Every one of these is a transparent newtype
//! over a fixed-size byte array or an underlying crypto library type, so that
//! function signatures throughout the crate can encode their intent in the type system
//! rather than relying on correctly-named `[u8; 32]` parameters.
//!
//! ## Addressing
//!
//! - [`Id`] — 32-byte opaque identifier used pervasively (peer IDs, client IDs, post IDs,
//!   bucket location IDs, Kademlia keys). Ordered by natural byte comparison so it plays
//!   well with XOR-distance routing.
//! - [`struct@Hash`] — 32-byte Blake3 digest. Structurally equivalent to `Id` but
//!   semantically distinct — "a hash of something" vs "a handle to something".
//!
//! ## Cryptography
//!
//! - [`SignatureKey`] / [`VerificationKey`] — Ed25519 secret and public halves.
//! - [`VerificationKeyBytes`] — the wire-format serialisation of a public key.
//! - [`Signature`] — a 64-byte Ed25519 signature.
//! - [`PQCommitmentBytes`] — the 32-byte post-quantum commitment (see
//!   [`crate::tools::keys_post_quantum`]).
//!
//! ## Proof-of-work
//!
//! - [`Pow`] — a leading-zero-bit count, effectively a difficulty level.
//! - [`Salt`] — the random bytes searched over during PoW.
//!
//! Every type exposes `from_hex_str` / `to_hex_str`, `from_slice`, and `from_buf`
//! constructors so they can be produced from URLs, wire bytes, or `bytes::Buf` streams
//! with consistent error handling.

use crate::tools::time::{DurationMillis, TimeMillis};
use crate::tools::{hashing, tools};
use bytes::Buf;
use ed25519_dalek::pkcs8::{EncodePrivateKey, SecretDocument};
use serde::{Deserialize, Serialize};
use serde_with::{hex::Hex, serde_as};
use std::fmt;

fn from_buf<const N: usize>(buf: &mut impl Buf, field: &str) -> anyhow::Result<[u8; N]> {
    anyhow::ensure!(buf.remaining() >= N, "Buffer too short for {}: need {}, have {}", field, N, buf.remaining());
    let mut arr = [0u8; N];
    buf.copy_to_slice(&mut arr);
    Ok(arr)
}

pub const ID_BYTES: usize = 32;

/// A 32-byte opaque identifier used pervasively throughout the protocol.
///
/// `Id` is the common currency of addressing in hashiverse: peer IDs, client IDs, post IDs,
/// hashtag IDs, bucket location IDs, and Kademlia keys are all `Id` values. The 32-byte size
/// matches the output of Blake3 and the other cryptographic hashes in the PoW chain, so IDs
/// are typically derived by hashing some canonical content (see [`Id::from_hash`] and
/// [`Id::from_hashtag_str`]) — though they can also be random ([`Id::random`]).
///
/// XOR distance between IDs drives the Kademlia DHT routing logic in [`crate::client::peer_tracker`],
/// so `Id` also implements `Ord` to allow placement in sorted structures.
///
/// `Display` and `Debug` print only the first four bytes as hex followed by `...` to keep logs
/// readable; use [`Id::to_hex_str`] when you need the full value.
#[serde_as]
#[derive(Ord, PartialOrd, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id(#[serde_as(as = "Hex")] pub [u8; ID_BYTES]);

impl Id {
    pub fn zero() -> Self {
        let bytes = [0; ID_BYTES];
        Self(bytes)
    }

    pub fn is_zero(&self) -> bool {
        tools::are_all_zeros(&self.0)
    }

    pub fn random() -> Self {
        let mut bytes = [0; ID_BYTES];
        tools::random_fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_hex_str(str: &str) -> anyhow::Result<Self> {
        tools::from_hex_str::<Self, ID_BYTES>(str, Self)
    }

    pub fn to_hex_str(&self) -> String {
        hex::encode(self.0)
    }

    pub fn as_bytes(&self) -> &[u8; ID_BYTES] {
        &self.0
    }

    pub fn from_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        let arr: [u8; ID_BYTES] = bytes.try_into().map_err(|_| anyhow::anyhow!("Invalid id length: expected {}, got {}", ID_BYTES, bytes.len()))?;
        Ok(Self(arr))
    }

    pub fn from_buf(buf: &mut impl Buf, field: &str) -> anyhow::Result<Self> {
        Ok(Self(from_buf::<ID_BYTES>(buf, field)?))
    }

    pub fn from_hash(hash: Hash) -> anyhow::Result<Id> {
        if hash.len() != 32 {
            anyhow::bail!("Invalid Hash length: expected 32 bytes, got {} bytes", hash.len());
        }

        let id = Id(hash.to_bytes());
        Ok(id)
    }

    pub fn from_hashtag_str(hashtag_str: &str) -> anyhow::Result<Id> {
        let lowercase_str = hashtag_str.to_lowercase();
        let str_stripped = lowercase_str.strip_prefix('#').unwrap_or(&lowercase_str);
        let hash = hashing::hash(str_stripped.as_bytes());
        Id::from_hash(hash)
    }
}

impl AsRef<[u8]> for Id {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}...", hex::encode(&self.0[0..4]))
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

pub const SALT_BYTES: usize = 8;

/// An 8-byte nonce used to vary the input of a proof-of-work search.
///
/// PoW in hashiverse is computed by hashing a fixed "data hash" concatenated with a `Salt`
/// and counting the leading zero bits of the result (see [`crate::tools::pow`]). The worker
/// keeps re-randomizing the `Salt` to explore the hash space until it finds one whose hash
/// satisfies the required [`Pow`] difficulty. The salt that succeeds is transmitted alongside
/// the payload so verifiers can re-compute the same hash without re-searching.
///
/// `Salt` is deliberately small (8 bytes) because it is only a search-space tag — it does not
/// need to be unpredictable to an adversary, only varied across attempts.
#[serde_as]
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Salt(#[serde_as(as = "Hex")] pub [u8; SALT_BYTES]);

impl Salt {
    pub fn zero() -> Self {
        Self([0; SALT_BYTES])
    }

    pub fn random() -> Self {
        let mut bytes = [0; SALT_BYTES];
        tools::random_fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        let arr: [u8; SALT_BYTES] = bytes.try_into().map_err(|_| anyhow::anyhow!("Invalid salt length: expected {}, got {}", SALT_BYTES, bytes.len()))?;
        Ok(Self(arr))
    }

    pub fn from_buf(buf: &mut impl Buf, field: &str) -> anyhow::Result<Self> {
        Ok(Self(from_buf::<SALT_BYTES>(buf, field)?))
    }

    pub fn randomize(&mut self) {
        tools::random_fill_bytes(&mut self.0);
    }
}

impl AsRef<[u8]> for Salt {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl std::ops::Deref for Salt {
    type Target = [u8; SALT_BYTES];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Display for Salt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let all_zero = self.0.iter().all(|&b| b == 0);
        match all_zero {
            true => write!(f, "0"),
            false => write!(f, "..."),
        }
    }
}

impl fmt::Debug for Salt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

pub const SIGNATURE_BYTES: usize = 64;

/// A 64-byte Ed25519 signature.
///
/// Every authenticated artefact in the protocol — RPC responses, peer announcements, encoded
/// posts, post-bundle headers, meta posts — carries a `Signature` produced by the author's
/// [`SignatureKey`] over a canonical byte representation of the artefact. Verification is
/// performed with the matching [`VerificationKey`] (typically obtained from a [`Peer`]'s
/// identity). The concrete signing algorithm is Ed25519 via `ed25519-dalek`; post-quantum
/// signatures (ML-DSA, FN-DSA) are layered on top via the PQ commitment on [`ClientId`]
/// rather than replacing this type.
#[serde_as]
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature(#[serde_as(as = "Hex")] pub [u8; SIGNATURE_BYTES]);

impl Signature {
    pub fn as_bytes(&self) -> &[u8; SIGNATURE_BYTES] {
        &self.0
    }

    pub fn from_bytes_exact(bytes: [u8; SIGNATURE_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn from_hex_str(str: &str) -> anyhow::Result<Self> {
        tools::from_hex_str::<Self, SIGNATURE_BYTES>(str, Self)
    }

    pub fn to_hex_str(&self) -> String {
        hex::encode(self.0)
    }


    pub fn from_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        let arr: [u8; SIGNATURE_BYTES] = bytes.try_into().map_err(|_| anyhow::anyhow!("Invalid signature length: expected {}, got {}", SIGNATURE_BYTES, bytes.len()))?;
        Ok(Self(arr))
    }

    pub fn zero() -> Self {
        Self([0; SIGNATURE_BYTES])
    }

    pub fn random() -> Self {
        let mut bytes = [0; SIGNATURE_BYTES];
        tools::random_fill_bytes(&mut bytes);
        Self(bytes)
    }

}

impl fmt::Display for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}...", hex::encode(&self.0[0..4]))
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::ops::Deref for Signature {
    type Target = [u8; SIGNATURE_BYTES];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// Implement From for easy conversion
impl From<[u8; SIGNATURE_BYTES]> for Signature {
    fn from(bytes: [u8; SIGNATURE_BYTES]) -> Self {
        Self(bytes)
    }
}

impl From<Signature> for [u8; SIGNATURE_BYTES] {
    fn from(sig: Signature) -> Self {
        sig.0
    }
}

/// A private Ed25519 signing key held by a single client identity.
///
/// `SignatureKey` is the secret half of the identity established by [`ClientId`]: it is what
/// a [`crate::client::key_locker::KeyLocker`] guards on behalf of the logged-in user and uses
/// to sign outbound posts, RPC responses, and peer announcements. Never serialize or leak this
/// value across trust boundaries. The matching public half is obtained via
/// [`SignatureKey::verification_key`] and is published freely as part of the client's identity.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct SignatureKey(pub ed25519_dalek::SigningKey);

impl SignatureKey {
    pub fn from_bytes(bytes: &[u8; SIGNATURE_KEY_BYTES]) -> anyhow::Result<Self> {
        Ok(Self(ed25519_dalek::SigningKey::from_bytes(bytes)))
    }

    pub fn verification_key(&self) -> VerificationKey {
        VerificationKey(self.0.verifying_key())
    }

    pub fn to_pkcs8_der(&self) -> anyhow::Result<SecretDocument> {
        Ok(self.0.to_pkcs8_der()?)
    }
}

impl AsRef<[u8]> for SignatureKey {
    fn as_ref(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VerificationKey(pub ed25519_dalek::VerifyingKey);

impl VerificationKey {
    pub fn from_bytes_raw(bytes: &[u8; VERIFICATION_KEY_BYTES]) -> anyhow::Result<Self> {
        Ok(Self(ed25519_dalek::VerifyingKey::from_bytes(bytes)?))
    }

    pub fn from_bytes(bytes: &VerificationKeyBytes) -> anyhow::Result<Self> {
        Ok(Self(ed25519_dalek::VerifyingKey::from_bytes(&bytes.0)?))
    }

    pub fn to_verification_key_bytes(&self) -> VerificationKeyBytes {
        VerificationKeyBytes(self.0.to_bytes())
    }

    pub fn to_hex(&self) -> String {
        let vkb = self.to_verification_key_bytes();
        hex::encode(vkb)
    }

    pub fn from_hex(hex_str: &str) -> anyhow::Result<Self> {
        let bytes = hex::decode(hex_str)?;
        if bytes.len() != VERIFICATION_KEY_BYTES {
            anyhow::bail!("Invalid hex string length for VerificationKeyBytes");
        }

        Ok(Self(ed25519_dalek::VerifyingKey::from_bytes(<&[u8; 32]>::try_from(bytes.as_slice())?)?))
    }
}

impl AsRef<[u8]> for VerificationKey {
    fn as_ref(&self) -> &[u8] {
        self.0.as_bytes()
    }
}


pub const SIGNATURE_KEY_BYTES: usize = 32;
#[serde_as]
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SignatureKeyBytes(#[serde_as(as = "Hex")] pub [u8; SIGNATURE_KEY_BYTES]);

pub const VERIFICATION_KEY_BYTES: usize = 32;

#[serde_as]
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct VerificationKeyBytes(#[serde_as(as = "Hex")] pub [u8; VERIFICATION_KEY_BYTES]);

impl VerificationKeyBytes {
    pub fn zero() -> Self {
        Self([0; VERIFICATION_KEY_BYTES])
    }
    pub fn to_verification_key(&self) -> anyhow::Result<VerificationKey> {
        Ok(VerificationKey(ed25519_dalek::VerifyingKey::from_bytes(&self.0).map_err(|_| anyhow::anyhow!("invalid verification key bytes"))?))
    }
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
    pub fn from_hex_str(str: &str) -> anyhow::Result<Self> {
        tools::from_hex_str::<Self, VERIFICATION_KEY_BYTES>(str, Self)
    }
}

impl AsRef<[u8]> for VerificationKeyBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

pub const PQ_COMMITMENT_BYTES: usize = 32;
#[serde_as]
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PQCommitmentBytes(#[serde_as(as = "Hex")] pub [u8; PQ_COMMITMENT_BYTES]);

impl PQCommitmentBytes {

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex_str(str: &str) -> anyhow::Result<Self> {
        tools::from_hex_str::<Self, PQ_COMMITMENT_BYTES>(str, Self)
    }


    pub fn zero() -> Self {
        Self([0; PQ_COMMITMENT_BYTES])
    }

    pub fn from_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        let arr: [u8; PQ_COMMITMENT_BYTES] = bytes.try_into().map_err(|_| anyhow::anyhow!("Invalid PQCommitmentBytes length: expected {}, got {}", PQ_COMMITMENT_BYTES, bytes.len()))?;
        Ok(Self(arr))
    }
}

impl AsRef<[u8]> for PQCommitmentBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}


impl PQCommitmentBytes {
    pub fn from(pq_commitment_falcon: [u8; 16], pq_commitment_dilithium: [u8; 16]) -> Self {
        let mut combined = [0u8; 32];
        combined[..16].copy_from_slice(&pq_commitment_falcon);
        combined[16..].copy_from_slice(&pq_commitment_dilithium);
        PQCommitmentBytes(combined)
    }
}

pub const HASH_BYTES: usize = 32;

/// A 32-byte cryptographic hash, the standard hash output throughout the protocol.
///
/// For general-purpose hashing (content addressing, ID derivation, deduplication) hashiverse
/// uses Blake3 via [`crate::tools::hashing`]. For proof-of-work, a longer chain spanning
/// Blake2, Blake3, SHA2, SHA3, Whirlpool, Groestl, and Skein is used instead to resist
/// ASIC-style shortcuts — see [`crate::tools::pow`]. Either way the 32-byte `Hash` is the
/// canonical output. [`Id`] is a sibling newtype over the same byte length and conversion
/// between them is cheap via [`Id::from_hash`].
///
/// `Display` and `Debug` deliberately hide the bytes (printing `"..."` or `"0"`) to avoid
/// noisy logs; use the slice accessors or hex helpers when the raw value is needed.
#[serde_as]
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hash(#[serde_as(as = "Hex")] pub [u8; HASH_BYTES]);

impl Hash {
    pub fn zero() -> Self {
        Self([0; HASH_BYTES])
    }

    pub fn random() -> Self {
        let mut bytes = [0; HASH_BYTES];
        tools::random_fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn to_bytes(&self) -> [u8; HASH_BYTES] {
        self.0
    }

    pub fn as_bytes(&self) -> &[u8; HASH_BYTES] {
        &self.0
    }

    pub fn from_slice(bytes: &[u8]) -> anyhow::Result<Self> {
        if bytes.len() != HASH_BYTES {
            anyhow::bail!("incorrect byte count: expected {}, got {}", HASH_BYTES, bytes.len())
        }

        let mut arr = [0u8; HASH_BYTES];
        arr.copy_from_slice(bytes);
        Ok(Self(arr))
    }

    pub fn randomize(&mut self) {
        tools::random_fill_bytes(&mut self.0);
    }
}

impl AsRef<[u8]> for Hash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl std::ops::Deref for Hash {
    type Target = [u8; HASH_BYTES];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let all_zero = self.0.iter().all(|&b| b == 0);
        match all_zero {
            true => write!(f, "0"),
            false => write!(f, "..."),
        }
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl From<[u8; HASH_BYTES]> for Hash {
    fn from(bytes: [u8; HASH_BYTES]) -> Hash {
        Hash(bytes)
    }
}

impl From<&[u8; HASH_BYTES]> for Hash {
    fn from(bytes: &[u8; HASH_BYTES]) -> Hash {
        Hash(*bytes)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct BucketKey {
    pub base_id: Id,
    pub timestamp: TimeMillis,
    pub granularity: DurationMillis,
    pub location_id: Id,
}

/// A proof-of-work difficulty level, expressed as the number of leading zero bits required
/// on the PoW hash output.
///
/// PoW is embedded throughout the protocol: in every RPC packet (see
/// [`crate::protocol::rpc`]), on peer announcements (see [`crate::protocol::peer`]), and on
/// reporting / feedback. Encoding difficulty as a single byte keeps packets compact and keeps
/// comparison cheap — `Ord` on `Pow` is the natural "harder or easier" ordering. Values are
/// produced by the chained-hash measurement in [`crate::tools::pow::pow_measure_from_data_hash`]
/// and searched for via [`ParallelPowGenerator`].
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[serde(transparent)]
pub struct Pow(pub u8);

impl Pow {
    pub const fn new(val: u8) -> Self {
        Self(val)
    }

    pub fn as_u8(self) -> u8 {
        self.0
    }
}

impl std::fmt::Display for Pow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u8> for Pow {
    fn from(val: u8) -> Self {
        Self(val)
    }
}

impl From<Pow> for u8 {
    fn from(pow: Pow) -> u8 {
        pow.0
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::keys::Keys;
    use crate::tools::types::{Id, Salt, VerificationKey};

    #[tokio::test]
    async fn salt_display_zero_test() {
        let salt = Salt::zero();
        assert_eq!(format!("{}", salt), "0");
    }

    #[tokio::test]
    async fn salt_display_nonzero_test() {
        let salt = Salt::random();
        assert_eq!(format!("{}", salt), "...");
    }

    #[tokio::test]
    async fn test_hash_to_base_id() -> anyhow::Result<()> {
        let hash = "walking";
        let hash1 = "#walking";
        let hash2 = "WaLkINg";
        let hash3 = "#WAlkING";
        let hash4 = "TALKING";

        let id = Id::from_hashtag_str(hash)?;
        let id1 = Id::from_hashtag_str(hash1)?;
        let id2 = Id::from_hashtag_str(hash2)?;
        let id3 = Id::from_hashtag_str(hash3)?;
        let id4 = Id::from_hashtag_str(hash4)?;

        assert_eq!(id, id1, "Hash to id should be the same");
        assert_eq!(id, id2, "Hash to id should be the same");
        assert_eq!(id, id3, "Hash to id should be the same");
        assert_ne!(id, id4, "Hash to id should not be the same");
        Ok(())
    }

    #[tokio::test]
    async fn test_id_to_str_reversible() -> anyhow::Result<()> {
        let id = Id::random();
        let id_str = id.to_hex_str();
        let id_from_str = Id::from_hex_str(&id_str)?;
        assert_eq!(id, id_from_str, "Id to string and string to id should be reversible");
        Ok(())
    }

    #[tokio::test]
    async fn test_verification_key() -> anyhow::Result<()> {
        let keys = Keys::from_phrase("this is a test")?;
        let hex = keys.verification_key.to_hex();
        let vfk = VerificationKey::from_hex(&hex)?;
        assert_eq!(keys.verification_key, vfk, "Verification key to hex and hex to verification key should be reversible");
        Ok(())
    }
}
