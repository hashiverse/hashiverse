//! # Production HTTPS transport
//!
//! The server-binding half of the transport stack. Wraps
//! [`hashiverse_lib::transport::partial_https_transport::PartialHttpsTransportFactory`]
//! (which supplies the client-side outbound `rpc()` and the bootstrap lookup) and
//! adds everything needed to accept inbound TLS connections:
//!
//! - a `TcpListener` acquired lazily so the factory can live inside `Arc` before the
//!   port is actually bound,
//! - a rustls `TlsAcceptor` driven by
//!   [`crate::transport::https_transport_cert_refresher::HttpsTransportCertRefresher`]
//!   so certificates roll over without downtime,
//! - a `Semaphore` capping concurrent connections at
//!   [`hashiverse_lib::tools::config::HTTPS_SERVER_TRANSPORT_MAX_CONNECTIONS`] so a
//!   connection-exhaustion attack can't starve the OS of file descriptors,
//! - handshake / header-read / body-read timeouts for Slow Loris defence (values
//!   in [`hashiverse_lib::tools::config`]).
//!
//! Per-connection state flows through
//! [`hashiverse_lib::transport::ddos::ddos::DdosConnectionGuard`]s so per-IP
//! accounting happens automatically without each handler needing to remember it.

use crate::transport::https_transport_cert_refresher::HttpsTransportCertRefresher;
use crate::tools::tools::get_public_ipv4;
use anyhow::anyhow;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Extension};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Router};
use bytes::Bytes;
use futures::stream;
use hashiverse_lib::tools::config;
use hashiverse_lib::transport::ddos::ddos::{DdosConnectionGuard, DdosProtection};
use hashiverse_lib::transport::transport::{IncomingRequest, ServerState, TransportFactory, TransportServer};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use log::{error, info, trace, warn};
use parking_lot::RwLock;
use rustls::ServerConfig;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex, Semaphore};
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tokio_util::sync::CancellationToken;
use tower::{Service, ServiceExt};
use tower_http::cors::CorsLayer;
use tower_http::timeout::RequestBodyTimeoutLayer;
use hashiverse_lib::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;

/// Full HTTPS transport factory for server use.
///
/// Provides `rpc()`, `get_bootstrap_addresses()`, and `create_server()` with
/// TLS, DDoS protection, connection limits, and certificate management.
/// Delegates `rpc()` and `get_bootstrap_addresses()` to `PartialHttpsTransportFactory`
/// from `hashiverse-lib`.
#[derive(Clone)]
pub struct FullHttpsTransportFactory {
    ddos_protection: Arc<dyn DdosProtection>,
    https_transport_factory: hashiverse_lib::transport::partial_https_transport::PartialHttpsTransportFactory,
}

pub struct FullHttpsTransportServer {
    base_path: String,
    force_local_network: bool,
    address: String,
    ip: String,
    port: u16,
    listener: Arc<Mutex<Option<TcpListener>>>, // Needs Mutex to make Server Send, and needs Option because we give the TcpListener to axum
    state: Arc<RwLock<ServerState>>,
    ddos_protection: Arc<dyn DdosProtection>,
}

impl FullHttpsTransportServer {
    async fn new(base_path: &str, address: String, ip: String, port: u16, force_local_network: bool, listener: TcpListener, ddos_protection: Arc<dyn DdosProtection>) -> anyhow::Result<Self> {
        Ok(FullHttpsTransportServer {
            base_path: base_path.to_string(),
            force_local_network,
            address,
            ip,
            port,
            listener: Arc::new(Mutex::new(Some(listener))),
            state: Arc::new(RwLock::new(ServerState::Created)),
            ddos_protection
        })
    }
}

