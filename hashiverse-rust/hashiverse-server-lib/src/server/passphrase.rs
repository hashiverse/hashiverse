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

/// Where to look for a passphrase file when none is given explicitly, most secure first:
/// secret files mount-injected by the container, then the working directory.
pub const DEFAULT_PASSPHRASE_PATHS: &[&str] = &[
    "/run/secrets/hashiverse_passphrase",
    "/run/secrets/HASHIVERSE_PASSPHRASE",
    "/mnt/secrets/hashiverse_passphrase",
    "/mnt/secrets/HASHIVERSE_PASSPHRASE",
    "/etc/secrets/hashiverse_passphrase",
    "/etc/secrets/HASHIVERSE_PASSPHRASE",
    "./.hashiverse_passphrase",
    "./.HASHIVERSE_PASSPHRASE",
];

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
    for default_passphrase_path in DEFAULT_PASSPHRASE_PATHS {
        if let Some(passphrase) = get_passphrase_from_file_if_exists(default_passphrase_path) { return Ok(passphrase); }
    }

    // Else try an env variable
    if let Ok(passphrase) = std::env::var("HASHIVERSE_PASSPHRASE") { return Ok(SecretString::new(Box::from(passphrase))); }

    // Error!
    anyhow::bail!("no passphrase found - please (at worst) set the HASHIVERSE_PASSPHRASE environment variable to something memorable");
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    // Regression: every default path used to start with a literal tab, so none was ever found.
    #[test]
    fn default_passphrase_paths_have_no_stray_whitespace() {
        for default_passphrase_path in DEFAULT_PASSPHRASE_PATHS {
            assert_eq!(default_passphrase_path.trim(), *default_passphrase_path, "whitespace in {:?}", default_passphrase_path);
            assert!(default_passphrase_path.starts_with('/') || default_passphrase_path.starts_with("./"), "not a path: {:?}", default_passphrase_path);
        }
    }

    #[test]
    fn explicit_passphrase_path_is_read() {
        let (_temp_dir, temp_dir_path) = hashiverse_lib::tools::tools::get_temp_dir().unwrap();
        let passphrase_path = std::path::Path::new(&temp_dir_path).join("passphrase");
        fs::write(&passphrase_path, "correct horse").unwrap();
        let passphrase = get_passphrase(Some(passphrase_path.to_string_lossy().into_owned())).unwrap();
        assert_eq!(passphrase.expose_secret(), "correct horse");
    }

    #[test]
    fn missing_explicit_passphrase_path_is_an_error() {
        let (_temp_dir, temp_dir_path) = hashiverse_lib::tools::tools::get_temp_dir().unwrap();
        let passphrase_path = std::path::Path::new(&temp_dir_path).join("does-not-exist");
        assert!(get_passphrase(Some(passphrase_path.to_string_lossy().into_owned())).is_err());
    }
}
