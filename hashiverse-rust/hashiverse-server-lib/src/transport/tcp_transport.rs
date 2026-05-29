//! # Plain-text TCP transport
//!
//! An unencrypted transport for local testing and private-LAN deployments where TLS
//! is unnecessary. Frames requests and responses with `tokio-util`'s
//! `LengthDelimitedCodec` — each message is prefixed with a u32 length, so there's
//! no application-level ambiguity about where one message ends and the next begins.
//!
//! Uses the same pluggable
//! [`hashiverse_lib::transport::ddos::ddos::DdosProtection`] trait as the HTTPS
//! transport, so `NoopDdosProtection`, `MemDdos`, or the ipset-backed protection can
//! all drop in unchanged. Per-request timeout is 2 seconds; anything slower is
//! considered either a buggy client or a slow-loris probe.

use crate::tools::tools::get_public_ipv4;
use anyhow::anyhow;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use hashiverse_lib::tools::config;
use hashiverse_lib::transport::ddos::ddos::{DdosConnectionGuard, DdosProtection};
use hashiverse_lib::transport::transport::{IncomingRequest, ServerState, TransportFactory, TransportServer};
use hashiverse_lib::transport::transport_ownership_proof::{EmptyMarkerOwnershipProof, TransportOwnershipProof};
use log::{info, trace, warn};
use parking_lot::RwLock;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio::time::sleep;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use hashiverse_lib::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;

#[derive(Clone)]
pub struct TcpTransportFactory {
    ddos_protection: Arc<dyn DdosProtection>,
    bootstrap_provider: Arc<dyn BootstrapProvider>,
}

impl TcpTransportFactory {
    pub fn new(ddos_protection: Arc<dyn DdosProtection>, bootstrap_provider: Arc<dyn BootstrapProvider>) -> Self {
        Self { ddos_protection, bootstrap_provider }
    }
}

pub struct TcpTransportServer {
    address: String,
    listener: Arc<Mutex<TcpListener>>,
    state: Arc<RwLock<ServerState>>,
    ddos_protection: Arc<dyn DdosProtection>,
}

impl TcpTransportServer {
    async fn new(address: String, listener: TcpListener, ddos_protection: Arc<dyn DdosProtection>) -> anyhow::Result<Self> {
        Ok(TcpTransportServer {
            address,
            listener: Arc::new(Mutex::new(listener)),
            state: Arc::new(RwLock::new(ServerState::Created)),
            ddos_protection,
        })
    }
}

#[async_trait::async_trait]
impl TransportServer for TcpTransportServer {
    fn get_address(&self) -> &String {
        &self.address
    }

    fn get_transport_ownership_proof(&self) -> Arc<dyn TransportOwnershipProof> {
        // Plain TCP is for trusted-network deployments only — no crypto proof exists, so
        // the empty-marker proof matches mem-transport's behaviour and lets V2 announces
        // flow within a TCP-only network without crossing wires with HTTPS peers.
        Arc::new(EmptyMarkerOwnershipProof)
    }

