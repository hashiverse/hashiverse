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
use crate::tools::time::TimeMillis;
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
    max_peers: usize,
}

impl PeerTracker {
    pub async fn new(runtime_services: Arc<RuntimeServices>, client_storage: Arc<dyn ClientStorage>) -> anyhow::Result<Self> {
        Self::new_with_max_peers(runtime_services, client_storage, config::CLIENT_PEER_TRACKER_MAX_PEERS).await
    }

    pub async fn new_with_max_peers(runtime_services: Arc<RuntimeServices>, client_storage: Arc<dyn ClientStorage>, max_peers: usize) -> anyhow::Result<Self> {
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

        let current_time_millis = runtime_services.time_provider.current_time_millis();
        let (peers, peers_were_dropped) = Self::sanitise_loaded_peers(peers, current_time_millis, max_peers);

        Ok(Self {
            runtime_services,
            client_storage,
            peers_need_flush: peers_were_dropped,
            peers,
            max_peers,
        })
    }

    /// Persisted peers predate the current bounds: drop far-future records (which could otherwise never be replaced,
    /// because newest-timestamp wins) and trim to `max_peers` by score.  Returns the peers sorted by id.
    fn sanitise_loaded_peers(peers: Vec<Peer>, current_time_millis: TimeMillis, max_peers: usize) -> (Vec<Peer>, bool) {
        let loaded_peer_count = peers.len();
        let mut peers: Vec<Peer> = peers
            .into_iter()
            .filter(|peer| match peer.verify_timestamps(current_time_millis) {
                Ok(()) => true,
                Err(e) => {
                    warn!("dropping stored peer: {}", e);
                    false
                }
            })
            .collect();

        if peers.len() > max_peers {
            peers.sort_by(|a, b| b.score(current_time_millis).total_cmp(&a.score(current_time_millis)));
            peers.truncate(max_peers);
        }
        peers.sort_by_key(|peer| peer.id);

        let peers_were_dropped = peers.len() != loaded_peer_count;
        (peers, peers_were_dropped)
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
        let search_result = self.peers.binary_search_by_key(&peer.id, |peer| peer.id);

        // Cheap dedupe before the expensive verify: servers resend the same peers in every peers_nearer list
        if let Ok(i) = search_result {
            if peer.timestamp <= self.peers[i].timestamp {
                return Ok(());
            }
        }

        // Sanity check that this peer is kosher
        if let Err(e) = peer.verify() {
            anyhow::bail!("peer verification error: {}", e);
        }

        // Check that its pow is reasonable
        if peer.pow_initial.pow < config::SERVER_KEY_POW_MIN {
            anyhow::bail!("peer peer.pow_initial.pow={} < {}", peer.pow_initial.pow, config::SERVER_KEY_POW_MIN);
        }

        // A far-future timestamp would win every later newest-timestamp comparison, so the real peer could never replace it
        let current_time_millis = self.runtime_services.time_provider.current_time_millis();
        peer.verify_timestamps(current_time_millis)?;

        match search_result {
            Ok(i) => {
                // Newer than what we hold (checked above)
                self.peers[i] = peer;
            }
            Err(i) => {
                if self.peers.len() < self.max_peers {
                    self.peers.insert(i, peer);
                }
                else {
                    // Full: the newcomer only gets in by out-scoring our weakest peer
                    let weakest_peer = self.peers.iter().enumerate().map(|(index, existing_peer)| (index, existing_peer.score(current_time_millis))).min_by(|a, b| a.1.total_cmp(&b.1));
                    let Some((weakest_peer_index, weakest_peer_score)) = weakest_peer else { return Ok(()) };
                    if peer.score(current_time_millis) <= weakest_peer_score {
                        return Ok(());
                    }
                    self.peers.remove(weakest_peer_index);
                    let insert_index = self.peers.binary_search_by_key(&peer.id, |peer| peer.id).unwrap_or_else(|insert_index| insert_index);
                    self.peers.insert(insert_index, peer);
                }
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
    pub fn needs_flush(&self) -> bool {
        self.peers_need_flush
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
                    if response.peers_random.len() > config::CLIENT_MAX_PEERS_CONSIDERED_PER_RESPONSE {
                        warn!("ignoring {} of {} bootstrapped peers from {} (cap is {})", response.peers_random.len() - config::CLIENT_MAX_PEERS_CONSIDERED_PER_RESPONSE, response.peers_random.len(), bootstrap_address, config::CLIENT_MAX_PEERS_CONSIDERED_PER_RESPONSE);
                    }
                    for peer in response.peers_random.into_iter().take(config::CLIENT_MAX_PEERS_CONSIDERED_PER_RESPONSE) {
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

    // ── Bounded tracker ──

    use crate::client::client_storage::client_storage::{self, BUCKET_PEER};
    use crate::client::peer_tracker::peer_iterator::PeerIterator;
    use crate::protocol::peer::Peer;
    use crate::tools::time::{TimeMillis, MILLIS_IN_DAY};
    use crate::tools::time_provider::manual_time_provider::ManualTimeProvider;
    use crate::tools::types::Id;
    use crate::tools::pow_generator::single_threaded_pow_generator::SingleThreadedPowGenerator;
    use crate::transport::mem_transport::MemTransportFactory;
    use std::sync::Arc;

    const START_TIME_MILLIS: TimeMillis = TimeMillis(1_800_000_000_000);

    fn manual_runtime_services() -> (Arc<RuntimeServices>, Arc<ManualTimeProvider>) {
        let manual_time_provider = Arc::new(ManualTimeProvider::new(START_TIME_MILLIS));
        let runtime_services = Arc::new(RuntimeServices {
            time_provider: manual_time_provider.clone(),
            transport_factory: MemTransportFactory::default(),
            pow_generator: Arc::new(SingleThreadedPowGenerator::new()),
        });
        (runtime_services, manual_time_provider)
    }

    /// A valid peer whose pows and signature are all dated "now" according to `runtime_services`.
    async fn make_peer(runtime_services: &RuntimeServices) -> anyhow::Result<(ServerId, Peer)> {
        let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, true, runtime_services.pow_generator.as_ref()).await?;
        let peer = server_id.to_peer(runtime_services.time_provider.as_ref())?;
        Ok((server_id, peer))
    }

    fn peer_ids(peer_tracker: &PeerTracker) -> Vec<Id> {
        peer_tracker.peers().iter().map(|peer| peer.id).collect()
    }

    #[tokio::test]
    async fn add_peer_never_exceeds_max_peers() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        const MAX_PEERS: usize = 4;
        let mut peer_tracker = PeerTracker::new_with_max_peers(runtime_services.clone(), client_storage, MAX_PEERS).await?;

        for _ in 0..MAX_PEERS + 4 {
            let (_, peer) = make_peer(&runtime_services).await?;
            peer_tracker.add_peer(peer)?;
            assert!(peer_tracker.len() <= MAX_PEERS);
        }
        assert_eq!(MAX_PEERS, peer_tracker.len());
        for window in peer_tracker.peers().windows(2) {
            assert!(window[0].id < window[1].id, "peers must stay sorted by id");
        }
        Ok(())
    }

    #[tokio::test]
    async fn full_tracker_evicts_weakest_peer_only_for_a_stronger_newcomer() -> anyhow::Result<()> {
        let (runtime_services, manual_time_provider) = manual_runtime_services();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new_with_max_peers(runtime_services.clone(), client_storage, 2).await?;

        // Two peers whose pows are 90 days old by the time the newcomer arrives: their score has decayed away.
        let (_, stale_peer_1) = make_peer(&runtime_services).await?;
        let (_, stale_peer_2) = make_peer(&runtime_services).await?;
        // A peer that will be fresh at the end, created after the clock moves on.
        manual_time_provider.set_time(START_TIME_MILLIS + MILLIS_IN_DAY.const_mul(90));
        let (_, fresh_peer) = make_peer(&runtime_services).await?;
        let (_, other_fresh_peer) = make_peer(&runtime_services).await?;
        let now = runtime_services.time_provider.current_time_millis();
        let weakest_fresh_score = fresh_peer.score(now).min(other_fresh_peer.score(now));
        let strongest_stale_score = stale_peer_1.score(now).max(stale_peer_2.score(now));
        assert!(weakest_fresh_score > strongest_stale_score, "test setup: fresh peers must out-score stale peers");

        // Full with {stale, fresh}: a stronger newcomer evicts the stale peer and keeps the fresh one.
        peer_tracker.add_peer(stale_peer_1.clone())?;
        peer_tracker.add_peer(fresh_peer.clone())?;
        peer_tracker.add_peer(other_fresh_peer.clone())?;
        let mut expected_ids = vec![fresh_peer.id, other_fresh_peer.id];
        expected_ids.sort();
        assert_eq!(expected_ids, peer_ids(&peer_tracker), "the weakest peer must be evicted for a stronger newcomer");

        // Full with {fresh, fresh}: a weaker newcomer is dropped and nothing changes.
        peer_tracker.add_peer(stale_peer_2.clone())?;
        assert_eq!(expected_ids, peer_ids(&peer_tracker), "a weaker newcomer must be dropped");
        Ok(())
    }

    #[tokio::test]
    async fn add_peer_rejects_far_future_timestamp() -> anyhow::Result<()> {
        let (runtime_services, _) = manual_runtime_services();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage).await?;

        let (server_id, mut peer) = make_peer(&runtime_services).await?;
        let future_time_provider = ManualTimeProvider::new(START_TIME_MILLIS + config::PEER_MAX_FUTURE_TIMESTAMP_MILLIS.const_mul(2));
        peer.sign(&future_time_provider, &server_id.keys.signature_key)?;
        assert!(peer.verify().is_ok(), "test setup: the record itself is validly signed");

        assert!(peer_tracker.add_peer(peer).is_err(), "far-future peer must be rejected");
        assert_eq!(0, peer_tracker.len());
        Ok(())
    }

    #[tokio::test]
    async fn add_peer_skips_verify_for_known_peer_that_is_not_newer() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage).await?;

        let (_, peer) = make_peer(&runtime_services).await?;
        peer_tracker.add_peer(peer.clone())?;
        peer_tracker.flush().await?;
        assert!(!peer_tracker.needs_flush());

        // Same timestamp but invalid: if verify ran this would be rejected; the dedupe must short-circuit first.
        let mut tampered_same_timestamp_peer = peer.clone();
        tampered_same_timestamp_peer.pow_initial.salt = crate::tools::types::Salt::zero();
        assert!(peer_tracker.add_peer(tampered_same_timestamp_peer).is_ok());
        assert!(!peer_tracker.needs_flush(), "a non-newer copy must not trigger a flush");
        assert_eq!(peer, peer_tracker.peers()[0]);

        // A newer but invalid copy does reach verify and is rejected.
        let mut tampered_newer_peer = peer.clone();
        tampered_newer_peer.timestamp = TimeMillis(peer.timestamp.0 + 1);
        assert!(peer_tracker.add_peer(tampered_newer_peer).is_err());
        assert_eq!(peer, peer_tracker.peers()[0]);
        Ok(())
    }

