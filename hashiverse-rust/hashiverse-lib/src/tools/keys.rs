//! # Ed25519 keypair bundle with post-quantum commitment
//!
//! [`Keys`] packages together everything a client or server needs to sign messages and be
//! identified on the network:
//! - the Ed25519 secret half (`signature_key`) used to sign,
//! - the Ed25519 public half in two forms (`verification_key` for verification, and the
//!   serialisable `verification_key_bytes` for wire/storage),
//! - and the 32-byte `pq_commitment_bytes` (Falcon + Dilithium commitments — see
//!   [`crate::tools::keys_post_quantum`]) that future-proofs the identity.
//!
//! Construction paths:
//! - [`Keys::from_rnd`] — generate a fresh random keypair (optionally skipping the
//!   slow PQ commitment derivation for test scenarios).
//! - [`Keys::from_seed`] — deterministic derivation from a 32-byte seed; used internally
//!   and by tests that need reproducible keys.
//! - [`Keys::from_phrase`] — Argon2 stretch a user-supplied passphrase into a 32-byte
//!   seed, then derive keys from it. Passphrase-recoverable accounts.
//!
//! Persistence:
//! - [`Keys::to_persistence`] / [`Keys::from_persistence`] encrypt the full keypair under
//!   a passphrase using ChaCha20Poly1305 for at-rest storage in the key locker.

use crate::tools::keys_post_quantum::pq_commitment_bytes_from_seed;
use crate::tools::tools;
use crate::tools::types::{PQCommitmentBytes, SignatureKey, VerificationKey, VerificationKeyBytes};
use anyhow::anyhow;
use argon2::Argon2;
use chacha20poly1305::aead::{Aead, Generate};
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};
use std::fmt::{self, Display, Formatter};

/// Argon2id (default params) over the passphrase and raw salt bytes, producing the 32-byte
/// ChaCha20Poly1305 key used for key persistence.  Identical output to the argon2 0.5
/// `hash_password(passphrase, SaltString::encode_b64(salt))` path it replaced (that path
/// decoded the salt back to raw bytes and used the default 32-byte output length) — pinned by
/// `keys_from_persistence_regression_vector`.
fn derive_persistence_key(passphrase: &str, salt: &[u8]) -> anyhow::Result<Key> {
    let mut key = Key::default();
    Argon2::default().hash_password_into(passphrase.as_bytes(), salt, &mut key).map_err(|e| anyhow!("error hashing passphrase: {}", e))?;
    Ok(key)
}

#[derive(Clone)]
pub struct Keys {
    pub signature_key: SignatureKey,
    pub verification_key: VerificationKey,
    pub verification_key_bytes: VerificationKeyBytes,
    pub pq_commitment_bytes: PQCommitmentBytes, // For a critique on this, check out https://www.reddit.com/r/cryptography/comments/1thjnaj/critique_of_a_hybrid_identity_scheme_ed25519/
}

impl Display for Keys {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "[ verification_key_bytes:{} pq_commitment:{} ]", hex::encode(self.verification_key.as_ref()), hex::encode(self.pq_commitment_bytes.as_ref()))
    }
}

impl Keys {
    pub fn from_rnd(skip_pq_commitment_bytes: bool) -> anyhow::Result<Keys> {
        let mut seed = [0u8; 32];
        tools::random_fill_bytes(&mut seed);
        Self::from_seed(&seed, skip_pq_commitment_bytes)
    }

    pub fn from_phrase(phrase: &str) -> anyhow::Result<Keys> {
        let mut seed = [0u8; 32];
        Argon2::default().hash_password_into(phrase.as_bytes(), b"hashiverse-global-salt", &mut seed).map_err(|e| anyhow!("error hashing phrase: {}", e))?;

        Self::from_seed(&seed, false)
    }

