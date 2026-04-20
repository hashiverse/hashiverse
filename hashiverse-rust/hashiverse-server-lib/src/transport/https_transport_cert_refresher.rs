//! # Let's Encrypt TLS certificate lifecycle manager
//!
//! Keeps the HTTPS transport's TLS certificate fresh without operator intervention.
//! Uses `instant-acme` to talk to Let's Encrypt (production or staging, picked via
//! [`hashiverse_lib::tools::config::USE_PRODUCTION_LETS_ENCRYPT`]) and
//! TLS-ALPN-01 to solve domain-validation challenges inline on the same HTTPS port
//! — no separate HTTP-01 listener needed.
//!
//! Two cert slots live side-by-side in `RwLock`s:
//! - `base_cert` — the currently-serving cert.
//! - `challenge_cert` — the short-lived self-signed (via `rcgen`) cert rustls serves
//!   only when ACME is mid-challenge.
//!
//! Swapping slots is atomic, so a refresh never drops a live TLS handshake.
//! Refresh cadence, retry-on-failure cadence, and renewal lead time all come from
//! the `MILLIS_TO_WAIT_BETWEEN_CERT_*` constants in
//! [`hashiverse_lib::tools::config`].

use anyhow::Context;
use hashiverse_lib::tools::config::USE_PRODUCTION_LETS_ENCRYPT;
use std::path::Path;
use hashiverse_lib::tools::time::{TimeMillis};
use hashiverse_lib::tools::{config, tools};
use instant_acme::{Account, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt, NewAccount, NewOrder, OrderStatus, RetryPolicy};
use log::{info, trace, warn};
use parking_lot::RwLock;
use rcgen::{CertificateParams, SanType, generate_simple_self_signed};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use hashiverse_lib::tools::time_provider::time_provider::{RealTimeProvider, TimeProvider};

pub const FILENAME_LAST_REFRESHED: &str = "last_refreshed";
pub const FILENAME_CERT: &str = "cert.pem";
pub const FILENAME_KEY: &str = "key.pem";

#[derive(Debug)]
pub struct HttpsTransportCertRefresher {
    force_local_network: bool,
    path_certs: PathBuf,
    filename_cert: PathBuf,
    filename_key: PathBuf,
    filename_last_refreshed: PathBuf,

    ip: String,
    port: u16,

    // The "real" certificate for your IP
    pub base_cert: Arc<RwLock<Option<Arc<CertifiedKey>>>>,

    // The temporary certificate used for the ACME challenge
    pub challenge_cert: Arc<RwLock<Option<Arc<CertifiedKey>>>>,
}

