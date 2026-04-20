//! # `hashiverse-lib` — core protocol library
//!
//! The shared heart of hashiverse: protocol types, client logic, and cross-cutting
//! utilities used by every component in the workspace (server binary, WASM browser
//! client, Python client, integration tests). Split into four top-level modules —
//! [`transport`] for moving bytes, [`protocol`] for wire-format types, [`client`]
//! for the participation API, and [`tools`] for the primitives the rest build on.

#![allow(async_fn_in_trait)]
#![feature(try_blocks)]


pub mod transport;
pub mod protocol;
pub mod client;
pub mod tools;

