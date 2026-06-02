//! # Top-level server orchestrator
//!
//! [`HashiverseServer`] is the server-binary analogue of
//! [`hashiverse_lib::client::hashiverse_client::HashiverseClient`]: the single struct
//! that wires together every subsystem the node needs to operate and hands them to
//! the inbound-request handler.
//!
//! What it owns:
//!
//! - **Identity** — a persisted [`hashiverse_lib::tools::server_id::ServerId`] is
//!   loaded from the [`crate::environment::environment::Environment`] or minted
//!   fresh with proof-of-work on first start.
//! - **Transport** — built via a `TransportFactory` (TLS in production, plain TCP
//!   or in-memory in tests) and bound to the port from
//!   [`crate::server::args::Args`].
//! - **DHT** — a [`crate::server::kademlia::kademlia::Kademlia`] populated from the
//!   persisted peer buckets at startup and kept up to date by the handler dispatch.
//! - **Caches** — [`crate::server::post_bundle_caching`] and
//!   [`crate::server::post_bundle_feedback_caching`] plus per-connection reply-salt
//!   and heal caches, all backed by `moka` with TTL/TTI eviction.
//! - **Replay protection** — a short-window set of observed request salts so an
//!   attacker can't replay a valid signed request back at us.

use crate::environment::environment::{Environment, EnvironmentDimensions, EnvironmentFactory, CONFIG_KADEMLIA_PEER_BUCKETS, CONFIG_SERVER_ID};
use crate::server::kademlia::kademlia;
use crate::server::kademlia::kademlia::Kademlia;
use crate::server::post_bundle_caching::PostBundleCache;
use crate::server::post_bundle_feedback_caching::PostBundleFeedbackCache;
use hashiverse_lib::anyhow_assert_eq;
use hashiverse_lib::protocol::payload::payload::{AnnounceResponseV1, AnnounceResponseV2, AnnounceV1, AnnounceV2, BootstrapResponseV1, BootstrapV1, PayloadRequestKind, PayloadResponseKind, PeerStatsResponseV1, PAYLOAD_REQUEST_KIND_COUNT};
use hashiverse_lib::protocol::peer::Peer;
use hashiverse_lib::protocol::rpc;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::server_id::ServerId;
use hashiverse_lib::tools::time::{TimeMillis, MILLIS_IN_MINUTE, MILLIS_IN_SECOND};
use hashiverse_lib::tools::time_provider::moka_clock::TimeProviderMokaClock;
use hashiverse_lib::tools::time_provider::time_provider::TimeProvider;
use hashiverse_lib::tools::types::{Id, Salt};
use hashiverse_lib::tools::{config, tools};
use hashiverse_lib::tools::json;
use hashiverse_lib::transport::transport::{IncomingRequest, TransportServer};
use log::{error, info, trace, warn};
use moka::sync::Cache;
use parking_lot::{Mutex, RwLock};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use bytes::Bytes;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use hashiverse_lib::protocol::rpc::rpc_response::RpcResponsePacketRx;
use hashiverse_lib::tools::hyper_log_log::HyperLogLog;
use hashiverse_lib::protocol::payload::payload::TrendingHashtagsFetchResponseV1;
use crate::server::args::Args;

pub struct HashiverseServer {
    pub runtime_services: Arc<RuntimeServices>,
    pub environment: Arc<Environment>,
    pub server_id: ServerId,
    pub kademlia: Arc<RwLock<Kademlia<Id, Peer>>>,
    pub transport_server: Arc<dyn TransportServer>,
    pub peer_self: Arc<RwLock<Peer>>,
    pub heal_in_progress: Cache<Id, ()>,
    pub seen_salts: Cache<Salt, ()>,
    pub post_bundle_cache: PostBundleCache,
    pub post_bundle_feedback_cache: PostBundleFeedbackCache,
    pub trending_hashtags: Cache<String, HyperLogLog>,
    pub trending_hashtags_response_cache: Mutex<Option<(TimeMillis, TrendingHashtagsFetchResponseV1)>>,
    /// Per-`PayloadRequestKind` running counter of inbound dispatches. Incremented
    /// after packet decode + replay-guard but before per-handler PoW gates, so
    /// adversarial load shows up too.
    pub request_counters: Arc<[AtomicU64; PAYLOAD_REQUEST_KIND_COUNT]>,
    /// Cached signed stats blob. The `TimeMillis` is the timestamp recorded in
    /// the cached `PeerStatsResponseV1` itself — the cache hands the response
    /// back verbatim so clients re-sharing it get a single canonical byte sequence.
    pub peer_stats_response_cache: Mutex<Option<(TimeMillis, PeerStatsResponseV1)>>,
}

