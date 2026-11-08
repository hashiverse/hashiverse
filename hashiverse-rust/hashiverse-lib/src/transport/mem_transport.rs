//! # In-memory transport for tests
//!
//! A fully synchronous in-process implementation of
//! [`crate::transport::transport::TransportFactory`] and
//! [`crate::transport::transport::TransportServer`]: every "server" registers itself in
//! a shared registry keyed by id, and every "client" request is just a channel send into
//! the matching server's request queue.
//!
//! This is what makes the integration-test harness fast and deterministic. A virtual
//! network of dozens of servers + clients runs inside a single test binary, with no
//! sockets, no TLS negotiation, no PoW relaxation fudge, and no flaky wall-clock
//! ordering. Swap `MemTransportFactory` for the HTTPS factory and the same protocol code
//! runs on the real network.

use crate::tools::types::Id;
use crate::transport::ddos::ddos::{DdosConnectionGuard, DdosProtection};
use crate::transport::transport::{IncomingRequest, ServerState, TransportFactory, TransportServer};
use crate::transport::transport_ownership_proof::{EmptyMarkerOwnershipProof, TransportOwnershipProof};
use anyhow::{Result, anyhow};
use bytes::Bytes;
use log::info;
use parking_lot::RwLock;
use std::collections::HashMap;
use crate::tools::time::DurationMillis;
use crate::tools::time_provider::time_provider::TimeProvider;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use crate::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;
use crate::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
use crate::transport::ddos::noop_ddos::NoopDdosProtection;

#[derive(Debug)]
struct RpcMessage {
    caller_address: String,
    bytes: Bytes,
    response_tx: oneshot::Sender<Result<Bytes>>,
}

struct ServerEntry {
    command_tx: mpsc::Sender<RpcMessage>,
}

struct ServerManager {
    servers: Arc<RwLock<HashMap<u16, Arc<ServerEntry>>>>,
}

