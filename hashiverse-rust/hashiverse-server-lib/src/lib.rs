//! # `hashiverse-server-lib` — server-specific extensions to `hashiverse-lib`
//!
//! Everything needed to run a hashiverse node as a public server: HTTPS with
//! automatic Let's Encrypt certificate rotation, kernel-level ipset-based DDoS
//! mitigation, on-disk persistence via `fjall`, the Kademlia routing table, and
//! the inbound RPC dispatch loop that ties all the [`hashiverse_lib::protocol`]
//! handlers together.

#![feature(try_blocks)]
#![feature(duration_constructors)]

pub mod server;
pub mod tools;
pub mod environment;
pub mod transport;