impl HashiverseServer {
    pub async fn new(runtime_services: Arc<RuntimeServices>, environment_factory: Arc<dyn EnvironmentFactory>, args: Args) -> anyhow::Result<Arc<Self>> {
        let environment_dimensions = EnvironmentDimensions::default().with_max_size_bytes(args.max_post_database_size_megabytes * 1024 * 1024); 
        let environment = environment_factory.open_next_available(environment_dimensions).await?;

        // let passphrase = passphrase::get_passphrase(args.passphrase_path);
        let config_server_id = environment.config_get_bytes(CONFIG_SERVER_ID)?;
        let server_id = match config_server_id {
            None => {
                let server_id = ServerId::new("own_pow", runtime_services.time_provider.as_ref(), config::SERVER_KEY_POW_MIN, args.skip_pq_commitment_bytes, runtime_services.pow_generator.as_ref()).await?;
                environment.config_put_bytes(CONFIG_SERVER_ID, server_id.encode()?)?;
                info!("starting new server with server_id={}", server_id);
                server_id
            }
            Some(config_server_id) => {
                let server_id = ServerId::decode(config_server_id.as_ref())?;
                server_id.verify()?;
                info!("restarting existing server with server_id={}", server_id);
                server_id
            }
        };

        let transport_server = runtime_services.transport_factory.create_server(&args.base_path, args.port, args.force_local_network).await?;

        // Update the address in our Peer record
        let mut peer_self = server_id.to_peer(runtime_services.time_provider.as_ref())?;
        peer_self.address = transport_server.get_address().to_string();
        peer_self.sign(runtime_services.time_provider.as_ref(), &server_id.keys.signature_key)?;

        // Boto our kademlia
        let mut kademlia = Kademlia::<Id, Peer>::new(server_id.id, config::SERVER_KADEMLIA_MAX_PEERS_PER_BUCKET);
        {
            let now = runtime_services.time_provider.current_time_millis();

            // We always belong in our own kademlia
            kademlia.add_peer(peer_self.clone(), now)?;

            // Have we previously persisted our peer buckets?
            {
                let try_result = try {
                    let peer_buckets = environment.config_get_struct::<Vec<Vec<Peer>>>(CONFIG_KADEMLIA_PEER_BUCKETS)?;
                    if let Some(peer_buckets) = peer_buckets {
                        for peer_bucket in peer_buckets {
                            for peer in peer_bucket {
                                kademlia.add_peer(peer, now)?;
                            }
                        }
                    }
                };

                if let Err(e) = try_result {
                    warn!("problem depersisting peer_buckets: {}", e);
                }
            }
        }

        info!("server_id={}", server_id);
        info!("peer_self={}", peer_self);

        // All time-based moka caches below run on our TimeProvider (scaled in tests), not wall time.
        let time_provider = runtime_services.time_provider.clone();

        let hashiverse_server = HashiverseServer {
            runtime_services,
            environment: Arc::new(environment),
            server_id,
            kademlia: Arc::new(RwLock::new(kademlia)),
            transport_server,
            peer_self: Arc::new(RwLock::new(peer_self)),
            heal_in_progress: Cache::builder().time_to_live(Duration::from_secs(60)).external_clock(Arc::new(TimeProviderMokaClock::new(time_provider.clone()))).build(),
            seen_salts: Cache::builder().time_to_live(Duration::from_mins(5)).max_capacity(100_000).external_clock(Arc::new(TimeProviderMokaClock::new(time_provider.clone()))).build(),
            post_bundle_cache: PostBundleCache::new(config::SERVER_POST_BUNDLE_CACHE_MAX_ORIGINATORS_PER_LOCATION, config::SERVER_POST_BUNDLE_CACHE_MAX_BYTES, time_provider.clone()),
            post_bundle_feedback_cache: PostBundleFeedbackCache::new(config::SERVER_POST_BUNDLE_FEEDBACK_CACHE_MAX_BYTES, time_provider.clone()),
            trending_hashtags: Cache::builder().max_capacity(256).build(),
            trending_hashtags_response_cache: Mutex::new(None),
            request_counters: Arc::new(std::array::from_fn(|_| AtomicU64::new(0))),
            peer_stats_response_cache: Mutex::new(None),
        };

        Ok(Arc::new(hashiverse_server))
    }

