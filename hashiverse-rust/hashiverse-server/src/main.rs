use clap::Parser;
use hashiverse_lib::tools::time_provider::time_provider::RealTimeProvider;
use hashiverse_lib::tools::tools::configure_logging_with_time_provider;
use hashiverse_lib::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;
use hashiverse_lib::transport::ddos::ddos::DdosProtection;
use hashiverse_server_lib::environment::disk_environment_store::DiskEnvironmentFactory;
use hashiverse_server_lib::environment::environment::EnvironmentFactory;
use hashiverse_server_lib::transport::full_https_transport::FullHttpsTransportFactory;
use hashiverse_server_lib::server::args::Args;
use hashiverse_server_lib::server::hashiverse_server::HashiverseServer;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use hashiverse_lib::tools::config;
use hashiverse_lib::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::transport::bootstrap_provider::dnssec_bootstrap_provider::DnssecBootstrapProvider;
use hashiverse_server_lib::transport::ddos::ipset_ddos::IpsetDdosProtection;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let time_provider = Arc::new(RealTimeProvider);
    configure_logging_with_time_provider(&args.log_level, time_provider.clone());

    // Set up TLS
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install aws-lc-rs as the crypto provider");

    let cancellation_token = CancellationToken::new();
    hashiverse_server_lib::tools::tools::spawn_shutdown_signal_handler(cancellation_token.clone())?;

    let environment_factory: Arc<dyn EnvironmentFactory> = Arc::new(DiskEnvironmentFactory::new(args.base_path.as_str()));
    let ddos_protection: Arc<dyn DdosProtection> = Arc::new(IpsetDdosProtection::new(config::SERVER_DDOS_IPSET_SET_NAME, config::SERVER_DDOS_SCORE_THRESHOLD, config::SERVER_DDOS_DECAY_PER_SECOND, config::SERVER_DDOS_BAD_REQUEST_PENALTY, config::SERVER_DDOS_MAX_CONNECTIONS_PER_IP, time_provider.clone()));
    let bootstrap_provider: Arc<dyn BootstrapProvider> = Arc::new(DnssecBootstrapProvider::new());
    let transport_factory = Arc::new(FullHttpsTransportFactory::new(ddos_protection, bootstrap_provider));
    let pow_generator = Arc::new(NativeParallelPowGenerator::new().with_cancellation_token(cancellation_token.clone()));
    let runtime_services = Arc::new(RuntimeServices { time_provider, transport_factory, pow_generator });

    let hashiverse_server = match HashiverseServer::new(runtime_services, environment_factory, args).await {
        Ok(hashiverse_server) => hashiverse_server,
        Err(_) if cancellation_token.is_cancelled() => {
            log::warn!("stopped during startup; a server identity not yet created will be computed again on next start");
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    hashiverse_server.run(cancellation_token).await;

    Ok(())
}