#[async_trait::async_trait]
impl TransportServer for FullHttpsTransportServer {
    fn get_address(&self) -> &String {
        &self.address
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

        info!("listening on address {}", self.address);

        let mut listener = self.listener.lock().await;
        let listener = match listener.take() {
            Some(listener) => listener,
            None => {
                return Err(anyhow!("listener had already been taken"));
            }
        };

        // The guard is injected per-connection in the accept loop (see below) and extracted here.
        // This replaces the old pattern of capturing Arc<dyn DdosProtection> directly.
        let handler_clone = handler.clone();
        let handle_blob = move |Extension(ddos_connection_guard): Extension<Arc<DdosConnectionGuard>>, bytes: Bytes| async move {
            let handler = handler_clone.clone();

            if !ddos_connection_guard.allow_request() {
                trace!("DDoS: request from {} blocked", ddos_connection_guard.ip());
                return Err(StatusCode::TOO_MANY_REQUESTS);
            }

            let caller_address = ddos_connection_guard.ip().to_string();

            let result: anyhow::Result<Response<axum::body::Body>> = try {
                let (reply_tx, reply_rx) = oneshot::channel();
                handler.send(IncomingRequest::new(caller_address, bytes, reply_tx, ddos_connection_guard.clone())).await.map_err(|e| anyhow::anyhow!("Failed to send message: {}", e))?;
                let response = reply_rx.await.map_err(|e| anyhow::anyhow!("Failed to receive message: {}", e))?;

                // Collapse the smaller multi-part byte sequences and stream the chunks
                let content_length = response.len();
                let segments = response.compact(config::TRANSPORT_BYTES_GATHERER_COMPACT_THRESHOLD).finish();
                let body = axum::body::Body::from_stream(stream::iter(segments.into_iter().map(Ok::<Bytes, Infallible>)));

                let response = axum::http::Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(header::CONTENT_LENGTH, content_length)
                    .body(body)
                    .map_err(|e| anyhow::anyhow!("Failed to build response: {}", e))?;

                response
            };

            match result {
                Ok(response) => Ok(response.into_response()),

                Err(e) => {
                    warn!("error processing blob: {}", e);
                    ddos_connection_guard.report_bad_request();
                    Err(StatusCode::BAD_REQUEST)
                }
            }
        };

        let fallback_handler = move |Extension(ddos_connection_guard): Extension<Arc<DdosConnectionGuard>>, uri: Uri| {
            async move {
                trace!("unhandled route for path: {} from {}", uri, ddos_connection_guard.ip());
                ddos_connection_guard.report_bad_request();
                StatusCode::NOT_FOUND
            }
        };

        let axum_app = Router::new()
            .route("/", get(|| async { "Hashiverse!" }).post(handle_blob))
            .layer(DefaultBodyLimit::max(config::PROTOCOL_MAX_BLOB_SIZE_REQUEST))
            .layer(RequestBodyTimeoutLayer::new(Duration::from_secs(config::HTTPS_SERVER_TRANSPORT_BODY_READ_TIMEOUT_SECS)))
            .layer(CorsLayer::permissive())
            .fallback(fallback_handler);

        let path_certs = PathBuf::from(self.base_path.clone()).join("certs");
        let cert_refresher = Arc::new(HttpsTransportCertRefresher::new(path_certs.clone(), self.ip.clone(), self.port, self.force_local_network)?);
        cert_refresher.reload_certs()?;

        let tls_acceptor = {
            let mut server_config = ServerConfig::builder().with_no_client_auth().with_cert_resolver(cert_refresher.clone());
            server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec(), b"acme-tls/1".to_vec()];
            TlsAcceptor::from(Arc::new(server_config))
        };

        let mut make_service = axum_app.into_make_service_with_connect_info::<std::net::SocketAddr>();
        let connection_semaphore = Arc::new(Semaphore::new(config::HTTPS_SERVER_TRANSPORT_MAX_CONNECTIONS));
        let mut join_set: JoinSet<()> = JoinSet::new();

        let ddos = self.ddos_protection.clone();

