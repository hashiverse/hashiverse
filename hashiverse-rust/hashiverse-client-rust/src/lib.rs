//! # `hashiverse-client-rust` — friendly Rust client wrapper
//!
//! Hashiverse is your open-source decentralized X/Twitter replacement. This crate
//! is the ergonomic Rust entry point: it picks sensible defaults for every
//! pluggable trait in [`hashiverse_lib`] (sqlite client storage, on-disk key
//! locker, DNSSEC-validated public bootstrap, native parallel PoW, real-time
//! clock, HTTPS transport) so you can spin up a working client in a few lines.
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! let hashiverse = hashiverse_client_rust::HashiverseBuilder::new()
//!     .build_with_keyphrase("correct horse battery staple")
//!     .await?;
//!
//! hashiverse.submit_post("<p>hello, hashiverse</p>").await?;
//! # Ok(()) }
//! ```
//!
//! If you need to override a default, use the builder methods
//! ([`HashiverseBuilder::data_dir`], [`HashiverseBuilder::passphrase`],
//! [`HashiverseBuilder::bootstrap`]). If you need full control, build a
//! [`hashiverse_lib::client::hashiverse_client::HashiverseClient`] directly
//! against [`hashiverse_lib`] — this crate is opinionated by design and
//! deliberately does not expose every knob.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use hashiverse_lib::client::args::Args;
use hashiverse_lib::client::client_storage::sqlite_client_storage::SqliteClientStorage;
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::disk_key_locker::{DiskKeyLocker, DiskKeyLockerManager};
use hashiverse_lib::client::key_locker::key_locker::KeyLockerManager;
use hashiverse_lib::tools::pow_generator::native_parallel_pow_generator::NativeParallelPowGenerator;
use hashiverse_lib::tools::runtime_services::RuntimeServices;
use hashiverse_lib::tools::time_provider::time_provider::RealTimeProvider;
use hashiverse_lib::transport::bootstrap_provider::bootstrap_provider::BootstrapProvider;
use hashiverse_lib::transport::bootstrap_provider::dnssec_bootstrap_provider::DnssecBootstrapProvider;
use hashiverse_lib::transport::bootstrap_provider::manual_bootstrap_provider::ManualBootstrapProvider;
use hashiverse_lib::transport::partial_https_transport::PartialHttpsTransportFactory;

pub use hashiverse_lib;

/// Builder for a [`Hashiverse`] client with sensible defaults for every
/// pluggable trait. See the crate-level docs for an example.
pub struct HashiverseBuilder {
    data_dir: PathBuf,
    passphrase: String,
    bootstrap_addresses: Option<Vec<String>>,
}

impl Default for HashiverseBuilder {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            passphrase: String::new(),
            bootstrap_addresses: None,
        }
    }
}

