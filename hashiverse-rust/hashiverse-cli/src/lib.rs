//! # `hashiverse-cli` — command-line client for Hashiverse
//!
//! Unlocks an identity (from a keyphrase, or from a key previously stored on disk under a
//! client id + password) and then exposes posting, timeline reading and trending hashtags
//! either as one-shot subcommands or as an interactive REPL.
//!
//! The binary lives in `main.rs`; everything else is a library so the integration tests can
//! drive the same code paths.

pub mod cli_args;
pub mod unlock;
