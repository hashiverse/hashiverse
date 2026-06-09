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

/// A fresh client whose identity is derived deterministically from `passphrase` (so two clients
/// with the same passphrase share a client_id, modelling two devices of one user).
async fn make_client(runtime_services: &Arc<RuntimeServices>, passphrase: &str) -> anyhow::Result<Arc<HashiverseClient>> {
    let key_locker_manager = MemKeyLockerManager::new().await?;
    let key_locker = key_locker_manager.create(passphrase.to_string()).await?;
    let client_storage = MemClientStorage::new().await?;
    Ok(Arc::new(HashiverseClient::new(runtime_services.clone(), client_storage, key_locker, hashiverse_lib::client::args::Args::default()).await?))
}

/// Exercises the user-config (meta-post) lifecycle end-to-end:
/// - **autopost**: `ensure_meta_post_in_current_bucket` publishes the config when none exists yet,
///   and is idempotent on a second call;
/// - **reload**: another user reading the author's timeline picks up the public config
///   (`process_incoming_meta_post`);
/// - **merge**: a second device of the same identity converges to the published config after a
///   reload (per-field merge logic itself is unit-tested in `meta_post`).
#[tokio::test(flavor = "multi_thread")]
async fn test_user_config_autopost_reload_and_merge() -> anyhow::Result<()> {
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
    let bootstrap = HashiverseServer::new(runtime_services.clone(), environment_factory.clone(), Args::default().with_port(443).with_force_local_network(true)).await?;
    { let ct = cancellation_token.clone(); tokio::spawn(async move { bootstrap.run(ct).await; }); }
    for i in 1..15u16 {
        let server = HashiverseServer::new(runtime_services.clone(), environment_factory.clone(), Args::default().with_port(20000 + i).with_force_local_network(true)).await?;
        let ct = cancellation_token.clone();
        tokio::spawn(async move { server.run(ct).await; });
    }
    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(30)).await;

    // --- Autopost: Alice sets a bio, then auto-publishes it. ---
    warn!("--- Alice sets bio and auto-publishes ---");
    let alice = make_client(&runtime_services, "alice-passphrase").await?;
    let alice_id = alice.client_id().id;
    alice.meta_post_manager().set_bio("alice".to_string(), "hello world".to_string(), String::new(), String::new()).await?;
    alice.ensure_meta_post_in_current_bucket().await?;
    // Idempotent: nothing more to publish this bucket, so a second call is a no-op (must not error).
    alice.ensure_meta_post_in_current_bucket().await?;

    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(5)).await;

    // --- Reload: Bob reads Alice's timeline and picks up her public config. ---
    warn!("--- Bob reads Alice's timeline ---");
    let bob = make_client(&runtime_services, "bob-passphrase").await?;
    let _ = bob.single_timeline_get_more(BucketType::User, &alice_id).await?;
    let bob_view = bob.meta_post_manager().get_meta_post_public(alice_id).await?;
    let bob_view = bob_view.ok_or_else(|| anyhow::anyhow!("Bob did not receive Alice's meta-post"))?;
    anyhow_assert!(bob_view.nickname.value.as_deref() == Some("alice"), "Bob should see Alice's published nickname, got {:?}", bob_view.nickname.value);
    anyhow_assert!(bob_view.status.value.as_deref() == Some("hello world"), "Bob should see Alice's published status, got {:?}", bob_view.status.value);

    // --- Merge / multi-device convergence: a second device of Alice's identity. ---
    warn!("--- Alice's second device reads and converges ---");
    let alice_device_2 = make_client(&runtime_services, "alice-passphrase").await?;
    anyhow_assert!(alice_device_2.client_id().id == alice_id, "same passphrase must yield the same client_id");
    let _ = alice_device_2.single_timeline_get_more(BucketType::User, &alice_id).await?;
    let device_2_public = alice_device_2.meta_post_manager().get_public().await?;
    anyhow_assert!(
        device_2_public.nickname.value.as_deref() == Some("alice"),
        "Alice's second device should converge to the published nickname after reload+merge, got {:?}",
        device_2_public.nickname.value
    );

    cancellation_token.cancel();
    Ok(())
}
