//! Command-line argument definitions.
//!
//! Secrets (the keyphrase and the key-file password) are deliberately NOT accepted as argv
//! flags: argv is visible to other users via `ps` and lands in shell history. They come from
//! the `HASHIVERSE_KEYPHRASE` / `HASHIVERSE_PASSWORD` environment variables only; the CLI never
//! prompts. See [`crate::unlock`].

use clap::{Parser, Subcommand};
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

    /// Client id (hex) of a key previously stored in the data dir. Requires HASHIVERSE_PASSWORD.
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

    #[command(subcommand)]
    pub command: Option<Command>,
}

pub const DEFAULT_TRENDING_LIMIT: u16 = 20;

#[derive(Subcommand, Debug, PartialEq, Eq, Clone)]
pub enum Command {
    /// Submit a plain-text post. #hashtags and @<64-hex client id> mentions are recognised.
    ///
    /// Options (e.g. --json) must come before the text: everything after the first word of text
    /// is part of the post, so `post hi --json` posts "hi --json".
    Post {
        /// The post text. Multiple words are joined with spaces, so quoting is optional.
        #[arg(required = true, num_args = 1.., trailing_var_arg = true, allow_hyphen_values = true)]
        text: Vec<String>,
    },
    /// List posts by a user. Repeating this in the REPL fetches progressively older posts.
    ///
    /// Switching to a different timeline starts that one from the newest again.
    User {
        /// The user's client id (hex).
        client_id_hex: String,
        /// How many batches of posts to fetch.
        #[arg(long, default_value_t = 1)]
        pages: u32,
    },
    /// List posts with a hashtag. Repeating this in the REPL fetches progressively older posts.
    ///
    /// Switching to a different timeline starts that one from the newest again.
    Hashtag {
        /// The hashtag, with or without the leading '#'.
        hashtag: String,
        /// How many batches of posts to fetch.
        #[arg(long, default_value_t = 1)]
        pages: u32,
    },
    /// Show trending hashtags.
    Trending {
        /// Maximum number of hashtags to show.
        #[arg(long, default_value_t = DEFAULT_TRENDING_LIMIT)]
        limit: u16,
    },
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

    fn parse_command(argv: &[&str]) -> Command {
        let mut full_argv = vec!["hashiverse-cli"];
        full_argv.extend_from_slice(argv);
        CliArgs::try_parse_from(full_argv).unwrap().command.unwrap()
    }

    #[test]
    fn no_subcommand_is_none() {
        assert_eq!(CliArgs::try_parse_from(["hashiverse-cli"]).unwrap().command, None);
    }

    #[test]
    fn post_joins_unquoted_words_and_keeps_hyphens() {
        assert_eq!(parse_command(&["post", "hello", "#world", "-", "--json-ish"]), Command::Post { text: vec!["hello".into(), "#world".into(), "-".into(), "--json-ish".into()] });
    }

    #[test]
    fn post_options_before_text_are_parsed_as_options() {
        let cli_args = CliArgs::try_parse_from(["hashiverse-cli", "post", "--json", "hello", "world"]).unwrap();
        assert!(cli_args.json);
        assert_eq!(cli_args.command, Some(Command::Post { text: vec!["hello".into(), "world".into()] }));
    }

    #[test]
    fn post_requires_text() {
        assert!(CliArgs::try_parse_from(["hashiverse-cli", "post"]).is_err());
    }

    #[test]
    fn global_json_flag_works_after_subcommand() {
        let cli_args = CliArgs::try_parse_from(["hashiverse-cli", "trending", "--json"]).unwrap();
        assert!(cli_args.json);
        assert_eq!(cli_args.command, Some(Command::Trending { limit: DEFAULT_TRENDING_LIMIT }));
    }

    #[test]
    fn user_and_hashtag_pages() {
        assert_eq!(parse_command(&["user", "abcd"]), Command::User { client_id_hex: "abcd".into(), pages: 1 });
        assert_eq!(parse_command(&["hashtag", "#rust", "--pages", "3"]), Command::Hashtag { hashtag: "#rust".into(), pages: 3 });
        assert_eq!(parse_command(&["trending", "--limit", "5"]), Command::Trending { limit: 5 });
    }

    #[test]
    fn no_secret_flags_exist() {
        assert!(CliArgs::try_parse_from(["hashiverse-cli", "--keyphrase", "x"]).is_err());
        assert!(CliArgs::try_parse_from(["hashiverse-cli", "--password", "x"]).is_err());
    }
}
