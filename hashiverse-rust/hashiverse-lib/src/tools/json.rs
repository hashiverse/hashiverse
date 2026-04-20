//! # Thin JSON (de)serialisation helpers
//!
//! Four trivial wrappers over `serde_json` that return `anyhow::Result` and either
//! `Bytes` or `String`. The point is consistency: every caller in the codebase goes
//! through these helpers instead of sprinkling `serde_json::to_vec` / `from_slice`
//! calls, so error handling and encoding choices stay uniform.
//!
//! JSON is used for the rare human-readable persisted blobs (configuration, on-disk
//! key locker entries). Wire-format payloads go through the hand-rolled binary
//! encoders in [`crate::protocol`] instead.

use bytes::Bytes;
use serde::{Serialize, de::DeserializeOwned};

pub fn bytes_to_struct<T: DeserializeOwned>(bytes: &[u8]) -> anyhow::Result<T> {
    let v = serde_json::from_slice(bytes)?;
    Ok(v)
}

pub fn struct_to_bytes<T: Serialize>(v: &T) -> anyhow::Result<Bytes> {
    let bytes = serde_json::to_vec(v)?;
    Ok(Bytes::from(bytes))
}

pub fn string_to_struct<T: DeserializeOwned>(str: &str) -> anyhow::Result<T> {
    let v = serde_json::from_str(str)?;
    Ok(v)
}

pub fn struct_to_string<T: Serialize>(v: &T) -> anyhow::Result<String> {
    let str = serde_json::to_string(v)?;
    Ok(str)
}
