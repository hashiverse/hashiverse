//! # Password-based authenticated encryption with multi-recipient key wrapping
//!
//! Symmetric encryption for data that must be readable by one *or more* passwords. Used
//! chiefly for at-rest protection of sensitive material (key lockers, encrypted
//! persistence) and for payloads where a small group of recipients needs shared access
//! without each receiving a copy.
//!
//! ## Construction
//!
//! 1. A random 32-byte *file key* is generated per encryption.
//! 2. The body (plaintext) is encrypted in-place with `ChaCha20Poly1305` using that file key
//!    and a fresh nonce — one authentication tag covers the whole body.
//! 3. For each recipient password, an Argon2id KDF produces a wrap key; the file key is
//!    then wrapped (AEAD-encrypted) under that wrap key with its own nonce and tag and
//!    appended to the header as a "slot".
//!
//! On decrypt, the caller's password is Argon2id-derived once and trial-decrypted against
//! each slot until one authenticates; the recovered file key unwraps the body. Tampering
//! anywhere after the Argon2 header is caught by one of the two AEAD tag layers.
//!
//! ## Strong vs weak
//!
//! Both flavours use Argon2id / ChaCha20Poly1305 — they differ only in the memory cost of
//! the KDF (encoded into the header):
//!
//! - [`encrypt_strong`] — `m_cost = 19 MiB`. For private, sensitive-at-rest data. Expensive
//!   enough to resist offline brute-force of weak passphrases.
//! - [`encrypt_weak`] — `m_cost = 4 MiB`. For public-at-rest data where throughput matters
//!   more than passphrase resistance (the secret itself is typically high-entropy).
//!
//! [`decrypt`] reads the algorithm parameters from the header, so a single function
//! decrypts either flavour.
//!
//! ## DoS hardening
//!
//! The header parser rejects absurd Argon2 parameters (`m_cost > 256 MiB`, `t_cost > 16`,
//! `p_cost > 4`) and absurd recipient counts (`> MAX_RECIPIENTS`) *before* running the KDF,
//! so a crafted ciphertext can't make a peer burn gigabytes of RAM trying to validate it.

