#![feature(try_blocks)]

use hashiverse_lib::anyhow_assert;
use hashiverse_lib::client::client_storage::mem_client_storage::MemClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
use hashiverse_lib::tools::buckets::BucketType;
use hashiverse_lib::tools::parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::time::MILLIS_IN_MINUTE;
use hashiverse_lib::tools::time_provider::time_provider::{ScaledTimeProvider, TimeProvider};
use hashiverse_lib::tools::tools::{configure_logging_with_time_provider, get_temp_dir};
use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
use hashiverse_lib::transport::ddos::noop_ddos::NoopDdosProtection;
use hashiverse_lib::transport::mem_transport::MemTransportFactory;
use hashiverse_server_lib::environment::environment::EnvironmentFactory;
use hashiverse_server_lib::environment::mem_environment_store::MemEnvironmentFactory;
use hashiverse_server_lib::server::args::Args;
use hashiverse_server_lib::server::hashiverse_server::HashiverseServer;
use log::{info, warn};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Number of independent clients that each fetch the same user timeline.
/// Must exceed CACHE_HIT_THRESHOLD (10) so that a CacheRequestToken is issued and the bundle
/// is uploaded to the intermediate server.
const NUM_FETCHING_CLIENTS: usize = 12;
const CLOCK_SCALE_FACTOR: f64 = 15.0 * 60.0;

/// End-to-end test for post-bundle caching:
///
/// 1. Start a cluster and let Kademlia converge.
/// 2. Post content.
/// 3. Have NUM_FETCHING_CLIENTS independent clients each fetch the poster's timeline — this
///    accumulates hit counts on every server in the Kademlia walk.
/// 4. After the threshold is crossed a CacheRequestToken is issued and the bundle is uploaded
///    via `CachePostBundleV1`.
/// 5. Verify that at least one server now returns non-empty `cached_items` from its
///    `post_bundle_cache.on_get`.
#[tokio::test(flavor = "multi_thread")]
async fn test_caching_post_bundles() -> anyhow::Result<()> {
    let (_, temp_dir_path) = get_temp_dir()?;
    let time_provider = Arc::new(ScaledTimeProvider::new(CLOCK_SCALE_FACTOR));
    configure_logging_with_time_provider("warn", time_provider.clone());

    let cancellation_token = CancellationToken::new();
    let environment_factory = Arc::new(MemEnvironmentFactory::new(&temp_dir_path));
    let transport_factory = Arc::new(MemTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::new(vec!["443".to_string()])));

    let mut all_servers: Vec<Arc<_>> = Vec::new();

    // Start servers as background tasks — no Send+'static requirement on the client logic below.
    let pow_generator = Arc::new(NativeParallelPowGenerator::new());

    warn!("--- Starting servers ---");
    let runtime_services = Arc::new(RuntimeServices {
        time_provider: time_provider.clone(),
        transport_factory: transport_factory.clone(),
        pow_generator: pow_generator.clone(),
    });
    let bootstrap_server = HashiverseServer::new(
        runtime_services.clone(),
        environment_factory.clone(),
        Args::default().with_port(443).with_force_local_network(true),
    ).await?;
    all_servers.push(Arc::clone(&bootstrap_server));
    tokio::spawn({
        let server = Arc::clone(&bootstrap_server);
        let ct = cancellation_token.clone();
        async move { server.run(ct).await; }
    });

    for i in 1..20u16 {
        let server = HashiverseServer::new(
            runtime_services.clone(),
            environment_factory.clone(),
            Args::default().with_port(20000 + i).with_force_local_network(true),
        ).await?;
        all_servers.push(Arc::clone(&server));
        tokio::spawn({
            let server = Arc::clone(&server);
            let ct = cancellation_token.clone();
            async move { server.run(ct).await; }
        });
    }

    // Let Kademlia converge across all nodes before any posting or fetching.
    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(30)).await;

    let make_client = |name: String| {
        let runtime_services = runtime_services.clone();
        async move {
            let key_locker_manager = MemKeyLockerManager::new().await?;
            let key_locker = key_locker_manager.create(name).await?;
            let client_storage = MemClientStorage::new().await?;
            let client = Arc::new(HashiverseClient::new(
                runtime_services,
                client_storage,
                key_locker,
                hashiverse_lib::client::args::Args::default(),
            ).await?);
            Ok::<Arc<_>, anyhow::Error>(client)
        }
    };

    warn!("--- Posting ---");
    let poster_client = make_client("poster".to_string()).await?;
    let (post_result, _) = poster_client.submit_post("Caching test post!").await?;
    anyhow_assert!(!post_result.is_empty(), "submit_post returned no commit tokens");
    let bucket_location = post_result[0].bucket_location.clone();
    let poster_client_id = poster_client.client_id().id;

    // Wait for the post to settle on responsible servers.
    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(5)).await;

    // Each independent client triggers a fresh Kademlia walk, incrementing hit counters on the
    // servers along the walk path.  After CACHE_HIT_THRESHOLD (10) hits, a CacheRequestToken
    // is issued and the next client that receives it uploads the bundle asynchronously.
    warn!("--- Fetching from {} independent clients to accumulate cache hits ---", NUM_FETCHING_CLIENTS);
    for i in 0..NUM_FETCHING_CLIENTS {
        let fetcher = make_client(format!("fetcher-{}", i)).await?;
        // single_timeline_get_more is called directly — no spawn, so no Send+'static needed.
        let _ = fetcher.single_timeline_get_more(BucketType::User, &poster_client_id).await;
        info!("fetcher {} done", i);
        // Give the async cache-upload tasks a moment to complete.
        time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(1)).await;
    }

    // Allow any remaining in-flight cache uploads to land.
    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(3)).await;

    warn!("--- Checking server cache state ---");
    let mut cached_server_count = 0usize;
    for server in &all_servers {
        let peer_self = server.peer_self.read().clone();
        let result = server.post_bundle_cache.on_get(
            &bucket_location,
            &[],
            &peer_self,
            &server.server_id,
            time_provider.current_time_millis(),
        );
        if !result.cached_items.is_empty() {
            cached_server_count += 1;
            info!("Server {} holds {} cached bundle(s)", server.server_id.id, result.cached_items.len());
        }
    }
    warn!("--- {} / {} servers have cached post-bundle data ---", cached_server_count, all_servers.len());

    anyhow_assert!(
        cached_server_count > 0,
        "at least one server should have cached the post bundle after {} independent client fetches",
        NUM_FETCHING_CLIENTS
    );

    cancellation_token.cancel();
    Ok(())
}