    pub fn from_seed(seed: &[u8; 32], skip_pq_commitment_bytes: bool) -> anyhow::Result<Keys> {
        let signature_key = {
            let ed25519_seed = blake3::derive_key("hashiverse-pk-ed25519", seed);
            SignatureKey::from_bytes(&ed25519_seed)?
        };

        let verification_key = signature_key.verification_key();
        let verification_key_bytes = verification_key.to_verification_key_bytes();
        let pq_commitment_bytes = match skip_pq_commitment_bytes {
            false => pq_commitment_bytes_from_seed(seed),
            true => Ok(PQCommitmentBytes::zero()),
        }?;

        Ok(Keys {
            signature_key,
            verification_key,
            verification_key_bytes,
            pq_commitment_bytes,
        })
    }

    pub fn to_persistence(&self, passphrase: &String) -> anyhow::Result<String> {
        // Concatenate key bytes into a buffer
        let mut buf = Vec::with_capacity(32 + 32 + 32);
        buf.extend_from_slice(self.signature_key.as_ref());
        buf.extend_from_slice(self.verification_key.as_ref());
        buf.extend_from_slice(self.pq_commitment_bytes.as_ref());

        // Derive encryption key from passphrase and random salt
        let mut salt = vec![0u8; 16];
        tools::random_fill_bytes(&mut salt);
        let key = derive_persistence_key(passphrase, &salt)?;

        // Encrypt the buffer
        let nonce = Nonce::generate();

        let cipher = ChaCha20Poly1305::new(&key);
        let ciphertext = cipher.encrypt(&nonce, buf.as_ref()).map_err(|e| anyhow!("error encrypting buffer: {}", e))?.to_vec();

        // Store lengths for salt and nonce (as u8)
        let salt_len = salt.len();
        let nonce_len = nonce.len();

        if salt_len > u8::MAX as usize || nonce_len > u8::MAX as usize {
            return Err(anyhow!("Salt or nonce too large"));
        }

        // Prepare output: [salt_len (1)][salt bytes][nonce_len (1)][nonce bytes][ciphertext]
        let mut out = Vec::with_capacity(1 + salt_len + 1 + nonce_len + ciphertext.len());
        out.push(salt_len as u8);
        out.extend_from_slice(&salt);
        out.push(nonce_len as u8);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);

