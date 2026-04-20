#![feature(try_blocks)]

use hashiverse_lib::anyhow_assert;
use hashiverse_lib::client::client_storage::mem_client_storage::MemClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
use hashiverse_lib::tools::buckets::BucketLocation;
use hashiverse_lib::tools::config;
use hashiverse_lib::tools::parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::time::MILLIS_IN_MINUTE;
use hashiverse_lib::tools::time_provider::time_provider::{ScaledTimeProvider, TimeProvider};
use hashiverse_lib::tools::tools::{configure_logging_with_time_provider, get_temp_dir};
use hashiverse_lib::tools::tools::{leading_agreement_bits_xor, LeadingAgreementBits};
use hashiverse_lib::tools::types::Id;
use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
use hashiverse_lib::transport::ddos::noop_ddos::NoopDdosProtection;
use hashiverse_lib::transport::mem_transport::MemTransportFactory;
use hashiverse_server_lib::environment::environment::EnvironmentFactory;
use hashiverse_server_lib::environment::mem_environment_store::MemEnvironmentFactory;
use hashiverse_server_lib::server::args::Args;
use hashiverse_server_lib::server::hashiverse_server::HashiverseServer;
use log::{error, info, warn};
use parking_lot::Mutex;
use std::sync::Arc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const CLOCK_SCALE_FACTOR: f64 = 15.0 * 60.0;

