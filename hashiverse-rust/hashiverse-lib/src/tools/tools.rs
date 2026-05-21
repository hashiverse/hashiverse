//! # Grab-bag of tiny cross-platform helpers
//!
//! Utility functions that don't fit in any of the more focused modules:
//!
//! - **Async yielding** ([`yield_now`]) — maps to `tokio::task::yield_now` on native,
//!   `gloo_timers` on wasm32-unknown, and `tokio::time::sleep(0)` on wasi.
//! - **Randomness** ([`random_fill_bytes`], [`random_bytes`], [`random_u32`]) — OS RNG
//!   helpers used by key generation and PoW salt selection.
//! - **Base64 and hex parsing** — consistent helpers used wherever we need to emit or
//!   accept textual byte blobs (key persistence, URLs, HTML attributes).
//! - **Byte reversal** used by server-id PoW hash-to-id mapping.
//! - **`LeadingAgreementBits`** typedef for the XOR-distance metric used by the DHT.
//! - **Logging bootstrap** (`tracing_subscriber` initialisation with consistent
//!   formatting across native and wasm).
//! - **`Cancellable`-style async helpers** that plug into `CancellationToken`.
//!
//! Anything here that grows a meaningful amount of functionality should graduate to
//! its own module.

use crate::tools::json;
use crate::tools::time::DurationMillis;
use crate::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
use crate::tools::BytesGatherer;
use argon2::password_hash::rand_core::{OsRng, RngCore};
use base64::Engine;
use bytes::Bytes;
use log::info;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub type LeadingAgreementBits = i32;

pub async fn yield_now() {
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        send_wrapper::SendWrapper::new(gloo_timers::future::TimeoutFuture::new(0)).await;
    }
    #[cfg(all(target_arch = "wasm32", target_os = "wasi"))]
    {
        tokio::time::sleep(std::time::Duration::from_millis(0u64)).await;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // On native platforms, use Tokio's optimized yield.
        tokio::task::yield_now().await;
    }
}

pub fn random_fill_bytes(dest: &mut [u8]) {
    OsRng.fill_bytes(dest);
}

pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut dest = vec![0u8; n];
    random_fill_bytes(&mut dest);
    dest
}

pub fn reverse_bytes<const N: usize>(bytes: &[u8; N]) -> [u8; N] {
    let mut result = [0u8; N];
    for (i, &byte) in bytes.iter().rev().enumerate() {
        result[i] = byte;
    }
    result
}

pub fn random_u32() -> u32 {
    OsRng.next_u32()
}

#[cfg(target_pointer_width = "64")]
pub fn random_usize() -> usize {
    OsRng.next_u64() as usize
}

#[cfg(target_pointer_width = "32")]
pub fn random_usize() -> usize {
    OsRng.next_u32() as usize
}

pub fn random_usize_bounded(upper: usize) -> usize {
    // Rejection sampling to avoid modulo bias.
    // We accept values in [0, zone) where zone is the largest multiple of `upper`.
    let zone = usize::MAX - (usize::MAX % upper);

    loop {
        let r = random_usize();
        if r < zone {
            return r % upper;
        }
    }
}

pub fn random_u8() -> u8 {
    OsRng.next_u32() as u8
}

pub fn random_base64(length: usize) -> String {
    let mut bytes = vec![0u8; length];
    random_fill_bytes(&mut bytes);
    encode_base64(bytes)
}

pub fn are_all_zeros<T: PartialEq + num_traits::Zero>(src: &[T]) -> bool {
    src.iter().all(|b| *b == T::zero())
}

pub fn are_all_equal<T: PartialEq>(src1: &[T], src2: &[T]) -> bool {
    if src1.len() != src2.len() {
        return false;
    }
    src1.iter().zip(src2).all(|(a, b)| a == b)
}

pub fn count_leading_zero_bits(bytes: &[u8]) -> u8 {
    let mut count = 0u64;

    for &byte in bytes {
        if byte == 0 {
            count += 8;
            continue;
        }

        // Count leading zeros in the non-zero byte
        let mut mask = 0x80; // 10000000 in binary
        while byte & mask == 0 {
            count += 1;
            mask >>= 1;
        }

        break; // Exit after processing the first non-zero byte
    }

    if count < 256 { count as u8 } else { 255 }
}