use anyhow::anyhow;
use chacha20poly1305::aead::{Aead, AeadCore, AeadInPlace, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

const HEADER_FORMAT_VERSION: u8 = 1;
const HEADER_SIZE: usize = 15;
const MAX_RECIPIENTS: usize = 32;
const NONCE_SIZE: usize = 12;
const FILE_KEY_SIZE: usize = 32;
const WRAPPED_KEY_SIZE: usize = FILE_KEY_SIZE + 16; // 32 key bytes + 16 poly1305 tag
const RECIPIENT_SLOT_SIZE: usize = NONCE_SIZE + WRAPPED_KEY_SIZE; // 60 bytes per slot

/// Serialisable description of the Argon2 parameters used during encryption.
/// Written as a fixed 15-byte prefix so any future `decrypt` call can reconstruct the
/// exact hasher that was used.
///
/// Layout (all numeric fields big-endian):
///   [0]      format version  (u8)  – always 1
///   [1]      algorithm       (u8)  – 0=Argon2d, 1=Argon2i, 2=Argon2id
///   [2]      argon2 version  (u8)  – 16=V0x10,  19=V0x13
///   [3..7]   m_cost          (u32)
///   [7..11]  t_cost          (u32)
///  [11..15]  p_cost          (u32)
struct Argon2Config {
    algorithm: u8,
    version: u8,
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
}

impl Argon2Config {
    /// Strong encryption – for private / sensitive data.
    fn strong() -> Self {
        Self { algorithm: 2, version: 19, m_cost: 19 * 1024, t_cost: 2, p_cost: 1 }
    }

    /// Weak encryption – for public-at-rest data where speed matters more.
    fn weak() -> Self {
        Self { algorithm: 2, version: 19, m_cost: 4 * 1024, t_cost: 2, p_cost: 1 }
    }

    fn to_argon2(&self) -> anyhow::Result<argon2::Argon2<'static>> {
        let algorithm = match self.algorithm {
            0 => argon2::Algorithm::Argon2d,
            1 => argon2::Algorithm::Argon2i,
            2 => argon2::Algorithm::Argon2id,
            _ => anyhow::bail!("unknown argon2 algorithm byte: {}", self.algorithm),
        };
        let version = match self.version {
            16 => argon2::Version::V0x10,
            19 => argon2::Version::V0x13,
            _ => anyhow::bail!("unknown argon2 version byte: {}", self.version),
        };
        let params = argon2::Params::new(self.m_cost, self.t_cost, self.p_cost, None)
            .map_err(|e| anyhow!("argon2 params error: {}", e))?;
        Ok(argon2::Argon2::new(algorithm, version, params))
    }

    fn write_header_to(&self, out: &mut Vec<u8>) {
        out.push(HEADER_FORMAT_VERSION);
        out.push(self.algorithm);
        out.push(self.version);
        out.extend_from_slice(&self.m_cost.to_be_bytes());
        out.extend_from_slice(&self.t_cost.to_be_bytes());
        out.extend_from_slice(&self.p_cost.to_be_bytes());
    }

    fn from_header_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        if bytes.len() < HEADER_SIZE {
            anyhow::bail!("ciphertext too short to contain argon2 header: {} < {}", bytes.len(), HEADER_SIZE);
        }
        if bytes[0] != HEADER_FORMAT_VERSION {
            anyhow::bail!("unknown argon2 header format version: {}", bytes[0]);
        }
        let m_cost = u32::from_be_bytes(bytes[3..7].try_into().map_err(|_| anyhow!("m_cost slice error"))?);
        let t_cost = u32::from_be_bytes(bytes[7..11].try_into().map_err(|_| anyhow!("t_cost slice error"))?);
        let p_cost = u32::from_be_bytes(bytes[11..15].try_into().map_err(|_| anyhow!("p_cost slice error"))?);

        // Reject unreasonable parameters that could cause OOM or CPU exhaustion.
        // Strong config uses m_cost=19456, t_cost=2, p_cost=1 — allow generous headroom.
        if m_cost > 256 * 1024 {
            anyhow::bail!("argon2 m_cost too large: {} (max 256 MiB)", m_cost);
        }
        if t_cost > 16 {
            anyhow::bail!("argon2 t_cost too large: {} (max 16)", t_cost);
        }
        if p_cost > 4 {
            anyhow::bail!("argon2 p_cost too large: {} (max 4)", p_cost);
        }

        Ok(Self { algorithm: bytes[1], version: bytes[2], m_cost, t_cost, p_cost })
    }
}

/// Derive a 32-byte ChaCha20Poly1305 key from `password` using the given argon2 instance.
fn derive_wrap_key(argon2: &argon2::Argon2, password: &[u8]) -> anyhow::Result<Key> {
    let mut key_bytes = [0u8; 32];
    argon2
        .hash_password_into(password, b"hashiverse-key-wrap", &mut key_bytes)
        .map_err(|e| anyhow!("key derivation error: {}", e))?;
    Ok(*Key::from_slice(&key_bytes))
}

fn encrypt_with_config(config: &Argon2Config, plaintext: &[u8], passwords: &Vec<Vec<u8>>) -> anyhow::Result<Vec<u8>> {
    if passwords.is_empty() {
        anyhow::bail!("at least one password required");
    }
    if passwords.len() > MAX_RECIPIENTS {
        anyhow::bail!("too many recipients: {} > {}", passwords.len(), MAX_RECIPIENTS);
    }

    let argon2 = config.to_argon2()?;
    let file_key = ChaCha20Poly1305::generate_key(&mut OsRng);

    let mut out = Vec::new();
    config.write_header_to(&mut out);
    out.push(passwords.len() as u8);

    // Wrap the file key for each recipient
    for password in passwords {
        let wrap_key = derive_wrap_key(&argon2, password)?;
        let wrap_nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let wrapped = ChaCha20Poly1305::new(&wrap_key)
            .encrypt(&wrap_nonce, file_key.as_slice())
            .map_err(|_| anyhow!("key wrap failed"))?;
        out.extend_from_slice(&wrap_nonce);
        out.extend_from_slice(&wrapped);
    }

    // Copy plaintext into out then encrypt in-place — no intermediate buffer
    let body_nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
    out.extend_from_slice(&body_nonce);
    let plaintext_start = out.len();
    out.extend_from_slice(plaintext);
    let tag = ChaCha20Poly1305::new(&file_key)
        .encrypt_in_place_detached(&body_nonce, b"", &mut out[plaintext_start..])
        .map_err(|_| anyhow!("body encryption failed"))?;
    out.extend_from_slice(&tag);

    Ok(out)
}

