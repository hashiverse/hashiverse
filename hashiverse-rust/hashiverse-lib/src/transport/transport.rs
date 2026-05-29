//! # Transport abstraction — the seam between protocol and network
//!
//! Three traits define everything the protocol code needs from a transport, with
//! concrete implementations (HTTPS in production, in-memory for tests) supplied by
//! sibling modules:
//!
//! - [`TransportFactory`] — builds a [`TransportServer`] and issues outbound client
//!   requests. Injected via [`crate::tools::runtime_services::RuntimeServices`] so the
//!   same protocol code runs over HTTPS on native, over HTTP via `gloo-net` in WASM,
//!   and over a deterministic in-memory queue in tests.
//! - [`TransportServer`] — binds an address, delivers inbound requests over an
//!   `mpsc::Receiver<IncomingRequest>`, and shuts down on a `CancellationToken`.
//! - [`TransportServerHandler`] — the application-level processor of incoming requests,
//!   implemented by the server binary.
//!
//! [`IncomingRequest`] wraps one inbound request: the raw bytes, the caller's address,
//! a oneshot response channel, and a
//! [`crate::transport::ddos::ddos::DdosConnectionGuard`] whose drop releases the
//! caller's per-IP connection slot — so request-level DDoS accounting is automatic and
//! can't be leaked.

use crate::tools::BytesGatherer;
use crate::transport::ddos::ddos::DdosConnectionGuard;
use crate::transport::transport_ownership_proof::{RejectAllTransportOwnershipProof, TransportOwnershipProof};
use bytes::Bytes;
use log::{info, trace, warn};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// A single request delivered from a [`TransportServer`] to the
/// application-level [`TransportServerHandler`] for processing.
///
/// The transport layer owns inbound sockets, parses wire-level framing, and forwards the
/// decoded payload to the handler as an `IncomingRequest`. It carries the raw request bytes
/// plus the metadata the handler may need to reason about: the caller's address (for logging
/// and DDoS accounting), a oneshot channel on which to send the response, and an
/// [`DdosConnectionGuard`] that ties the request's lifetime to its underlying connection slot
/// — dropping the guard releases the per-IP connection count.
///
/// Handlers do not construct `IncomingRequest` directly; they `.await` them off the
/// `mpsc::Receiver` passed to [`TransportServerHandler::run`].
pub struct IncomingRequest {
    pub caller_address: String,
    pub bytes: Bytes,
    pub reply: oneshot::Sender<BytesGatherer>,
    ddos_connection_guard: Arc<DdosConnectionGuard>,
}

impl IncomingRequest {
    pub fn new(caller_address: String, bytes: Bytes, reply: oneshot::Sender<BytesGatherer>, ddos_connection_guard: Arc<DdosConnectionGuard>) -> Self {
        Self { caller_address, bytes, reply, ddos_connection_guard }
    }

    pub fn report_bad_request(&self) {
        self.ddos_connection_guard.report_bad_request();
    }
}

/// The lifecycle state of a [`TransportServer`].
///
/// A server starts in `Created`, transitions to `Listening` when [`TransportServer::listen`]
/// is called for the first time, and moves to `Shutdown` when its cancellation token fires.
/// Implementations use this to reject double-start (`Listening` → `Listening`) and
/// use-after-shutdown (`Shutdown` → anything) errors cleanly rather than silently racing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ServerState {
    Created,
    Listening,
    Shutdown,
}

/// The application-level request handler that sits behind a [`TransportServer`].
///
/// Implementors receive decoded request bytes and produce response bytes without caring
/// about sockets, framing, or connection lifecycle — all of that stays in the transport
/// layer. The default [`TransportServerHandler::run`] implementation is the canonical event
/// loop: it reads [`IncomingRequest`]s off a channel, dispatches each one to
/// [`TransportServerHandler::handle`], and ships the response back to the originating client.
/// Shutdown is cooperative via a [`CancellationToken`].
///
/// The server binary's RPC dispatcher and the client's own loopback handler both implement
/// this trait, which lets them share the same event-loop plumbing.
pub trait TransportServerHandler {
    async fn handle(&self, bytes: Bytes) -> BytesGatherer;

