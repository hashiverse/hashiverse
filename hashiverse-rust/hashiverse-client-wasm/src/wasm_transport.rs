use crate::wasm_bootstrap_provider::WasmBootstrapProvider;
use anyhow::anyhow;
use bytes::Bytes;
use gloo_net::http::Request;
use hashiverse_lib::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;
use hashiverse_lib::transport::ddos::ddos::DdosProtection;
use hashiverse_lib::transport::ddos::noop_ddos::NoopDdosProtection;
use hashiverse_lib::transport::transport::{IncomingRequest, TransportFactory, TransportServer};
use send_wrapper::SendWrapper;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct WasmTransportFactory {
    bootstrap_provider: Arc<dyn BootstrapProvider>,
}

impl WasmTransportFactory {
    pub fn new(_ddos_protection: Arc<dyn DdosProtection>, bootstrap_provider: Arc<dyn BootstrapProvider>) -> Self {
        WasmTransportFactory { bootstrap_provider }
    }
}

impl Default for WasmTransportFactory {
    fn default() -> Self {
        Self::new(NoopDdosProtection::default(), Arc::new(WasmBootstrapProvider::new()))
    }
}

pub struct WasmTransportServer {}

#[async_trait::async_trait]
impl TransportServer for WasmTransportServer {
    fn get_address(&self) -> &String {
        panic!("not implemented");
    }

    async fn listen(&self, _: CancellationToken, _: tokio::sync::mpsc::Sender<IncomingRequest>) -> anyhow::Result<()> {
        panic!("not implemented");
    }
}

#[async_trait::async_trait]
impl TransportFactory for WasmTransportFactory {
    async fn get_bootstrap_addresses(&self) -> Vec<String> {
        self.bootstrap_provider.get_bootstrap_addresses().await
    }

    async fn create_server(&self, _: &str, _: u16, _: bool) -> anyhow::Result<Arc<dyn TransportServer>> {
        panic!("not implemented");
    }

    async fn rpc(&self, address: &str, bytes: Bytes) -> anyhow::Result<Bytes> {
        let address = format!("https://{}/", address);

        let response_bytes = async {
            let bytes_uint8array = js_sys::Uint8Array::from(bytes.as_ref());
            let request = Request::post(&address).header("Accept", "application/octet-stream").body(bytes_uint8array)?;

            let response = request.send().await.map_err(|e| anyhow!("Fetch error: {}", e))?;

            let response_bytes = response.binary().await.map_err(|e| anyhow!("Failed to read response: {}", e))?;

            Ok::<Vec<u8>, anyhow::Error>(response_bytes)
        };

        let response_bytes = SendWrapper::new(response_bytes).await?;

        Ok(Bytes::from(response_bytes))
    }
}
