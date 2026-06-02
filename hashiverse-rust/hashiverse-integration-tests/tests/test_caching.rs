use futures::future::join_all;
use hashiverse_lib::anyhow_assert;
use hashiverse_lib::anyhow_assert_eq;
use hashiverse_lib::client::client_storage::mem_client_storage::MemClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
use hashiverse_lib::protocol::payload::payload::{CachePostBundleV1, PayloadRequestKind, PayloadResponseKind};
use hashiverse_lib::protocol::posting::encoded_post_bundle::EncodedPostBundleV1;
use hashiverse_lib::protocol::rpc;
use hashiverse_lib::tools::buckets::BucketLocation;
use hashiverse_lib::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::time::{MILLIS_IN_MINUTE, MILLIS_IN_SECOND};
use hashiverse_lib::tools::time_provider::time_provider::{ScaledTimeProvider, TimeProvider};
use hashiverse_lib::tools::tools::{configure_logging_with_time_provider, get_temp_dir, leading_agreement_bits_xor};
use hashiverse_lib::tools::types::Id;
use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
use hashiverse_lib::transport::ddos::noop_ddos::NoopDdosProtection;
use hashiverse_lib::transport::mem_transport::MemTransportFactory;
use hashiverse_server_lib::environment::environment::EnvironmentFactory;
use hashiverse_server_lib::environment::mem_environment_store::MemEnvironmentFactory;
use hashiverse_server_lib::server::args::Args;
use hashiverse_server_lib::server::hashiverse_server::HashiverseServer;
use hashiverse_server_lib::server::post_bundle_caching_shared::CACHE_HIT_THRESHOLD;
use log::warn;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const CLOCK_SCALE_FACTOR: f64 = 15.0 * 60.0;

// --------------------------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------------------------

/// Start a bootstrap server on port 443 plus `num_servers - 1` more on 20001.., each running in
/// the background, and return them.
async fn start_cluster(
    runtime_services: &Arc<RuntimeServices>,
    environment_factory: &Arc<MemEnvironmentFactory>,
    cancellation_token: &CancellationToken,
    num_servers: u16,
) -> anyhow::Result<Vec<Arc<HashiverseServer>>> {
    let mut servers: Vec<Arc<HashiverseServer>> = Vec::new();

    let spawn = |server: Arc<HashiverseServer>| {
        let ct = cancellation_token.clone();
        tokio::spawn(async move { server.run(ct).await; });
    };

    let bootstrap = HashiverseServer::new(runtime_services.clone(), environment_factory.clone(), Args::default().with_port(443).with_force_local_network(true)).await?;
    servers.push(Arc::clone(&bootstrap));
    spawn(bootstrap);

    for i in 1..num_servers {
        let server = HashiverseServer::new(runtime_services.clone(), environment_factory.clone(), Args::default().with_port(20000 + i).with_force_local_network(true)).await?;
        servers.push(Arc::clone(&server));
        spawn(server);
    }

    Ok(servers)
}

async fn make_client(runtime_services: &Arc<RuntimeServices>, name: &str) -> anyhow::Result<Arc<HashiverseClient>> {
    let key_locker_manager = MemKeyLockerManager::new().await?;
    let key_locker = key_locker_manager.create(name.to_string()).await?;
    let client_storage = MemClientStorage::new().await?;
    Ok(Arc::new(HashiverseClient::new(runtime_services.clone(), client_storage, key_locker, hashiverse_lib::client::args::Args::default()).await?))
}

