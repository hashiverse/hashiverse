//! # Ed25519 signing and verification helpers
//!
//! Thin wrappers over `ed25519-dalek` that accept and return hashiverse's newtype-wrapped
//! [`Signature`], [`SignatureKey`] and [`VerificationKey`] instead of raw byte arrays.
//!
//! Three functions cover the common cases:
//! - [`sign`] — sign a single slice directly.
//! - [`sign_multiple`] — Blake3-hash a list of slices then sign the 32-byte digest. Used
//!   anywhere the "message" is a tuple of fields (post headers, bucket commitments, peer
//!   announcements) — cheaper and produces a deterministic canonical form regardless of
//!   field sizes.
//! - [`verify`] — verify an Ed25519 signature against its signed data.
//!
//! Post-quantum fallbacks (Falcon, Dilithium) live in [`crate::tools::keys_post_quantum`]
//! and are invoked only once the network signals a transition away from Ed25519.

use crate::tools::hashing;
use crate::tools::types::{Signature, SignatureKey, VerificationKey};

pub fn sign(signature_key: &SignatureKey, data: &[u8]) -> Signature {
    use ed25519_dalek::Signer;
    let signature = signature_key.0.sign(data);
    Signature::from_bytes_exact(signature.into())
}

pub fn sign_multiple(signature_key: &SignatureKey, datas: &[&[u8]]) -> Signature {
    let hash = hashing::hash_multiple(datas);
    sign(signature_key, hash.as_ref())
}

pub fn verify(verification_key: &VerificationKey, signature: &Signature, data: &[u8]) -> anyhow::Result<()> {
    use ed25519_dalek::{Verifier};
    let signature = ed25519_dalek::Signature::from_bytes(signature);
    let verifying_key = verification_key.0;
    verifying_key.verify(data, &signature)?;
    Ok(())
}
