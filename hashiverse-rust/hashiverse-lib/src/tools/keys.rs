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
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHasher};
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{AeadCore, ChaCha20Poly1305, Key, KeyInit};
use std::fmt::{self, Display, Formatter};

#[derive(Clone)]
pub struct Keys {
    pub signature_key: SignatureKey,
    pub verification_key: VerificationKey,
    pub verification_key_bytes: VerificationKeyBytes,
    pub pq_commitment_bytes: PQCommitmentBytes,
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
        Argon2::default()
            .hash_password_into(phrase.as_bytes(), b"hashiverse-global-salt", &mut seed)
            .map_err(|e| anyhow!("error hashing phrase: {}", e))?;

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
            false => pq_commitment_bytes_from_seed(&seed),
            true => Ok(PQCommitmentBytes::zero())
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
        let salt_string = SaltString::encode_b64(&salt).map_err(|e| anyhow!("error creating salt: {}", e))?;

        let argon2 = Argon2::default();
        let hash = argon2
            .hash_password(passphrase.as_bytes(), &salt_string)
            .map_err(|e| anyhow!("error hashing passphrase: {}", e))?
            .hash
            .ok_or_else(|| anyhow::anyhow!("argon2 failed"))?;
        let key_bytes = hash.as_bytes();
        let mut key = Key::default();
        let copy_len = key_bytes.len().min(key.len());
        key[..copy_len].copy_from_slice(&key_bytes[..copy_len]);

        // Encrypt the buffer
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let nonce_slice = nonce.as_slice();

        let cipher = ChaCha20Poly1305::new(&key);
        let ciphertext = cipher.encrypt(nonce_slice.into(), buf.as_ref()).map_err(|e| anyhow!("error encrypting buffer: {}", e))?.to_vec();

        // Store lengths for salt and nonce (as u8)
        let salt_len = salt.len();
        let nonce_len = nonce_slice.len();

        if salt_len > u8::MAX as usize || nonce_len > u8::MAX as usize {
            return Err(anyhow!("Salt or nonce too large"));
        }

        // Prepare output: [salt_len (1)][salt bytes][nonce_len (1)][nonce bytes][ciphertext]
        let mut out = Vec::with_capacity(1 + salt_len + 1 + nonce_len + ciphertext.len());
        out.push(salt_len as u8);
        out.extend_from_slice(&salt);
        out.push(nonce_len as u8);
        out.extend_from_slice(nonce_slice);
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

        // Convert salt to usable type
        let salt_string = SaltString::encode_b64(salt).map_err(|e| anyhow!("error creating salt: {}", e))?;

        // Derive encryption key from passphrase and salt
        let argon2 = Argon2::default();
        let hash = argon2
            .hash_password(passphrase.as_bytes(), &salt_string)
            .map_err(|e| anyhow!("error hashing passphrase: {}", e))?
            .hash
            .ok_or_else(|| anyhow!("argon2 failed"))?;
        let key_bytes = hash.as_bytes();
        let mut key = Key::default();
        let copy_len = key_bytes.len().min(key.len());
        key[..copy_len].copy_from_slice(&key_bytes[..copy_len]);

        // Decrypt the buffer
        let cipher = ChaCha20Poly1305::new(&key);
        let buf = cipher.decrypt(nonce.into(), ciphertext).map_err(|e| anyhow!("Decryption failed: {}", e))?;

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
    use std::string::ToString;
    use ml_dsa::signature::Keypair;
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

        assert_eq!(
            keys1.pq_commitment_bytes.as_ref(),
            keys2.pq_commitment_bytes.as_ref(),
            "PQ key commitments must be deterministic from the same seed"
        );
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
        let pk_rehydrated = falcon512::PublicKey::from_bytes(&pk.to_bytes())
            .map_err(|e| anyhow::anyhow!("Failed to decode Falcon public key: {:?}", e))?;
        assert!(falcon512::verify(msg, &sig, &pk_rehydrated), "Rehydrated Falcon verifying key should verify");

        // Rehydrated signing key: re-serialised, signs a new message
        let sk_rehydrated = falcon512::SecretKey::from_bytes(&sk.to_bytes())
            .map_err(|e| anyhow::anyhow!("Failed to decode Falcon secret key: {:?}", e))?;
        let msg2 = b"second message";
        let sig2 = falcon512::sign(msg2, &sk_rehydrated);
        assert!(falcon512::verify(msg2, &sig2, &pk), "Rehydrated Falcon signing key should produce valid signatures");

        // Wrong message should not verify
        assert!(!falcon512::verify(b"wrong message", &sig, &pk), "Falcon should reject wrong message");

        Ok(())
    }

    #[tokio::test]
    async fn test_dilithium_sign_and_verify() -> anyhow::Result<()> {
        use ml_dsa::{KeyGen, MlDsa44};
        use ml_dsa::signature::{Signer, Verifier};

        let mut seed = [0u8; 32];
        tools::random_fill_bytes(&mut seed);
        let dilithium_seed = blake3::derive_key("hashiverse-pk-dilithium", &seed);

        // Generate key pair — same derivation as Keys::from_seed
        let kp = MlDsa44::from_seed(&dilithium_seed.into());

        let msg = b"hello hashiverse";
        let sig = kp.signing_key().sign(msg);

        // Verify with original verifying key
        assert!(kp.verifying_key().verify(msg, &sig).is_ok(), "Dilithium signature should verify");

        // Rehydrated: regenerate from the same seed (the seed is the Dilithium private key)
        let kp_rehydrated = MlDsa44::from_seed(&dilithium_seed.into());
        let vk_encoded = kp.verifying_key().encode();
        let vk_rehydrated_encoded = kp_rehydrated.verifying_key().encode();
        assert_eq!(vk_encoded, vk_rehydrated_encoded, "Dilithium keys must be identical for the same seed");
        assert!(
            kp_rehydrated.verifying_key().verify(msg, &sig).is_ok(),
            "Rehydrated Dilithium verifying key should verify the same signature"
        );

        // Wrong message should fail
        assert!(kp.verifying_key().verify(b"wrong message", &sig).is_err(), "Dilithium should reject wrong message");

        Ok(())
    }

    #[tokio::test]
    async fn test_pq_commitment_matches_key() -> anyhow::Result<()> {
        use falcon_rust::falcon512;
        use ml_dsa::{KeyGen, MlDsa44};

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
            let kp = MlDsa44::from_seed(&dilithium_seed.into());
            let vk_bytes = kp.verifying_key().encode();
            let hash = blake3::hash(vk_bytes.as_ref());
            hash.as_bytes()[..16].try_into()?
        };

        let expected: Vec<u8> = [expected_falcon, expected_dilithium].concat();
        assert_eq!(
            keys.pq_commitment_bytes.as_ref(),
            expected.as_slice(),
            "pq_commitment_bytes must match independently computed PQ commitments"
        );

        Ok(())
    }
}
