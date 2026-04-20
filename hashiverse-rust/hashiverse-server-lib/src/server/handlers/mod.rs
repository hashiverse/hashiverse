//! # Inbound RPC handler dispatch
//!
//! The async loop that drains the transport's incoming-request queue, decodes
//! each packet, validates PoW and replay guards, upgrades any improved peer
//! record, and routes by payload discriminator to the matching per-op handler.

pub mod dispatch;
