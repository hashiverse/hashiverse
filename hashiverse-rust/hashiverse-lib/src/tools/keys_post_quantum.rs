//! # Post-Quantum Cryptography resilience
//!
//! ## The problem
//!
//! Hashiverse identities are derived from public keys:
//! `ID = Blake3[ ed25519_pub || Blake3[Falcon_pub][0..16] || Blake3[Dilithium_pub][0..16] ]`
//!
//! Because the identity *is* the key, there is no upgrade path that preserves identity —
//! switching algorithms would require replacing every server and client ID on the network.
//! Post-quantum support must therefore be baked in from the start, not bolted on later.
//!
//! ## Current design
//!
//! Rather than signing everything with PQ algorithms today (which carry large key and signature
//! sizes and lack `window.crypto.subtle` browser support), each identity commits to two PQ
//! public keys via their 16-byte Blake3 hashes:
//!
//! - **FN-DSA / Falcon (FIPS 205)** — smaller signatures than Dilithium; preferred first-step
//!   upgrade when Ed25519 is threatened.
//! - **ML-DSA / Dilithium (FIPS 204)** — fallback if Falcon is later compromised. Already
//!   under consideration for `window.crypto.subtle`, making it the most likely candidate for
//!   full browser-native signing support in future.
//!
//! The commitments are shipped with every message as part of the identity header; the full PQ
//! public keys are not shipped today and private keys remain unused until quantum-day.
//!
//! ## Upgrade path
//!
//! - **Ed25519 compromised** → network upgrades to Falcon. The full Falcon public key begins
//!   shipping with every message and is used to verify Falcon signatures. Receivers confirm
//!   `Blake3[Falcon_pub][0..16] == commitment_falcon` embedded in the sender's identity.
//! - **Falcon compromised** → network similarly upgrades to Dilithium.
//! - **Dilithium threatened** → a migration to future PQ algorithms would be needed, likely
//!   requiring a "please follow me on my new identity" protocol. The same protocol would also
//!   serve other identity migration use cases (e.g. a compromised key).
//!
//! ## Why not SPHINCS+ (FIPS 205)?
//!
//! SPHINCS+ has the strongest security assumptions of the standardised PQ algorithms, but its
//! signatures are multiple kilobytes — infeasible for a high-throughput messaging network.
//! A 16-byte commitment to a SPHINCS+ key was considered as an ultimate fallback, but the
//! cost of including it in every message header outweighs the benefit of covering an already
//! unlikely scenario (both Falcon and Dilithium broken simultaneously).

use crate::tools::types::PQCommitmentBytes;
use falcon_rust::falcon512;
use ml_dsa::{KeyGen, MlDsa44};
use ml_dsa::signature::Keypair;

pub fn pq_commitment_bytes_from_seed(seed: &[u8; 32]) -> anyhow::Result<PQCommitmentBytes> {
    // The Falcon PQ public key commitment
    let pq_commitment_falcon: [u8; 16] = {
        let falcon_seed: [u8; 32] = blake3::derive_key("hashiverse-pk-falcon", seed);
        let (_, verifying_key) = falcon512::keygen(falcon_seed);
        let verifying_key_bytes = verifying_key.to_bytes();

        let mut pq_commitment_falcon = [0u8; 16];
        let hash = blake3::hash(&verifying_key_bytes);
        pq_commitment_falcon.copy_from_slice(&hash.as_bytes()[..16]);

        pq_commitment_falcon
    };

    // The Dilithium public key commitment
    let pq_commitment_dilithium: [u8; 16] = {
        let mut dilithium_seed = [0u8; 32];
        dilithium_seed.copy_from_slice(&blake3::derive_key("hashiverse-pk-dilithium", seed));

        // trace!("Generating dilithium key from seed");
        let key_pair = MlDsa44::from_seed(&dilithium_seed.into());
        let verifying_key = key_pair.verifying_key();
        let verifying_key_bytes = verifying_key.encode();

        // *** TODO: This will only stabilise at v1c.0 of crate ml-dsa - until then the generated keys may change!!

        let mut pq_commitment_dilithium = [0u8; 16];
        let hash = blake3::hash(&verifying_key_bytes);
        pq_commitment_dilithium.copy_from_slice(&hash.as_bytes()[..16]);

        pq_commitment_dilithium
    };

    // Combine the PQ commitments into a single 32-byte array as we are constantly going to be hashing this and doing nothing else until quantum-day.
    let pq_commitment_bytes = PQCommitmentBytes::from(pq_commitment_falcon, pq_commitment_dilithium);
    Ok(pq_commitment_bytes)
}