pub async fn cancellable_sleep_millis(time_provider: &dyn TimeProvider, millis: DurationMillis, cancellation_token: &CancellationToken) {
    tokio::select! {
        _ = time_provider.sleep_millis(millis) => {},
        _ = cancellation_token.cancelled() => {},
    }
}

pub fn format_vec<T: std::fmt::Display>(items: &[T]) -> String {
    format!("[ {} ]", items.iter().map(|item| format!("{}", item)).collect::<Vec<_>>().join(", "))
}

pub fn leading_agreement_bits_xor(key1: &[u8], key2: &[u8]) -> LeadingAgreementBits {
    let mut leading_bits_in_agreement: i32 = 0;

    let min_len = std::cmp::min(key1.len(), key2.len());
    for byte_idx in 0..min_len {
        let xor = key1[byte_idx] ^ key2[byte_idx];

        // Do we have differing bytes?
        if xor != 0 {
            leading_bits_in_agreement += xor.leading_zeros() as LeadingAgreementBits;
            return leading_bits_in_agreement;
        }
        else {
            leading_bits_in_agreement += 8;
        }
    }

    leading_bits_in_agreement
}

pub fn encode_base64<T: AsRef<[u8]>>(input: T) -> String {
    base64::engine::general_purpose::STANDARD.encode(&input)
}

pub fn decode_base64<T: AsRef<[u8]>>(input: T) -> anyhow::Result<Vec<u8>> {
    Ok(base64::engine::general_purpose::STANDARD.decode(input)?)
}

pub fn usize_encode_le64(v: usize) -> [u8; 8] {
    u64::to_le_bytes(v as u64)
}

pub fn usize_decode_le64(v_bytes: &[u8]) -> anyhow::Result<usize> {
    let v = u64::from_le_bytes(v_bytes.try_into()?);
    Ok(v as usize)
}

pub fn write_length_prefixed_json<T: serde::Serialize>(bytes_gatherer: &mut BytesGatherer, value: &T) -> anyhow::Result<()> {
    let json_bytes = json::struct_to_bytes(value)?;
    bytes_gatherer.put_u64(json_bytes.len() as u64);
    bytes_gatherer.put_bytes(json_bytes);
    Ok(())
}
pub fn read_length_prefixed_json<T: serde::de::DeserializeOwned>(bytes: &mut Bytes) -> anyhow::Result<T> {
    use bytes::Buf;

    if bytes.remaining() < 8 {
        anyhow::bail!("Invalid buffer: missing json length");
    }

    let len = bytes.get_u64() as usize;

    if bytes.remaining() < len {
        anyhow::bail!("Invalid buffer: json data truncated");
    }

    let json_bytes = bytes.copy_to_bytes(len);
    json::bytes_to_struct::<T>(&json_bytes)
}

pub fn random_element<T>(range: &[T]) -> &T {
    let index = random_usize_bounded(range.len());
    &range[index]
}

pub fn shuffle<T>(source: &mut [T]) {
    // Fisher–Yates / Knuth shuffle (uniform)
    for i in 1..source.len() {
        let j = random_usize_bounded(i + 1);
        source.swap(i, j);
    }
}

pub struct CustomTimeFormatter {
    time_provider: Arc<dyn TimeProvider>,
}

impl CustomTimeFormatter {
    pub fn new(time_provider: Arc<dyn TimeProvider>) -> Self {
        Self { time_provider }
    }
}

impl FormatTime for CustomTimeFormatter {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> fmt::Result {
        write!(w, "{}", self.time_provider.current_time_str())
    }
}

pub fn configure_logging() {
    configure_logging_with_time_provider("trace", Arc::new(RealTimeProvider))
}

pub fn configure_logging_with_time_provider(level: &str, time_provider: Arc<dyn TimeProvider>) {
    // The filter
    let filter = format!("{},hyper=off,warp=off,reqwest=off,rustls=off,h2=off,h2=off,html5ever=off,selectors=off,fjall=off,lsm_tree=off,sfa=off,hickory_resolver=off,hickory_proto=off", level);
    let env_filter = tracing_subscriber::EnvFilter::new(&filter);

    // Prepare the Standard Logging Layer
    let fmt_layer = tracing_subscriber::fmt::layer().with_timer(CustomTimeFormatter::new(time_provider));

    let registry = tracing_subscriber::registry();

    // Prepare the Console Layer (Conditional) - we only enable this if the 'tokio_unstable' cfg is present and we are not WASM
    #[cfg(all(tokio_unstable, not(target_arch = "wasm32")))]
    registry.with(console_subscriber::spawn());

    // Register everything
    registry.with(fmt_layer).with(env_filter).init();

    info!("Logging initialized");
}

