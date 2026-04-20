//! # Post and feedback wire-format types
//!
//! The canonical on-the-wire shapes for individual posts, aggregate post bundles,
//! individual feedback entries, and aggregate feedback bundles — all
//! self-verifying via signatures and proof-of-work. The PoW-amplification rules
//! that gate how much work a post must cost live alongside them.

pub mod amplification;
pub mod encoded_post;
pub mod encoded_post_feedback;
pub mod encoded_post_bundle;
pub mod encoded_post_bundle_feedback;

