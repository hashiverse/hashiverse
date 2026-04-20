//! # Per-account profile and config as a "meta-post"
//!
//! One post per month per account, carrying the user's profile and client config
//! instead of conversational content. The public section (nickname, avatar,
//! follows) is readable by everyone; the private section (per-feedback thresholds,
//! preferences) is symmetric-encrypted under a key derived from the account's own
//! signing key, so only its owner — and their other devices — can read it.

pub mod meta_post;
pub mod meta_post_crypto;
pub mod meta_post_manager;

