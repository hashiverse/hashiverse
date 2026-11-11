//! Deciding how to unlock an identity, and then unlocking it.
//!
//! Resolution order (see [`resolve_unlock_method`]):
//! 1. `HASHIVERSE_KEYPHRASE` set → derive the key from the keyphrase. The key is also saved to
//!    the data dir, encrypted with `HASHIVERSE_PASSWORD` (empty if unset), so later runs can use
//!    `--profile <client_id>` instead.
//! 2. `--profile` / `HASHIVERSE_PROFILE` set → load that stored key, decrypting with
//!    `HASHIVERSE_PASSWORD`, or a hidden prompt if that is unset.
//! 3. Interactive terminal → prompt (hidden) for the keyphrase and a password to protect it.
//! 4. Otherwise → error naming the environment variables.

use std::io::IsTerminal;

use anyhow::{anyhow, Result};
use hashiverse_client_rust::{Hashiverse, HashiverseBuilder};

use crate::cli_args::{CliArgs, ENV_KEYPHRASE, ENV_PASSWORD, ENV_PROFILE};

/// Source of secrets typed by the user. Abstracted so resolution can be tested without a terminal.
pub trait SecretPrompter {
    fn is_interactive(&self) -> bool;
    fn prompt_secret(&self, prompt: &str) -> Result<String>;
}

/// Prompts on the real terminal with echo disabled.
pub struct TerminalSecretPrompter;

impl SecretPrompter for TerminalSecretPrompter {
    fn is_interactive(&self) -> bool {
        std::io::stdin().is_terminal()
    }

