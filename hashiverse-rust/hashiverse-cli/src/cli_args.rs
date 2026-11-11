//! Command-line argument definitions.
//!
//! Secrets (the keyphrase and the key-file password) are deliberately NOT accepted as argv
//! flags: argv is visible to other users via `ps` and lands in shell history. They come from
//! the `HASHIVERSE_KEYPHRASE` / `HASHIVERSE_PASSWORD` environment variables, or from a hidden
//! console prompt. See [`crate::unlock`].

use clap::Parser;
use std::path::PathBuf;

pub const ENV_KEYPHRASE: &str = "HASHIVERSE_KEYPHRASE";
pub const ENV_PASSWORD: &str = "HASHIVERSE_PASSWORD";
pub const ENV_PROFILE: &str = "HASHIVERSE_PROFILE";
pub const ENV_DATA_DIR: &str = "HASHIVERSE_DATA_DIR";

#[derive(Parser, Debug)]
#[command(name = "hashiverse-cli", version, about = "Command-line client for Hashiverse")]
pub struct CliArgs {
    /// Directory holding the stored keys and client storage. Defaults to the platform data dir.
    #[arg(long, env = ENV_DATA_DIR, global = true)]
    pub data_dir: Option<PathBuf>,

    /// Client id (hex) of a key previously stored in the data dir. Unlocked with HASHIVERSE_PASSWORD or a prompt.
    #[arg(long, env = ENV_PROFILE, global = true)]
    pub profile: Option<String>,

    /// Override the bootstrap peers with a comma-separated list of host:port.
    #[arg(long, value_delimiter = ',', global = true)]
    pub bootstrap: Vec<String>,

    /// Emit machine-readable JSON instead of text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Log level for the underlying client (error, warn, info, debug, trace).
    #[arg(long, default_value = "warn", global = true)]
    pub log_level: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_args_definition_is_valid() {
        CliArgs::command().debug_assert();
    }

    #[test]
    fn bootstrap_is_comma_separated() {
        let cli_args = CliArgs::try_parse_from(["hashiverse-cli", "--bootstrap", "1.2.3.4:443,5.6.7.8:443"]).unwrap();
        assert_eq!(cli_args.bootstrap, vec!["1.2.3.4:443".to_string(), "5.6.7.8:443".to_string()]);
    }

    #[test]
    fn no_secret_flags_exist() {
        assert!(CliArgs::try_parse_from(["hashiverse-cli", "--keyphrase", "x"]).is_err());
        assert!(CliArgs::try_parse_from(["hashiverse-cli", "--password", "x"]).is_err());
    }
}
