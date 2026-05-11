#![feature(try_blocks)]
#![feature(duration_constructors)]

use clap::Parser;
use hashiverse_lib::client::client_storage::mem_client_storage::MemClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::client::key_locker::mem_key_locker::MemKeyLockerManager;
use hashiverse_lib::tools::time_provider::time_provider::{RealTimeProvider};
use hashiverse_lib::tools::tools::configure_logging_with_time_provider;
use hashiverse_server_lib::environment::disk_environment_store::DiskEnvironmentFactory;
use hashiverse_server_lib::environment::environment::EnvironmentFactory;
use hashiverse_server_lib::server::hashiverse_server::HashiverseServer;
use log::{error, info, trace, warn};
use std::sync::Arc;
use futures::StreamExt;
use tokio::select;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::codec::{FramedRead, LinesCodec};
use tokio_util::sync::CancellationToken;
use hashiverse_lib::tools::parallel_pow_generator::{NativeParallelPowGenerator, ParallelPowGenerator};
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;
use hashiverse_lib::transport::ddos::ddos::DdosProtection;
use hashiverse_lib::transport::ddos::noop_ddos::NoopDdosProtection;
use hashiverse_server_lib::transport::full_https_transport::FullHttpsTransportFactory;
use hashiverse_server_lib::server::args::Args;

const NUM_CLIENTS: usize = 10;
const NUM_ADDITIONAL_SERVERS: usize = 15;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    // Set up TLS
    {
        rustls::crypto::ring::default_provider().install_default().expect("Failed to install Ring as the crypto provider");
    }

    let time_provider = Arc::new(RealTimeProvider);
    // let time_provider = Arc::new(ScaledTimeProvider::new(60.0));
    configure_logging_with_time_provider("trace", time_provider.clone());

    let args = Args::parse();

    let environment_factory = Arc::new(DiskEnvironmentFactory::new(args.base_path.as_str()));
    let ddos_protection: Arc<dyn DdosProtection> = NoopDdosProtection::default();
    let bootstrap_provider: Arc<dyn BootstrapProvider> = hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider::new(vec!["127.0.0.1:443".to_string()]);
    let transport_factory = Arc::new(FullHttpsTransportFactory::new(ddos_protection, bootstrap_provider));
    let pow_generator: Arc<dyn ParallelPowGenerator> = Arc::new(NativeParallelPowGenerator::new());

    let runtime_services = Arc::new(RuntimeServices {
        time_provider: time_provider.clone(),
        transport_factory: transport_factory.clone(),
        pow_generator: pow_generator.clone(),
    });

    let cancellation_token = CancellationToken::new();

    let mut join_set = JoinSet::new();

    // Spawn the primary server
    {
        let cancellation_token = cancellation_token.clone();
        let environment_factory = environment_factory.clone();
        let args = args.clone();
        let hashiverse_server = HashiverseServer::new(runtime_services.clone(), environment_factory, args).await;
        join_set.spawn(async move {
            match hashiverse_server {
                Ok(hashiverse_server) => {
                    hashiverse_server.run(cancellation_token.clone()).await;
                }
                Err(e) => {
                    error!("Failed to start primary hashiverse_server: {}", e);
                }
            }
        });
    }

    // Spawn some secondary servers
    {
        for i in 1..=NUM_ADDITIONAL_SERVERS {
            let args = args.clone().with_port(10000u16 + (i as u16));
            let cancellation_token = cancellation_token.clone();
            let environment_factory = environment_factory.clone();
            let hashiverse_server = HashiverseServer::new(runtime_services.clone(), environment_factory, args).await;
            join_set.spawn(async move {
                match hashiverse_server {
                    Ok(hashiverse_server) => {
                        hashiverse_server.run(cancellation_token.clone()).await;
                    }
                    Err(e) => {
                        error!("Failed to start secondary hashiverse_server: {}", e);
                    }
                }
            });
        }
    }

    // Add clients and their "command processing loop"


    // Create all the clients and get
    let mut client_txs = Vec::new();
    {
        for i in 1..=NUM_CLIENTS {
            let args = hashiverse_lib::client::args::Args::new();
            let passphrase = format!("client {}", i);

            let key_locker_manager = MemKeyLockerManager::new().await?;
            let key_locker = key_locker_manager.create(passphrase).await?;
            let client_storage = MemClientStorage::new().await?;
            let hashiverse_client = Arc::new(HashiverseClient::new(runtime_services.clone(), client_storage, key_locker, args).await?);

            let (tx, mut rx) = mpsc::channel::<String>(8);
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build().expect("failed to create tokio runtime");

                rt.block_on(async move {
                    while let Some(line) = rx.recv().await {
                        if let Err(e) =  hashiverse_client.dispatch_command(&line).await {
                            error!("problem processing line [{}]: {}", line, e);
                        }
                    }
                    info!("rx thread exiting: channel closed and hashiverse_client destroyed");
                });
            });

            client_txs.push(tx);
        }
    }

    // Lets do some command line action for the clients
    {
        if !client_txs.is_empty() {
            let cancellation_token = cancellation_token.clone();

            join_set.spawn(async move {
                info!("console_reader started");

                let mut reader = FramedRead::new(tokio::io::stdin(), LinesCodec::new());

                loop {
                    select! {
                        _ = cancellation_token.cancelled() => {
                            info!("console_reader stopping");
                            break;
                        },

                        next_line = reader.next() => {
                            match next_line {
                                Some(Ok(mut line)) => {

                                    if line.trim().is_empty() {
                                        continue;
                                    }

                                    let mut client_i: usize = 0;

                                    let splits: Vec<&str> = line.splitn(2, ":").collect();
                                    if 1 < splits.len() {
                                        client_i = splits[0].parse().unwrap_or(1) - 1;
                                        line = splits[1].to_string();
                                    }

                                    if client_i >= client_txs.len() {
                                        error!("there is no client {} so sending to first client", client_i);
                                        client_i = 0;
                                    }

                                    trace!("dispatching command to client {}", client_i);
                                    let result = client_txs[client_i].send(line).await;
                                    if let Err(e) = result {
                                        error!("error sending command to client {}: {}", client_i, e);
                                    }
                                },
                                Some(Err(e)) => {
                                    error!("error: {}", e);
                                    break;
                                },
                                None => {
                                    warn!("end of input stream");
                                    break;
                                },
                            }
                        },
                    }
                }

                info!("console_reader stopped");
            });
        }
    }

    hashiverse_server_lib::tools::tools::spawn_ctrl_c_handler(cancellation_token.clone());

    while let Some(res) = join_set.join_next().await {
        match res {
            Ok(()) => info!("headline service completed"),
            Err(e) => warn!("headline service failed: {}", e),
        }
    }

    info!("Exiting");

    Ok(())
}