    pub async fn run(&self, cancellation_token: CancellationToken) {
        info!("server started");

        let (tx, rx) = mpsc::channel::<IncomingRequest>(32);

        let res = tokio::try_join!(
            self.wrap_and_dispatch_network_envelopes(cancellation_token.clone(), rx),
            self.maintain_environment(cancellation_token.clone(), self.runtime_services.time_provider.clone()),
            self.maintain_kademlia(cancellation_token.clone(), self.runtime_services.time_provider.clone()),
            self.transport_server.listen(cancellation_token.clone(), tx),
        );

        match res {
            Ok(_) => info!("server stopped"),
            Err(e) => error!("server stopped with error: {}", e),
        }
    }

    pub async fn add_potential_peer_to_kademlia(&self, peer: Peer, time_millis: TimeMillis) {
        // Verify this peer
        let result = peer.verify();
        if let Err(e) = result {
            warn!("peer {} failed verification: {}", peer, e);
            return;
        }

        // Has this peer done enough work?
        if peer.pow_initial.pow < config::SERVER_KEY_POW_MIN {
            warn!("peer {} failed pow so not adding to our kademlia", peer);
            return;
        }

        let result = self.kademlia.write().add_peer(peer, time_millis);
        if let Err(e) = result {
            warn!("problem adding peer: {}", e);
        }
    }

    async fn rpc_server_unknown(&self, address: &str, payload_request_kind: PayloadRequestKind, payload: Bytes) -> anyhow::Result<RpcResponsePacketRx> {
        rpc::rpc::rpc_server_unknown(&self.runtime_services, &self.server_id.id, address, payload_request_kind, payload).await
    }

    async fn rpc_server_known(&self, destination_peer: &Peer, payload_request_kind: PayloadRequestKind, payload: Bytes) -> anyhow::Result<RpcResponsePacketRx> {
        rpc::rpc::rpc_server_known(&self.runtime_services, &self.server_id.id, destination_peer, payload_request_kind, payload).await
    }

    async fn maintain_environment(&self, cancellation_token: CancellationToken, time_provider: Arc<dyn TimeProvider>) -> Result<(), anyhow::Error> {
        loop {
            if cancellation_token.is_cancelled() {
                break;
            }

            let time_millis = time_provider.current_time_millis();

            self.environment.do_maintenance(&cancellation_token, time_millis).await?;

            tools::cancellable_sleep_millis(self.runtime_services.time_provider.as_ref(), MILLIS_IN_MINUTE, &cancellation_token).await;
        }

        Ok(())
    }

