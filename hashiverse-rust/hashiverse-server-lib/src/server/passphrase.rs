//! # Passphrase resolution for server secrets
//!
//! Finds the operator's passphrase (used to decrypt the persisted
//! [`hashiverse_lib::tools::server_id::ServerId`]) in a platform-agnostic way, looking
//! in this order:
//!
//! 1. an explicit file path supplied on the command line,
//! 2. standard container-secret mounts (`/run/secrets/`, `/mnt/secrets/`,
//!    `/etc/secrets/`) — Kubernetes, Docker Swarm and Podman all land here,
//! 3. an environment variable — still supported for legacy non-container deployments.
//!
//! The resolved passphrase is wrapped in `secrecy::SecretString` so it is zeroised on
//! drop and never accidentally logged.

use std::fs;
use secrecy::SecretString;

pub fn get_passphrase(passphrase_path: Option<String>) -> anyhow::Result<SecretString> {
    fn get_passphrase_from_file_if_exists(path: &str) -> Option<SecretString> {
        let contents = fs::read_to_string(path);
        match contents {
            Ok(contents) => Some(SecretString::new(Box::from(contents))),
            Err(_) => None
        }
    }

    // If the user has specified a container-injected passphrase, use that or die
    if let Some(passphrase_path) = passphrase_path {
        let passphrase = get_passphrase_from_file_if_exists(&passphrase_path);
        match passphrase {
            Some(passphrase) => return Ok(passphrase),
            None => anyhow::bail!("no passphrase found at {}", passphrase_path)
        }
    }

    // Else try the most secure routes - a secret file mount-injected by the container
    if let Some(passphrase) = get_passphrase_from_file_if_exists("	/run/secrets/hashiverse_passphrase") { return Ok(passphrase); }
    if let Some(passphrase) = get_passphrase_from_file_if_exists("	/run/secrets/HASHIVERSE_PASSPHRASE") { return Ok(passphrase); }
    if let Some(passphrase) = get_passphrase_from_file_if_exists("	/mnt/secrets/hashiverse_passphrase") { return Ok(passphrase); }
    if let Some(passphrase) = get_passphrase_from_file_if_exists("	/mnt/secrets/HASHIVERSE_PASSPHRASE") { return Ok(passphrase); }
    if let Some(passphrase) = get_passphrase_from_file_if_exists("	/etc/secrets/hashiverse_passphrase") { return Ok(passphrase); }
    if let Some(passphrase) = get_passphrase_from_file_if_exists("	/etc/secrets/HASHIVERSE_PASSPHRASE") { return Ok(passphrase); }
    if let Some(passphrase) = get_passphrase_from_file_if_exists("	./.hashiverse_passphrase") { return Ok(passphrase); }
    if let Some(passphrase) = get_passphrase_from_file_if_exists("	./.HASHIVERSE_PASSPHRASE") { return Ok(passphrase); }

    // Else try an env variable
    if let Ok(passphrase) = std::env::var("HASHIVERSE_PASSPHRASE") { return Ok(SecretString::new(Box::from(passphrase))); }

    // Error!
    anyhow::bail!("no passphrase found - please (at worst) set the HASHIVERSE_PASSPHRASE environment variable to something memorable");
}