    async fn run(&self, cancellation_token: CancellationToken, mut rx: mpsc::Receiver<IncomingRequest>) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                _ = cancellation_token.cancelled() => {
                    break;
                },

                receipt = rx.recv() => {
                    match receipt {
                            Some(incoming) => {
                                info!("received packet from {}: {:?}", incoming.caller_address, incoming.bytes);
                                let result = self.handle(incoming.bytes.clone()).await;
                                let result = incoming.reply.send(result);
                                match result {
                                    Ok(_) => { trace!("sent reply"); },
                                    Err(_) => { warn!("failed to send reply"); },
                                }
                            },
                            None => {
                                warn!("channel closed");
                                break;
                            }
                        }
                },
            }
        }

        Ok(())
    }
}

/// A server-side endpoint that accepts inbound connections and forwards each request to a
/// handler via an mpsc channel.
///
/// `TransportServer` abstracts the concrete listening strategy — `MemTransportServer` for
/// in-memory tests, the HTTPS implementation in the server crate for production, a browser
/// stub in the wasm client that panics on `listen`. All of them expose the same two-operation
/// surface: report where they can be reached (`get_address`) and run the accept loop until
/// the supplied `CancellationToken` fires (`listen`).
///
/// A `TransportServer` instance is single-shot — once `listen` has completed (cancelled or
/// errored) the server transitions to [`ServerState::Shutdown`] and must not be re-used.
#[async_trait::async_trait]
pub trait TransportServer: Send + Sync {
    fn get_address(&self) -> &String;

    async fn listen(&self, cancellation_token: CancellationToken, handler: mpsc::Sender<IncomingRequest>) -> anyhow::Result<()>;

    /// Returns the per-transport ownership-proof object used to (a) produce the proof bytes
    /// embedded in this server's outbound `AnnounceV2` requests and (b) verify proof bytes
    /// received from peers in inbound `AnnounceV2` requests. The default reject-all
    /// implementation is overridden by transports that have a real notion of address
    /// ownership (HTTPS via ACME cert, mem-transport via an empty marker, …).
    fn get_transport_ownership_proof(&self) -> Arc<dyn TransportOwnershipProof> {
        Arc::new(RejectAllTransportOwnershipProof)
    }
}

/// The pluggable network layer of the protocol — the single point where the crate touches
/// "how do we move bytes around the world".
///
/// A `TransportFactory` knows three things: (1) where to find the network's bootstrap peers
/// for initial peer discovery, (2) how to create a [`TransportServer`] that listens on a given
/// port, and (3) how to perform an outbound unary RPC against a peer address. Everything
/// above this layer — the RPC packet framing, PoW, peer tracking, Kademlia — is network-
/// agnostic and simply calls [`TransportFactory::rpc`].
///
/// Concrete implementations include the in-memory `MemTransportFactory` used by integration
/// tests, `FullHttpsTransportFactory` in the server crate for production TLS+HTTPS, and
/// `WasmTransportFactory` in the browser client that speaks HTTP via `gloo-net`. Swapping
/// the factory on [`crate::tools::runtime_services::RuntimeServices`] changes the wire protocol
/// without any other code having to care.
#[async_trait::async_trait]
pub trait TransportFactory: Send + Sync {
    async fn get_bootstrap_addresses(&self) -> Vec<String>;
    async fn create_server(&self, base_path: &str, port: u16, force_local_network: bool) -> anyhow::Result<Arc<dyn TransportServer>>;
    async fn rpc(&self, address: &str, bytes: Bytes) -> anyhow::Result<Bytes>;
}