    async fn maintain_kademlia(&self, cancellation_token: CancellationToken, time_provider: Arc<dyn TimeProvider>) -> Result<(), anyhow::Error> {
        let mut last_bootstrap = TimeMillis::zero();
        let mut last_announce = TimeMillis::zero();
        let mut last_peers_dump_to_storage = self.runtime_services.time_provider.current_time_millis();

        loop {
            if cancellation_token.is_cancelled() {
                break;
            }

            let now = time_provider.current_time_millis();

            // Do we need to bootstrap?
            if now - last_bootstrap > config::MILLIS_TO_WAIT_BETWEEN_BOOTSTRAPS {
                last_bootstrap = now;

                let needs_bootstrapping = { self.kademlia.read().len() < config::MINIMUM_PEERS_TO_STOP_BOOTSTRAPPING };

                if needs_bootstrapping {

                    // Lets randomize these addresses so that the first one is not snowed
                    let mut bootstrap_addresses = self.runtime_services.transport_factory.get_bootstrap_addresses().await;
                    tools::shuffle(&mut bootstrap_addresses);
                    trace!("bootstrap addresses: {:?}", bootstrap_addresses);

                    for bootstrap_address in bootstrap_addresses {
                        if cancellation_token.is_cancelled() {
                            break;
                        }

                        let try_result: anyhow::Result<()> = try {
                            {
                                trace!("bootstrapping {}", bootstrap_address);
                                let rpc_response_packet_rx = self.rpc_server_unknown(&bootstrap_address, PayloadRequestKind::BootstrapV1, json::struct_to_bytes(&BootstrapV1 {})?).await?;
                                anyhow_assert_eq!(&PayloadResponseKind::BootstrapResponseV1, &rpc_response_packet_rx.response_request_kind);
                                let response = json::bytes_to_struct::<BootstrapResponseV1>(&rpc_response_packet_rx.bytes)?;
                                for peer in response.peers_random {
                                    self.add_potential_peer_to_kademlia(peer, now).await;
                                }
                            }
                        };

                        if let Err(e) = try_result {
                            warn!("problem bootstrapping {}: {}", bootstrap_address, e);
                        }

                        let needs_bootstrapping = { self.kademlia.read().len() < config::MINIMUM_PEERS_TO_STOP_BOOTSTRAPPING };
                        if !needs_bootstrapping {
                            break;
                        }
                        
                    }

                    trace!("We now have {} peers", self.kademlia.read().len());
                }
            }

            // Do we need to announce?
            if now - last_announce > config::MILLIS_TO_WAIT_BETWEEN_ANNOUNCES {
                last_announce = now;

                let peer_self = self.peer_self.read().clone();


                // Pick some candidates who we will poke
                let mut announce_peers = Vec::<Peer>::new();
                {
                    let kademlia = self.kademlia.read();

                    // One that is potentially dead
                    let peer_with_lowest_score = kademlia.get_peer_with_lowest_score();
                    if let Some(peer_with_lowest_score) = peer_with_lowest_score {
                        announce_peers.push(peer_with_lowest_score.clone());
                    }

                    // Someone in our vicinity so our vicinity becomes more tightly knit
                    let (peers_nearest, _) = kademlia.get_peers_for_key(&peer_self.id, 8);
                    if !peers_nearest.is_empty() {
                        announce_peers.push(tools::random_element(&peers_nearest).clone());
                    }
                }

                // Pick the peer that we havent heard from for the longest
                for announce_peer in announce_peers {
                    if cancellation_token.is_cancelled() {
                        break;
                    }

                    // If we have to announce to ourself, then simply push our own freshest peer
                    if announce_peer == peer_self {
                        // trace!("We are own own oldest peer!");
                        self.add_potential_peer_to_kademlia(peer_self.clone(), now).await;
                        continue;
                    }

                    // V2-first outbound. The transport ownership proof carries our current
                    // ACME chain (HTTPS) or an empty marker (mem / TCP). If we don't have a
                    // proof to send yet — e.g. ACME hasn't completed first issuance — we skip
                    // this announce tick; the next tick will pick up the chain as soon as the
                    // cert refresher loads it (see HttpsTransportOwnershipProof's cache + the
                    // RwLock pull pattern).
                    let proof_payload = match self.transport_server.get_transport_ownership_proof().make_ownership_proof_payload() {
                        Some(p) => p,
                        None => {
                            trace!("skipping announce to {}: no transport ownership proof available yet", announce_peer);
                            continue;
                        }
                    };

                    let try_result: anyhow::Result<()> = try {
                        // Try V2 first. If V2 fails for any reason — peer is V1-only and doesn't
                        // know the variant, our chain didn't validate at their end, transient
                        // network error — fall back to V1. Only if V1 *also* fails do we prune.
                        // This keeps the network functional during the V1 coexistence window:
                        // we never drop a peer just because they haven't upgraded yet.
                        let v2_result = self.rpc_server_known(
                            &announce_peer,
                            PayloadRequestKind::AnnounceV2,
                            json::struct_to_bytes(&AnnounceV2 { peer_self: peer_self.clone(), proof_payload: proof_payload.to_vec() })?,
                        ).await;

                        match v2_result {
                            Ok(rpc_response_packet_rx) => {
                                anyhow_assert_eq!(&PayloadResponseKind::AnnounceResponseV2, &rpc_response_packet_rx.response_request_kind);
                                let response = json::bytes_to_struct::<AnnounceResponseV2>(&rpc_response_packet_rx.bytes)?;
                                self.add_potential_peer_to_kademlia(response.peer_self, now).await;
                                for peer in response.peers_nearest {
                                    self.add_potential_peer_to_kademlia(peer, now).await;
                                }
                            }
                            Err(v2_err) => {
                                trace!("V2 announce to {} failed: {}; falling back to V1", announce_peer, v2_err);
                                let rpc_response_packet_rx = self.rpc_server_known(&announce_peer, PayloadRequestKind::AnnounceV1, json::struct_to_bytes(&AnnounceV1 { peer_self: peer_self.clone() })?).await?;
                                anyhow_assert_eq!(&PayloadResponseKind::AnnounceResponseV1, &rpc_response_packet_rx.response_request_kind);
                                let response = json::bytes_to_struct::<AnnounceResponseV1>(&rpc_response_packet_rx.bytes)?;
                                self.add_potential_peer_to_kademlia(response.peer_self, now).await;
                                for peer in response.peers_nearest {
                                    self.add_potential_peer_to_kademlia(peer, now).await;
                                }
                            }
                        }
                    };

                    if let Err(e) = try_result {
                        warn!("problem announcing {}: {}", announce_peer, e);
                        self.kademlia.write().remove_peer(&announce_peer.id, now);
                    }
                }
            }

            // Do we need to persist our peer buckets?
            {
                let try_result = try {
                    let kademlia = self.kademlia.read();
                    if last_peers_dump_to_storage < kademlia.peers_last_changed() && now - last_peers_dump_to_storage > config::MILLIS_TO_WAIT_BETWEEN_PEER_DUMPS {
                        last_peers_dump_to_storage = kademlia.peers_last_changed();
                        let peer_buckets = kademlia.get_peer_buckets();
                        //let total_peers: usize = peer_buckets.iter().map(|peers| peers.len()).sum();
                        // trace!("persisting peer_buckets of length={}", total_peers);
                        self.environment.config_put_struct(CONFIG_KADEMLIA_PEER_BUCKETS, &peer_buckets)?;
                    }
                };

                if let Err(e) = try_result {
                    warn!("problem persisting peer_buckets: {}", e);
                }
            }

            tools::cancellable_sleep_millis(self.runtime_services.time_provider.as_ref(), MILLIS_IN_SECOND.const_mul(30), &cancellation_token).await;
        }

        Ok(())
    }
}

impl kademlia::Peer<Id> for Peer {
    fn id(&self) -> &Id {
        &self.id
    }
    fn score(&self, time_millis: TimeMillis) -> f64 {
        // This score makes sure that peers are currently active, but also benefits peers who have been active for a while
        self.pow_current_day.pow_decayed_day(time_millis) + self.pow_current_month.pow_decayed_month(time_millis)
    }
}
