use std::sync::Arc;
use crate::transport::full_https_transport::FullHttpsTransportFactory;
use crate::transport::tcp_transport::TcpTransportFactory;
use anyhow::Result;
use bytes::Bytes;
use hashiverse_lib::tools::tools::get_temp_dir;
use hashiverse_lib::tools::BytesGatherer;
use hashiverse_lib::transport::ddos::noop_ddos::NoopDdosProtection;
use hashiverse_lib::transport::mem_transport::MemTransportFactory;
use hashiverse_lib::transport::transport::{IncomingRequest, TransportFactory, TransportServerHandler};
use lazy_static::lazy_static;
use serial_test::serial;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;

lazy_static! {
    static ref BYTES: Bytes = Bytes::from("test_request");
}

// Rustls 0.23+ requires a process-global CryptoProvider. Binaries install it from `main`
// (see hashiverse-server/src/main.rs:26); library tests have no main, so each TLS-touching
// test installs the provider itself. `install_default` is atomic and idempotent — `Err`
// means another test already installed the same provider, which is fine.
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[tokio::test]
async fn test_server_client_rpc_mem() -> Result<()> {
    let factory: Arc<dyn TransportFactory> = MemTransportFactory::default();
    test_server_client_rpc_single_message(factory, 23850).await
}

#[tokio::test]
#[serial(tcp)]
async fn test_server_client_rpc_tcp() -> Result<()> {
    let factory: Arc<dyn TransportFactory> = Arc::new(TcpTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::new_tcp_localhost()));
    test_server_client_rpc_single_message(factory, 25438).await
}

#[tokio::test]
#[serial(http)]
async fn test_server_client_rpc_http() -> Result<()> {
    install_crypto_provider();
    let factory: Arc<dyn TransportFactory> = Arc::new(FullHttpsTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::new_tcp_localhost()));
    test_server_client_rpc_single_message(factory, 37284).await
}

async fn test_server_client_rpc_single_message(
    transport_factory: Arc<dyn TransportFactory>,
    port: u16
) -> Result<()> {
    let test_request = "test_request".to_string();

    let cancellation_token = CancellationToken::new();

    struct MyHandler {}
    impl TransportServerHandler for MyHandler {
        async fn handle(&self, bytes: Bytes) -> BytesGatherer {
            let response = "echo:".to_owned() + std::str::from_utf8(&bytes).expect("invalid utf-8");
            BytesGatherer::from_bytes(Bytes::from(response))
        }
    }

    let my_handler = MyHandler {};
    let (_temp_dir, temp_dir_path) = get_temp_dir()?;
    let transport_server = transport_factory.create_server(&temp_dir_path, port, true).await?;
    let address = transport_server.get_address().clone();

    let mut final_response = Bytes::new();

    let (tx, rx) = mpsc::channel::<IncomingRequest>(32);

    let _ = tokio::join!(
        my_handler.run(cancellation_token.clone(), rx),
        transport_server.listen(cancellation_token.clone(), tx),
        async {
            sleep(Duration::from_secs(1)).await;
            final_response = transport_factory.rpc(&address, BYTES.clone()).await?;
            cancellation_token.cancel();
            Ok::<_, anyhow::Error>(())
        }
    );

    // Verify the response
    assert_eq!(final_response, format!("echo:{}", test_request));

    Ok(())
}

#[tokio::test]
async fn test_server_client_rpc_no_server_mem() -> Result<()> {
    let factory: Arc<dyn TransportFactory> = MemTransportFactory::default();
    test_server_client_rpc_no_server(factory, 17483).await
}

#[tokio::test]
#[serial(tcp)]
async fn test_server_client_rpc_no_server_tcp() -> Result<()> {
    let factory: Arc<dyn TransportFactory> = Arc::new(TcpTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::new_tcp_localhost()));
    test_server_client_rpc_no_server(factory, 15384).await
}

#[tokio::test]
#[serial(http)]
async fn test_server_client_rpc_no_server_http() -> Result<()> {
    install_crypto_provider();
    let factory: Arc<dyn TransportFactory> = Arc::new(FullHttpsTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::new_tcp_localhost()));
    test_server_client_rpc_no_server(factory, 18280).await
}

async fn test_server_client_rpc_no_server(
    transport_factory: Arc<dyn TransportFactory>,
    port: u16,
) -> Result<()> {
    let cancellation_token = CancellationToken::new();

    let (_temp_dir, temp_dir_path) = get_temp_dir()?;
    let transport_server = transport_factory.create_server(&temp_dir_path, port, true).await?;
    let address = transport_server.get_address().clone();

    // Run and shutdown the server
    let (tx, _rx) = mpsc::channel::<IncomingRequest>(32);
    let results = tokio::join! {
        transport_server.listen(cancellation_token.clone(), tx),
        async {
            sleep(Duration::from_secs(1)).await;
            cancellation_token.cancel();
        }
    };

    if let Err(e) = results.0 {
        anyhow::bail!("error running server: {}", e);
    }

    tokio::select! {
        result = async {
            let response_future = transport_factory.rpc(
                &address,
                BYTES.clone(),
            ).await;

            match response_future {
                Ok(_) => { Err(anyhow::anyhow!("there should be no response")) },
                Err(_) => { Ok(()) },
            }
          } => { result }
    }
}
