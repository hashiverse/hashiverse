use hashiverse_lib::client::client_storage::mem_client_storage::MemClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
use hashiverse_lib::tools::config;
use hashiverse_lib::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::time::MILLIS_IN_MINUTE;
use hashiverse_lib::tools::time_provider::time_provider::{ScaledTimeProvider, TimeProvider};
use hashiverse_lib::tools::tools::get_temp_dir;
use hashiverse_lib::tools::tools::{leading_agreement_bits_xor, LeadingAgreementBits};
use hashiverse_lib::tools::types::Id;
use hashiverse_lib::transport::mem_transport::MemTransportFactory;
use hashiverse_server_lib::environment::environment::EnvironmentFactory;
use hashiverse_server_lib::environment::mem_environment_store::MemEnvironmentFactory;
use hashiverse_server_lib::server::args::Args;
use hashiverse_server_lib::server::hashiverse_server::HashiverseServer;
use log::{error, info, warn};
use std::sync::Arc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const CLOCK_SCALE_FACTOR: f64 = 5.0 * 60.0;

#[tokio::test(flavor = "multi_thread")]
async fn test_client_post() -> anyhow::Result<()> {
    let (_, temp_dir_path) = get_temp_dir()?;
    let time_provider = Arc::new(ScaledTimeProvider::new(CLOCK_SCALE_FACTOR));
    hashiverse_lib::tools::tools::configure_logging_with_time_provider("info", time_provider.clone());
    let cancellation_token = CancellationToken::new();
    let environment_factory = Arc::new(MemEnvironmentFactory::new(&temp_dir_path));
    let transport_factory = MemTransportFactory::default();

    let mut join_set = JoinSet::new();

    const NUM_SERVERS: usize = 50;
    const NUM_CLIENTS: usize = 3;

    let pow_generator = Arc::new(NativeParallelPowGenerator::new());

    // Some servers
    let hashiverse_servers = {
        let mut hashiverse_servers = Vec::new();
        for i in 0..NUM_SERVERS {
            let runtime_services = Arc::new(RuntimeServices {
                time_provider: time_provider.clone(),
                transport_factory: transport_factory.clone(),
                pow_generator: pow_generator.clone(),
            });
            let environment_factory = environment_factory.clone();
            let args = Args::default().with_port(20000 + i as u16).with_force_local_network(true).with_skip_pq_commitment_bytes(true);
            let hashiverse_server = HashiverseServer::new(runtime_services, environment_factory, args).await?;
            hashiverse_servers.push(hashiverse_server);
        }
        Arc::new(hashiverse_servers)
    };

    // The clients
    let hashiverse_clients = {
        let mut hashiverse_clients = Vec::new();
        for i in 0..NUM_CLIENTS {
            let args = hashiverse_lib::client::args::Args::default();
            let passphrase = format!("client {}", i);

            let key_locker_manager = MemKeyLockerManager::new().await?;
            let key_locker = key_locker_manager.create(passphrase).await?;
            let client_storage = MemClientStorage::new().await?;
            let runtime_services = Arc::new(RuntimeServices {
                time_provider: time_provider.clone(),
                transport_factory: transport_factory.clone(),
                pow_generator: pow_generator.clone(),
            });
            let hashiverse_client = Arc::new(HashiverseClient::new(runtime_services, client_storage, key_locker, args).await?);
            hashiverse_clients.push(hashiverse_client);
        }
        Arc::new(hashiverse_clients)
    };

    // Start the servers
    {
        for hashiverse_server in hashiverse_servers.iter() {
            let hashiverse_server = hashiverse_server.clone();
            let cancellation_token = cancellation_token.clone();

            join_set.spawn(async move {
                hashiverse_server.run(cancellation_token.clone()).await;
            });
        }
    }

    // Fiddle with the environment
    {
        let time_provider = time_provider.clone();

        join_set.spawn(async move {
            scopeguard::defer! {
               warn!("--- Killing all the servers ------------------------------------------------");
               cancellation_token.cancel();
            }

            warn!("--- Test fiddler started ------------------------------------------------");

            time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(30)).await;

            for i in 0..NUM_CLIENTS {
                warn!("--- Posting from client {} ------------------------------------------------", i);
                {
                    let hashiverse_client = hashiverse_clients[i].clone();

                    let (post_result, _) = hashiverse_client.submit_post("Hi there!").await.expect("Posting failed");
                    assert_eq!(post_result.len(), config::REDUNDANT_SERVERS_PER_POST, "Didn't post to sufficient servers");

                    let location_ids: Vec<Id> = post_result.iter().map(|post_commit_token| post_commit_token.bucket_location.location_id).collect();

                    // Check that all the location ids are the same
                    {
                        let location_id_first = location_ids[0];
                        location_ids.iter().for_each(|location_id| assert_eq!(location_id, &location_id_first, "All location ids are not the same"));
                    }

                    // Check that these are indeed the nearest servers
                    {
                        let location_id = location_ids[0];
                        error!("The location_ids: {:?}", location_ids);

                        let peer_ids: Vec<Id> = post_result.iter().map(|post_commit_token| post_commit_token.peer.id).collect();
                        error!("The peer_ids: {:?}", post_result);

                        let mut server_distances: Vec<(Id, LeadingAgreementBits)> = hashiverse_servers
                            .iter()
                            .map(|hashiverse_server| (hashiverse_server.server_id.id, leading_agreement_bits_xor(&hashiverse_server.server_id.id.0, &location_id.0)))
                            .collect();
                        server_distances.sort_by_key(|(_, distance)| -(*distance));
                        error!("The server distances are {:?}", server_distances);

                        let furthest_distance = server_distances[config::REDUNDANT_SERVERS_PER_POST - 1].1;
                        error!("The furthest_distance: {:?}", furthest_distance);
                        let server_distances: Vec<(Id, LeadingAgreementBits)> = server_distances.into_iter().filter(|(_, distance)| *distance >= furthest_distance).collect();
                        error!("The truncated server distances are {:?}", server_distances);

                        peer_ids.iter().for_each(|peer_id| {
                            assert!(server_distances.iter().find(|server_distance| server_distance.0 == *peer_id).is_some());
                        });
                    }
                }
            }

            time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(15)).await;
        });
    }

    // Let them exit gracefully
    let mut had_a_failure = false;
    while let Some(res) = join_set.join_next().await {
        match res {
            Ok(()) => info!("headline service completed"),
            Err(e) => {
                had_a_failure = true;
                warn!("headline service failed: {}", e)
            }
        }
    }
    assert!(!had_a_failure, "One of the headline services failed");

    Ok(())
}
