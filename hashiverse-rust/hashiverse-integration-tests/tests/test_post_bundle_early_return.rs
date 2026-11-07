//! `LivePostBundleManager::get_post_bundle` answers on the first authoritative bundle, while the walk carries on in the
//! background to its best-of-quorum answer, which it caches (and heals/uploads) before `get_post_bundle_settled` returns.
//!
//! Every RPC is given latency on the test's scaled clock (`MemTransportFactory::set_rpc_delay`), so "earlier" and "later"
//! are measured in whole RPCs rather than scheduling noise.

use bytes::Bytes;
use hashiverse_lib::anyhow_assert;
use hashiverse_lib::client::client_storage::client_storage::{ClientStorage, BUCKET_POST_BUNDLE};
use hashiverse_lib::client::client_storage::mem_client_storage::MemClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
use hashiverse_lib::client::peer_tracker::peer_tracker::{PeerTracker, SharedPeerTracker};
use hashiverse_lib::client::post_bundle::live_post_bundle_manager::LivePostBundleManager;
use hashiverse_lib::client::post_bundle::post_bundle_manager::PostBundleManager;
use hashiverse_lib::protocol::posting::encoded_post_bundle::EncodedPostBundleV1;
use hashiverse_lib::tools::buckets::{generate_bucket_location, BucketLocation, BucketType, BUCKET_DURATIONS};
use hashiverse_lib::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::time::{DurationMillis, TimeMillis, MILLIS_IN_MINUTE};
use hashiverse_lib::tools::time_provider::time_provider::{ScaledTimeProvider, TimeProvider};
use hashiverse_lib::tools::tools::{configure_logging_with_time_provider, get_temp_dir, TempDirHandle};
use hashiverse_lib::tools::types::Id;
use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
use hashiverse_lib::transport::ddos::noop_ddos::NoopDdosProtection;
use hashiverse_lib::transport::mem_transport::MemTransportFactory;
use hashiverse_server_lib::environment::environment::EnvironmentFactory;
use hashiverse_server_lib::environment::mem_environment_store::MemEnvironmentFactory;
use hashiverse_server_lib::server::args::Args;
use hashiverse_server_lib::server::hashiverse_server::HashiverseServer;
use log::warn;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const CLOCK_SCALE_FACTOR: f64 = 15.0 * 60.0;
const NUM_SERVERS: u16 = 12;
/// Per-RPC latency on the scaled clock (~133ms of real time); see test_concurrent_walks.rs.
const RPC_DELAY: DurationMillis = MILLIS_IN_MINUTE.const_mul(2);
/// Bound on waiting for a background walk to settle.
const SETTLE_DEADLINE: DurationMillis = MILLIS_IN_MINUTE.const_mul(60);

struct Cluster {
    time_provider: Arc<ScaledTimeProvider>,
    runtime_services: Arc<RuntimeServices>,
    transport_factory: Arc<MemTransportFactory>,
    cancellation_token: CancellationToken,
    _temp_dir: TempDirHandle,
}

async fn start_cluster() -> anyhow::Result<Cluster> {
    let (temp_dir, temp_dir_path) = get_temp_dir()?;
    let time_provider = Arc::new(ScaledTimeProvider::new(CLOCK_SCALE_FACTOR));
    configure_logging_with_time_provider("warn", time_provider.clone());

    let cancellation_token = CancellationToken::new();
    let environment_factory = Arc::new(MemEnvironmentFactory::new(&temp_dir_path));
    let transport_factory = Arc::new(MemTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::new(vec!["443".to_string()])));
    let runtime_services = Arc::new(RuntimeServices {
        time_provider: time_provider.clone(),
        transport_factory: transport_factory.clone(),
        pow_generator: Arc::new(NativeParallelPowGenerator::new()),
    });

    for i in 0..NUM_SERVERS {
        let port = if i == 0 { 443 } else { 20000 + i };
        let server = HashiverseServer::new(runtime_services.clone(), environment_factory.clone(), Args::default().with_port(port).with_force_local_network(true)).await?;
        let server_cancellation_token = cancellation_token.clone();
        tokio::spawn(async move { server.run(server_cancellation_token).await });
    }
    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(30)).await;

    Ok(Cluster { time_provider, runtime_services, transport_factory, cancellation_token, _temp_dir: temp_dir })
}