/// Strong encrypt data with one or more passwords – for private / sensitive data.
pub fn encrypt_strong(plaintext: &[u8], passwords: &Vec<Vec<u8>>) -> anyhow::Result<Vec<u8>> {
    encrypt_with_config(&Argon2Config::strong(), plaintext, passwords)
}

/// Weak encrypt data with one or more passwords - for public-at-rest data where speed matters more.
pub fn encrypt_weak(plaintext: &[u8], passwords: &Vec<Vec<u8>>) -> anyhow::Result<Vec<u8>> {
    encrypt_with_config(&Argon2Config::weak(), plaintext, passwords)
}

/// Decrypt ciphertext produced by `encrypt_strong` or `encrypt_weak` (or any future variant) using the associated password.
///
/// The argon2 parameters and recipient count are read from the prepended header.
pub fn decrypt(ciphertext: &[u8], password: &[u8]) -> anyhow::Result<Vec<u8>> {
    let config = Argon2Config::from_header_bytes(ciphertext)?;
    let argon2 = config.to_argon2()?;

    let mut pos = HEADER_SIZE;

    if ciphertext.len() <= pos {
        anyhow::bail!("ciphertext too short: missing recipient count");
    }
    let num_recipients = ciphertext[pos] as usize;
    pos += 1;

    if num_recipients == 0 || num_recipients > MAX_RECIPIENTS {
        anyhow::bail!("invalid recipient count: {}", num_recipients);
    }

    let recipients_end = pos + num_recipients * RECIPIENT_SLOT_SIZE;
    if ciphertext.len() < recipients_end + NONCE_SIZE + 16 {
        anyhow::bail!("ciphertext too short for claimed recipient count");
    }

    // Derive this password's wrap key and try each slot
    let wrap_key = derive_wrap_key(&argon2, password)?;
    let wrap_cipher = ChaCha20Poly1305::new(&wrap_key);

    let mut file_key: Option<Key> = None;
    for i in 0..num_recipients {
        let slot = pos + i * RECIPIENT_SLOT_SIZE;
        let nonce = Nonce::from_slice(&ciphertext[slot..slot + NONCE_SIZE]);
        let wrapped = &ciphertext[slot + NONCE_SIZE..slot + RECIPIENT_SLOT_SIZE];
        if let Ok(key_bytes) = wrap_cipher.decrypt(nonce, wrapped) {
            file_key = Some(*Key::from_slice(&key_bytes));
            break;
        }
    }

    let file_key = file_key.ok_or_else(|| anyhow!("password did not match any recipient"))?;

    let body_nonce = Nonce::from_slice(&ciphertext[recipients_end..recipients_end + NONCE_SIZE]);
    let body_ciphertext = &ciphertext[recipients_end + NONCE_SIZE..];

    ChaCha20Poly1305::new(&file_key)
        .decrypt(body_nonce, body_ciphertext)
        .map_err(|_| anyhow!("body decryption failed"))
}


