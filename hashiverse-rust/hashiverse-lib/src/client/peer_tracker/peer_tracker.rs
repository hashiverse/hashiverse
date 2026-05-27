//! # Kademlia-style local peer set
//!
//! `PeerTracker` is the client's working memory of the network: a deduplicated list of
//! [`crate::protocol::peer::Peer`] records loaded from `BUCKET_PEER`, validated, and
//! indexed by XOR distance so [`crate::client::peer_tracker::peer_iterator::PeerIterator`]
//! can walk them in order of closeness to any target
//! [`crate::tools::types::Id`].
//!
//! Responsibilities:
//!
//! - **Bootstrap** — when the tracker is empty (new install, wiped storage) it calls
//!   `BootstrapV1` against seed domains from [`crate::tools::config::BOOTSTRAP_DOMAINS`]
//!   to obtain a starting set of peers.
//! - **Freshness** — stale peers (missed announces, failed RPCs) are demoted or dropped;
//!   fresh ones from gossip (`AnnounceV1`) or from RPC responses are folded in.
//! - **Flush** — peer mutations set `peers_need_flush`; the outer client batches those
//!   into periodic writes to storage so every RPC response doesn't trigger a disk write.

use crate::anyhow_assert_eq;
use crate::client::client_storage::client_storage;
use crate::client::client_storage::client_storage::{ClientStorage, BUCKET_PEER};
use crate::client::peer_tracker::peer_iterator::PeerIterator;
use crate::protocol::payload::payload::{BootstrapResponseV1, BootstrapV1, PayloadRequestKind, PayloadResponseKind};
use crate::protocol::peer::Peer;
use crate::protocol::rpc;
use crate::tools::runtime_services::RuntimeServices;
use crate::tools::tools::LeadingAgreementBits;
use crate::tools::types::Id;
use crate::tools::{config, json, tools};
use log::{info, trace, warn};
use std::sync::Arc;

/// The client's local view of the known peer set and the entry point for Kademlia-style
/// routing.
///
/// `PeerTracker` owns the in-memory list of [`Peer`] records the client has seen, persists
/// them to [`ClientStorage`] under `BUCKET_PEER` so they survive restarts, and exposes the
/// iteration primitives used throughout the client when it needs to answer "who should I
/// talk to next about this [`Id`]?". When the list is empty (first launch, or after a
/// reset), it seeds itself via a `BootstrapV1` RPC against the
/// [`crate::transport::bootstrap_provider::BootstrapProvider`] addresses configured on the
/// transport.
///
/// The tracker is the single source of truth for peer freshness: stale or bad peers get
/// evicted here, new peers get folded in here, and the `peers_need_flush` flag coalesces
/// rapid updates into a single disk write.
pub struct PeerTracker {
    runtime_services: Arc<RuntimeServices>,
    client_storage: Arc<dyn ClientStorage>,
    peers_need_flush: bool,
    peers: Vec<Peer>,
}

impl PeerTracker {
    pub async fn new(runtime_services: Arc<RuntimeServices>, client_storage: Arc<dyn ClientStorage>) -> anyhow::Result<Self> {
        let peers: anyhow::Result<Vec<Peer>> = try {
            let peers = client_storage::get_struct::<Vec<Peer>>(client_storage.as_ref(), BUCKET_PEER, "peers", runtime_services.time_provider.current_time_millis()).await?;
            match peers {
                Some(peers) => {
                    info!("PeerTracker is starting with {} peers", peers.len());
                    trace!("{:?}", peers);
                    peers
                }
                None => Vec::new(),
            }
        };

        let peers = peers.unwrap_or_else(|e| {
            warn!("Failed to load peers from storage: {}", e);
            Vec::new()
        });

        Ok(Self {
            runtime_services,
            client_storage,
            peers_need_flush: false,
            peers,
        })
    }

    pub async fn flush(&mut self) -> anyhow::Result<()> {
        if !self.peers_need_flush {
            return Ok(());
        }

        trace!("Flushing peers to storage");
        self.peers_need_flush = false;
        client_storage::put_struct(self.client_storage.as_ref(), BUCKET_PEER, "peers", &self.peers, self.runtime_services.time_provider.current_time_millis()).await?;

        Ok(())
    }

    pub fn add_peer(&mut self, peer: Peer) -> anyhow::Result<()> {
        // Sanity check that this peer is kosher
        if let Err(e) = peer.verify() {
            anyhow::bail!("peer verification error: {}", e);
        }

        // Check that its pow is reasonable
        if peer.pow_initial.pow < config::SERVER_KEY_POW_MIN {
            anyhow::bail!("peer peer.pow_initial.pow={} < {}", peer.pow_initial.pow, config::SERVER_KEY_POW_MIN);
        }

        let search_result = self.peers.binary_search_by_key(&peer.id, |peer| peer.id);
        match search_result {
            Ok(i) => {
                // Sanity check that the ids are the same
                assert_eq!(peer.id, self.peers[i].id);

                // If the newer peer is more recent, replace the older one
                if peer.timestamp > self.peers[i].timestamp {
                    self.peers[i] = peer;
                }
            }
            Err(i) => {
                self.peers.insert(i, peer);
            }
        }

        self.peers_need_flush = true;

        Ok(())
    }

