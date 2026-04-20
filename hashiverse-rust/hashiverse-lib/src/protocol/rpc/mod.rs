//! # RPC packet encoding, dispatch, and verification
//!
//! The request/response packet formats, the proof-of-work search and verification
//! that gate them, the server-identity signature that binds a response to the
//! request it answers, and the high-level helpers that wrap the whole round trip
//! for callers.

pub mod rpc;
pub mod rpc_request;
pub mod rpc_response;