/// Drive `server`'s hit counter to the threshold (the same `on_get` the `GetPostBundleV1`
/// handler calls per request), assert it hands back a signed token, upload the bundle back via
/// the real `CachePostBundleV1` RPC, and assert the server then serves it from cache.
///
/// Request pressure is applied directly because under the scaled test clock a 60-second cache
/// TTI is only ~66ms of real time — far less than a client's bundle walk takes — so 10 real
/// walks can never land inside one TTI window.
async fn drive_hits_upload_and_verify_cached(
    server: &Arc<HashiverseServer>,
    bucket_location: &BucketLocation,
    bundle_bytes: &[u8],
    runtime_services: &Arc<RuntimeServices>,
    sponsor_id: &Id,
) -> anyhow::Result<()> {
    let now = runtime_services.time_provider.current_time_millis();
    let peer_self = server.peer_self.read().clone();

    let mut cache_request_token = None;
    for _ in 0..CACHE_HIT_THRESHOLD {
        let result = server.post_bundle_cache.on_get(bucket_location, &[], &peer_self, &server.server_id, now);
        cache_request_token = result.cache_request_token.or(cache_request_token);
    }
    let cache_request_token = cache_request_token.ok_or_else(|| anyhow::anyhow!("server {} did not issue a CacheRequestToken after the hit threshold", server.server_id.id))?;

    let request_bytes = CachePostBundleV1::new_to_bytes(&cache_request_token, &[bundle_bytes])?;
    let response = rpc::rpc::rpc_server_known(runtime_services, sponsor_id, &cache_request_token.peer, PayloadRequestKind::CachePostBundleV1, request_bytes).await?;
    anyhow_assert_eq!(&PayloadResponseKind::CachePostBundleResponseV1, &response.response_request_kind);

    let cached = server.post_bundle_cache.on_get(bucket_location, &[], &peer_self, &server.server_id, now);
    anyhow_assert!(
        !cached.cached_items.is_empty(),
        "server {} should serve the post bundle from cache after the CachePostBundleV1 upload",
        server.server_id.id
    );
    Ok(())
}

fn holds_bundle(server: &Arc<HashiverseServer>, location_id: &Id, now: hashiverse_lib::tools::time::TimeMillis) -> bool {
    server.environment.get_post_bundle_bytes(now, location_id).ok().flatten().is_some()
}

// --------------------------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------------------------

/// The nearest bundle-holding (primary) server caches the bundle once it is "hot": after
/// `CACHE_HIT_THRESHOLD` requests it issues a token and a `CachePostBundleV1` upload populates
/// its cache.
#[tokio::test(flavor = "multi_thread")]
async fn test_caching_post_bundles() -> anyhow::Result<()> {
    let (_temp_dir, temp_dir_path) = get_temp_dir()?;
    let time_provider = Arc::new(ScaledTimeProvider::new(CLOCK_SCALE_FACTOR));
    configure_logging_with_time_provider("warn", time_provider.clone());

    let cancellation_token = CancellationToken::new();
    let environment_factory = Arc::new(MemEnvironmentFactory::new(&temp_dir_path));
    let transport_factory = Arc::new(MemTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::new(vec!["443".to_string()])));
    let runtime_services = Arc::new(RuntimeServices {
        time_provider: time_provider.clone(),
        transport_factory,
        pow_generator: Arc::new(NativeParallelPowGenerator::new()),
    });

    warn!("--- Starting servers ---");
    let servers = start_cluster(&runtime_services, &environment_factory, &cancellation_token, 20).await?;
    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(30)).await;

    warn!("--- Posting ---");
    let poster = make_client(&runtime_services, "poster").await?;
    let (post_result, _) = poster.submit_post("Caching test post!").await?;
    anyhow_assert!(!post_result.is_empty(), "submit_post returned no commit tokens");
    let bucket_location = post_result[0].bucket_location.clone();
    let location_id = bucket_location.location_id;
    let sponsor_id = poster.client_id().id;

    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(5)).await;

    // The nearest server that actually holds the bundle.
    let now = time_provider.current_time_millis();
    let mut holders: Vec<Arc<HashiverseServer>> = servers.iter().filter(|s| holds_bundle(s, &location_id, now)).cloned().collect();
    anyhow_assert!(!holders.is_empty(), "no server holds the posted bundle after settling");
    holders.sort_by_key(|s| -leading_agreement_bits_xor(&s.server_id.id.0, &location_id.0));
    let server = holders[0].clone();
    let bundle_bytes = server.environment.get_post_bundle_bytes(now, &location_id)?.expect("bundle bytes present");
    let _ = EncodedPostBundleV1::from_bytes(bundle_bytes.clone(), true)?; // sanity: a real signed bundle

    warn!("--- Driving cache on nearest holder {} ---", server.server_id.id);
    drive_hits_upload_and_verify_cached(&server, &bucket_location, bundle_bytes.as_ref(), &runtime_services, &sponsor_id).await?;
    warn!("--- Nearest holder is serving the bundle from cache ---");

    cancellation_token.cancel();
    Ok(())
}