impl ServerManager {
    pub fn new() -> Self {
        ServerManager {
            servers: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    pub async fn remove_server(&self, port: u16) {
        let mut servers_locked = self.servers.write();
        servers_locked.remove(&port);
    }
}

/// An entirely in-process [`TransportServer`] used by the integration test harness.
///
/// Servers created by `MemTransportFactory` share a process-wide registry keyed by port;
/// "sending a request" from one client to one server becomes a channel send on the registry.
/// There is no serialization to sockets, no DNS, no kernel — which makes this both
/// dramatically faster than a real network and fully deterministic when paired with a virtual
/// [`crate::tools::time_provider::time_provider::TimeProvider`]. Port `0` is translated to a
/// freshly-allocated port number, mirroring the semantics of a real OS bind.
///
/// Not for production use: there is nothing here that crosses a process or host boundary.
pub struct MemTransportServer {
    port: u16,
    address: String,
    server_manager: Arc<ServerManager>,
    command_rx: Arc<RwLock<Option<mpsc::Receiver<RpcMessage>>>>,
    state: Arc<RwLock<ServerState>>,
    ddos_protection: Arc<dyn DdosProtection>,
}

#[async_trait::async_trait]
impl TransportServer for MemTransportServer {
    fn get_address(&self) -> &String {
        &self.address
    }

    fn get_transport_ownership_proof(&self) -> Arc<dyn TransportOwnershipProof> {
        Arc::new(EmptyMarkerOwnershipProof)
    }

    async fn listen(&self, cancellation_token: CancellationToken, handler: mpsc::Sender<IncomingRequest>) -> Result<()> {
        async fn process_connection(_cancellation_token: CancellationToken, handler: mpsc::Sender<IncomingRequest>, message: RpcMessage, ddos_protection: Arc<dyn DdosProtection>) -> anyhow::Result<()> {
            // trace!("accepted connection");
            // scopeguard::defer! { trace!("dropped connection"); }
            // trace!("received packet={:?}", message.bytes);
            let ddos_connection_guard = match DdosConnectionGuard::try_new(ddos_protection, message.caller_address.as_str()) {
                Some(guard) => Arc::new(guard),
                None => return Ok(()),
            };
            let caller_address = ddos_connection_guard.ip().to_string();
            let (reply_tx, reply_rx) = oneshot::channel();
            handler.send(IncomingRequest::new(caller_address, message.bytes, reply_tx, ddos_connection_guard)).await?;
            let response = reply_rx.await?;
            let _ = message.response_tx.send(Ok(response.to_bytes()));

            Ok(())
        }

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

        let task_tracker = TaskTracker::new();

        info!("listening on address {}", self.address);

        // Take ownership of the receiver.  If there's no receiver, we can't listen.  Should never happen!
        let mut receiver = match self.command_rx.write().take() {
            Some(r) => r,
            None => {
                return Err(anyhow!("no receiver available on address {}", self.address));
            }
        };

        loop {
            tokio::select! {
                _ = cancellation_token.cancelled() => {
                    break;
                }

                Some(msg) = receiver.recv() => {
                    task_tracker.spawn(
                        process_connection(cancellation_token.clone(), handler.clone(), msg, self.ddos_protection.clone())
                    );
                }
            }
        }

        info!("stopped listening on port {}", self.address);
        self.server_manager.remove_server(self.port).await;

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

#[derive(Clone)]
pub struct MemTransportFactory {
    server_manager: Arc<ServerManager>,
    ddos_protection: Arc<dyn DdosProtection>,
    bootstrap_provider: Arc<dyn BootstrapProvider>,
    /// Simulated latency added to every `rpc` call, slept on the given clock.  Shared across clones of this factory.
    rpc_delay: Arc<RwLock<Option<(Arc<dyn TimeProvider>, DurationMillis)>>>,
}

impl MemTransportFactory {
    pub fn new(ddos_protection: Arc<dyn DdosProtection>, bootstrap_provider: Arc<dyn BootstrapProvider>) -> Self {
        Self {
            server_manager: Arc::new(ServerManager::new()),
            ddos_protection,
            bootstrap_provider,
            rpc_delay: Arc::new(RwLock::new(None)),
        }
    }

    /// Simulates network latency: every subsequent `rpc` call sleeps `rpc_delay` on `time_provider` before reaching the server,
    /// so it scales with the test's clock.  Lets tests tell RPCs that overlap from RPCs that run one after another.
    pub fn set_rpc_delay(&self, time_provider: Arc<dyn TimeProvider>, rpc_delay: DurationMillis) {
        *self.rpc_delay.write() = Some((time_provider, rpc_delay));
    }

    pub fn clear_rpc_delay(&self) {
        *self.rpc_delay.write() = None;
    }

    #[allow(clippy::should_implement_trait)] // wraps Arc<Self>, can't satisfy the Default trait
    pub fn default() -> Arc<Self> {
        Arc::new(Self::new(NoopDdosProtection::default(), ManualBootstrapProvider::new_mem_multiple()))
    }
}

#[async_trait::async_trait]
impl TransportFactory for MemTransportFactory {
    async fn get_bootstrap_addresses(&self) -> Vec<String> {
        self.bootstrap_provider.get_bootstrap_addresses().await
    }

    async fn create_server(&self, _base_path: &str, port: u16, force_local_network: bool) -> anyhow::Result<Arc<dyn TransportServer>> {
        if !force_local_network {
            return Err(anyhow!("only local network is supported"));
        }

        let mut servers_locked = self.server_manager.servers.write();

        if servers_locked.contains_key(&port) {
            return Err(anyhow!("server already exists on port {}", port));
        }

        // If they have requested port 0, pick the first available empty slot
        let bound_port = match port {
            0 => {
                servers_locked.keys().max().unwrap_or(&0u16) + 1
            }
            _ => port
        };

        let address = format!("{}", bound_port);

        // Create channels for communication.  Buffer sized generously so bursts of
        // concurrent in-memory RPCs don't trip capacity limits; backpressure is still
        // applied via awaited `send` below, which is closer to the behaviour of a real
        // TCP socket than `try_send`'s fail-fast.
        let (tx, rx) = mpsc::channel::<RpcMessage>(256);

        // Create the server
        let mem_transport_server = Arc::new(MemTransportServer {
            port: bound_port,
            address,
            server_manager: self.server_manager.clone(),
            command_rx: Arc::new(RwLock::new(Some(rx))),
            state: Arc::new(RwLock::new(ServerState::Created)),
            ddos_protection: self.ddos_protection.clone(),
        });

        // Store the server and its sender in the map
        servers_locked.insert(bound_port, Arc::new(ServerEntry { command_tx: tx }));

        Ok(mem_transport_server)
    }

    async fn rpc(&self, address: &str, bytes: Bytes) -> Result<Bytes> {
        let rpc_delay = self.rpc_delay.read().clone();
        if let Some((time_provider, rpc_delay)) = rpc_delay {
            time_provider.sleep_millis(rpc_delay).await;
        }

        let port: u16 = address.parse()?;

        let server_entry = {
            let servers = self.server_manager.servers.read();
            let server_entry = servers.get(&port).ok_or_else(|| anyhow::anyhow!("no server found with port {}", port))?;
            server_entry.clone()
        };

        // trace!("connected to: {:?}", address);
        // defer! { trace!("disconnected from: {:?}", &address); }

        // Create a oneshot channel for the response
        let (response_tx, response_rx) = oneshot::channel();

        // Create the message
        let message = RpcMessage { caller_address: format!("mem:{}", Id::random()), bytes, response_tx };

        // Send the message to the server using the sender from the server entry.
        // Awaited `send` applies backpressure if the receiver is saturated, rather than
        // dropping the request — mirrors how a real TCP transport would behave.
        server_entry.command_tx.send(message).await.map_err(|e| anyhow::anyhow!("failed to send request: {}", e))?;

        // Wait for the response
        response_rx.await.map_err(|_| anyhow::anyhow!("server disconnected before responding"))?
    }
}


#[cfg(test)]
mod tests {
    use crate::transport::mem_transport::MemTransportFactory;
    use crate::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
    use crate::transport::ddos::noop_ddos::NoopDdosProtection;
    use std::sync::Arc;

    #[tokio::test]
    async fn rpc_test() -> anyhow::Result<()> {
        let factory: Arc<dyn crate::transport::transport::TransportFactory> = Arc::new(MemTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::default()));
        crate::transport::transport::tests::rpc_test(factory).await
    }

    #[tokio::test]
    async fn bind_port_zero_test() -> anyhow::Result<()> {
        let factory: Arc<dyn crate::transport::transport::TransportFactory> = Arc::new(MemTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::default()));
        crate::transport::transport::tests::bind_port_zero_test(factory).await
    }

    /// The delay is slept on the supplied (here scaled) clock, applies to every call, is shared by clones of the factory,
    /// and delays concurrent calls independently.  (An RPC to a port with no server still pays the delay first, so no server is needed.)
    #[tokio::test]
    async fn rpc_delay_is_per_call_concurrent_and_on_the_given_clock() -> anyhow::Result<()> {
        use crate::tools::time::MILLIS_IN_SECOND;
        use crate::tools::time_provider::time_provider::{ScaledTimeProvider, TimeProvider};
        use crate::transport::transport::TransportFactory;
        use bytes::Bytes;

        // 5 scaled seconds per rpc = 500ms of real time at 10x: each assertion's pass/fail boundary is a whole delay
        // (500ms of real time) away from the expected timing, which leaves headroom on a loaded CI runner
        let time_provider = Arc::new(ScaledTimeProvider::new(10.0));
        let rpc_delay = MILLIS_IN_SECOND.const_mul(5);
        const CONCURRENT_RPCS: usize = 5;

        let factory = MemTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::default());
        let factory_clone = factory.clone();

        // No delay by default
        let started = time_provider.current_time_millis();
        assert!(factory.rpc("65000", Bytes::new()).await.is_err());
        assert!(time_provider.current_time_millis() - started < rpc_delay);

        // Setting it on a clone affects the original, and the delay elapses on the scaled clock
        factory_clone.set_rpc_delay(time_provider.clone(), rpc_delay);
        let started = time_provider.current_time_millis();
        assert!(factory.rpc("65000", Bytes::new()).await.is_err());
        assert!(time_provider.current_time_millis() - started >= rpc_delay);

        // Concurrent calls overlap: the batch takes about one delay, not CONCURRENT_RPCS of them
        let started = time_provider.current_time_millis();
        let results = futures::future::join_all((0..CONCURRENT_RPCS).map(|_| factory.rpc("65000", Bytes::new()))).await;
        assert!(results.iter().all(|result| result.is_err()));
        let elapsed = time_provider.current_time_millis() - started;
        assert!(elapsed >= rpc_delay);
        assert!(elapsed < rpc_delay.const_mul(2), "concurrent delayed rpcs must overlap, took {}", elapsed);

        // And it can be switched off again
        factory.clear_rpc_delay();
        let started = time_provider.current_time_millis();
        assert!(factory.rpc("65000", Bytes::new()).await.is_err());
        assert!(time_provider.current_time_millis() - started < rpc_delay);
        Ok(())
    }
}
