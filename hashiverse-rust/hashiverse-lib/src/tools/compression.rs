//! # Versioned compression helpers
//!
//! Every compressed payload produced here is self-describing: the first byte is a
//! **compression version** that names the algorithm (`0` = passthrough, `1` = lz4,
//! `2` = brotli). This lets [`decompress`] dispatch on the byte alone, so callers only
//! need to know which *kind* of compression to aim for, not which algorithm they'll
//! actually get back.
//!
//! Two algorithms are exposed, picked by the caller based on expected access pattern:
//!
//! - [`compress_for_speed`] — **lz4-flex**. For RPC wire traffic: the payload is
//!   compressed once per request and decompressed once per response. Fast, low CPU, modest
//!   ratio. Falls back to passthrough if the input is below `MIN_BYTES_FOR_LZ4` (~64 B) or
//!   if the compressed form would be larger than the input.
//! - [`compress_for_size`] — **brotli q11**. For write-once / read-many post storage on
//!   servers: spend CPU once at write time in exchange for the smallest bytes-on-disk and
//!   bytes-on-wire when the post is later served. Same passthrough fallback at
//!   `MIN_BYTES_FOR_BROTLI` (~128 B).
//!
//! ## Defence against decompression bombs
//!
//! Both decompress paths cap output at `MAX_DECOMPRESSED_SIZE` (32 MiB, with headroom over
//! the protocol response limit) so a malicious tiny payload cannot OOM a peer.

use crate::tools::BytesGatherer;
use bytes::{BufMut, Bytes, BytesMut};
use std::io::{Read, Write};

/// Version byte prepended to every compressed payload.
/// This allows the decompressor to evolve the algorithm independently of callers.
const COMPRESSION_VERSION_NONE: u8 = 0;
const COMPRESSION_VERSION_LZ4: u8 = 1;
const COMPRESSION_VERSION_BROTLI: u8 = 2;

/// Below this threshold lz4's frame overhead (~11 bytes) rarely yields a net win.
const MIN_BYTES_FOR_LZ4: usize = 64;

/// Below this threshold brotli's block overhead rarely yields a net win.
const MIN_BYTES_FOR_BROTLI: usize = 128;

/// Hard cap on decompressed output size to prevent decompression bombs.
/// Set to the largest protocol blob limit (response = 16 MB) with headroom.
const MAX_DECOMPRESSED_SIZE: usize = 32 * 1024 * 1024;

fn compress_passthrough(input: &[u8]) -> anyhow::Result<BytesGatherer> {
    let mut bytes_gatherer = BytesGatherer::default();
    bytes_gatherer.put_u8(COMPRESSION_VERSION_NONE);
    bytes_gatherer.put_slice(input);
    Ok(bytes_gatherer)
}

fn decompress_passthrough(input: &[u8]) -> anyhow::Result<BytesGatherer> {
    Ok(BytesGatherer::from_bytes(Bytes::copy_from_slice(&input[1..])))
}

fn compress_lz4(input: &[u8]) -> anyhow::Result<BytesGatherer> {
    if input.len() < MIN_BYTES_FOR_LZ4 {
        return compress_passthrough(input);
    }

    // Layout: [version(1)][uncompressed_len u32 le(4)][compressed bytes]
    // compress_into writes directly into the pre-allocated tail — no intermediate Vec.
    let max_out = lz4_flex::block::get_maximum_output_size(input.len());
    let mut result = BytesMut::with_capacity(5 + max_out);
    result.put_u8(COMPRESSION_VERSION_LZ4);
    result.put_slice(&(input.len() as u32).to_le_bytes());
    let data_start = result.len(); // 5
    result.resize(data_start + max_out, 0);
    let n = lz4_flex::block::compress_into(input, &mut result[data_start..]).map_err(|e| anyhow::anyhow!("lz4 compression failed: {}", e))?;
    result.truncate(data_start + n);
    Ok(BytesGatherer::from_bytes(result.freeze()))
}

