//! Deciding how to unlock an identity, and then unlocking it.
//!
//! Everything comes from the environment; the CLI never prompts for secrets.
//!
//! Resolution (see [`resolve_unlock_method`]):
//! - Neither `HASHIVERSE_KEYPHRASE` nor `HASHIVERSE_PROFILE` set → read-only guest (the empty
//!   keyphrase, [`GUEST_CLIENT_ID`]). Reading posts and trending hashtags needs no identity;
//!   [`crate::commands::post`] refuses for guests.
//! - Both set → error: they name potentially different identities.
//! - Exactly one set → `HASHIVERSE_PASSWORD` must also be set (an empty value is allowed, but
//!   the variable must exist), otherwise it is an error.
//!   - `HASHIVERSE_KEYPHRASE` → derive the key from the keyphrase and save it
//!     to the data dir encrypted with the password, so later runs can use `HASHIVERSE_PROFILE`.
//!   - `HASHIVERSE_PROFILE` / `--profile` → load that stored key, decrypting with the password.

use anyhow::{anyhow, Result};
use hashiverse_client_rust::{Hashiverse, HashiverseBuilder};
use hashiverse_lib::client::hashiverse_client::HashiverseClient;
use hashiverse_lib::client::key_locker::key_locker::GUEST_CLIENT_ID;

use crate::cli_args::{CliArgs, ENV_KEYPHRASE, ENV_PASSWORD, ENV_PROFILE};

#[derive(Debug, PartialEq, Eq)]
pub enum UnlockMethod {
    Keyphrase { key_phrase: String, password: String },
    StoredKey { client_id_hex: String, password: String },
    Guest,
}

pub fn is_guest(hashiverse_client: &HashiverseClient) -> bool {
    hashiverse_client.client_id().id_hex() == GUEST_CLIENT_ID
}

