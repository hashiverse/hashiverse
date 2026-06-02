#![feature(try_blocks)]

use hashiverse_lib::anyhow_assert;
use hashiverse_lib::client::client_storage::mem_client_storage::MemClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
use hashiverse_lib::tools::buckets::BucketType;
use hashiverse_lib::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::time::MILLIS_IN_MINUTE;
use hashiverse_lib::tools::time_provider::time_provider::{ScaledTimeProvider, TimeProvider};
use hashiverse_lib::tools::tools::{configure_logging_with_time_provider, get_temp_dir};
use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
use hashiverse_lib::tools::time_provider::manual_time_provider::ManualTimeProvider;
use hashiverse_lib::transport::ddos::ddos::{DdosConnectionGuard, DdosProtection};
use hashiverse_lib::transport::ddos::mem_ddos::MemDdosProtection;
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

/// Verify that servers built with `MemDdosProtection` handle normal operations without false
/// positives.  The protection must not interfere with legitimate posting and fetching traffic.
///
/// Servers are started as background tasks; client logic runs directly in the test function
/// to avoid the `Send + 'static` constraint that would otherwise prevent borrowing `&Id`.
#[tokio::test(flavor = "multi_thread")]
async fn test_ddos_protection_does_not_block_legitimate_traffic() -> anyhow::Result<()> {
    let (_, temp_dir_path) = get_temp_dir()?;
    let time_provider = Arc::new(ScaledTimeProvider::new(CLOCK_SCALE_FACTOR));
    configure_logging_with_time_provider("warn", time_provider.clone());

    let cancellation_token = CancellationToken::new();
    let environment_factory = Arc::new(MemEnvironmentFactory::new(&temp_dir_path));

    // Generous threshold so that normal traffic never triggers the ban.
    let ddos_protection: Arc<MemDdosProtection> = Arc::new(MemDdosProtection::new(
        10_000.0, // score_threshold
        0.1,      // decay_per_second
        5.0,      // bad_request_penalty
        50,       // max_connections_per_ip
        time_provider.clone(),
    ));
    let transport_factory = Arc::new(MemTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::new(vec!["443".to_string()])));


    let pow_generator = Arc::new(NativeParallelPowGenerator::new());
    let runtime_services = Arc::new(RuntimeServices {
        time_provider: time_provider.clone(),
        transport_factory: transport_factory.clone(),
        pow_generator: pow_generator.clone(),
    });

    warn!("--- Starting servers with MemDdosProtection ---");
    for port in [443u16, 20001, 20002, 20003, 20004, 20005] {
        let server = HashiverseServer::new(
            runtime_services.clone(),
            environment_factory.clone(),
            Args::default().with_port(port).with_force_local_network(true),
        ).await?;
        tokio::spawn({
            let ct = cancellation_token.clone();
            async move { server.run(ct).await; }
        });
    }

    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(15)).await;

    warn!("--- Posting with MemDdosProtection active ---");
    let key_locker_manager = MemKeyLockerManager::new().await?;
    let key_locker = key_locker_manager.create("ddos-test-client".to_string()).await?;
    let client_storage = MemClientStorage::new().await?;
    let client = Arc::new(HashiverseClient::new(
        runtime_services.clone(),
        client_storage,
        key_locker,
        hashiverse_lib::client::args::Args::default(),
    ).await?);

    let (post_result, _) = client.submit_post("DDoS protection test post").await?;
    anyhow_assert!(!post_result.is_empty(), "submit_post must succeed even with MemDdosProtection active");

    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(5)).await;

    let poster_id = client.client_id().id;
    let (posts, _oldest_processed_time_millis) = client.single_timeline_get_more(BucketType::User, &poster_id).await?;
    anyhow_assert!(!posts.is_empty(), "timeline fetch must return the posted content");
    warn!("--- Legitimate traffic succeeded: {} posts fetched with MemDdosProtection active ---", posts.len());

    cancellation_token.cancel();
    Ok(())
}

/// Unit-level integration test: a burst of bad-request calls escalates the DDoS score and
/// bans the attacker IP while leaving other IPs completely unaffected.
///
/// This exercises the complete `MemDdosProtection` state machine — penalty accumulation,
/// threshold detection, connection blocking — without requiring a running server.
#[test]
fn test_ddos_bad_request_escalation_and_ban() {
    let score_threshold = 15.0f64;
    let bad_request_penalty = 5.0f64;
    let max_connections = 4usize;

    let ddos = Arc::new(MemDdosProtection::new(
        score_threshold,
        0.001, // very low decay so tests complete before scores decay
        bad_request_penalty,
        max_connections,
        // Fixed manual clock: time never advances, so scores never decay — fully deterministic.
        Arc::new(ManualTimeProvider::default()),
    ));

    // TEST-NET addresses (RFC 5737) — safe to use in tests.
    let attacker_ip = "203.0.113.42";
    let legitimate_ip = "198.51.100.1";

    // Three bad-request calls: 3 × 5 = 15 = threshold → IP is banned.
    ddos.report_bad_request(attacker_ip); // score = 5
    ddos.report_bad_request(attacker_ip); // score = 10
    ddos.report_bad_request(attacker_ip); // score = 15

    assert!(!ddos.allow_request(attacker_ip), "attacker must be blocked after penalty accumulation");
    assert!(
        DdosConnectionGuard::try_new(ddos.clone(), attacker_ip).is_none(),
        "attacker must not acquire a new connection slot"
    );

    // The legitimate IP is completely unaffected.
    assert!(ddos.allow_request(legitimate_ip), "legitimate IP must not be affected by attacker's ban");
    assert!(
        DdosConnectionGuard::try_new(ddos.clone(), legitimate_ip).is_some(),
        "legitimate IP must be able to connect"
    );
}
