//! # Blake3 hashing helpers
//!
//! Thin wrappers over `blake3::Hasher` that return a strongly-typed
//! [`crate::tools::types::Hash`] (32 bytes) instead of raw bytes. Blake3 is used as the
//! canonical *fast* hash throughout the protocol — for [`crate::tools::types::Id`]
//! derivation, content addressing, bucket location hashing, and as the last step of
//! any multi-input hash that needs a single output.
//!
//! The four functions are minor ergonomic variants:
//! - [`hash`] — one slice.
//! - [`hash_two`] — two slices, concatenated without copying.
//! - [`hash_multiple`] — arbitrary slice of slices.
//! - [`hash_multiple_plus_one`] — the common case of "hash this list, plus one more tail
//!   value" without allocating a combined slice.
//!
//! For the deliberately *slow* multi-algorithm chain used in proof-of-work, see
//! [`crate::tools::pow`].

use crate::tools::types::Hash;

pub fn hash(data: &[u8]) -> Hash {

    let mut hasher = blake3::Hasher::new();
    hasher.update(data);
    let hash = hasher.finalize();

    Hash(hash.into())
}

pub fn hash_two(data_1: &[u8], data_2: &[u8]) -> Hash {

    let mut hasher = blake3::Hasher::new();
    hasher.update(data_1);
    hasher.update(data_2);
    let hash = hasher.finalize();

    Hash(hash.into())
}

pub fn hash_multiple(datas: &[&[u8]]) -> Hash {

    let mut hasher = blake3::Hasher::new();
    for &data in datas {
        hasher.update(data);
    }
    let hash = hasher.finalize();

    Hash(hash.into())
}

pub fn hash_multiple_plus_one(datas: &[&[u8]], data_plus_one: &[u8]) -> Hash {

    let mut hasher = blake3::Hasher::new();
    for &data in datas {
        hasher.update(data);
    }
    hasher.update(data_plus_one);
    let hash = hasher.finalize();

    Hash(hash.into())
}
