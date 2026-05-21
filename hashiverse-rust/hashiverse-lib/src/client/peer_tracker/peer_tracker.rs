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
    use crate::tools::buckets::{generate_bucket_location, BucketLocation, BucketType, BUCKET_DURATIONS};
    use crate::tools::config;
    use crate::tools::runtime_services::RuntimeServices;
    use crate::tools::server_id::ServerId;
    use crate::tools::time::{DurationMillis, TimeMillis};
    use crate::tools::types::{Id, Pow};

    #[tokio::test]
    async fn general_tests() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        assert!(peer_tracker.is_empty());
        assert_eq!(0, peer_tracker.len());

        // Dont accept insufficient pow
        {
            // We have to loop because sometimes the diminished pow actually is sufficient by chance
            loop {
                let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), Pow(config::SERVER_KEY_POW_MIN.0 / 2), true, runtime_services.pow_generator.as_ref()).await?;

                // Check that we havent succeeded by statistical mistake
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

            {
                let result = peer_tracker.add_peer(peer);
                assert!(result.is_ok());
                assert_eq!(1, peer_tracker.len());
            }
        }

        // Cant add individual twice
        {
            let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
            let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;

            {
                let result = peer_tracker.add_peer(peer.clone());
                assert!(result.is_ok());
                assert_eq!(2, peer_tracker.len());
            }
            {
                let result = peer_tracker.add_peer(peer.clone());
                assert!(result.is_ok());
                assert_eq!(2, peer_tracker.len());
            }
        }

        // Add an individual, then remove it
        {
            let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
            let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;

            {
                let result = peer_tracker.add_peer(peer.clone());
                assert!(result.is_ok());
                assert_eq!(3, peer_tracker.len());
            }

            {
                peer_tracker.remove_peer(&peer);
                assert_eq!(2, peer_tracker.len());
            }
        }

        // Remove an unknown individual
        {
            let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
            let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;

            {
                peer_tracker.remove_peer(&peer);
                assert_eq!(2, peer_tracker.len());
            }
        }

        Ok(())
    }

    #[tokio::test]
    async fn converge_basics_test() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;
        // configure_logging_with_time_provider("trace", runtime_services.time_provider.clone());

        const NUM_PEERS: usize = 100;

        {
            for _ in 0..NUM_PEERS {
                let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
                let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
                peer_tracker.add_peer(peer)?;
            }
            assert_eq!(NUM_PEERS, peer_tracker.len());
        }

        {
            let bucket_location = generate_bucket_location(BucketType::User, Id::random(), BUCKET_DURATIONS[0], runtime_services.time_provider.current_time_millis())?;
            let mut count = 0;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, usize::MAX, None).await?;
            while let Some(_peer) = peer_iter.next_peer() { count += 1; }
            assert_eq!(NUM_PEERS, count);
        };

        {
            let bucket_location = generate_bucket_location(BucketType::User, Id::random(), BUCKET_DURATIONS[0], runtime_services.time_provider.current_time_millis())?;
            let mut count = 0;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, usize::MAX, None).await?;
            if peer_iter.next_peer().is_some() { count += 1; }
            assert_eq!(1, count);
        };

        {
            let bucket_location = generate_bucket_location(BucketType::User, Id::random(), BUCKET_DURATIONS[0], runtime_services.time_provider.current_time_millis())?;
            let mut count = 0;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, usize::MAX, None).await?;
            while let Some((peer, _)) = peer_iter.next_peer() {
                count += 1;
                if 0 == count % 2 { peer_iter.remove_peer(&peer); }
            }
            assert_eq!(NUM_PEERS, count);
            assert_eq!(NUM_PEERS / 2, peer_tracker.len());
        }

        Ok(())
    }

    #[tokio::test]
    async fn converge_termination_test() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        const NUM_PEERS: usize = 100;

        {
            for _ in 0..NUM_PEERS {
                let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
                let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
                peer_tracker.add_peer(peer)?;
            }
            assert_eq!(NUM_PEERS, peer_tracker.len());
        }

        {
            let bucket_location = generate_bucket_location(BucketType::User, Id::random(), BUCKET_DURATIONS[0], runtime_services.time_provider.current_time_millis())?;
            let mut count = 0;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, 3, None).await?;
            while let Some(_peer) = peer_iter.next_peer() { count += 1; }
            assert_eq!(3 + 1, count);
        }

        Ok(())
    }

    #[tokio::test]
    async fn converge_insertions_test() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        const NUM_PEERS: usize = 100;

        {
            for _ in 0..NUM_PEERS {
                let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
                let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
                peer_tracker.add_peer(peer)?;
            }
            assert_eq!(NUM_PEERS, peer_tracker.len());
        }

        {
            let bucket_location = generate_bucket_location(BucketType::User, Id::random(), BUCKET_DURATIONS[0], runtime_services.time_provider.current_time_millis())?;
            let mut count = 0;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, usize::MAX, None).await?;
            while let Some(_peer) = peer_iter.next_peer() {
                count += 1;
                if 0 == count % 10 {
                    let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
                    let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
                    peer_iter.add_peers(vec![peer]);
                }
                if 50 == count { break; }
            }
            assert_eq!(50, count);
            assert_eq!(NUM_PEERS + 5, peer_tracker.len());
        }

        Ok(())
    }

    #[tokio::test]
    async fn converge_targeting_test() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        const NUM_PEERS: usize = 100;

        {
            for _ in 0..NUM_PEERS {
                let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
                let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
                peer_tracker.add_peer(peer)?;
            }
            assert_eq!(NUM_PEERS, peer_tracker.len());
        }

        let target_server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
        let target_peer = target_server_id.to_peer(runtime_services.time_provider.as_ref())?;

        {
            const PEER_DISCOVERY_I: usize = 37usize;
            const PEER_DISCOVERY_I_PLUS_1: usize = PEER_DISCOVERY_I + 1;

            let bucket_location = {
                let mut location_id = target_peer.id;
                for i in 10..31 { location_id.0[i] = 0u8; }
                BucketLocation {
                    bucket_type: BucketType::User,
                    base_id: location_id,
                    duration: DurationMillis::zero(),
                    bucket_time_millis: TimeMillis::zero(),
                    location_id,
                }
            };

            let mut count = 0;
            let mut peer_iter = peer_tracker.iterate_to_location(bucket_location.location_id, usize::MAX, None).await?;
            while let Some((peer, _)) = peer_iter.next_peer() {
                count += 1;
                match count {
                    PEER_DISCOVERY_I => { peer_iter.add_peers(vec![target_peer.clone()]); }
                    PEER_DISCOVERY_I_PLUS_1 => {
                        if peer.id != target_peer.id { anyhow::bail!("peer is not the one we expected"); }
                        break;
                    }
                    _ => {}
                }
            }
            assert_eq!(PEER_DISCOVERY_I_PLUS_1, count);
            assert_eq!(NUM_PEERS + 1, peer_tracker.len());
        }

        Ok(())
    }

    /// Verify that `cache_radius` starts by skipping peers inside the radius, then opens up
    /// one ring per step so that closer peers are eventually visited too.
    #[tokio::test]
    async fn converge_cache_radius_test() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage.clone()).await?;

        let location_id = Id::zero();

        let make_peer_with_lab = |lab_bits: usize| -> anyhow::Result<crate::protocol::peer::Peer> {
            let mut id_bytes = [0u8; 32];
            let byte_idx = lab_bits / 8;
            let bit_idx = 7 - (lab_bits % 8);
            id_bytes[byte_idx] = 1u8 << bit_idx;
            let id = Id(id_bytes);
            let _ = id;
            anyhow::bail!("use direct ServerId below")
        };
        let _ = make_peer_with_lab;

        const NUM_PEERS: usize = 100;
        let mut labs_added: Vec<crate::tools::tools::LeadingAgreementBits> = Vec::new();
        for _ in 0..NUM_PEERS {
            let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
            let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
            let lab = crate::tools::tools::leading_agreement_bits_xor(&location_id.0, &peer.id.0);
            labs_added.push(lab);
            peer_tracker.add_peer(peer)?;
        }
        assert_eq!(NUM_PEERS, peer_tracker.len());

        let mut sorted_labs = labs_added.clone();
        sorted_labs.sort();
        let cache_radius = sorted_labs[NUM_PEERS / 2];

        let mut labs_visited: Vec<crate::tools::tools::LeadingAgreementBits> = Vec::new();
        let mut peer_iter = peer_tracker.iterate_to_location(location_id, usize::MAX, Some(cache_radius)).await?;
        while let Some((_, lab)) = peer_iter.next_peer() { labs_visited.push(lab); }

        assert_eq!(NUM_PEERS, labs_visited.len(), "all peers should be visited");

        let has_outside_peers = labs_added.iter().any(|&lab| lab < cache_radius);
        if has_outside_peers {
            assert!(labs_visited[0] < cache_radius, "first peer should be outside the initial cache zone, got lab={} cache_radius={}", labs_visited[0], cache_radius);
        }

        Ok(())
    }
}
