//! Concurrent DHT walks from one client must neither deadlock nor serialise behind the shared `PeerTracker`.
//!
//! The `MemTransportFactory` RPC delay gives every request real latency on the test's (scaled) clock, so walks that hold
//! a lock across their RPCs run measurably one after another, while walks that only lock per step overlap.

use futures::future::join_all;
use hashiverse_lib::anyhow_assert;
use hashiverse_lib::client::client_storage::mem_client_storage::MemClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
use hashiverse_lib::tools::buckets::{generate_bucket_location, BucketLocation, BucketType, BUCKET_DURATIONS};
use hashiverse_lib::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::time::{DurationMillis, MILLIS_IN_MINUTE};
use hashiverse_lib::tools::time_provider::time_provider::{ScaledTimeProvider, TimeProvider};
use hashiverse_lib::tools::tools::{configure_logging_with_time_provider, get_temp_dir};
use hashiverse_lib::tools::types::Id;
use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
use hashiverse_lib::transport::ddos::noop_ddos::NoopDdosProtection;
use hashiverse_lib::transport::mem_transport::MemTransportFactory;
use hashiverse_server_lib::environment::mem_environment_store::MemEnvironmentFactory;
use hashiverse_server_lib::server::args::Args;
use hashiverse_server_lib::server::hashiverse_server::HashiverseServer;
use log::warn;
use std::future::Future;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const CLOCK_SCALE_FACTOR: f64 = 15.0 * 60.0;
const NUM_SERVERS: u16 = 12;
const CONCURRENT_WALKS: usize = 4;
/// Per-RPC latency on the scaled clock (~133ms of real time): large enough to dominate the reduced test PoW,
/// small enough that a whole walk stays well inside the bundle timestamp threshold.
const RPC_DELAY: DurationMillis = MILLIS_IN_MINUTE.const_mul(2);
/// Far longer than the serial baseline: hitting it means the walks deadlocked.
const DEADLOCK_DEADLINE: DurationMillis = MILLIS_IN_MINUTE.const_mul(60 * 4);

struct Cluster {
    time_provider: Arc<ScaledTimeProvider>,
    transport_factory: Arc<MemTransportFactory>,
    client: Arc<HashiverseClient>,
    cancellation_token: CancellationToken,
    _temp_dir: tempfile::TempDir,
}

async fn start_cluster_and_client() -> anyhow::Result<Cluster> {
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

    let key_locker_manager = MemKeyLockerManager::new().await?;
    let key_locker = key_locker_manager.create("walker".to_string()).await?;
    let client_storage = MemClientStorage::new().await?;
    let client = Arc::new(HashiverseClient::new(runtime_services.clone(), client_storage, key_locker, hashiverse_lib::client::args::Args::default()).await?);

    // Warm the client's PeerTracker (bootstrap + a first walk) before any timing, so no measured walk pays for it
    let _ = client.get_post(fresh_bucket_location(&time_provider)?, &Id::zero()).await;

    Ok(Cluster { time_provider, transport_factory, client, cancellation_token, _temp_dir: temp_dir })
}

/// A bucket nobody has fetched before, so the client's own cache can never short-circuit the walk.
fn fresh_bucket_location(time_provider: &Arc<ScaledTimeProvider>) -> anyhow::Result<BucketLocation> {
    generate_bucket_location(BucketType::User, Id::random(), BUCKET_DURATIONS[0], time_provider.current_time_millis())
}

/// One post-bundle walk.  The bucket is empty, so `get_post` errors with "not found" - but only after the walk completed.
async fn post_bundle_walk(client: &HashiverseClient, bucket_location: BucketLocation) {
    let _ = client.get_post(bucket_location, &Id::zero()).await;
}

/// One feedback walk.
async fn feedback_walk(client: &HashiverseClient, bucket_location: BucketLocation) {
    let _ = client.get_post_feedbacks(bucket_location, Id::random()).await;
}

/// Runs `future` against the test clock; errors if it hasn't finished by `deadline`.
async fn within_deadline<T>(time_provider: &Arc<ScaledTimeProvider>, deadline: DurationMillis, future: impl Future<Output = T>) -> anyhow::Result<T> {
    tokio::select! {
        result = future => Ok(result),
        _ = time_provider.sleep_millis(deadline) => anyhow::bail!("did not finish within {} - deadlocked?", deadline),
    }
}