/// Posts once, waiting for every redundant submission, and returns where the post landed and its id.
async fn post_something(cluster: &Cluster) -> anyhow::Result<(BucketLocation, Id)> {
    let key_locker_manager = MemKeyLockerManager::new().await?;
    let key_locker = key_locker_manager.create("poster".to_string()).await?;
    let client_storage = MemClientStorage::new().await?;
    let poster = HashiverseClient::new(cluster.runtime_services.clone(), client_storage, key_locker, hashiverse_lib::client::args::Args::default()).await?;
    let (submit_post_commit_tokens, _) = poster.submit_post_with_wait("Early return test post", true).await?;
    anyhow_assert!(!submit_post_commit_tokens.is_empty(), "submit_post returned no commit tokens");
    Ok((submit_post_commit_tokens[0].bucket_location.clone(), submit_post_commit_tokens[0].post_id))
}

/// A manager with its own storage and peer tracker, its tracker already bootstrapped (by a walk to an empty bucket).
async fn new_manager(cluster: &Cluster) -> anyhow::Result<(LivePostBundleManager, Arc<MemClientStorage>)> {
    let client_storage = MemClientStorage::new().await?;
    let peer_tracker: SharedPeerTracker = Arc::new(parking_lot::RwLock::new(PeerTracker::new(cluster.runtime_services.clone(), client_storage.clone()).await?));
    let manager = LivePostBundleManager::new(cluster.runtime_services.clone(), Id::random(), client_storage.clone(), peer_tracker);
    let _ = manager.get_post_bundle_settled(&fresh_bucket_location(cluster)?, cluster.time_provider.current_time_millis()).await;
    Ok((manager, client_storage))
}

fn fresh_bucket_location(cluster: &Cluster) -> anyhow::Result<BucketLocation> {
    generate_bucket_location(BucketType::User, Id::random(), BUCKET_DURATIONS[0], cluster.time_provider.current_time_millis())
}

async fn cached_post_bundle(client_storage: &MemClientStorage, bucket_location: &BucketLocation, time_millis: TimeMillis) -> anyhow::Result<Option<EncodedPostBundleV1>> {
    let raw = client_storage.get(BUCKET_POST_BUNDLE, &bucket_location.location_id.to_hex_str(), time_millis).await?;
    raw.map(|raw| EncodedPostBundleV1::from_bytes(Bytes::from(raw), true)).transpose()
}

/// Runs `get_post_bundle` and `get_post_bundle_settled` against the same walk, returning each answer with the time it arrived.
async fn early_and_settled(cluster: &Cluster, manager: &LivePostBundleManager, bucket_location: &BucketLocation) -> anyhow::Result<((EncodedPostBundleV1, TimeMillis), (EncodedPostBundleV1, TimeMillis))> {
    let time_millis = cluster.time_provider.current_time_millis();
    let early = async {
        let encoded_post_bundle = manager.get_post_bundle(bucket_location, time_millis).await?;
        anyhow::Ok((encoded_post_bundle, cluster.time_provider.current_time_millis()))
    };
    let settled = async {
        let encoded_post_bundle = manager.get_post_bundle_settled(bucket_location, time_millis).await?;
        anyhow::Ok((encoded_post_bundle, cluster.time_provider.current_time_millis()))
    };
    let (early, settled) = tokio::join!(early, settled);
    Ok((early?, settled?))
}

/// A bucket holding a post: the early answer (already containing the post) arrives at least one RPC before the settled one.
#[tokio::test(flavor = "multi_thread")]
async fn test_early_answer_arrives_before_the_walk_settles() -> anyhow::Result<()> {
    let cluster = start_cluster().await?;
    let (bucket_location, post_id) = post_something(&cluster).await?;
    let (manager, _) = new_manager(&cluster).await?;
    cluster.transport_factory.set_rpc_delay(cluster.time_provider.clone(), RPC_DELAY);

    let ((early_encoded_post_bundle, early_arrived), (settled_encoded_post_bundle, settled_arrived)) = early_and_settled(&cluster, &manager, &bucket_location).await?;
    warn!("--- early answer at {}, settled answer at {} ---", early_arrived, settled_arrived);

    anyhow_assert!(early_encoded_post_bundle.header.encoded_post_ids.contains(&post_id), "the early answer must already hold the post");
    anyhow_assert!(settled_encoded_post_bundle.header.encoded_post_ids.contains(&post_id), "the settled answer must hold the post");
    anyhow_assert!(
        early_arrived + RPC_DELAY <= settled_arrived,
        "the early answer must arrive at least one rpc before the walk settles: early={} settled={}",
        early_arrived,
        settled_arrived
    );

    cluster.cancellation_token.cancel();
    Ok(())
}

