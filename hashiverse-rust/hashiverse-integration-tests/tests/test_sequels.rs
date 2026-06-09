use hashiverse_lib::anyhow_assert;
use hashiverse_lib::client::client_storage::mem_client_storage::MemClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
use hashiverse_lib::protocol::posting::encoded_post::EncodedPostV1;
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

/// A "sequel" continues an earlier post: it is posted into a `BucketType::Sequel` bucket keyed by
/// the parent post's id, so the parent's sequel timeline lists it.
#[tokio::test(flavor = "multi_thread")]
async fn test_sequels() -> anyhow::Result<()> {
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

    let key_locker_manager = MemKeyLockerManager::new().await?;
    let key_locker = key_locker_manager.create("sequel-author".to_string()).await?;
    let client_storage = MemClientStorage::new().await?;
    let client = Arc::new(HashiverseClient::new(runtime_services.clone(), client_storage, key_locker, hashiverse_lib::client::args::Args::default()).await?);

    warn!("--- Posting the parent ---");
    let (parent_tokens, (_parent_post, parent_bytes)) = client.submit_post("The original post").await?;
    anyhow_assert!(!parent_tokens.is_empty(), "parent post returned no commit tokens");
    let parent_post_id = parent_tokens[0].post_id;
    // The header bytes the sequel references for the same-author check (as the web client does).
    let parent_header_hex = hex::encode(EncodedPostV1::bytes_without_body(parent_bytes.clone())?);

    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(2)).await;

    warn!("--- Posting a sequel referencing the parent ---");
    let sequel_html = format!(
        "<sequel post_id=\"{}\" post_header_hex=\"{}\">A sequel to the original</sequel>",
        parent_post_id.to_hex_str(),
        parent_header_hex,
    );
    let (sequel_tokens, _) = client.submit_post(&sequel_html).await?;
    anyhow_assert!(!sequel_tokens.is_empty(), "sequel post returned no commit tokens");
    anyhow_assert!(
        sequel_tokens.iter().any(|t| matches!(t.bucket_location.bucket_type, BucketType::Sequel) && t.bucket_location.base_id == parent_post_id),
        "sequel was not posted into the parent's Sequel bucket"
    );

    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(3)).await;

    warn!("--- Fetching the parent's sequel timeline ---");
    let (posts, _) = client.single_timeline_get_more(BucketType::Sequel, &parent_post_id).await?;
    anyhow_assert!(
        posts.iter().any(|(_, encoded_post, _, _)| encoded_post.post.contains("A sequel to the original")),
        "the sequel did not appear in the parent's Sequel timeline ({} posts found)",
        posts.len()
    );
    warn!("--- Sequel appeared in the parent's Sequel timeline ---");

    cancellation_token.cancel();
    Ok(())
}
