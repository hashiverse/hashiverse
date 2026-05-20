use hashiverse_lib::protocol::peer::Peer;
use hashiverse_lib::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::time::{MILLIS_IN_MINUTE, MILLIS_IN_SECOND};
use hashiverse_lib::tools::time_provider::time_provider::{ScaledTimeProvider, TimeProvider};
use hashiverse_lib::tools::tools::get_temp_dir;
use hashiverse_lib::tools::{compression, json};
use hashiverse_lib::transport::mem_transport::MemTransportFactory;
use hashiverse_server_lib::environment::environment::{EnvironmentFactory, CONFIG_KADEMLIA_PEER_BUCKETS};
use hashiverse_server_lib::environment::mem_environment_store::MemEnvironmentFactory;
use hashiverse_server_lib::server::args::Args;
use hashiverse_server_lib::server::hashiverse_server::HashiverseServer;
use log::{info, warn};
use std::sync::Arc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const CLOCK_SCALE_FACTOR: f64 = 15.0 * 60.0;

#[tokio::test]
async fn test_simple_server() -> anyhow::Result<()> {
    let (_, temp_dir_path) = get_temp_dir()?;
    let time_provider = Arc::new(ScaledTimeProvider::new(CLOCK_SCALE_FACTOR));
    let cancellation_token = CancellationToken::new();
    let environment_factory = Arc::new(MemEnvironmentFactory::new(&temp_dir_path));
    let transport_factory = MemTransportFactory::default();

    let mut join_set = JoinSet::new();

    let pow_generator = Arc::new(NativeParallelPowGenerator::new());

    // The server
    {
        let cancellation_token = cancellation_token.clone();
        let runtime_services = Arc::new(RuntimeServices {
            time_provider: time_provider.clone(),
            transport_factory: transport_factory.clone(),
            pow_generator: pow_generator.clone(),
        });
        let environment_factory = environment_factory.clone();
        let args = Args::default_for_testing();
        let hashiverse_server = HashiverseServer::new(runtime_services, environment_factory, args).await;
        join_set.spawn(async move {
            match hashiverse_server {
                Ok(hashiverse_server) => {
                    hashiverse_server.run(cancellation_token.clone()).await;
                }
                Err(e) => {
                    panic!("Failed to start hashiverse_server: {}", e);
                }
            }
        });
    }

    // Kill the server after a bit
    {
        join_set.spawn(async move {
            time_provider.sleep_millis(MILLIS_IN_SECOND.const_mul(5)).await;
            warn!("--- Sending cancellation signal ------------------------------------------------");
            cancellation_token.cancel();
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

#[tokio::test(flavor = "multi_thread")]
async fn test_server_meets_server() -> anyhow::Result<()> {
    let (_, temp_dir_path) = get_temp_dir()?;
    let time_provider = Arc::new(ScaledTimeProvider::new(CLOCK_SCALE_FACTOR));
    hashiverse_lib::tools::tools::configure_logging_with_time_provider("info", time_provider.clone());
    let cancellation_token = CancellationToken::new();
    let environment_factory = Arc::new(MemEnvironmentFactory::new(&temp_dir_path));
    let transport_factory = MemTransportFactory::default();

    let mut join_set = JoinSet::new();

    const NUM_EXTRA_SERVERS: usize = 10;

    let pow_generator = Arc::new(NativeParallelPowGenerator::new());

    // The bootstrap server
    let hashiverse_server_main = {
        let runtime_services = Arc::new(RuntimeServices {
            time_provider: time_provider.clone(),
            transport_factory: transport_factory.clone(),
            pow_generator: pow_generator.clone(),
        });
        let args = Args::default_for_testing().with_port(443);
        HashiverseServer::new(runtime_services, environment_factory.clone(), args).await?
    };

    // The server we will interrogate
    let hashiverse_server_to_interrogate = {
        let runtime_services = Arc::new(RuntimeServices {
            time_provider: time_provider.clone(),
            transport_factory: transport_factory.clone(),
            pow_generator: pow_generator.clone(),
        });
        let args = Args::default_for_testing().with_port(10000);
        HashiverseServer::new(runtime_services, environment_factory.clone(), args).await?
    };

    // Some other servers
    let hashiverse_servers = {
        let mut hashiverse_servers = Vec::new();
        for i in 0..NUM_EXTRA_SERVERS {
            let runtime_services = Arc::new(RuntimeServices {
                time_provider: time_provider.clone(),
                transport_factory: transport_factory.clone(),
                pow_generator: pow_generator.clone(),
            });
            let args = Args::default_for_testing().with_port(20000 + i as u16);
            let hashiverse_server = HashiverseServer::new(runtime_services, environment_factory.clone(), args).await?;
            hashiverse_servers.push(hashiverse_server);
        }
        hashiverse_servers
    };

    // Start the servers
    {
        {
            let cancellation_token = cancellation_token.clone();
            join_set.spawn(async move {
                hashiverse_server_main.run(cancellation_token.clone()).await;
            });
        }

        {
            let cancellation_token = cancellation_token.clone();
            let hashiverse_server_to_interrogate = hashiverse_server_to_interrogate.clone();
            join_set.spawn(async move {
                hashiverse_server_to_interrogate.run(cancellation_token.clone()).await;
            });
        }

        for hashiverse_server in hashiverse_servers {
            let cancellation_token = cancellation_token.clone();
            join_set.spawn(async move {
                hashiverse_server.run(cancellation_token.clone()).await;
            });
        }
    }

    // Fiddle with the environment
    {
        let time_provider = time_provider.clone();
        let hashiverse_server_to_interrogate = hashiverse_server_to_interrogate.clone();

        join_set.spawn(async move {
            scopeguard::defer! {
               warn!("--- Killing all the servers ------------------------------------------------");
               cancellation_token.cancel();
            }

            warn!("--- Test fiddler started ------------------------------------------------");

            time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(30)).await;

            warn!("--- Checking that servers have discovered each other ------------------------------------------------");
            {
                let environment = hashiverse_server_to_interrogate.environment.clone();
                let peer_buckets_bytes = environment.config_get_bytes(CONFIG_KADEMLIA_PEER_BUCKETS).unwrap().unwrap();
                let peer_buckets = json::bytes_to_struct::<Vec<Vec<Peer>>>(&compression::decompress(&peer_buckets_bytes).unwrap().to_bytes()).unwrap();
                let total_peers = peer_buckets.iter().flatten().count();
                assert_eq!(total_peers, 2 + NUM_EXTRA_SERVERS, "The servers have not discovered each other!");
            }
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

#[tokio::test(flavor = "multi_thread")]
async fn test_server_meets_server_thousands() -> anyhow::Result<()> {
    let (_, temp_dir_path) = get_temp_dir()?;
    let time_provider = Arc::new(ScaledTimeProvider::new(CLOCK_SCALE_FACTOR));
    hashiverse_lib::tools::tools::configure_logging_with_time_provider("info", time_provider.clone());
    let cancellation_token = CancellationToken::new();
    let environment_factory = Arc::new(MemEnvironmentFactory::new(&temp_dir_path));
    let transport_factory = MemTransportFactory::default();

    let mut join_set = JoinSet::new();

    const NUM_EXTRA_SERVERS: usize = 50;

    let pow_generator = Arc::new(NativeParallelPowGenerator::new());

    // The bootstrap server
    let hashiverse_server_main = {
        let runtime_services = Arc::new(RuntimeServices {
            time_provider: time_provider.clone(),
            transport_factory: transport_factory.clone(),
            pow_generator: pow_generator.clone(),
        });
        let args = Args::default_for_testing().with_port(443);
        HashiverseServer::new(runtime_services, environment_factory.clone(), args).await?
    };

    // The server we will kill
    let hashiverse_server_to_kill = {
        let runtime_services = Arc::new(RuntimeServices {
            time_provider: time_provider.clone(),
            transport_factory: transport_factory.clone(),
            pow_generator: pow_generator.clone(),
        });
        let args = Args::default_for_testing().with_port(10001);
        HashiverseServer::new(runtime_services, environment_factory.clone(), args).await?
    };
    let hashiverse_server_to_kill_id = hashiverse_server_to_kill.server_id.id.clone();

    // Some other servers
    let hashiverse_servers = {
        let mut hashiverse_servers = Vec::new();
        for i in 0..NUM_EXTRA_SERVERS {
            let runtime_services = Arc::new(RuntimeServices {
                time_provider: time_provider.clone(),
                transport_factory: transport_factory.clone(),
                pow_generator: pow_generator.clone(),
            });
            let args = Args::default_for_testing().with_port(20000 + i as u16);
            let hashiverse_server = HashiverseServer::new(runtime_services, environment_factory.clone(), args).await?;
            hashiverse_servers.push(hashiverse_server);
        }
        hashiverse_servers
    };

    let cancellation_token_to_kill = CancellationToken::new();

    // Start the servers
    {
        {
            let cancellation_token = cancellation_token.clone();
            join_set.spawn(async move {
                hashiverse_server_main.run(cancellation_token.clone()).await;
            });
        }
        {
            let cancellation_token_to_kill = cancellation_token_to_kill.clone();
            join_set.spawn(async move {
                hashiverse_server_to_kill.run(cancellation_token_to_kill.clone()).await;
            });
        }

        for hashiverse_server in hashiverse_servers.as_slice() {
            let cancellation_token = cancellation_token.clone();
            let hashiverse_server = hashiverse_server.clone();
            join_set.spawn(async move {
                hashiverse_server.run(cancellation_token.clone()).await;
            });
        }
    }

    // Fiddle with the environment
    {
        let time_provider = time_provider.clone();
        let cancellation_token_to_kill = cancellation_token_to_kill.clone();
        let hashiverse_servers = hashiverse_servers.clone();

        join_set.spawn(async move {
            scopeguard::defer! {
               warn!("--- Killing all the servers ------------------------------------------------");
               cancellation_token_to_kill.cancel();
               cancellation_token.cancel();
            }

            warn!("--- Test fiddler started ------------------------------------------------");

            time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(60)).await;

            warn!("--- Killing one of the servers ------------------------------------------------");
            cancellation_token_to_kill.cancel();

            time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(1)).await;

            warn!("--- Checking that the killed server is propagating ------------------------------------------------");

            const MAX_MINUTES_TO_WAIT_FOR_KILLING_PROPAGATION: usize = 360;
            const MINUTES_TO_SLEEP_BETWEEN_PROPAGATION_CHECKS: usize = 5;
            let mut current_minute = 0;
            let mut server_presence_still_felt: usize = 0;
            while current_minute <= MAX_MINUTES_TO_WAIT_FOR_KILLING_PROPAGATION {
                server_presence_still_felt = 0;
                for hashiverse_server in hashiverse_servers.as_slice() {
                    let environment = hashiverse_server.environment.clone();
                    let peer_buckets_bytes = environment.config_get_bytes(CONFIG_KADEMLIA_PEER_BUCKETS).unwrap().unwrap();
                    let peer_buckets = json::bytes_to_struct::<Vec<Vec<Peer>>>(&compression::decompress(&peer_buckets_bytes).unwrap().to_bytes()).unwrap();
                    let peer_to_kill = peer_buckets.iter().flatten().find(|peer| peer.id == hashiverse_server_to_kill_id);
                    if peer_to_kill.is_some() {
                        server_presence_still_felt += 1;
                    }
                }

                if server_presence_still_felt > 0 {
                    warn!("The killed server is still known by {} peers after {} minutes", server_presence_still_felt, current_minute);
                }
                else {
                    warn!("The killed server vanquished after {} minutes", current_minute);
                    break;
                }

                current_minute += MINUTES_TO_SLEEP_BETWEEN_PROPAGATION_CHECKS;
                time_provider.sleep_millis(MILLIS_IN_MINUTE.const_mul(MINUTES_TO_SLEEP_BETWEEN_PROPAGATION_CHECKS as i64)).await;
            }

            assert_eq!(0, server_presence_still_felt, "The killed server is still known by {} peers", server_presence_still_felt);
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