    #[tokio::test]
    async fn load_drops_far_future_peers_and_trims_to_max_peers() -> anyhow::Result<()> {
        let (runtime_services, _) = manual_runtime_services();
        let client_storage = MemClientStorage::new().await?;
        const MAX_PEERS: usize = 3;

        let mut stored_peers = Vec::new();
        for _ in 0..MAX_PEERS + 2 {
            let (_, peer) = make_peer(&runtime_services).await?;
            stored_peers.push(peer);
        }
        let (far_future_server_id, mut far_future_peer) = make_peer(&runtime_services).await?;
        let future_time_provider = ManualTimeProvider::new(START_TIME_MILLIS + config::PEER_MAX_FUTURE_TIMESTAMP_MILLIS.const_mul(2));
        far_future_peer.sign(&future_time_provider, &far_future_server_id.keys.signature_key)?;
        stored_peers.push(far_future_peer.clone());

        let now = runtime_services.time_provider.current_time_millis();
        client_storage::put_struct(client_storage.as_ref(), BUCKET_PEER, "peers", &stored_peers, now).await?;

        let peer_tracker = PeerTracker::new_with_max_peers(runtime_services.clone(), client_storage, MAX_PEERS).await?;
        assert_eq!(MAX_PEERS, peer_tracker.len());
        assert!(!peer_ids(&peer_tracker).contains(&far_future_peer.id), "far-future stored peer must be dropped");
        assert!(peer_tracker.needs_flush(), "trimmed set must be persisted");
        for window in peer_tracker.peers().windows(2) {
            assert!(window[0].id < window[1].id, "loaded peers must be sorted by id");
        }
        Ok(())
    }

    #[tokio::test]
    async fn add_peers_considers_at_most_the_per_response_cap() -> anyhow::Result<()> {
        let runtime_services = RuntimeServices::default_for_testing();
        let client_storage = MemClientStorage::new().await?;
        let mut peer_tracker = PeerTracker::new(runtime_services.clone(), client_storage).await?;

        let mut response_peers = Vec::new();
        for _ in 0..config::CLIENT_MAX_PEERS_CONSIDERED_PER_RESPONSE + 3 {
            let (_, peer) = make_peer(&runtime_services).await?;
            response_peers.push(peer);
        }

        let mut peer_iterator = PeerIterator::new(&mut peer_tracker, Id::random(), 1, None);
        peer_iterator.add_peers(response_peers);
        assert_eq!(config::CLIENT_MAX_PEERS_CONSIDERED_PER_RESPONSE, peer_tracker.len());
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
