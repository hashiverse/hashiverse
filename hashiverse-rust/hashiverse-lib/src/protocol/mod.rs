//! # Wire-level protocol types
//!
//! What two peers agree on in order to talk to each other: packet layouts, payload
//! variants, signed record shapes, and PoW-gated envelopes. Peer records and their
//! credentials, the RPC packets that carry them, every request/response payload the
//! network understands, and the post-and-feedback data model all live here.

pub mod posting;
pub mod payload;
pub mod peer;
pub mod rpc;