/// After an early answer the walk keeps going: the provisional bundle is never stored, the settled one is stored in the
/// background, and the next lookup is then served from the local cache without any RPC.
#[tokio::test(flavor = "multi_thread")]
async fn test_walk_settles_and_caches_in_the_background() -> anyhow::Result<()> {
    let cluster = start_cluster().await?;
    let (bucket_location, post_id) = post_something(&cluster).await?;
    let (manager, client_storage) = new_manager(&cluster).await?;
    cluster.transport_factory.set_rpc_delay(cluster.time_provider.clone(), RPC_DELAY);

    let time_millis = cluster.time_provider.current_time_millis();
    let early_encoded_post_bundle = manager.get_post_bundle(&bucket_location, time_millis).await?;
    anyhow_assert!(early_encoded_post_bundle.header.encoded_post_ids.contains(&post_id), "the early answer must hold the post");
    anyhow_assert!(cached_post_bundle(&client_storage, &bucket_location, cluster.time_provider.current_time_millis()).await?.is_none(), "a provisional bundle must never be stored");

    // Nobody is waiting any more, yet the walk must still settle and store its result
    let settle_started = cluster.time_provider.current_time_millis();
    let settled_encoded_post_bundle = loop {
        if let Some(cached_encoded_post_bundle) = cached_post_bundle(&client_storage, &bucket_location, cluster.time_provider.current_time_millis()).await? {
            break cached_encoded_post_bundle;
        }
        anyhow_assert!(cluster.time_provider.current_time_millis() - settle_started < SETTLE_DEADLINE, "the background walk never settled");
        cluster.time_provider.sleep_millis(RPC_DELAY).await;
    };
    anyhow_assert!(settled_encoded_post_bundle.header.encoded_post_ids.contains(&post_id), "the stored bundle must hold the post");

    let cached_lookup_started = cluster.time_provider.current_time_millis();
    let cached_encoded_post_bundle = manager.get_post_bundle(&bucket_location, cached_lookup_started).await?;
    let cached_lookup_elapsed = cluster.time_provider.current_time_millis() - cached_lookup_started;
    anyhow_assert!(cached_lookup_elapsed < RPC_DELAY, "a settled bucket must be served from the local cache, took {}", cached_lookup_elapsed);
    anyhow_assert!(cached_encoded_post_bundle.header.encoded_post_ids == settled_encoded_post_bundle.header.encoded_post_ids);

    cluster.cancellation_token.cancel();
    Ok(())
}

/// An empty bucket has no authoritative bundle to show early, so both entry points answer only once the walk has settled
/// (and stored the empty bundle).
#[tokio::test(flavor = "multi_thread")]
async fn test_empty_bucket_answers_only_once_settled() -> anyhow::Result<()> {
    let cluster = start_cluster().await?;
    let (manager, client_storage) = new_manager(&cluster).await?;
    cluster.transport_factory.set_rpc_delay(cluster.time_provider.clone(), RPC_DELAY);

    let bucket_location = fresh_bucket_location(&cluster)?;
    let ((early_encoded_post_bundle, early_arrived), (_, settled_arrived)) = early_and_settled(&cluster, &manager, &bucket_location).await?;
    warn!("--- empty bucket: early answer at {}, settled answer at {} ---", early_arrived, settled_arrived);

    anyhow_assert!(early_encoded_post_bundle.header.num_posts == 0);
    let gap = if early_arrived > settled_arrived { early_arrived - settled_arrived } else { settled_arrived - early_arrived };
    anyhow_assert!(gap < RPC_DELAY, "both answers must come from the settled walk: early={} settled={}", early_arrived, settled_arrived);
    anyhow_assert!(cached_post_bundle(&client_storage, &bucket_location, cluster.time_provider.current_time_millis()).await?.is_some(), "the settled empty bundle is stored");

    cluster.cancellation_token.cancel();
    Ok(())
}