#[cfg(not(target_arch = "wasm32"))]
pub type TempDirHandle = tempfile::TempDir;

#[cfg(not(target_arch = "wasm32"))]
pub fn get_temp_dir() -> anyhow::Result<(TempDirHandle, String)> {
    let mut base = std::env::temp_dir();
    base.push("hashiverse-temp");

    // Ensure the base directory exists
    std::fs::create_dir_all(&base)?;

    let temp_dir = tempfile::Builder::new().prefix("hashiverse-").tempdir_in(&base)?;
    let temp_dir_path = temp_dir.path().to_str().unwrap().to_string();
    Ok((temp_dir, temp_dir_path))
}

#[cfg(target_arch = "wasm32")]
pub type TempDirHandle = ();

#[cfg(target_arch = "wasm32")]
pub fn get_temp_dir() -> anyhow::Result<(TempDirHandle, String)> {
    Ok(((), "".to_string()))
}

pub fn from_hex_str<T, const T_BYTES: usize>(str: &str, ctor: impl FnOnce([u8; T_BYTES]) -> T) -> anyhow::Result<T> {
    if str.len() != 2 * T_BYTES {
        anyhow::bail!("Invalid hex string length: expected {} hex characters ({} bytes), got {} characters.", 2 * T_BYTES, T_BYTES, str.len(),);
    }

    // Try to decode the hex string
    let decoded = hex::decode(str)?;

    // Check if the decoded bytes are exactly xxx bytes
    if decoded.len() != T_BYTES {
        anyhow::bail!("Invalid hex string length: expected {} bytes, got {} bytes", T_BYTES, decoded.len());
    }

    // Convert Vec<u8> to [u8; xxx]
    let mut decoded_bytes = [0u8; T_BYTES];
    decoded_bytes.copy_from_slice(&decoded);

    Ok(ctor(decoded_bytes))
}

/// Spawn a background async task, using the appropriate runtime for the current target.
pub fn spawn_background_task<F>(task: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    tokio::spawn(task);
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    wasm_bindgen_futures::spawn_local(task);
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn xor_distance_bits_test() -> anyhow::Result<()> {
        use crate::tools::tools::leading_agreement_bits_xor;

        let tests = [
            // Identical
            ("0000", "0000", 16),
            ("ffff", "ffff", 16),
            ("1234", "1234", 16),
            ("abcd", "abcd", 16),
            // MSB
            ("0000", "ffff", 0),
            ("0000", "0fff", 4),
            ("0000", "00ff", 8),
            ("0000", "000f", 12),
            // Units
            ("0000", "efff", 0),
            ("0000", "7fff", 1),
            ("0000", "3fff", 2),
            ("0000", "1fff", 3),
            ("0000", "0fff", 4),
            ("0000", "07ff", 5),
            ("0000", "03ff", 6),
            ("0000", "01ff", 7),
            ("0000", "00ff", 8),
            ("0000", "007f", 9),
            ("0000", "003f", 10),
            ("0000", "001f", 11),
            ("0000", "000f", 12),
            ("0000", "0007", 13),
            ("0000", "0003", 14),
            ("0000", "0001", 15),
            // MSB + random
            ("0000", "fff9", 0),
            ("0000", "0ff9", 4),
            ("0000", "00f9", 8),
            // Different lengths
            ("", "0000", 0),
            ("00", "0000", 8),
            ("0000", "000000", 16),
        ];

        for (a, b, expected) in tests {
            let a_binary = hex::decode(a)?;
            let b_binary = hex::decode(b)?;
            {
                let distance = leading_agreement_bits_xor(&a_binary, &b_binary);
                assert_eq!(distance, expected, "Failed for {} and {}.  Got {} expected {}.", a, b, distance, expected);
            }
            {
                let distance = leading_agreement_bits_xor(&b_binary, &a_binary);
                assert_eq!(distance, expected, "Failed for {} and {}.  Got {} expected {}.", a, b, distance, expected);
            }
        }
        Ok(())
    }
}