impl HttpsTransportCertRefresher {
    pub async fn refresh_cert(&self) -> anyhow::Result<()> {
        trace!("refreshing certificate");

        let directory_url = match USE_PRODUCTION_LETS_ENCRYPT {
            true => LetsEncrypt::Production.url().to_owned(),
            false => LetsEncrypt::Staging.url().to_owned(),
        };

        let (account, _credentials) = Account::builder()?
            .create(
                &NewAccount {
                    contact: &[],
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                directory_url,
                None,
            )
            .await?;

        // info!("letsencrypt.credentials: {}", serde_json::to_string_pretty(&credentials)?);

        let ip_addr = IpAddr::from_str(&self.ip)?;
        let identifiers = [Identifier::Ip(ip_addr)];
        let new_order = NewOrder::new(&identifiers).profile("shortlived");

        let mut order = account.new_order(&new_order).await?;
        let mut challenge_url = "".to_string();
        // info!("order state: {:#?}", order.state());

        // Fulfil any of the TlsAlpn01 authorizations we can (there should be only one)
        {
            let mut authorizations = order.authorizations();
            while let Some(result) = authorizations.next().await {
                let mut authz = result?;
                match authz.status {
                    AuthorizationStatus::Valid => continue,
                    AuthorizationStatus::Pending => {}
                    _ => {
                        warn!("unexpected AuthorizationStatus {:?}", authz.status);
                        continue;
                    }
                }

                let mut challenge = authz.challenge(ChallengeType::TlsAlpn01).ok_or_else(|| anyhow::anyhow!("no tlsalpn01 challenge found"))?;
                challenge_url = challenge.url.clone();

                // Create the temporary challenge cert
                {
                    let key_auth_sha256 = challenge.key_authorization().digest();

                    let mut certificate_params = CertificateParams::default();
                    certificate_params.subject_alt_names = vec![SanType::IpAddress(ip_addr)];
                    certificate_params.custom_extensions.push(rcgen::CustomExtension::new_acme_identifier(key_auth_sha256.as_ref()));

                    let key_pair = rcgen::KeyPair::generate()?;
                    let cert = certificate_params.self_signed(&key_pair)?;

                    let cert_chain = vec![rustls_pki_types::CertificateDer::from(cert.der().to_vec())];
                    let key_der = rustls_pki_types::PrivatePkcs8KeyDer::from(key_pair.serialize_der());
                    let key = rustls::crypto::ring::sign::any_supported_type(&rustls_pki_types::PrivateKeyDer::Pkcs8(key_der)).map_err(|_| anyhow::anyhow!("Unsupported key type"))?;

                    let certified_key = CertifiedKey { cert: cert_chain, key, ocsp: None };

                    *self.challenge_cert.write() = Some(Arc::new(certified_key));
                }

                // Tell letsencrypt that our challenge handler is ready for them
                challenge.set_ready().await?;
            }
        }

        //
        trace!("challenge.url: {}", challenge_url);

        // Wait for our request to be validated
        let status = order.poll_ready(&RetryPolicy::default()).await?;
        if status != OrderStatus::Ready {
            anyhow::bail!("unexpected order status: {status:?}");
        }

        // Get the certificate
        let private_key_pem = order.finalize().await?;
        let cert_chain_pem = order.poll_certificate(&RetryPolicy::default()).await?;

        // Write to disk
        info!("writing new certificate to disk");
        fs::create_dir_all(&self.path_certs)?;
        fs::write(&self.filename_cert, cert_chain_pem)?;
        write_private_key_file(&self.filename_key, private_key_pem.as_bytes())?;
        fs::write(&self.filename_last_refreshed, challenge_url)?;

        info!("refreshed certificate");

        Ok(())
    }

    pub fn reload_certs(&self) -> anyhow::Result<()> {
        trace!("reloading certificate");

        let certified_key: CertifiedKey = {
            let bytes_cert = fs::read(&self.filename_cert)?;
            let bytes_key = fs::read(&self.filename_key)?;

            // Return the CertifiedKey
            let cert_chain = rustls_pemfile::certs(&mut &bytes_cert[..]).collect::<Result<Vec<_>, _>>()?;
            let key_der = rustls_pemfile::private_key(&mut &bytes_key[..])?.ok_or_else(|| anyhow::anyhow!("No private key found in {}", self.filename_key.display()))?;
            let key = rustls::crypto::ring::sign::any_supported_type(&key_der).map_err(|_| anyhow::anyhow!("Unsupported key type"))?;

            CertifiedKey { cert: cert_chain, key, ocsp: None }
        };

        *self.base_cert.write() = Some(Arc::new(certified_key));

        Ok(())
    }

    fn certs_last_refreshed(&self) -> anyhow::Result<TimeMillis> {
        let certs_last_refreshed = fs::metadata(&self.filename_last_refreshed)
            .map(|metadata| metadata.modified())
            .unwrap_or_else(|_| Ok(std::time::SystemTime::UNIX_EPOCH))
            .with_context(|| "checking last refreshed filename")?;

        let certs_last_refreshed: TimeMillis = certs_last_refreshed.into();

        Ok(certs_last_refreshed)
    }

    pub async fn process(&self, cancellation_token: CancellationToken) -> anyhow::Result<()> {
        let time_provider = RealTimeProvider;

        let mut certs_last_attempted = TimeMillis::zero();

        loop {
            if cancellation_token.is_cancelled() {
                break;
            }

            let result: anyhow::Result<()> = try {
                // Do nothing if we are forced local
                if !self.force_local_network {
                    // If we are in control of port 443, we can do the actual cert renewal from letsencrypt
                    if 443u16 == self.port {
                        // And do nothing if we have tried too recently
                        let now_millis = time_provider.current_time_millis();
                        if (now_millis - self.certs_last_refreshed()?) > config::MILLIS_TO_WAIT_BETWEEN_CERT_RENEWALS {
                            if (now_millis - certs_last_attempted) > config::MILLIS_TO_WAIT_BETWEEN_CERT_RENEWAL_FAILURES {
                                certs_last_attempted = now_millis;
                                self.refresh_cert().await?;
                            }
                            else {
                                trace!("we refreshed certs too recently to try again");
                            }
                        }
                        else {
                            trace!("we have a recent enough cert on disk");
                        }
                    }
                    else {
                        trace!("skipping cert refresh because port {} != 443", self.port);
                    }
                }
                else {
                    trace!("skipping cert refresh because force_local_network");
                }

                // Reload our certs from disk
                self.reload_certs()?;
            };

            if let Err(e) = result {
                warn!("error while refreshing certs: {}", e);
            }

            tools::cancellable_sleep_millis(&time_provider, config::MILLIS_TO_WAIT_BETWEEN_CERT_RENEWAL_CHECKS, &cancellation_token).await;
        }

        trace!("stopped HttpsTransportCertRefresher");

        Ok(())
    }
}

/// Write a private key file with owner-read-only permissions (0600) on Unix.
/// Falls back to a plain write on non-Unix platforms.
fn write_private_key_file(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents)?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents)?;
    }
    Ok(())
}