fn decompress_lz4(input: &[u8]) -> anyhow::Result<BytesGatherer> {
    let data = &input[1..];
    if data.len() < 4 {
        anyhow::bail!("lz4 decompression failed: missing size prefix");
    }
    let uncompressed_size = u32::from_le_bytes(data[..4].try_into().unwrap()) as usize;
    if uncompressed_size > MAX_DECOMPRESSED_SIZE {
        anyhow::bail!("lz4 decompressed size {} exceeds limit {}", uncompressed_size, MAX_DECOMPRESSED_SIZE);
    }
    lz4_flex::decompress_size_prepended(data).map(|v| BytesGatherer::from_bytes(Bytes::from(v))).map_err(|e| anyhow::anyhow!("lz4 decompression failed: {}", e))
}

fn compress_brotli(input: &[u8]) -> anyhow::Result<BytesGatherer> {
    if input.len() < MIN_BYTES_FOR_BROTLI {
        return compress_passthrough(input);
    }

    // Quality 11 (max), lgwin 22 (4 MB window) — optimal for write-once post storage.
    // CompressorWriter wraps &mut Vec<u8> (impl Write) and appends directly — no intermediate buffer.
    let mut result = vec![COMPRESSION_VERSION_BROTLI];
    {
        let mut writer = brotli::CompressorWriter::new(&mut result, 4096, 11, 22);
        writer.write_all(input)?;
    }
    Ok(BytesGatherer::from_bytes(Bytes::from(result)))
}

fn decompress_brotli(input: &[u8]) -> anyhow::Result<BytesGatherer> {
    let mut output = Vec::new();
    let bytes_read = brotli::Decompressor::new(&input[1..], 4096).take(MAX_DECOMPRESSED_SIZE as u64 + 1).read_to_end(&mut output)?;
    if bytes_read > MAX_DECOMPRESSED_SIZE {
        anyhow::bail!("brotli decompressed size {} exceeds limit {}", bytes_read, MAX_DECOMPRESSED_SIZE);
    }
    Ok(BytesGatherer::from_bytes(Bytes::from(output)))
}

/// Compress using lz4 (fast). Use for RPC wire transfer.
///
/// Falls back to verbatim (version 0) if:
/// - input is below the minimum threshold, or
/// - the compressed form would not be smaller than the original.
pub fn compress_for_speed(input: &[u8]) -> anyhow::Result<BytesGatherer> {
    let result = compress_lz4(input)?;
    if result.len() < input.len() { Ok(result) } else { compress_passthrough(input) }
}

/// Compress using brotli (small). Use for post storage (compress once, read many).
///
/// Falls back to verbatim (version 0) if:
/// - input is below the minimum threshold, or
/// - the compressed form would not be smaller than the original.
pub fn compress_for_size(input: &[u8]) -> anyhow::Result<BytesGatherer> {
    let result = compress_brotli(input)?;
    if result.len() < input.len() { Ok(result) } else { compress_passthrough(input) }
}

/// Decompress any payload produced by [`compress_for_speed`] or [`compress_for_size`].
///
/// Dispatches on the leading version byte.
pub fn decompress(input: &[u8]) -> anyhow::Result<BytesGatherer> {
    if input.is_empty() {
        anyhow::bail!("missing compression version byte");
    }
    match input[0] {
        COMPRESSION_VERSION_LZ4 => decompress_lz4(input),
        COMPRESSION_VERSION_BROTLI => decompress_brotli(input),
        COMPRESSION_VERSION_NONE => decompress_passthrough(input),
        v => anyhow::bail!("unsupported compression version byte {}", v),
    }
}




#[cfg(test)]
mod tests {
    use crate::tools::compression::{compress_for_size, compress_for_speed, decompress};
    use crate::tools::tools;

    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    extern crate wasm_bindgen_test;
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    use wasm_bindgen_test::*;
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    fn roundtrip_speed(input: &[u8], msg: &str) -> anyhow::Result<()> {
        let compressed = compress_for_speed(input)?.to_bytes();
        let output = decompress(&compressed)?.to_bytes();
        assert_eq!(input, output.as_ref(), "{}", msg);
        Ok(())
    }