/// The federated cache spreads to servers *around* the primaries: a server that does NOT hold
/// the bundle still issues a token once requested enough ("asking") and stores a cached copy
/// after the upload ("storing at itself").
#[tokio::test(flavor = "multi_thread")]
async fn test_caching_spreads_to_surrounding_servers() -> anyhow::Result<()> {
    let (_temp_dir, temp_dir_path) = get_temp_dir()?;
    let time_provider = Arc::new(ScaledTimeProvider::new(CLOCK_SCALE_FACTOR));
    configure_logging_with_time_provider("warn", time_provider.clone());

    let cancellation_token = CancellationToken::new();
    let environment_factory = Arc::new(MemEnvironmentFactory::new(&temp_dir_path));
    let transport_factory = Arc::new(MemTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::new(vec!["443".to_string()])));
    let runtime_services = Arc::new(RuntimeServices {
        time_provider: time_provider.clone(),
        transport_factory,
        pow_generator: Arc::new(NativeParallelPowGenerator::new()),
    });

    warn!("--- Starting servers ---");
    let servers = start_cluster(&runtime_services, &environment_factory, &cancellation_token, 20).await?;
    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(30)).await;

    warn!("--- Posting ---");
    let poster = make_client(&runtime_services, "poster").await?;
    let (post_result, _) = poster.submit_post("Spreading test post!").await?;
    anyhow_assert!(!post_result.is_empty(), "submit_post returned no commit tokens");
    let bucket_location = post_result[0].bucket_location.clone();
    let location_id = bucket_location.location_id;
    let sponsor_id = poster.client_id().id;

    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(5)).await;

    let now = time_provider.current_time_millis();

    // The bundle bytes a client would forward, taken from a holder.
    let holder = servers.iter().find(|s| holds_bundle(s, &location_id, now)).expect("a holder exists").clone();
    let bundle_bytes = holder.environment.get_post_bundle_bytes(now, &location_id)?.expect("bundle bytes present");

    // The non-holders nearest to the location — the ring "around" the primaries.
    let mut surrounding: Vec<Arc<HashiverseServer>> = servers.iter().filter(|s| !holds_bundle(s, &location_id, now)).cloned().collect();
    surrounding.sort_by_key(|s| -leading_agreement_bits_xor(&s.server_id.id.0, &location_id.0));
    anyhow_assert!(surrounding.len() >= 3, "need at least 3 surrounding non-holder servers, got {}", surrounding.len());

    for server in surrounding.iter().take(3) {
        anyhow_assert!(!holds_bundle(server, &location_id, now), "surrounding server {} must not hold the bundle", server.server_id.id);
        warn!("--- Spreading cache to surrounding (non-holder) server {} ---", server.server_id.id);
        drive_hits_upload_and_verify_cached(server, &bucket_location, bundle_bytes.as_ref(), &runtime_services, &sponsor_id).await?;
        // It serves the bundle from cache yet still holds no copy of its own in the environment.
        anyhow_assert!(!holds_bundle(server, &location_id, now), "surrounding server {} should hold only a cached copy, not an environment copy", server.server_id.id);
    }
    warn!("--- 3 surrounding servers now serve the bundle from cache ---");

    cancellation_token.cancel();
    Ok(())
}