impl HashiverseBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Directory for the sqlite client storage and the on-disk key locker.
    /// Default: the platform data-dir (`dirs_next::data_dir()`) joined with
    /// `"hashiverse"`. Tildes in the supplied path are expanded.
    pub fn data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let expanded = shellexpand::tilde(&dir.to_string_lossy()).into_owned();
        self.data_dir = PathBuf::from(expanded);
        self
    }

    /// Passphrase used to encrypt the on-disk key locker. Empty by default.
    pub fn passphrase(mut self, passphrase: impl Into<String>) -> Self {
        self.passphrase = passphrase.into();
        self
    }

    /// Override the bootstrap peer list with a hand-curated set of
    /// `host:port` strings. Default: [`DnssecBootstrapProvider`], which
    /// resolves the public seed list via DoH with DNSSEC validation.
    pub fn bootstrap(mut self, addresses: Vec<String>) -> Self {
        self.bootstrap_addresses = Some(addresses);
        self
    }

    /// Build a client by deriving the identity from `key_phrase`. The derived
    /// key is stored in the on-disk locker so a subsequent run can pick it up
    /// via [`HashiverseBuilder::build_from_stored_key`].
    pub async fn build_with_keyphrase(self, key_phrase: impl Into<String>) -> Result<Hashiverse> {
        std::fs::create_dir_all(&self.data_dir)?;
        let key_locker_manager = DiskKeyLockerManager::with_data_dir(self.data_dir.clone(), self.passphrase.clone())?;
        let key_locker: Arc<DiskKeyLocker> = key_locker_manager.create(key_phrase.into()).await?;
        self.assemble(key_locker_manager, key_locker).await
    }

    /// Build a client by loading an identity previously stored in the on-disk
    /// key locker. `client_id_hex` selects which stored key to use.
    pub async fn build_from_stored_key(self, client_id_hex: impl Into<String>) -> Result<Hashiverse> {
        std::fs::create_dir_all(&self.data_dir)?;
        let key_locker_manager = DiskKeyLockerManager::with_data_dir(self.data_dir.clone(), self.passphrase.clone())?;
        let key_locker: Arc<DiskKeyLocker> = key_locker_manager.switch(client_id_hex.into()).await?;
        self.assemble(key_locker_manager, key_locker).await
    }

    async fn assemble(
        self,
        key_locker_manager: Arc<DiskKeyLockerManager>,
        key_locker: Arc<DiskKeyLocker>,
    ) -> Result<Hashiverse> {
        let client_storage_dir = self.data_dir.join("client_storage");
        std::fs::create_dir_all(&client_storage_dir)?;

        let time_provider = Arc::new(RealTimeProvider);
        let bootstrap_provider: Arc<dyn BootstrapProvider> = match self.bootstrap_addresses {
            Some(addresses) => ManualBootstrapProvider::new(addresses),
            None => Arc::new(DnssecBootstrapProvider::new()),
        };
        let transport_factory = Arc::new(PartialHttpsTransportFactory::new(bootstrap_provider));
        let pow_generator = Arc::new(NativeParallelPowGenerator::new());

        let runtime_services = Arc::new(RuntimeServices {
            time_provider,
            transport_factory,
            pow_generator,
        });

        let client_storage = SqliteClientStorage::new(client_storage_dir).await?;
        let hashiverse_client = HashiverseClient::new(
            runtime_services,
            client_storage,
            key_locker,
            Args::default(),
        )
        .await?;

        Ok(Hashiverse {
            client: Arc::new(hashiverse_client),
            _key_locker_manager: key_locker_manager,
        })
    }
}

/// A configured Hashiverse client. Deref into a
/// [`hashiverse_lib::client::hashiverse_client::HashiverseClient`] for the full API.
pub struct Hashiverse {
    client: Arc<HashiverseClient>,
    _key_locker_manager: Arc<DiskKeyLockerManager>,
}

impl Hashiverse {
    /// Direct access to the underlying `Arc<HashiverseClient>`. Use this when
    /// you need to share the client between tasks; for one-shot calls you can
    /// rely on the `Deref` impl and call methods on `&Hashiverse` directly.
    pub fn client(&self) -> &Arc<HashiverseClient> {
        &self.client
    }
}

impl std::ops::Deref for Hashiverse {
    type Target = HashiverseClient;
    fn deref(&self) -> &HashiverseClient {
        &self.client
    }
}

fn default_data_dir() -> PathBuf {
    dirs_next::data_dir().unwrap_or_else(std::env::temp_dir).join("hashiverse")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults_resolve_to_a_data_dir() {
        let builder = HashiverseBuilder::new();
        assert!(builder.data_dir.ends_with("hashiverse"), "default data dir should end in 'hashiverse': {:?}", builder.data_dir);
        assert!(builder.passphrase.is_empty());
        assert!(builder.bootstrap_addresses.is_none());
    }

    #[test]
    fn data_dir_tilde_expansion() {
        let builder = HashiverseBuilder::new().data_dir("~/hashiverse-test");
        let expanded = builder.data_dir.to_string_lossy();
        assert!(!expanded.starts_with("~"), "tilde should be expanded: {}", expanded);
    }
}