    async fn listen(&self, cancellation_token: CancellationToken, handler: mpsc::Sender<IncomingRequest>) -> anyhow::Result<()> {
        // Check that we can transition to listening
        {
            let mut state = self.state.write();
            match *state {
                ServerState::Listening => {
                    anyhow::bail!("server is already listening");
                }
                ServerState::Shutdown => {
                    anyhow::bail!("server has been shut down");
                }
                ServerState::Created => {
                    *state = ServerState::Listening;
                }
            }
        }

        async fn process_connection(cancellation_token: CancellationToken, handler: mpsc::Sender<IncomingRequest>, socket: TcpStream, socket_addr: SocketAddr, ddos_protection: Arc<dyn DdosProtection>) -> anyhow::Result<()> {
            // trace!("accepted connection on: {socket_addr}");
            // defer! { trace!("dropped connection from: {socket_addr}"); }

            let ip = socket_addr.ip().to_string();
            let ddos_connection_guard = match DdosConnectionGuard::try_new(ddos_protection, &ip) {
                Some(guard) => Arc::new(guard),
                None => {
                    trace!("DDoS: dropping TCP connection from {}", ip);
                    return Ok(());
                }
            };
            let caller_address = ddos_connection_guard.ip().to_string();
            let mut framed = LengthDelimitedCodec::builder().max_frame_length(config::PROTOCOL_MAX_BLOB_SIZE_REQUEST).new_framed(socket);

            let result = tokio::select! {
                _ = cancellation_token.cancelled() => { return Err(anyhow!("cancelled")) },

                _ = sleep(Duration::from_secs(2)) => {
                    Err(anyhow::anyhow!("timeout waiting for request"))
                },

                next = framed.next() => {
                    match next {
                        None => Ok(()),
                        Some(Ok(bytes)) => {
                            // trace!("received bytes={:?}", bytes);
                            let (reply_tx, reply_rx) = oneshot::channel();
                            handler.send(IncomingRequest::new(caller_address, bytes.into(), reply_tx, ddos_connection_guard)).await?;
                            let response = reply_rx.await?;
                            framed.send(response.to_bytes()).await?;
                            Ok(())
                        },
                        Some(Err(e)) => Err(anyhow!("error reading string from framed stream: {}", e)),
                    }
                }
            };

            if let Err(e) = result {
                warn!("error processing connection: {}", e);
            }

            Ok(())
        }

        let task_tracker = TaskTracker::new();

        info!("listening on address {}", self.address);

        loop {
            let listener = self.listener.lock().await;

            tokio::select! {
                _ = cancellation_token.cancelled() => {
                    break;
                },
                Ok((socket, socket_addr)) = listener.accept() => {
                    task_tracker.spawn(
                        process_connection(cancellation_token.clone(), handler.clone(), socket, socket_addr, self.ddos_protection.clone())
                    );
                },
            }
        }

        // Stop accepting new connections
        info!("stopped listening on address {}", self.address);
        drop(self.listener.lock().await);

        // Wait for existing connections to complete
        info!("waiting for open connections to complete");
        task_tracker.close();
        task_tracker.wait().await;

        // Notify the "shutdown" coroutine that we have successfully shutdown
        info!("all open connections complete");
        *self.state.write() = ServerState::Shutdown;

        Ok(())
    }
}

#[async_trait::async_trait]
impl TransportFactory for TcpTransportFactory {
    async fn get_bootstrap_addresses(&self) -> Vec<String> {
        self.bootstrap_provider.get_bootstrap_addresses().await
    }

    async fn create_server(&self, _base_path: &str, port: u16, force_local_network: bool) -> anyhow::Result<Arc<dyn TransportServer>> {
        // Deliberately IPv4-only.  See https_transport.rs for the reasoning.
        let address_to_bind = format!("0.0.0.0:{}", port);
        info!("bind on: {}", address_to_bind);
        let listener = TcpListener::bind(address_to_bind).await?;

        let address_bound_ip = get_public_ipv4(force_local_network).await?;
        let address_bound_port = listener.local_addr()?.port();
        let address = format!("{}:{}", address_bound_ip, address_bound_port);

        let tcp_transport_server = Arc::new(TcpTransportServer::new(address, listener, self.ddos_protection.clone()).await?);
        Ok(tcp_transport_server)
    }

    async fn rpc(&self, address: &str, bytes: Bytes) -> anyhow::Result<Bytes> {
        let stream = TcpStream::connect(address).await?;
        // trace!("connected to: {}", address.address);
        // defer! { trace!("disconnected from: {}", &address.address); }

        let mut framed: Framed<TcpStream, LengthDelimitedCodec> = Framed::new(stream, LengthDelimitedCodec::new());
        framed.send(bytes).await?;

        // Return the response
        trace!("awaiting response");
        tokio::select! {
            _ = sleep(Duration::from_secs(2)) => {
                trace!("timeout");
                Err(anyhow::anyhow!("timeout waiting for response"))
            },

            next_frame = framed.next() => {
                match next_frame {
                    Some(Ok(bytes)) => {
                        Ok(bytes.into())
                    }
                    Some(Err(e)) => {
                        Err(anyhow::anyhow!("error reading response: {}", e)) },
                    None => {
                        Err(anyhow::anyhow!("no response")) },
                }
           }
        }
    }
}


#[cfg(test)]
mod tests {
    use crate::transport::tcp_transport::TcpTransportFactory;
    use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
    use hashiverse_lib::transport::ddos::noop_ddos::NoopDdosProtection;
    use hashiverse_lib::transport::transport::TransportFactory;
    use std::sync::Arc;

    #[tokio::test]
    async fn rpc_test() -> anyhow::Result<()> {
        let factory: Arc<dyn TransportFactory> = Arc::new(TcpTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::default()));
        hashiverse_lib::transport::transport::tests::rpc_test(factory).await
    }

    #[tokio::test]
    async fn bind_port_zero_test() -> anyhow::Result<()> {
        let factory: Arc<dyn TransportFactory> = Arc::new(TcpTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::default()));
        hashiverse_lib::transport::transport::tests::bind_port_zero_test(factory).await
    }
}