/// Post-bundle and feedback walks, plus tracker reads, all running at once on one client must all finish.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_walks_do_not_deadlock() -> anyhow::Result<()> {
    let cluster = start_cluster_and_client().await?;
    cluster.transport_factory.set_rpc_delay(cluster.time_provider.clone(), RPC_DELAY);

    warn!("--- Running {} post-bundle walks, {} feedback walks and tracker reads concurrently ---", CONCURRENT_WALKS, CONCURRENT_WALKS);
    let mut bucket_locations = Vec::new();
    for _ in 0..2 * CONCURRENT_WALKS {
        bucket_locations.push(fresh_bucket_location(&cluster.time_provider)?);
    }
    let client = &cluster.client;
    let post_bundle_walks = join_all(bucket_locations[..CONCURRENT_WALKS].iter().cloned().map(|bucket_location| post_bundle_walk(client, bucket_location)));
    let feedback_walks = join_all(bucket_locations[CONCURRENT_WALKS..].iter().cloned().map(|bucket_location| feedback_walk(client, bucket_location)));
    let tracker_reads = async {
        for _ in 0..20 {
            anyhow_assert!(!client.get_all_known_peers().await.is_empty(), "the tracker must stay readable during walks");
            cluster.time_provider.sleep_millis(RPC_DELAY).await;
        }
        Ok::<(), anyhow::Error>(())
    };

    let (_, _, tracker_reads_result) = within_deadline(&cluster.time_provider, DEADLOCK_DEADLINE, async { tokio::join!(post_bundle_walks, feedback_walks, tracker_reads) }).await?;
    tracker_reads_result?;

    cluster.cancellation_token.cancel();
    Ok(())
}

/// Concurrent walks overlap: `CONCURRENT_WALKS` of them at once take much less time than the same number one after another.
/// Before the PeerTracker lock was made per-step, each walk held the write lock across all its RPCs, so these took as long
/// as the serial baseline.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_walks_overlap() -> anyhow::Result<()> {
    let cluster = start_cluster_and_client().await?;
    cluster.transport_factory.set_rpc_delay(cluster.time_provider.clone(), RPC_DELAY);
    let client = &cluster.client;

    warn!("--- Serial baseline: {} post-bundle walks one after another ---", CONCURRENT_WALKS);
    let serial_started = cluster.time_provider.current_time_millis();
    within_deadline(&cluster.time_provider, DEADLOCK_DEADLINE, async {
        for _ in 0..CONCURRENT_WALKS {
            post_bundle_walk(client, fresh_bucket_location(&cluster.time_provider).expect("bucket location")).await;
        }
    })
    .await?;
    let serial_elapsed = cluster.time_provider.current_time_millis() - serial_started;

    warn!("--- {} post-bundle walks concurrently ---", CONCURRENT_WALKS);
    let mut bucket_locations = Vec::new();
    for _ in 0..CONCURRENT_WALKS {
        bucket_locations.push(fresh_bucket_location(&cluster.time_provider)?);
    }
    let concurrent_started = cluster.time_provider.current_time_millis();
    within_deadline(&cluster.time_provider, DEADLOCK_DEADLINE, join_all(bucket_locations.into_iter().map(|bucket_location| post_bundle_walk(client, bucket_location)))).await?;
    let concurrent_elapsed = cluster.time_provider.current_time_millis() - concurrent_started;

    warn!("--- serial={} concurrent={} ---", serial_elapsed, concurrent_elapsed);
    anyhow_assert!(serial_elapsed >= RPC_DELAY.const_mul(CONCURRENT_WALKS as i64), "test setup: every serial walk must pay at least one rpc delay, serial={}", serial_elapsed);
    // Perfect overlap would be 1/CONCURRENT_WALKS of the baseline; serialised walks would be ~1x.  Half leaves room for scheduling noise.
    anyhow_assert!(
        concurrent_elapsed.const_mul(2) < serial_elapsed,
        "concurrent walks must overlap: concurrent={} serial={}",
        concurrent_elapsed,
        serial_elapsed
    );

    cluster.cancellation_token.cancel();
    Ok(())
}