impl HttpsTransportCertRefresher {
    pub fn new(path_certs: PathBuf, ip: String, port: u16, force_local_network: bool) -> anyhow::Result<Self> {
        let filename_cert = path_certs.join(FILENAME_CERT);
        let filename_key = path_certs.join(FILENAME_KEY);
        let filename_last_refreshed = path_certs.join(FILENAME_LAST_REFRESHED);

        // Make sure we start with at least some baseline certificates.
        // Self-signed certs are generated whenever none exist on disk — including force_local_network
        // (test/local) mode.  For production, Let's Encrypt replaces them via process().
        if !fs::exists(&filename_cert)? || !fs::exists(&filename_key)? {
            info!("generating self-signed certs");
            let subject_alt_names = vec![ip.clone()];

            let certified_key = generate_simple_self_signed(subject_alt_names)?;
            fs::create_dir_all(&path_certs)?;
            fs::write(&filename_cert, certified_key.cert.pem())?;
            write_private_key_file(&filename_key, certified_key.signing_key.serialize_pem().as_bytes())?;
        }

        // Initially our certs are empty
        let base_cert = Arc::new(RwLock::new(None));
        let challenge_cert = Arc::new(RwLock::new(None));

        Ok(Self {
            force_local_network,
            path_certs,
            filename_cert,
            filename_key,
            filename_last_refreshed,
            ip,
            port,
            base_cert,
            challenge_cert,
        })
    }
}

impl ResolvesServerCert for HttpsTransportCertRefresher {
    fn resolve(&self, client_hello: ClientHello) -> Option<Arc<CertifiedKey>> {
        // Check if the client (the CA) is specifically asking for the ACME protocol
        let is_acme = client_hello.alpn().map(|mut iter| iter.any(|proto| proto == b"acme-tls/1")).unwrap_or(false);

        if is_acme {
            // Return the temporary challenge cert generated from instant-acme tokens
            info!("we have an acme challenge");
            self.challenge_cert.read().clone()
        }
        else {
            // Return the standard certificate for your IP
            self.base_cert.read().clone()
        }
    }
}