        let accept_loop = async {
            loop {
                // Reap completed connection tasks without blocking
                while join_set.try_join_next().is_some() {}

                tokio::select! {
                    accept_result = listener.accept() => {
                        let (tcp_stream, peer_addr) = match accept_result {
                            Ok(v) => v,
                            Err(e) => { warn!("accept error: {}", e); continue; }
                        };
                        let ip = peer_addr.ip().to_string();

                        // Create the per-connection guard — checks per-IP ban score and per-IP connection cap.
                        // Fires before the TLS handshake so blocked/over-cap IPs consume no handshake resources.
                        // The guard is held for the full TCP connection lifetime; dropping it decrements the slot.
                        let ddos_connection_guard = match DdosConnectionGuard::try_new(ddos.clone(), ip.clone()) {
                            Some(guard) => Arc::new(guard),
                            None => {
                                trace!("DDoS: dropping connection from {} (blocked or per-IP cap reached)", ip);
                                continue;
                            }
                        };

                        // Hard connection cap — prevents file-descriptor exhaustion.
                        let permit = match Arc::clone(&connection_semaphore).try_acquire_owned() {
                            Ok(p) => p,
                            Err(_) => {
                                warn!("connection cap ({}) reached, dropping {}", config::HTTPS_SERVER_TRANSPORT_MAX_CONNECTIONS, ip);
                                continue;
                            }
                        };

                        // Pre-create the per-connection axum service (injects ConnectInfo).
                        // IntoMakeServiceWithConnectInfo::call is synchronous (returns future::ok),
                        // so this is effectively free.
                        let tower_service = match make_service.call(peer_addr).await {
                            Ok(s) => s,
                            Err(e) => { warn!("make_service error for {}: {:?}", ip, e); continue; }
                        };

                        let tls_acceptor = tls_acceptor.clone();

                        join_set.spawn(async move {
                            let _permit = permit; // released when the connection closes

                            // TLS handshake with timeout — shuts out ClientHello floods and
                            // TLS-layer slow connections.
                            let tls_stream = match tokio::time::timeout(
                                Duration::from_secs(config::HTTPS_SERVER_TRANSPORT_TLS_HANDSHAKE_TIMEOUT_SECS),
                                tls_acceptor.accept(tcp_stream),
                            ).await {
                                Ok(Ok(s))  => s,
                                Ok(Err(e)) => { trace!("TLS error from {}: {}", ip, e); ddos_connection_guard.report_bad_request(); return; }
                                Err(_)     => { trace!("TLS handshake timeout from {}", ip); ddos_connection_guard.report_bad_request(); return; }
                            };

                            let io = TokioIo::new(tls_stream);

                            // Inject the connection guard into every request on this connection.
                            // Handlers extract it via Extension<Arc<DdosConnectionGuard>> rather than
                            // holding a raw Arc<dyn DdosProtection>.  The guard also keeps the per-IP
                            // connection slot alive until the connection closes.
                            let hyper_service = hyper::service::service_fn(move |mut req: hyper::Request<Incoming>| {
                                req.extensions_mut().insert(ddos_connection_guard.clone());
                                tower_service.clone().oneshot(req.map(Body::new))
                            });

                            // http1_header_read_timeout is the core Slow Loris defence: if the
                            // client takes longer than N seconds to send complete HTTP/1.1 headers
                            // the connection is dropped.
                            let mut auto_builder = AutoBuilder::new(TokioExecutor::new());
                            auto_builder.http1()
                                .timer(TokioTimer::new())
                                .header_read_timeout(Duration::from_secs(config::HTTPS_SERVER_TRANSPORT_HEADER_READ_TIMEOUT_SECS));

                            if let Err(e) = auto_builder.serve_connection(io, hyper_service).await {
                                trace!("connection error from {}: {}", ip, e);
                            }
                        });
                    }
                    _ = cancellation_token.cancelled() => break,
                }
            }

            // Graceful shutdown: wait up to the configured timeout for in-flight connections, then abort stragglers.
            let shutdown_deadline = tokio::time::sleep(Duration::from_secs(config::HTTPS_SERVER_TRANSPORT_SHUTDOWN_TIMEOUT_SECS));
            tokio::pin!(shutdown_deadline);
            loop {
                tokio::select! {
                    result = join_set.join_next() => {
                        match result {
                            None => break,
                            Some(Err(e)) => warn!("connection task error during shutdown: {}", e),
                            Some(Ok(())) => {}
                        }
                    }
                    _ = &mut shutdown_deadline => {
                        join_set.abort_all();
                        break;
                    }
                }
            }

            anyhow::Ok(())
        };

