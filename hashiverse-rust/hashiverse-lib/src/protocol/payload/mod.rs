//! # RPC payload variants
//!
//! Every request and response type on the wire, plus the `u16` discriminators that
//! route them to the right handler. Post-submission carries enough special-case
//! envelope logic (header compression, body encryption, signing-authority choice)
//! that it has been factored out into its own module.

pub mod payload;
pub mod submit_post_v1;