#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use log::info;
    use crate::tools::encryption::*;

    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    extern crate wasm_bindgen_test;
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    use wasm_bindgen_test::*;
    use crate::tools::time_provider::stop_watch::StopWatch;
    use crate::tools::time_provider::time_provider::RealTimeProvider;

    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_multiple_encryption_strong() -> anyhow::Result<()> {
        test_multiple_encryption(encrypt_strong).await
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_multiple_encryption_weak() -> anyhow::Result<()> {
        test_multiple_encryption(encrypt_weak).await
    }

    async fn test_multiple_encryption(encrypt_fn: fn(&[u8], &Vec<Vec<u8>>) -> anyhow::Result<Vec<u8>>) -> anyhow::Result<()> {
        let plaintext = "Jimme was here and then some...".as_bytes();
        let passwords = vec!["alice".to_string().into_bytes(), "bob".to_string().into_bytes(), "charlie".to_string().into_bytes()];
        let encrypted = encrypt_fn(plaintext, &passwords)?;

        {
            let decrypted = decrypt(&encrypted, &passwords[0])?;
            assert_eq!(plaintext, &decrypted);
        }
        {
            let decrypted = decrypt(&encrypted, &passwords[1])?;
            assert_eq!(plaintext, &decrypted);
        }
        {
            let decrypted = decrypt(&encrypted, &passwords[2])?;
            assert_eq!(plaintext, &decrypted);
        }
        {
            assert!(decrypt(&encrypted, &"incorrect password".to_string().into_bytes()).is_err());
        }
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_encryption_speeds() -> anyhow::Result<()> {
        // configure_logging();

        let plaintext = "Jimme was here and then some...".as_bytes();
        let passwords = vec!["alice".to_string().into_bytes(), "bob".to_string().into_bytes(), "charlie".to_string().into_bytes()];
        let encrypted_strong = encrypt_strong(plaintext, &passwords)?;
        let encrypted_weak = encrypt_weak(plaintext, &passwords)?;
        assert_ne!(encrypted_strong, encrypted_weak, "weak and strong encryption should not be identical");

        const ITERATIONS: usize = 128;
        let time_provider = Arc::new(RealTimeProvider::default());

        let stopwatch_strong = StopWatch::new(time_provider.clone());
        for _ in 0..ITERATIONS {
            let decrypted = decrypt(&encrypted_strong, &passwords[0])?;
            assert_eq!(plaintext, &decrypted);
        }
        let elapsed_strong = stopwatch_strong.elapsed_time_millis();
        info!("Strong encryption took {}", elapsed_strong);

        let stopwatch_weak = StopWatch::new(time_provider.clone());
        for _ in 0..ITERATIONS {
            let decrypted = decrypt(&encrypted_weak, &passwords[0])?;
            assert_eq!(plaintext, &decrypted);
        }
        let elapsed_weak = stopwatch_weak.elapsed_time_millis();
        info!("Weak encryption took {}", elapsed_weak);

        assert!(elapsed_weak < elapsed_strong, "Weak encryption should be faster than strong encryption");

        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_zero_recipients_rejected() -> anyhow::Result<()> {
        let result = encrypt_weak(b"test", &vec![]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("at least one password"));
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_too_many_recipients_rejected() -> anyhow::Result<()> {
        let passwords: Vec<Vec<u8>> = (0..=MAX_RECIPIENTS).map(|i| format!("password{}", i).into_bytes()).collect();
        let result = encrypt_weak(b"test", &passwords);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too many recipients"));
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_max_recipients() -> anyhow::Result<()> {
        let plaintext = b"test";
        let passwords: Vec<Vec<u8>> = (0..MAX_RECIPIENTS).map(|i| format!("password{}", i).into_bytes()).collect();
        let encrypted = encrypt_weak(plaintext, &passwords)?;

        // Every password should decrypt successfully, including from the last slot
        for password in &passwords {
            let decrypted = decrypt(&encrypted, password)?;
            assert_eq!(plaintext, decrypted.as_slice());
        }
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_nonce_uniqueness() -> anyhow::Result<()> {
        let plaintext = b"same plaintext every time";
        let passwords = vec![b"key".to_vec()];
        // Body nonce sits after: argon2 header (15) + num_recipients byte (1) + one recipient slot (60)
        let body_nonce_start = HEADER_SIZE + 1 + RECIPIENT_SLOT_SIZE;

        let mut seen_nonces = std::collections::HashSet::new();
        let mut seen_ciphertexts = std::collections::HashSet::new();
        for _ in 0..256 {
            let encrypted = encrypt_weak(plaintext, &passwords)?;
            let nonce = encrypted[body_nonce_start..body_nonce_start + NONCE_SIZE].to_vec();
            assert!(seen_nonces.insert(nonce), "body nonce was reused");
            assert!(seen_ciphertexts.insert(encrypted), "ciphertext was reused");
        }
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_tamper_detection() -> anyhow::Result<()> {
        let plaintext = b"tamper me if you dare";
        let passwords = vec![b"key".to_vec()];
        let encrypted = encrypt_weak(plaintext, &passwords)?;

        // Flip every byte after the argon2 header and verify decryption always fails.
        // Recipient slots are protected by their own ChaCha20Poly1305 tag;
        // the body is protected by a second tag. Any single-byte corruption must be detected.
        for i in HEADER_SIZE..encrypted.len() {
            let mut tampered = encrypted.clone();
            tampered[i] ^= 0xff;
            assert!(decrypt(&tampered, &passwords[0]).is_err(), "tamper at byte {} was not detected", i);
        }
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_dos_rejection() -> anyhow::Result<()> {
        // A crafted ciphertext claiming too many recipients must be rejected
        // before any expensive crypto is attempted
        let mut crafted = vec![0u8; HEADER_SIZE + 1];
        crafted[0] = HEADER_FORMAT_VERSION;
        crafted[1] = 2; // Argon2id
        crafted[2] = 19; // V0x13
        crafted[3..7].copy_from_slice(&(4u32 * 1024).to_be_bytes()); // m_cost
        crafted[7..11].copy_from_slice(&2u32.to_be_bytes());          // t_cost
        crafted[11..15].copy_from_slice(&1u32.to_be_bytes());         // p_cost
        crafted[HEADER_SIZE] = (MAX_RECIPIENTS + 1) as u8;

        let result = decrypt(&crafted, b"any");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid recipient count"));

        Ok(())
    }

    #[test]
    fn test_argon2_params_reject_excessive_m_cost() {
        let mut header = vec![0u8; HEADER_SIZE + 100];
        header[0] = HEADER_FORMAT_VERSION;
        header[1] = 2; // Argon2id
        header[2] = 19; // V0x13
        header[3..7].copy_from_slice(&(u32::MAX).to_be_bytes()); // absurd m_cost
        header[7..11].copy_from_slice(&2u32.to_be_bytes());
        header[11..15].copy_from_slice(&1u32.to_be_bytes());
        let result = decrypt(&header, b"any");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("m_cost too large"));
    }

    #[test]
    fn test_argon2_params_reject_excessive_t_cost() {
        let mut header = vec![0u8; HEADER_SIZE + 100];
        header[0] = HEADER_FORMAT_VERSION;
        header[1] = 2;
        header[2] = 19;
        header[3..7].copy_from_slice(&(4u32 * 1024).to_be_bytes());
        header[7..11].copy_from_slice(&1000u32.to_be_bytes()); // absurd t_cost
        header[11..15].copy_from_slice(&1u32.to_be_bytes());
        let result = decrypt(&header, b"any");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("t_cost too large"));
    }

    #[test]
    fn test_argon2_params_reject_excessive_p_cost() {
        let mut header = vec![0u8; HEADER_SIZE + 100];
        header[0] = HEADER_FORMAT_VERSION;
        header[1] = 2;
        header[2] = 19;
        header[3..7].copy_from_slice(&(4u32 * 1024).to_be_bytes());
        header[7..11].copy_from_slice(&2u32.to_be_bytes());
        header[11..15].copy_from_slice(&100u32.to_be_bytes()); // absurd p_cost
        let result = decrypt(&header, b"any");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("p_cost too large"));
    }
}