    pub fn remove_peer(&mut self, peer: &Peer) {
        if let Ok(i) = self.peers.binary_search_by_key(&peer.id, |peer| peer.id) {
            self.peers.remove(i);
            self.peers_need_flush = true;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn peers(&self) -> &Vec<Peer> {
        &self.peers
    }

    pub async fn iterate_to_location(&mut self, bucket_location_id: Id, max_iterations_since_high_watermark: usize, cache_radius: Option<LeadingAgreementBits>) -> anyhow::Result<PeerIterator<'_>> {
        self.bootstrap().await?;

        Ok(PeerIterator::new(self, bucket_location_id, max_iterations_since_high_watermark, cache_radius))
    }

    pub async fn bootstrap(&mut self) -> anyhow::Result<()> {
        // We only need to bootsrap if we have noone to talk to!
        if !self.is_empty() {
            return Ok(());
        }

        info!("bootstrapping PeerTracker");

        // Lets randomize these addresses so that the first one is not snowed
        let mut bootstrap_addresses = self.runtime_services.transport_factory.get_bootstrap_addresses().await;
        tools::shuffle(&mut bootstrap_addresses);

        // Our bootstrap process has handed us a bunch of raw addresses, so we need to convert them into peers
        for bootstrap_address in bootstrap_addresses {
            let try_result: anyhow::Result<()> = try {
                {
                    info!("bootstrapping {}", bootstrap_address);

                    let request = json::struct_to_bytes(&BootstrapV1 {})?;
                    let response = rpc::rpc::rpc_server_unknown(&self.runtime_services, &Id::zero(), &bootstrap_address, PayloadRequestKind::BootstrapV1, request).await?;
                    anyhow_assert_eq!(&PayloadResponseKind::BootstrapResponseV1, &response.response_request_kind);
                    let response = json::bytes_to_struct::<BootstrapResponseV1>(&response.bytes)?;
                    for peer in response.peers_random {
                        let result = self.add_peer(peer);
                        if let Err(e) = result {
                            warn!("problem while adding bootstrapped peer: {}", e);
                        }
                    }
                }
            };

            if let Err(e) = try_result {
                warn!("problem bootstrapping peer {}: {}", bootstrap_address, e);
            }

            // We only need to continue bootstrapping if we still have noone to talk to!
            if !self.is_empty() {
                break;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::client::client_storage::mem_client_storage::MemClientStorage;
    use crate::client::peer_tracker::peer_tracker::PeerTracker;
    use crate::tools::config;
    use crate::tools::runtime_services::RuntimeServices;
    use crate::tools::server_id::ServerId;
    use crate::tools::types::Pow;

    #[tokio::test]
    async fn general_tests() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        assert!(peer_tracker.is_empty());
        assert_eq!(0, peer_tracker.len());

        // Dont accept insufficient pow
        {
            loop {
                let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), Pow(config::SERVER_KEY_POW_MIN.0 / 2), true, runtime_services.pow_generator.as_ref()).await?;
                if server_id.pow >= config::SERVER_KEY_POW_MIN {
                    continue;
                }
                let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
                let result = peer_tracker.add_peer(peer);
                assert!(result.is_err());
                assert_eq!(0, peer_tracker.len());
                break;
            }
        }

        // Add an individual
        {
            let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
            let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
            let result = peer_tracker.add_peer(peer);
            assert!(result.is_ok());
            assert_eq!(1, peer_tracker.len());
        }

        // Cant add individual twice
        {
            let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
            let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
            let result = peer_tracker.add_peer(peer.clone());
            assert!(result.is_ok());
            assert_eq!(2, peer_tracker.len());
            let result = peer_tracker.add_peer(peer.clone());
            assert!(result.is_ok());
            assert_eq!(2, peer_tracker.len());
        }

        // Add an individual, then remove it
        {
            let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
            let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
            let result = peer_tracker.add_peer(peer.clone());
            assert!(result.is_ok());
            assert_eq!(3, peer_tracker.len());
            peer_tracker.remove_peer(&peer);
            assert_eq!(2, peer_tracker.len());
        }

        // Remove an unknown individual
        {
            let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
            let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
            peer_tracker.remove_peer(&peer);
            assert_eq!(2, peer_tracker.len());
        }

        Ok(())
    }

    /// A peer whose pow_initial has been mutated after signing must fail verify(),
    /// so add_peer rejects it.
    #[tokio::test]
    async fn add_peer_rejects_tampered_pow_initial() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
        let mut peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;

        peer.pow_initial.salt = crate::tools::types::Salt::zero();

        let result = peer_tracker.add_peer(peer);
        assert!(result.is_err(), "tampered peer should be rejected");
        assert_eq!(0, peer_tracker.len());

        Ok(())
    }

    /// peers() must stay sorted by id after arbitrary insertion order, because
    /// add_peer uses binary_search_by_key to dedupe.
    #[tokio::test]
    async fn add_peer_keeps_peers_sorted_by_id() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        const NUM_PEERS: usize = 30;
        for _ in 0..NUM_PEERS {
            let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
            let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
            peer_tracker.add_peer(peer)?;
        }

        let peers = peer_tracker.peers();
        assert_eq!(NUM_PEERS, peers.len());
        for window in peers.windows(2) {
            assert!(window[0].id < window[1].id, "peers must be sorted by id; got {} then {}", window[0].id, window[1].id);
        }

        Ok(())
    }
}