/// Best-effort, end-to-end version of the spreading behaviour, driven purely by real client
/// fetches (no direct `on_get`): many clients repeatedly fetch the same bundle; once the
/// primaries are cached, each client's `cache_radius` pushes its next walk one ring wider, so the
/// surrounding servers start receiving requests, issue tokens and get the bundle uploaded to them.
///
/// `#[ignore]` because this depends on delicate timing — surrounding servers must accumulate
/// `CACHE_HIT_THRESHOLD` hits within the 60s server-side window, which needs enough concurrent
/// clients and a slow-enough clock that the request burst fits. Run on demand with
/// `--run-ignored all` (or `cargo test -- --ignored`). The deterministic test above is the
/// CI-stable coverage; this one documents and exercises the genuine dynamic.
#[ignore = "timing-sensitive cache-radius spreading; run manually with --run-ignored"]
#[tokio::test(flavor = "multi_thread")]
async fn test_caching_spreads_via_client_fetches() -> anyhow::Result<()> {
    // Slower clock so a concurrent fetch burst fits inside the 60-second server hit window.
    let time_provider = Arc::new(ScaledTimeProvider::new(20.0));
    let (_temp_dir, temp_dir_path) = get_temp_dir()?;
    configure_logging_with_time_provider("warn", time_provider.clone());

    let cancellation_token = CancellationToken::new();
    let environment_factory = Arc::new(MemEnvironmentFactory::new(&temp_dir_path));
    let transport_factory = Arc::new(MemTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::new(vec!["443".to_string()])));
    let runtime_services = Arc::new(RuntimeServices {
        time_provider: time_provider.clone(),
        transport_factory,
        pow_generator: Arc::new(NativeParallelPowGenerator::new()),
    });

    warn!("--- Starting servers ---");
    let servers = start_cluster(&runtime_services, &environment_factory, &cancellation_token, 20).await?;
    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(20)).await;

    warn!("--- Posting ---");
    let poster = make_client(&runtime_services, "poster").await?;
    let (post_result, _) = poster.submit_post("Client-driven spreading test!").await?;
    anyhow_assert!(!post_result.is_empty(), "submit_post returned no commit tokens");
    let bucket_location = post_result[0].bucket_location.clone();
    let location_id = bucket_location.location_id;
    let post_id = post_result[0].post_id;

    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(5)).await;

    // Many clients (the more concurrent walks, the more hits concentrate on the closest
    // surrounding server in a single burst).
    let mut clients = Vec::new();
    for i in 0..20 {
        clients.push(make_client(&runtime_services, &format!("fetcher-{}", i)).await?);
    }

    // Prime each client's cache radius (walk reaches the primaries and records the radius).
    warn!("--- Priming cache radius across {} clients ---", clients.len());
    let _ = join_all(clients.iter().map(|c| c.get_post(bucket_location.clone(), &post_id))).await;

    let mut spread = false;
    for round in 1..=4 {
        // Let each client's local bundle cache expire (5 min) so the next fetch re-walks —
        // now radius-pruned, hitting the surrounding ring first.
        time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(6)).await;

        warn!("--- Spreading round {} ---", round);
        let _ = join_all(clients.iter().map(|c| c.get_post(bucket_location.clone(), &post_id))).await;

        // Let the fire-and-forget cache uploads land (well within the 60s server TTI).
        time_provider.sleep_millis(MILLIS_IN_SECOND.const_mul(30)).await;

        let now = time_provider.current_time_millis();
        let surrounding_cached = servers
            .iter()
            .filter(|s| !holds_bundle(s, &location_id, now))
            .filter(|s| {
                let peer_self = s.peer_self.read().clone();
                !s.post_bundle_cache.on_get(&bucket_location, &[], &peer_self, &s.server_id, now).cached_items.is_empty()
            })
            .count();
        warn!("--- Round {}: {} surrounding (non-holder) servers now serve a cached copy ---", round, surrounding_cached);
        if surrounding_cached > 0 {
            spread = true;
            break;
        }
    }

    cancellation_token.cancel();
    anyhow_assert!(spread, "expected the cache to spread to at least one surrounding (non-holder) server via client fetches");
    Ok(())
}
