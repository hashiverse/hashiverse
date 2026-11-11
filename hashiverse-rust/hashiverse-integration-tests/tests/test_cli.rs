//! End-to-end test of the `hashiverse-cli` command layer against an in-memory network.
//!
//! The CLI commands take a plain `&HashiverseClient`, so here they run against mem-transport
//! clients: a guest (empty keyphrase) that can read but not post, and a keyed author.

use hashiverse_cli::cli_args::Command;
use hashiverse_cli::commands;
use hashiverse_cli::render::render_post_html_to_text;
use hashiverse_cli::unlock::is_guest;
use hashiverse_lib::anyhow_assert;
use hashiverse_lib::client::client_storage::mem_client_storage::MemClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
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

async fn new_client(runtime_services: Arc<RuntimeServices>, key_phrase: &str) -> anyhow::Result<Arc<HashiverseClient>> {
    let key_locker_manager = MemKeyLockerManager::new().await?;
    let key_locker = key_locker_manager.create(key_phrase.to_string()).await?;
    let client_storage = MemClientStorage::new().await?;
    Ok(Arc::new(HashiverseClient::new(runtime_services, client_storage, key_locker, hashiverse_lib::client::args::Args::default()).await?))
}

#[tokio::test(flavor = "multi_thread")]
async fn test_cli_commands() -> anyhow::Result<()> {
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
    let bootstrap_server = HashiverseServer::new(runtime_services.clone(), environment_factory.clone(), Args::default().with_port(443).with_force_local_network(true)).await?;
    {
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move { bootstrap_server.run(cancellation_token).await; });
    }
    for i in 1..15u16 {
        let hashiverse_server = HashiverseServer::new(runtime_services.clone(), environment_factory.clone(), Args::default().with_port(20000 + i).with_force_local_network(true)).await?;
        let cancellation_token = cancellation_token.clone();
        tokio::spawn(async move { hashiverse_server.run(cancellation_token).await; });
    }
    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(30)).await;

    let guest_client = new_client(runtime_services.clone(), "").await?;
    let author_client = new_client(runtime_services.clone(), "cli-author").await?;
    anyhow_assert!(is_guest(&guest_client), "empty keyphrase should be the guest identity");
    anyhow_assert!(!is_guest(&author_client), "a real keyphrase must not be the guest identity");
    let author_client_id_hex = author_client.client_id().id_hex();

    warn!("--- Guest cannot post ---");
    let guest_post_result = commands::post(&guest_client, "guests should not be able to post").await;
    anyhow_assert!(
        guest_post_result.as_ref().is_err_and(|error| error.to_string().contains("posting requires an identity")),
        "guest post should be refused, got {:?}",
        guest_post_result
    );

    warn!("--- Author posts ---");
    let post_text = "hello from the cli #CliTest\nsecond line & <escaped>";
    let cli_post_submitted = commands::post(&author_client, post_text).await?;
    anyhow_assert!(!cli_post_submitted.bucket_locations.is_empty(), "post returned no bucket locations");

    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(3)).await;

    warn!("--- Guest reads the hashtag timeline ---");
    let hashtag_cli_posts = commands::hashtag_timeline(&guest_client, "#clitest", 1).await?;
    let hashtag_cli_post = hashtag_cli_posts.iter().find(|cli_post| cli_post.post_id == cli_post_submitted.post_id);
    anyhow_assert!(hashtag_cli_post.is_some(), "post missing from the hashtag timeline ({} posts found)", hashtag_cli_posts.len());
    let hashtag_cli_post = hashtag_cli_post.unwrap();
    anyhow_assert!(hashtag_cli_post.client_id == author_client_id_hex, "hashtag post has the wrong author: {}", hashtag_cli_post.client_id);
    let rendered_text = render_post_html_to_text(&hashtag_cli_post.post_html);
    anyhow_assert!(rendered_text == post_text, "rendered text {:?} != posted text {:?}", rendered_text, post_text);

    warn!("--- Guest reads the author's timeline ---");
    let user_cli_posts = commands::user_timeline(&guest_client, &author_client_id_hex, 1).await?;
    anyhow_assert!(user_cli_posts.iter().any(|cli_post| cli_post.post_id == cli_post_submitted.post_id), "post missing from the user timeline ({} posts found)", user_cli_posts.len());

    warn!("--- Guest fetches trending hashtags ---");
    // Trending counts are per-server and the client asks a random peer, so the new hashtag is not
    // guaranteed to appear; servers top the list up with fallback hashtags, so it is never empty.
    let cli_trending_hashtags = commands::trending(&guest_client, 5).await?;
    anyhow_assert!(!cli_trending_hashtags.is_empty(), "trending returned nothing");

    warn!("--- run_command produces text and JSON ---");
    let trending_text = commands::run_command(&guest_client, &Command::Trending { limit: 5 }, false).await?;
    anyhow_assert!(trending_text.contains('#'), "trending text has no hashtags: {}", trending_text);
    let trending_json = commands::run_command(&guest_client, &Command::Trending { limit: 5 }, true).await?;
    let trending_json_value: serde_json::Value = serde_json::from_str(&trending_json)?;
    anyhow_assert!(trending_json_value.as_array().is_some_and(|entries| !entries.is_empty()), "trending JSON is not a non-empty array: {}", trending_json);

    cancellation_token.cancel();
    Ok(())
}