    fn roundtrip_size(input: &[u8], msg: &str) -> anyhow::Result<()> {
        let compressed = compress_for_size(input)?.to_bytes();
        let output = decompress(&compressed)?.to_bytes();
        assert_eq!(input, output.as_ref(), "{}", msg);
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_compression_is_reversible() -> anyhow::Result<()> {
        let input = b"Some example string to test compression and decompression.";
        roundtrip_speed(input, "lz4 roundtrip")?;
        roundtrip_size(input, "brotli roundtrip")?;
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_compression_is_reversible_short() -> anyhow::Result<()> {
        // Below MIN_BYTES_FOR_LZ4 and MIN_BYTES_FOR_BROTLI — both should passthrough
        let input = b"Some...";
        roundtrip_speed(input, "lz4 short passthrough")?;
        roundtrip_size(input, "brotli short passthrough")?;
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_compression_is_reversible_empty() -> anyhow::Result<()> {
        let input = b"";
        roundtrip_speed(input, "lz4 empty passthrough")?;
        roundtrip_size(input, "brotli empty passthrough")?;
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_compression_is_reversible_random() -> anyhow::Result<()> {
        // Random data is incompressible; both should fall back to passthrough.
        // Use lz4 path only — brotli quality 11 on 64 MB of random data is too slow for a test.
        const LENGTH: usize = 8192 * 8192;
        let input: Vec<u8> = tools::random_bytes(LENGTH);
        roundtrip_speed(&input, "lz4 random passthrough")?;
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_brotli_actually_compresses_html() -> anyhow::Result<()> {
        // Brotli should produce a smaller result on compressible text.
        let input = "<!DOCTYPE html><html><head><title>Test</title></head><body>".repeat(50);
        let compressed = compress_for_size(input.as_bytes())?.to_bytes();
        assert!(
            compressed.len() < input.len(),
            "brotli should compress repetitive HTML: {} -> {}",
            input.len(),
            compressed.len()
        );
        let output = decompress(&compressed)?.to_bytes();
        assert_eq!(input.as_bytes(), output.as_ref());
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_lz4_rejects_oversized_decompressed_payload() {
        // Craft an lz4 payload with a claimed uncompressed size exceeding the limit.
        // Layout: [version=1][uncompressed_len u32 le][compressed data...]
        let fake_size: u32 = (super::MAX_DECOMPRESSED_SIZE as u32) + 1;
        let mut payload = vec![super::COMPRESSION_VERSION_LZ4];
        payload.extend_from_slice(&fake_size.to_le_bytes());
        payload.extend_from_slice(&[0u8; 16]); // garbage compressed data
        let result = decompress(&payload);
        let error_message = result.err().expect("should have failed").to_string();
        assert!(error_message.contains("exceeds limit"), "unexpected error: {}", error_message);
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_lz4_accepts_valid_decompressed_payload() -> anyhow::Result<()> {
        // A normal roundtrip should still work fine
        let input = "hello world! ".repeat(100);
        let compressed = compress_for_speed(input.as_bytes())?.to_bytes();
        let output = decompress(&compressed)?.to_bytes();
        assert_eq!(input.as_bytes(), output.as_ref());
        Ok(())
    }

    #[cfg_attr(not(all(target_arch = "wasm32", target_os = "unknown")), tokio::test)]
    #[cfg_attr(all(target_arch = "wasm32", target_os = "unknown"), wasm_bindgen_test)]
    async fn test_lz4_actually_compresses_text() -> anyhow::Result<()> {
        // lz4 should produce a smaller result on compressible text.
        let input = "The quick brown fox jumps over the lazy dog. ".repeat(100);
        let compressed = compress_for_speed(input.as_bytes())?.to_bytes();
        assert!(
            compressed.len() < input.len(),
            "lz4 should compress repetitive text: {} -> {}",
            input.len(),
            compressed.len()
        );
        let output = decompress(&compressed)?.to_bytes();
        assert_eq!(input.as_bytes(), output.as_ref());
        Ok(())
    }
}