        Ok(tools::encode_base64(&out))
    }

    pub fn from_persistence(passphrase: &String, persistence: &str) -> anyhow::Result<Keys> {
        let decoded = tools::decode_base64(persistence)?;

        if decoded.len() < 2 {
            return Err(anyhow!("Input too short for salt/nonce lengths"));
        }

        // Get salt length and slice
        let salt_len = decoded[0] as usize;
        if decoded.len() < 1 + salt_len + 1 {
            return Err(anyhow!("Input too short for salt data"));
        }
        let salt_start = 1;
        let salt_end = salt_start + salt_len;
        let salt = &decoded[salt_start..salt_end];

        // Get nonce length and slice
        let nonce_len = decoded[salt_end] as usize;
        let nonce_start = salt_end + 1;
        let nonce_end = nonce_start + nonce_len;
        if decoded.len() < nonce_end {
            return Err(anyhow!("Input too short for nonce data"));
        }
        let nonce = &decoded[nonce_start..nonce_end];
        let ciphertext = &decoded[nonce_end..];

        // Derive encryption key from passphrase and salt
        let key = derive_persistence_key(passphrase, salt)?;

        // Decrypt the buffer
        let nonce = <&Nonce>::try_from(nonce).map_err(|_| anyhow!("Invalid nonce length: {}", nonce.len()))?;
        let cipher = ChaCha20Poly1305::new(&key);
        let buf = cipher.decrypt(nonce, ciphertext).map_err(|e| anyhow!("Decryption failed: {}", e))?;

        // Split the buffer back into the key materials
        if buf.len() != 32 * 3 {
            return Err(anyhow!("Decrypted keys len mismatch"));
        }
        let signature_key_bytes = <&[u8; 32]>::try_from(&buf[0..32])?;
        let verification_key_bytes = <&[u8; 32]>::try_from(&buf[32..64])?;
        let pq_commitment_bytes = <&[u8; 32]>::try_from(&buf[64..96])?;

        let signature_key = SignatureKey::from_bytes(signature_key_bytes)?;
        let verification_key = VerificationKey::from_bytes_raw(verification_key_bytes)?;
        let verification_key_bytes = verification_key.to_verification_key_bytes();
        let pq_commitment_bytes = PQCommitmentBytes::from_slice(pq_commitment_bytes)?;

        Ok(Keys {
            signature_key,
            verification_key,
            verification_key_bytes,
            pq_commitment_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ml_dsa::signature::Keypair;
    use std::string::ToString;
    use ml_dsa::SigningKey;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_keys_to_and_from_persistence_roundtrip() -> anyhow::Result<()> {
        for _ in 0..8 {
            let passphrase = Uuid::new_v4().to_string();

            // Generate random keys (could also use from_seed/phrase if you want determinism)
            let keys = Keys::from_rnd(false)?;
            let keys_persisted = keys.to_persistence(&passphrase)?;
            let keys_unpersisted = Keys::from_persistence(&passphrase, &keys_persisted)?;

            assert_eq!(keys.signature_key, keys_unpersisted.signature_key);
            assert_eq!(keys.verification_key, keys_unpersisted.verification_key);
            assert_eq!(keys.pq_commitment_bytes, keys_unpersisted.pq_commitment_bytes);

            // Since the keys are complex objects, let's compare their byte representations
            assert_eq!(keys.signature_key.as_ref(), keys_unpersisted.signature_key.as_ref());
            assert_eq!(keys.verification_key.as_ref(), keys_unpersisted.verification_key.as_ref());
            assert_eq!(keys.pq_commitment_bytes.as_ref(), keys_unpersisted.pq_commitment_bytes.as_ref());
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_pq_keys_are_deterministic_from_seed() {
        let mut seed = [0u8; 32];
        tools::random_fill_bytes(&mut seed);

        let keys1 = Keys::from_seed(&seed, false).unwrap();
        let keys2 = Keys::from_seed(&seed, false).unwrap();

        assert_eq!(keys1.pq_commitment_bytes.as_ref(), keys2.pq_commitment_bytes.as_ref(), "PQ key commitments must be deterministic from the same seed");
    }

    #[tokio::test]
    async fn test_falcon_sign_and_verify() -> anyhow::Result<()> {
        use falcon_rust::falcon512;

        let mut seed = [0u8; 32];
        tools::random_fill_bytes(&mut seed);

        let falcon_seed: [u8; 32] = blake3::derive_key("hashiverse-pk-falcon", &seed);
        let (sk, pk) = falcon512::keygen(falcon_seed);

        let msg = b"hello hashiverse";
        let sig = falcon512::sign(msg, &sk);
        assert!(falcon512::verify(msg, &sig, &pk), "Falcon signature should verify");

        // Rehydrated verifying key
        let pk_rehydrated = falcon512::PublicKey::from_bytes(&pk.to_bytes()).map_err(|e| anyhow::anyhow!("Failed to decode Falcon public key: {:?}", e))?;
        assert!(falcon512::verify(msg, &sig, &pk_rehydrated), "Rehydrated Falcon verifying key should verify");

        // Rehydrated signing key: re-serialised, signs a new message
        let sk_rehydrated = falcon512::SecretKey::from_bytes(&sk.to_bytes()).map_err(|e| anyhow::anyhow!("Failed to decode Falcon secret key: {:?}", e))?;
        let msg2 = b"second message";
        let sig2 = falcon512::sign(msg2, &sk_rehydrated);
        assert!(falcon512::verify(msg2, &sig2, &pk), "Rehydrated Falcon signing key should produce valid signatures");

        // Wrong message should not verify
        assert!(!falcon512::verify(b"wrong message", &sig, &pk), "Falcon should reject wrong message");

        Ok(())
    }

    #[tokio::test]
    async fn test_dilithium_sign_and_verify() -> anyhow::Result<()> {
        use ml_dsa::MlDsa44;
        use ml_dsa::signature::{Signer, Verifier};

        let mut seed = [0u8; 32];
        tools::random_fill_bytes(&mut seed);
        let dilithium_seed = blake3::derive_key("hashiverse-pk-dilithium", &seed);

        // Generate key pair — same derivation as Keys::from_seed
        let signing_key = SigningKey::<MlDsa44>::from_seed(&dilithium_seed.into());

        let msg = b"hello hashiverse";
        let sig = signing_key.sign(msg);

        // Verify with original verifying key
        assert!(signing_key.verifying_key().verify(msg, &sig).is_ok(), "Dilithium signature should verify");

        // Rehydrated: regenerate from the same seed (the seed is the Dilithium private key)
        let kp_rehydrated = SigningKey::<MlDsa44>::from_seed(&dilithium_seed.into());
        let vk_encoded = signing_key.verifying_key().encode();
        let vk_rehydrated_encoded = kp_rehydrated.verifying_key().encode();
        assert_eq!(vk_encoded, vk_rehydrated_encoded, "Dilithium keys must be identical for the same seed");
        assert!(kp_rehydrated.verifying_key().verify(msg, &sig).is_ok(), "Rehydrated Dilithium verifying key should verify the same signature");

        // Wrong message should fail
        assert!(signing_key.verifying_key().verify(b"wrong message", &sig).is_err(), "Dilithium should reject wrong message");

        Ok(())
    }

    #[tokio::test]
    async fn test_pq_commitment_matches_key() -> anyhow::Result<()> {
        use falcon_rust::falcon512;
        use ml_dsa::MlDsa44;

        let seed = [123u8; 32];
        let keys = Keys::from_seed(&seed, false)?;

        // Independently compute expected Falcon commitment
        let expected_falcon: [u8; 16] = {
            let falcon_seed: [u8; 32] = blake3::derive_key("hashiverse-pk-falcon", &seed);
            let (_, pk) = falcon512::keygen(falcon_seed);
            let vrfy_key = pk.to_bytes();
            let hash = blake3::hash(&vrfy_key);
            hash.as_bytes()[..16].try_into()?
        };

        // Independently compute expected Dilithium commitment
        let expected_dilithium: [u8; 16] = {
            let dilithium_seed = blake3::derive_key("hashiverse-pk-dilithium", &seed);
            let kp = SigningKey::<MlDsa44>::from_seed(&dilithium_seed.into());
            let vk_bytes = kp.verifying_key().encode();
            let hash = blake3::hash(vk_bytes.as_ref());
            hash.as_bytes()[..16].try_into()?
        };

        let expected: Vec<u8> = [expected_falcon, expected_dilithium].concat();
        assert_eq!(keys.pq_commitment_bytes.as_ref(), expected.as_slice(), "pq_commitment_bytes must match independently computed PQ commitments");

        Ok(())
    }

    // ------------------------------------------------------------------------------------------
    // Regression vectors: guard against a dependency bump (ed25519-dalek, argon2, chacha20poly1305,
    // falcon-rust, ml-dsa, blake3) silently changing derived identities or breaking persisted keys.
    // Captured against the crate versions in Cargo.lock at the time of writing.  If an *intentional*
    // change alters them, that is a protocol-level identity break — do not just regenerate.
    // ------------------------------------------------------------------------------------------

    const REGRESSION_SEED: [u8; 32] = [7u8; 32];
    const REGRESSION_SIGNATURE_KEY_HEX: &str = "5e2620e5ef4b81d5f32d34482c7ba0bdd2168aa19927f1faa4962eeff361b99c";
    const REGRESSION_VERIFICATION_KEY_HEX: &str = "277931d3eee9562cb808fe237a07e189fcda5eb05aeadf52e26533558339b8ba";
    const REGRESSION_PQ_COMMITMENT_HEX: &str = "25b30ae387a506732efab27ee3845d92904145fc863edb7dc34be7bf13c801a3";
    const REGRESSION_MESSAGE: &[u8] = b"hashiverse regression message";

    #[test]
    fn keys_from_seed_regression_vector() -> anyhow::Result<()> {
        let keys = Keys::from_seed(&REGRESSION_SEED, false)?;
        assert_eq!(hex::encode(keys.signature_key.as_ref()), REGRESSION_SIGNATURE_KEY_HEX, "ed25519 signature key drift");
        assert_eq!(hex::encode(keys.verification_key.as_ref()), REGRESSION_VERIFICATION_KEY_HEX, "ed25519 verification key drift");
        assert_eq!(hex::encode(keys.pq_commitment_bytes.as_ref()), REGRESSION_PQ_COMMITMENT_HEX, "pq commitment drift (falcon-rust or ml-dsa keygen output changed)");
        Ok(())
    }

    #[test]
    fn keys_from_phrase_regression_vector() -> anyhow::Result<()> {
        let keys = Keys::from_phrase("correct horse battery staple")?;
        assert_eq!(hex::encode(keys.verification_key.as_ref()), "86a7b26ecabf7db65b8f3fa2be529a7eb45985a8a2fc3d035b0e08f797253257", "phrase-derived identity drift (argon2 default params or output changed)");
        Ok(())
    }

    /// A key blob persisted by the current code must remain loadable after dependency bumps.
    #[test]
    fn keys_from_persistence_regression_vector() -> anyhow::Result<()> {
        let persistence = "ELTv4mnbtmhDRvUfZLqKhtUMvWMijc86P8PZ214rrdjOfxd2NBZMd4Ee9c53wJ3efxW4TE9jn8NSEj9fQR/xInRsSPThPUYkv7X6a5gjmFmvaOqVqA/O/hEoMJ4rwkPA26QXFDpIT2tCic37y7M9QHMLVTQMlpDJ7T2eiSIzA/wsDvoO0bQdtb/UBFICsA==";
        let keys = Keys::from_persistence(&"hashiverse-passphrase".to_string(), persistence)?;
        assert_eq!(hex::encode(keys.signature_key.as_ref()), REGRESSION_SIGNATURE_KEY_HEX);
        assert_eq!(hex::encode(keys.verification_key.as_ref()), REGRESSION_VERIFICATION_KEY_HEX);
        assert_eq!(hex::encode(keys.pq_commitment_bytes.as_ref()), REGRESSION_PQ_COMMITMENT_HEX);
        assert!(Keys::from_persistence(&"wrong-passphrase".to_string(), persistence).is_err());
        Ok(())
    }

    #[test]
    fn falcon_regression_vector() -> anyhow::Result<()> {
        use falcon_rust::falcon512;

        let falcon_seed: [u8; 32] = blake3::derive_key("hashiverse-pk-falcon", &REGRESSION_SEED);
        let (falcon_secret_key, falcon_public_key) = falcon512::keygen(falcon_seed);
        assert_eq!(blake3::hash(&falcon_public_key.to_bytes()).to_hex().as_str(), "25b30ae387a506732efab27ee3845d9268c45aae814162e5d8d4f944dce9bbda", "falcon public key drift");
        assert_eq!(blake3::hash(&falcon_secret_key.to_bytes()).to_hex().as_str(), "a68398a1b0ddf21ed4272859c890cc06fc6d5e67754c70ab730ef9ac70d1cb3a", "falcon secret key drift");

        // Falcon signing is randomised, so check that a previously produced signature still decodes and verifies.
        let falcon_signature_hex = "596518c44e776396daf86f2e3813a17a683f47063329881cf3a22e1bfc0408b5b8e4d7abe8cdc1ccb1b875e718a97d79b0f4f7a8ab432d35cc413ead54fb1ac83fe49b248cc6b295969f1a9128889328b26e2507bf9470db69bab79e390d36f99a90ecf8caa284f33b12882fe730923588b243dcf854275932c3be4d6bdaaa857ae5209ceff5d4c87b51b8560d121dc6d2e575feb890ca44bade76c790a7f39aec0cf4e75ea71885a992169dff1da94f50c5e4f645e1cbff6369c0a9be8a176b9599e8534cbd466bdfcdcdf1f9161cb2e9e81e2436a18f32c33ada213f7e4caa3530dbba90ca4d09a0d0d079bb290f7290a9f4d0a140c52768230f1b8638e9633b499c0689109f71d118831b0e686cba8adb3fce8dbb1bbd2d43386138d0f86b9580438b8d55a949bafb677facc0d83a9bfbe775f3491d4a849a88f274e99606f32d6f6f2c6a4bdc575fb6c5cbc2a8f5c65d558c0cf0e6562aebb4b4cb64c9a6a1bd38ec0b399e310bb1e381145c920d8e89ceea7a48b43853d7e678ace8e8c8b68193c6ccd5847610631d8bd1559c77154c5f7d2c723031b335c63dbc097a2abfd81ab3209ecd3489fb89b0da61d0b5bb20cee8b3f981d9e2df3fdaba217742325047891089df18e414841edd6afab8dcf5aa32637c78206e0b6ac7b577f8bec9e94fee50646bcf7fe211cfe21906509c9c4c64fbb1a985a3f501e93668a73316d4acf40a77aa58ff876f20581ecb72069c3edbb94300c9de792d86beafade5d01df8be362e84c73508d0f04a38d38cba98f63a6d299235d7ac464cb8ecde8e276ac92f9441b4e81b30e523c997d08acf5658de688734310184ec7ed3f5b2f5134db04427a66c4d496cdacbd07f3ff397b8c8410f9ecd53f34869aba22334ea5b4870e2ba341fc5997bf6f60864000000000000000000000000";
        let falcon_signature = falcon512::Signature::from_bytes(&hex::decode(falcon_signature_hex)?).map_err(|e| anyhow::anyhow!("failed to decode falcon signature: {:?}", e))?;
        assert!(falcon512::verify(REGRESSION_MESSAGE, &falcon_signature, &falcon_public_key), "stored falcon signature no longer verifies");
        assert!(!falcon512::verify(b"wrong message", &falcon_signature, &falcon_public_key));
        Ok(())
    }

    #[test]
    fn dilithium_regression_vector() -> anyhow::Result<()> {
        use ml_dsa::MlDsa44;
        use ml_dsa::signature::{Signer, Verifier};

        let dilithium_seed = blake3::derive_key("hashiverse-pk-dilithium", &REGRESSION_SEED);
        let dilithium_signing_key = SigningKey::<MlDsa44>::from_seed(&dilithium_seed.into());
        let dilithium_verifying_key = dilithium_signing_key.verifying_key();
        assert_eq!(blake3::hash(dilithium_verifying_key.encode().as_ref()).to_hex().as_str(), "904145fc863edb7dc34be7bf13c801a3076c0304f90c36a2c309d531d915a759", "ml-dsa verifying key drift");

        // ml-dsa's `Signer::sign` is the deterministic variant, so the signature itself is stable.
        let dilithium_signature = dilithium_signing_key.sign(REGRESSION_MESSAGE);
        assert_eq!(blake3::hash(dilithium_signature.encode().as_ref()).to_hex().as_str(), "23e107d5dc11d8b202108ff08673e175bb18aa0318d91701ca5ca66ccdef6e3e", "ml-dsa signature drift");
        assert!(dilithium_verifying_key.verify(REGRESSION_MESSAGE, &dilithium_signature).is_ok());
        Ok(())
    }
}
