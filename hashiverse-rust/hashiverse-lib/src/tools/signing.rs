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

#[cfg(test)]
mod tests {
    use crate::tools::keys::Keys;
    use crate::tools::signing::{sign, sign_multiple, verify};
    use crate::tools::types::Signature;

    // Regression vectors: Ed25519 signing is deterministic, so a dependency bump (ed25519-dalek,
    // blake3) that changes output — or stops accepting previously produced signatures — fails here.
    // Seed matches the regression vectors in `keys.rs`.
    const REGRESSION_SEED: [u8; 32] = [7u8; 32];
    const REGRESSION_MESSAGE: &[u8] = b"hashiverse regression message";
    const REGRESSION_SIGNATURE_HEX: &str = "bbb20840a165224789ddf9e2d5feafe659dbd039ced23737af04cce93076e289c5850b5f73110835fda1c4532677245722734d0740dcd3e5a2c685a0bb894303";
    const REGRESSION_SIGNATURE_MULTIPLE_HEX: &str = "6dd9dfe34264da8655305fc0b05a44545826458bce43899fc268056321156955da8327cb8b9e2e83293b281bc7fb000d080a2ce79bf613ad2b0882411b990909";

    #[test]
    fn sign_regression_vector() -> anyhow::Result<()> {
        let keys = Keys::from_seed(&REGRESSION_SEED, true)?;
        assert_eq!(sign(&keys.signature_key, REGRESSION_MESSAGE).to_hex_str(), REGRESSION_SIGNATURE_HEX, "ed25519 signature drift");
        assert_eq!(sign_multiple(&keys.signature_key, &[b"alpha", b"beta", b"gamma"]).to_hex_str(), REGRESSION_SIGNATURE_MULTIPLE_HEX, "ed25519 sign_multiple drift");
        Ok(())
    }

    #[test]
    fn verify_regression_vector() -> anyhow::Result<()> {
        let keys = Keys::from_seed(&REGRESSION_SEED, true)?;
        let signature = Signature::from_hex_str(REGRESSION_SIGNATURE_HEX)?;
        verify(&keys.verification_key, &signature, REGRESSION_MESSAGE)?;
        assert!(verify(&keys.verification_key, &signature, b"wrong message").is_err());

        let mut tampered_signature_bytes = signature.0;
        tampered_signature_bytes[0] ^= 0x01;
        assert!(verify(&keys.verification_key, &Signature::from_bytes_exact(tampered_signature_bytes), REGRESSION_MESSAGE).is_err());
        Ok(())
    }
}