#[tokio::test(flavor = "multi_thread")]
async fn test_healing_post_bundles() -> anyhow::Result<()> {
    let (_, temp_dir_path) = get_temp_dir()?;
    let time_provider = Arc::new(ScaledTimeProvider::new(CLOCK_SCALE_FACTOR));
    configure_logging_with_time_provider("info", time_provider.clone());

    let pow_generator = Arc::new(NativeParallelPowGenerator::new());
    let cancellation_token = CancellationToken::new();
    let environment_factory = Arc::new(MemEnvironmentFactory::new(&temp_dir_path));
    let transport_factory = Arc::new(MemTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::new(vec!["443".to_string()])));

    // Server tasks live here so we can wait for clean exit.
    // Wrapped in Arc<Mutex<_>> so add_another_server (running inside the fiddler task) can spawn into it.
    // After the fiddler task finishes and drops its Arc clone, Arc::try_unwrap gives us exclusive
    // ownership back so we can drain with join_next().await without any lock across .await.
    let server_join_set: Arc<Mutex<JoinSet<()>>> = Arc::new(Mutex::new(JoinSet::new()));

    // Shared list of servers — mutated by add_another_server, read by check_post_is_at_nearest_servers
    let hashiverse_servers: Arc<Mutex<Vec<Arc<_>>>> = Arc::new(Mutex::new(Vec::new()));

    // We're going to fire up another server every few minutes.  Each time, the client will make a post.
    // We will check between server creations that the posts have correctly healed their way to "closest" servers.

    let add_another_server = {
        let time_provider = time_provider.clone();
        let environment_factory = environment_factory.clone();
        let transport_factory = transport_factory.clone();
        let pow_generator = pow_generator.clone();
        let cancellation_token = cancellation_token.clone();
        let hashiverse_servers = Arc::clone(&hashiverse_servers);
        let server_join_set = Arc::clone(&server_join_set);
        move |port: u16| {
            let runtime_services = Arc::new(RuntimeServices {
                time_provider: time_provider.clone(),
                transport_factory: transport_factory.clone(),
                pow_generator: pow_generator.clone(),
            });
            let environment_factory = environment_factory.clone();
            let cancellation_token = cancellation_token.clone();
            let hashiverse_servers = Arc::clone(&hashiverse_servers);
            let server_join_set = Arc::clone(&server_join_set);
            async move {
                let args = Args::default().with_port(port).with_force_local_network(true);
                let hashiverse_server = HashiverseServer::new(runtime_services, environment_factory, args).await?;
                hashiverse_servers.lock().push(Arc::clone(&hashiverse_server));
                server_join_set.lock().spawn(async move {
                    hashiverse_server.run(cancellation_token).await;
                });
                Ok::<(), anyhow::Error>(())
            }
        }
    };

    // The client
    let hashiverse_client = {
        let args = hashiverse_lib::client::args::Args::default();
        let passphrase = format!("client {}", 0);
        let key_locker_manager = MemKeyLockerManager::new().await?;
        let key_locker = key_locker_manager.create(passphrase).await?;
        let client_storage = MemClientStorage::new().await?;
        let runtime_services = Arc::new(RuntimeServices {
            time_provider: time_provider.clone(),
            transport_factory: transport_factory.clone(),
            pow_generator: pow_generator.clone(),
        });
        Arc::new(HashiverseClient::new(runtime_services, client_storage, key_locker, args).await?)
    };

    let check_post_is_at_nearest_servers = {
        let hashiverse_servers = Arc::clone(&hashiverse_servers);
        let time_provider = time_provider.clone();
        move |post_bucket_locations: &Vec<BucketLocation>, post_i: usize| -> anyhow::Result<()> {
            let bucket_location = &post_bucket_locations[post_i];
            let hashiverse_servers = hashiverse_servers.lock();

            let mut server_distances: Vec<(Arc<_>, LeadingAgreementBits)> = hashiverse_servers.iter().map(|s: &Arc<_>| (Arc::clone(s), leading_agreement_bits_xor(&s.server_id.id.0, &bucket_location.location_id.0))).collect();
            server_distances.sort_by_key(|(_, distance)| -(*distance));

            for i in 0..config::REDUNDANT_SERVERS_PER_POST.min(server_distances.len()) {
                let post_bundle_metadata = server_distances[i].0.environment.get_post_bundle_metadata(time_provider.current_time_millis(), &bucket_location.location_id)?;
                anyhow_assert!(
                    post_bundle_metadata.is_some(),
                    "Post bundle metadata is None for location_id={}, server_id={}",
                    bucket_location.location_id,
                    server_distances[i].0.server_id
                );
            }

            Ok(())
        }
    };

    // Fiddler task — drives the test scenario.  Lives in its own local JoinSet so we can wait for
    // it without holding any lock across .await.  When it finishes it drops its Arc clones of
    // server_join_set (via add_another_server) and cancels the token, letting all servers stop.
    let mut fiddler_join_set: JoinSet<anyhow::Result<()>> = JoinSet::new();
    {
        let time_provider = time_provider.clone();

        fiddler_join_set.spawn(async move {
            scopeguard::defer! {
               warn!("--- Killing all the servers ------------------------------------------------");
               cancellation_token.cancel();
            }

            let try_result: anyhow::Result<()> = try {
                warn!("--- Test fiddler started ------------------------------------------------");

                let mut post_bucket_locations = Vec::new();

                warn!("--- Starting first server -----------------------------------------------");

                // The first (and bootstrap) server
                add_another_server(443).await?;
                time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(5)).await;

                // First post
                warn!("--- Posting from client ------------------------------------------------");
                let (post_result, _) = hashiverse_client.submit_post("Hi there!").await.expect("Posting failed");
                assert_eq!(post_result.len(), config::REDUNDANT_SERVERS_PER_POST.min(hashiverse_servers.lock().len()), "Didn't post to sufficient servers");
                post_bucket_locations.push(post_result[0].bucket_location.clone());

                warn!("--- Checking post is at servers  ------------------------------------------------");
                check_post_is_at_nearest_servers(&post_bucket_locations, 0)?;

                warn!("--- Ramping up additional servers  ------------------------------------------------");

                for i in 2..=10 {
                    warn!("--- Starting additional server {} -----------------------------------------------", i);
                    add_another_server(20000 + i).await?;

                    // We have to wait a ridiculous amount o time to make sure that the server is known to all of the network to make this test pass
                    // In reality we dont care about a few failures in healing.  eventually some client somewhere will heal a server that should be healed.
                    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(30)).await;

                    warn!("--- Causing potential healing {} -----------------------------------------------", i);
                    let _ = hashiverse_client.get_post(post_bucket_locations[0].clone(), &Id::zero()).await; // We dont expect this to work for post_id::Zero
                    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(5)).await;

                    warn!("--- Checking post is at servers  ------------------------------------------------");
                    check_post_is_at_nearest_servers(&post_bucket_locations, 0)?;
                    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(1)).await;

                    time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(5)).await;
                }

                warn!("--- Waiting before exiting ------------------------------------------------");

                time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(60)).await;
            };

            try_result
        });
    }

    {
        // Wait for the fiddler to finish (it cancels the token on exit, stopping all servers)
        let mut had_a_failure = false;
        while let Some(res) = fiddler_join_set.join_next().await {
            match res {
                Ok(Ok(())) => info!("fiddler completed"),
                Ok(Err(e)) => {
                    had_a_failure = true;
                    error!("fiddler failed: {}", e)
                }
                Err(e) => {
                    had_a_failure = true;
                    error!("fiddler panicked: {}", e)
                }
            }
        }
        assert!(!had_a_failure, "The fiddler failed");
    }

    {
        // Fiddler has dropped its Arc clone of server_join_set; unwrap to drain servers for clean exit
        let mut had_a_failure = false;
        let mut server_join_set = Arc::try_unwrap(server_join_set).expect("fiddler should have dropped its server_join_set Arc clone").into_inner();
        while let Some(res) = server_join_set.join_next().await {
            match res {
                Ok(()) => info!("server completed"),
                Err(e) => {
                    had_a_failure = true;
                    error!("server failed: {}", e)
                }
            }
        }
        assert!(!had_a_failure, "One of the headline services failed");
    }

    Ok(())
}