    fn prompt_secret(&self, prompt: &str) -> Result<String> {
        Ok(rpassword::prompt_password(prompt)?)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum UnlockMethod {
    Keyphrase { key_phrase: String, password: String },
    StoredKey { client_id_hex: String, password: String },
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

pub fn resolve_unlock_method(profile: Option<String>, keyphrase_env: Option<String>, password_env: Option<String>, secret_prompter: &dyn SecretPrompter) -> Result<UnlockMethod> {
    // An empty keyphrase or profile means "not supplied"; an empty password is a legitimate password.
    let keyphrase_env = non_empty(keyphrase_env);
    let profile = non_empty(profile);

    if let Some(key_phrase) = keyphrase_env {
        return Ok(UnlockMethod::Keyphrase {
            key_phrase,
            password: password_env.unwrap_or_default(),
        });
    }

    if let Some(client_id_hex) = profile {
        let password = match password_env {
            Some(password) => password,
            None if secret_prompter.is_interactive() => secret_prompter.prompt_secret(&format!("Password for {}: ", client_id_hex))?,
            None => return Err(anyhow!("{} is set but {} is not (set it, possibly to an empty string, or run interactively)", ENV_PROFILE, ENV_PASSWORD)),
        };
        return Ok(UnlockMethod::StoredKey { client_id_hex, password });
    }

    if secret_prompter.is_interactive() {
        let key_phrase = secret_prompter.prompt_secret("Keyphrase: ")?;
        if key_phrase.trim().is_empty() {
            return Err(anyhow!("No keyphrase entered"));
        }
        let password = match password_env {
            Some(password) => password,
            None => secret_prompter.prompt_secret("Password to protect the stored key (blank for none): ")?,
        };
        return Ok(UnlockMethod::Keyphrase { key_phrase, password });
    }

    Err(anyhow!("No identity supplied: set {} (optionally with {}), or {} with {}, or run interactively", ENV_KEYPHRASE, ENV_PASSWORD, ENV_PROFILE, ENV_PASSWORD))
}

/// Builds a connected [`Hashiverse`] client for the resolved identity.
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    struct FakeSecretPrompter {
        interactive: bool,
        responses: RefCell<VecDeque<String>>,
        prompts_seen: RefCell<Vec<String>>,
    }

    impl FakeSecretPrompter {
        fn new(interactive: bool, responses: &[&str]) -> Self {
            Self {
                interactive,
                responses: RefCell::new(responses.iter().map(|response| response.to_string()).collect()),
                prompts_seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl SecretPrompter for FakeSecretPrompter {
        fn is_interactive(&self) -> bool {
            self.interactive
        }

        fn prompt_secret(&self, prompt: &str) -> Result<String> {
            self.prompts_seen.borrow_mut().push(prompt.to_string());
            self.responses.borrow_mut().pop_front().ok_or_else(|| anyhow!("unexpected prompt: {}", prompt))
        }
    }

    fn some(value: &str) -> Option<String> {
        Some(value.to_string())
    }

    #[test]
    fn keyphrase_env_with_password() {
        let secret_prompter = FakeSecretPrompter::new(true, &[]);
        let unlock_method = resolve_unlock_method(None, some("correct horse"), some("pw"), &secret_prompter).unwrap();
        assert_eq!(unlock_method, UnlockMethod::Keyphrase { key_phrase: "correct horse".to_string(), password: "pw".to_string() });
        assert!(secret_prompter.prompts_seen.borrow().is_empty());
    }

    #[test]
    fn keyphrase_env_without_password_uses_empty_and_does_not_prompt() {
        let secret_prompter = FakeSecretPrompter::new(true, &[]);
        let unlock_method = resolve_unlock_method(None, some("correct horse"), None, &secret_prompter).unwrap();
        assert_eq!(unlock_method, UnlockMethod::Keyphrase { key_phrase: "correct horse".to_string(), password: String::new() });
        assert!(secret_prompter.prompts_seen.borrow().is_empty());
    }

    #[test]
    fn keyphrase_wins_over_profile() {
        let secret_prompter = FakeSecretPrompter::new(false, &[]);
        let unlock_method = resolve_unlock_method(some("abcd"), some("correct horse"), some("pw"), &secret_prompter).unwrap();
        assert!(matches!(unlock_method, UnlockMethod::Keyphrase { .. }));
    }

    #[test]
    fn empty_keyphrase_env_is_ignored() {
        let secret_prompter = FakeSecretPrompter::new(false, &[]);
        let unlock_method = resolve_unlock_method(some("abcd"), some("  "), some("pw"), &secret_prompter).unwrap();
        assert_eq!(unlock_method, UnlockMethod::StoredKey { client_id_hex: "abcd".to_string(), password: "pw".to_string() });
    }

    #[test]
    fn profile_with_password_env() {
        let secret_prompter = FakeSecretPrompter::new(true, &[]);
        let unlock_method = resolve_unlock_method(some(" abcd "), None, some(""), &secret_prompter).unwrap();
        assert_eq!(unlock_method, UnlockMethod::StoredKey { client_id_hex: "abcd".to_string(), password: String::new() });
        assert!(secret_prompter.prompts_seen.borrow().is_empty());
    }

    #[test]
    fn profile_prompts_for_password_when_interactive() {
        let secret_prompter = FakeSecretPrompter::new(true, &["typed pw"]);
        let unlock_method = resolve_unlock_method(some("abcd"), None, None, &secret_prompter).unwrap();
        assert_eq!(unlock_method, UnlockMethod::StoredKey { client_id_hex: "abcd".to_string(), password: "typed pw".to_string() });
        assert_eq!(secret_prompter.prompts_seen.borrow().len(), 1);
    }

    #[test]
    fn profile_without_password_non_interactive_errors() {
        let secret_prompter = FakeSecretPrompter::new(false, &[]);
        let error = resolve_unlock_method(some("abcd"), None, None, &secret_prompter).unwrap_err();
        assert!(error.to_string().contains(ENV_PASSWORD));
    }

    #[test]
    fn interactive_prompts_for_keyphrase_and_password() {
        let secret_prompter = FakeSecretPrompter::new(true, &["correct horse", "pw"]);
        let unlock_method = resolve_unlock_method(None, None, None, &secret_prompter).unwrap();
        assert_eq!(unlock_method, UnlockMethod::Keyphrase { key_phrase: "correct horse".to_string(), password: "pw".to_string() });
        assert_eq!(secret_prompter.prompts_seen.borrow().len(), 2);
    }

    #[test]
    fn interactive_uses_password_env_without_prompting_for_it() {
        let secret_prompter = FakeSecretPrompter::new(true, &["correct horse"]);
        let unlock_method = resolve_unlock_method(None, None, some("pw"), &secret_prompter).unwrap();
        assert_eq!(unlock_method, UnlockMethod::Keyphrase { key_phrase: "correct horse".to_string(), password: "pw".to_string() });
        assert_eq!(secret_prompter.prompts_seen.borrow().len(), 1);
    }

    #[test]
    fn interactive_empty_keyphrase_errors() {
        let secret_prompter = FakeSecretPrompter::new(true, &[""]);
        assert!(resolve_unlock_method(None, None, None, &secret_prompter).is_err());
    }

    #[test]
    fn nothing_supplied_non_interactive_errors() {
        let secret_prompter = FakeSecretPrompter::new(false, &[]);
        let error = resolve_unlock_method(None, None, None, &secret_prompter).unwrap_err();
        assert!(error.to_string().contains(ENV_KEYPHRASE));
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