fn non_empty_trimmed(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

pub fn resolve_unlock_method(profile: Option<String>, keyphrase_env: Option<String>, password_env: Option<String>) -> Result<UnlockMethod> {
    let keyphrase_env = non_empty_trimmed(keyphrase_env);
    let profile = non_empty_trimmed(profile);

    if keyphrase_env.is_none() && profile.is_none() {
        return Ok(UnlockMethod::Guest);
    }
    if keyphrase_env.is_some() && profile.is_some() {
        return Err(anyhow!("set only one of {} or {}, not both", ENV_KEYPHRASE, ENV_PROFILE));
    }

    // Passwords are not trimmed (surrounding whitespace may be deliberate) and may be empty;
    // only an absent variable is an error.
    let password = password_env.ok_or_else(|| anyhow!("{} must be set when {} or {} is set", ENV_PASSWORD, ENV_KEYPHRASE, ENV_PROFILE))?;

    match (keyphrase_env, profile) {
        (Some(key_phrase), None) => Ok(UnlockMethod::Keyphrase { key_phrase, password }),
        (None, Some(client_id_hex)) => Ok(UnlockMethod::StoredKey { client_id_hex, password }),
        _ => unreachable!("guest and both-set cases handled above"),
    }
}

/// Builds a [`Hashiverse`] client for the resolved identity.
pub async fn unlock(cli_args: &CliArgs, unlock_method: UnlockMethod) -> Result<Hashiverse> {
    let mut hashiverse_builder = HashiverseBuilder::new();
    if let Some(data_dir) = &cli_args.data_dir {
        hashiverse_builder = hashiverse_builder.data_dir(data_dir.clone());
    }
    if !cli_args.bootstrap.is_empty() {
        hashiverse_builder = hashiverse_builder.bootstrap(cli_args.bootstrap.clone());
    }

    match unlock_method {
        UnlockMethod::Keyphrase { key_phrase, password } => hashiverse_builder.passphrase(password).build_with_keyphrase(key_phrase).await,
        UnlockMethod::StoredKey { client_id_hex, password } => hashiverse_builder.passphrase(password).build_from_stored_key(client_id_hex).await,
        UnlockMethod::Guest => hashiverse_builder.build_with_keyphrase("").await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(value: &str) -> Option<String> {
        Some(value.to_string())
    }

    #[test]
    fn nothing_supplied_is_guest() {
        assert_eq!(resolve_unlock_method(None, None, None).unwrap(), UnlockMethod::Guest);
    }

    #[test]
    fn password_alone_is_still_guest() {
        assert_eq!(resolve_unlock_method(None, None, some("pw")).unwrap(), UnlockMethod::Guest);
    }

    #[test]
    fn blank_keyphrase_and_profile_are_guest() {
        assert_eq!(resolve_unlock_method(some("  "), some(""), None).unwrap(), UnlockMethod::Guest);
    }

    #[test]
    fn keyphrase_with_password() {
        assert_eq!(resolve_unlock_method(None, some("correct horse"), some("pw")).unwrap(), UnlockMethod::Keyphrase { key_phrase: "correct horse".to_string(), password: "pw".to_string() });
    }

    #[test]
    fn profile_with_password() {
        assert_eq!(resolve_unlock_method(some(" abcd "), None, some("pw")).unwrap(), UnlockMethod::StoredKey { client_id_hex: "abcd".to_string(), password: "pw".to_string() });
    }

    #[test]
    fn keyphrase_and_profile_together_error() {
        let error = resolve_unlock_method(some("abcd"), some("correct horse"), some("pw")).unwrap_err();
        assert!(error.to_string().contains(ENV_KEYPHRASE) && error.to_string().contains(ENV_PROFILE));
        // The conflict is reported even without a password, rather than the missing password.
        let error = resolve_unlock_method(some("abcd"), some("correct horse"), None).unwrap_err();
        assert!(error.to_string().contains("not both"));
    }

    #[test]
    fn password_is_not_trimmed() {
        assert_eq!(resolve_unlock_method(some("abcd"), None, some(" pw ")).unwrap(), UnlockMethod::StoredKey { client_id_hex: "abcd".to_string(), password: " pw ".to_string() });
    }

    #[test]
    fn keyphrase_without_password_errors() {
        let error = resolve_unlock_method(None, some("correct horse"), None).unwrap_err();
        assert!(error.to_string().contains(ENV_PASSWORD));
    }

    #[test]
    fn profile_without_password_errors() {
        let error = resolve_unlock_method(some("abcd"), None, None).unwrap_err();
        assert!(error.to_string().contains(ENV_PASSWORD));
    }

    #[test]
    fn empty_password_is_allowed_when_set() {
        assert_eq!(resolve_unlock_method(some("abcd"), None, some("")).unwrap(), UnlockMethod::StoredKey { client_id_hex: "abcd".to_string(), password: String::new() });
        assert_eq!(resolve_unlock_method(None, some("correct horse"), some("")).unwrap(), UnlockMethod::Keyphrase { key_phrase: "correct horse".to_string(), password: String::new() });
    }

    #[tokio::test]
    async fn empty_keyphrase_is_the_guest_identity() {
        use hashiverse_lib::client::key_locker::disk_key_locker::DiskKeyLockerManager;
        use hashiverse_lib::client::key_locker::key_locker::{KeyLocker, KeyLockerManager};

        let (_temp_dir, temp_dir_path) = hashiverse_lib::tools::tools::get_temp_dir().unwrap();
        let key_locker_manager = DiskKeyLockerManager::with_data_dir(std::path::PathBuf::from(temp_dir_path), String::new()).unwrap();
        let guest_key_locker = key_locker_manager.create(String::new()).await.unwrap();
        assert_eq!(guest_key_locker.client_id().id_hex(), GUEST_CLIENT_ID);
        assert!(key_locker_manager.list().await.unwrap().is_empty(), "guest key must not show up as a stored profile");
    }

    // Exercises the real on-disk key locker: a keyphrase unlock stores the key, and a
    // subsequent stored-key unlock with the same password recovers the same identity.
    #[tokio::test]
    async fn keyphrase_then_stored_key_roundtrip_on_disk() {
        use hashiverse_lib::client::key_locker::disk_key_locker::DiskKeyLockerManager;
        use hashiverse_lib::client::key_locker::key_locker::{KeyLocker, KeyLockerManager};

        let (_temp_dir, temp_dir_path) = hashiverse_lib::tools::tools::get_temp_dir().unwrap();
        let data_dir = std::path::PathBuf::from(temp_dir_path);

        let key_locker_manager = DiskKeyLockerManager::with_data_dir(data_dir.clone(), "pw".to_string()).unwrap();
        let created_key_locker = key_locker_manager.create("correct horse battery staple".to_string()).await.unwrap();
        let client_id_hex = created_key_locker.client_id().id_hex();

        let reloaded_key_locker_manager = DiskKeyLockerManager::with_data_dir(data_dir.clone(), "pw".to_string()).unwrap();
        let reloaded_key_locker = reloaded_key_locker_manager.switch(client_id_hex.clone()).await.unwrap();
        assert_eq!(reloaded_key_locker.client_id().id_hex(), client_id_hex);

        let wrong_password_key_locker_manager = DiskKeyLockerManager::with_data_dir(data_dir, "wrong".to_string()).unwrap();
        assert!(wrong_password_key_locker_manager.switch(client_id_hex).await.is_err());
    }
}
