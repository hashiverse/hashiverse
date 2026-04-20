//! # Bootstrap address supply — breaking Kademlia's chicken-and-egg
//!
//! A fresh node has no peers and Kademlia needs peers to find peers. The
//! [`BootstrapProvider`] trait supplies the initial list of addresses to contact so
//! that first exchange can happen; after that
//! [`crate::client::peer_tracker`] takes over and the bootstrap list stops mattering.
//!
//! Two implementations ship with the crate:
//! - [`crate::transport::bootstrap_provider::dnssec_bootstrap_provider`] — production
//!   path, resolves DNSSEC-validated TXT/A records under the domains in
//!   [`crate::tools::config::BOOTSTRAP_DOMAINS`].
//! - [`crate::transport::bootstrap_provider::manual_bootstrap_provider`] — explicit
//!   address list, used by tests and by private deployments that want to pin seed nodes.

/// Supplies the seed peer addresses a node contacts first when it comes online.
///
/// Joining a hashiverse network is a chicken-and-egg problem: Kademlia needs peers to find
/// peers. A `BootstrapProvider` hands the client a starter list — hard-coded domain names,
/// a file on disk, a DNS TXT record, whatever the deployment prefers — and the client uses
/// those to make its first RPC. Once any successful peer exchange happens the
/// [`crate::client::peer_tracker::PeerTracker`] takes over and the bootstrap list stops
/// mattering.
///
/// This trait is deliberately narrow so it is easy to stub in tests
/// (`ManualBootstrapProvider`) or point at a custom source in a private deployment.
#[async_trait::async_trait]
pub trait BootstrapProvider: Send + Sync {
    async fn get_bootstrap_addresses(&self) -> Vec<String>;
}