        // Run the accept loop and certificate refresher concurrently
        let results = tokio::join!(
            accept_loop,
            cert_refresher.process(cancellation_token.clone()),
        );

        if let Err(e) = results.0 {
            error!("error in accept loop: {}", e)
        }
        if let Err(e) = results.1 {
            error!("error in cert refresher: {}", e)
        }

        info!("stopped listening on address {}", self.address);
        info!("all open connections complete");
        *self.state.write() = ServerState::Shutdown;

        Ok(())
    }
}

impl FullHttpsTransportFactory {
    pub fn new(ddos_protection: Arc<dyn DdosProtection>, bootstrap_provider: Arc<dyn BootstrapProvider>) -> Self {
        let https_transport_factory = hashiverse_lib::transport::partial_https_transport::PartialHttpsTransportFactory::new(bootstrap_provider);
        Self { ddos_protection, https_transport_factory }
    }
}

#[async_trait::async_trait]
impl TransportFactory for FullHttpsTransportFactory {
    async fn get_bootstrap_addresses(&self) -> Vec<String> {
        self.https_transport_factory.get_bootstrap_addresses().await
    }

    async fn create_server(&self, base_path: &str, port: u16, force_local_network: bool) -> anyhow::Result<Arc<dyn TransportServer>> {
        // Deliberately IPv4-only.  IPv6 per-IP DDoS limiting is ineffective without
        // prefix-level tracking (/64) because attackers can trivially cycle through an
        // entire /64 allocation.  Add IPv6 support only alongside prefix normalisation
        // in DdosConnectionGuard and hash:net support in IpsetDdosProtection.
        let address_to_bind = format!("0.0.0.0:{}", port);
        info!("bind on: {}", address_to_bind);
        let listener = TcpListener::bind(address_to_bind).await?;

        let address_bound_ip = get_public_ipv4(force_local_network).await?;
        let address_bound_port = listener.local_addr()?.port();
        let address = format!("{}:{}", address_bound_ip, address_bound_port);

        let http_transport_server: Arc<dyn TransportServer> = Arc::new(FullHttpsTransportServer::new(base_path, address, address_bound_ip, address_bound_port, force_local_network, listener, self.ddos_protection.clone()).await?);
        Ok(http_transport_server)
    }

    async fn rpc(&self, address: &str, bytes: Bytes) -> anyhow::Result<Bytes> {
        self.https_transport_factory.rpc(address, bytes).await
    }
}


#[cfg(test)]
mod tests {
    use crate::transport::full_https_transport::FullHttpsTransportFactory;
    use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
    use hashiverse_lib::transport::ddos::noop_ddos::NoopDdosProtection;
    use hashiverse_lib::transport::transport::TransportFactory;
    use std::sync::Arc;

    #[tokio::test]
    async fn rpc_test() -> anyhow::Result<()> {
        let factory: Arc<dyn TransportFactory> = Arc::new(FullHttpsTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::default()));
        hashiverse_lib::transport::transport::tests::rpc_test(factory).await
    }

    #[tokio::test]
    async fn bind_port_zero_test() -> anyhow::Result<()> {
        let factory: Arc<dyn TransportFactory> = Arc::new(FullHttpsTransportFactory::new(NoopDdosProtection::default(), ManualBootstrapProvider::default()));
        hashiverse_lib::transport::transport::tests::bind_port_zero_test(factory).await
    }
}
