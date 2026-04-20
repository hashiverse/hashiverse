//! # Stable client (user-account) identity on the network
//!
//! A [`ClientId`] is the self-describing identity of a single user. It bundles:
//! - the Ed25519 public key (`verification_key_bytes`) used to verify everything the client
//!   signs (posts, feedback, follows, …);
//! - a 32-byte post-quantum commitment (`pq_commitment_bytes`) — see
//!   [`crate::tools::keys_post_quantum`] — that future-proofs the identity against Ed25519
//!   breakage without requiring a new identity;
//! - a derived 32-byte [`crate::tools::types::Id`] that is the Blake3 hash of the two above.
//!
//! Because the `id` is derived from the other two fields, any tampering is detectable by
//! [`ClientId::verify`]: the recomputed id simply won't match. Wherever the protocol refers
//! to "who authored this" (post headers, RPC responses, peer records) it is referring to a
//! `ClientId`.
//!
//! This is distinct from [`crate::tools::server_id::ServerId`], which is the analogous
//! identity for a server and additionally carries a proof-of-work "birth certificate".

use crate::tools::types::{Id, PQCommitmentBytes, VerificationKeyBytes};
use crate::tools::{hashing};
use serde::{Deserialize, Serialize};
use std::fmt;

/// The stable identity of a single client (user account) on the network.
///
/// A `ClientId` binds together three pieces:
/// - an Ed25519 `verification_key_bytes`, the public half of the client's [`crate::tools::types::SignatureKey`],
/// - a `pq_commitment_bytes` — a 32-byte post-quantum commitment (Falcon + Dilithium concatenation)
///   that future-proofs the identity against breakage of Ed25519, and
/// - a derived 32-byte [`crate::tools::types::Id`] which is the Blake3 hash of the other two
///   fields concatenated.
///
/// Because the `id` is deterministically derived from the other two fields, tampering with
/// either half is detectable via [`ClientId::verify`]: the recomputed id will not match the
/// one carried on the struct. This is the identity anchor used wherever the protocol refers
/// to "who authored this" — e.g. on posts, RPC responses, and peer records.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct ClientId {
    pub verification_key_bytes: VerificationKeyBytes,
    pub pq_commitment_bytes: PQCommitmentBytes,
    pub id: Id,
}

impl ClientId {
    pub fn id_hex(&self) -> String {
        hex::encode(self.id)
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "id={}", self.id_hex(), )
    }
}

impl ClientId {
    pub fn new(verification_key_bytes: VerificationKeyBytes, pq_commitment_bytes: PQCommitmentBytes) -> anyhow::Result<Self> {
        let id = ClientId::id_from_parts(&verification_key_bytes, &pq_commitment_bytes)?;
        Ok(Self {
            verification_key_bytes,
            pq_commitment_bytes,
            id,
        })
    }

    pub fn verify(&self) -> anyhow::Result<()> {
        let id = ClientId::id_from_parts(&self.verification_key_bytes, &self.pq_commitment_bytes)?;
        if id != self.id {
            anyhow::bail!("ClientID pow does not verify");
        }

        Ok(())
    }

    pub fn id_from_parts(verification_key_bytes: &VerificationKeyBytes, pq_commitment_bytes: &PQCommitmentBytes) -> anyhow::Result<Id> {
        let hash = hashing::hash_multiple(&[verification_key_bytes.as_ref(), pq_commitment_bytes.as_ref()]);
        Id::from_hash(hash)
    }
}

#[cfg(test)]
mod tests {
    use crate::tools::client_id::ClientId;
    use crate::tools::keys::Keys;

    #[tokio::test]
    async fn verify_test() -> anyhow::Result<()> {
        let keys = Keys::from_rnd(false)?;
        let client_id = ClientId::new(keys.verification_key_bytes, keys.pq_commitment_bytes)?;
        client_id.verify()?;

        // Now mess with something in the verification key
        {
            let mut client_id = client_id.clone();
            client_id.verification_key_bytes.0[0] = 255 - client_id.verification_key_bytes.0[0];
            let result = client_id.verify();
            anyhow::ensure!(result.is_err(), "Verification should fail when verification key is modified");
        }

        // Now mess with something in the pq_commitment_bytes
        {
            let mut client_id = client_id.clone();
            client_id.pq_commitment_bytes.0[0] = 255 - client_id.pq_commitment_bytes.0[0];
            let result = client_id.verify();
            anyhow::ensure!(result.is_err(), "Verification should fail when pq_commitment_bytes is modified");
        }

        Ok(())
    }
}