#[cfg(any(test, feature = "generic-tests"))]
pub mod tests {
    use crate::tools::time::{MILLIS_IN_MILLISECOND, MILLIS_IN_SECOND};
    use crate::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};
    use crate::tools::tools::get_temp_dir;
    use crate::tools::BytesGatherer;
    use crate::transport::transport::{IncomingRequest, TransportFactory, TransportServerHandler};
    use bytes::Bytes;
    use log::{info, trace};
    use std::sync::Arc;
    use tokio::join;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    pub async fn rpc_test(transport_factory: Arc<dyn TransportFactory>) -> anyhow::Result<()> {
        let time_provider = Arc::new(RealTimeProvider);
        // configure_logging_with_time_provider("trace", time_provider.clone());

        let cancellation_token = CancellationToken::new();
        let (_, temp_dir_str) = get_temp_dir()?;
        let transport_server = transport_factory.create_server(&temp_dir_str, 0u16, true).await?;
        let address = transport_server.get_address().clone();
        trace!("server address is {}", address);

        let (tx, rx) = mpsc::channel::<IncomingRequest>(32);

        struct MyHandler {}
        impl TransportServerHandler for MyHandler {
            async fn handle(&self, _: Bytes) -> BytesGatherer {
                BytesGatherer::from_bytes(Bytes::from("here is the reply"))
            }
        }

        let my_handler = MyHandler {};

        info!("running server and clients in parallel");
        let results = join!(
            my_handler.run(cancellation_token.clone(), rx),
            transport_server.listen(cancellation_token.clone(), tx),
            // The driver process
            async {
                info!("waiting for server to start");
                time_provider.sleep_millis(MILLIS_IN_SECOND.const_mul(1)).await;

                for _ in 0..20 {
                    info!("calling server");
                    let bytes = Bytes::from("hello");
                    let response = transport_factory.rpc(&address, bytes).await.unwrap();
                    assert_eq!(response, Bytes::from("here is the reply"));
                    time_provider.sleep_millis(MILLIS_IN_MILLISECOND.const_mul(100)).await;
                }

                info!("shutting down servers");
                cancellation_token.cancel();
                time_provider.sleep_millis(MILLIS_IN_SECOND.const_mul(1)).await;

                Ok::<(), anyhow::Error>(())
            }
        );

        // Check all the processes exited happily
        assert!(results.0.is_ok());
        assert!(results.1.is_ok());
        assert!(results.2.is_ok());

        Ok(())
    }

    pub async fn bind_port_zero_test(transport_factory: Arc<dyn TransportFactory>) -> anyhow::Result<()> {
        let time_provider = Arc::new(RealTimeProvider);
        // configure_logging_with_time_provider("trace", time_provider.clone());

        info!("starting test");

        let cancellation_token = CancellationToken::new();
        let (_, temp_dir_str) = get_temp_dir()?;
        let transport_server_1 = transport_factory.create_server(&temp_dir_str, 0u16, true).await?;
        let transport_server_2 = transport_factory.create_server(&temp_dir_str, 0u16, true).await?;

        let (tx_1, rx_1) = mpsc::channel::<IncomingRequest>(32);
        let (tx_2, rx_2) = mpsc::channel::<IncomingRequest>(32);

        struct MyHandler {}
        impl TransportServerHandler for MyHandler {
            async fn handle(&self, _: Bytes) -> BytesGatherer {
                BytesGatherer::from_bytes(Bytes::from("here is the reply"))
            }
        }

        let my_handler = MyHandler {};

        info!("running server and clients in parallel");
        let results = join!(
            my_handler.run(cancellation_token.clone(), rx_1),
            my_handler.run(cancellation_token.clone(), rx_2),
            transport_server_1.listen(cancellation_token.clone(), tx_1),
            transport_server_2.listen(cancellation_token.clone(), tx_2),
            // The driver process
            async {
                info!("waiting for server to start");
                time_provider.sleep_millis(MILLIS_IN_SECOND.const_mul(1)).await;

                info!("shutting down servers");
                cancellation_token.cancel();
                time_provider.sleep_millis(MILLIS_IN_SECOND.const_mul(1)).await;

                Ok::<(), anyhow::Error>(())
            }
        );

        // Check all the processes exited happily
        assert!(results.0.is_ok());
        assert!(results.1.is_ok());
        assert!(results.2.is_ok());
        assert!(results.3.is_ok());
        assert!(results.4.is_ok());

        Ok(())
    }
}
