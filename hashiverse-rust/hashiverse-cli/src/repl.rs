//! Interactive prompt, used when `hashiverse-cli` is run without a command.
//!
//! Each line is split into words (see [`split_repl_words`]) and is either a REPL-only command (`help`, `quit`/`exit`,
//! `login <client_id_hex>`) or one of the regular [`Command`]s, parsed by the same clap
//! definitions as the one-shot CLI. The identity and network connection persist across lines,
//! so repeating `user X` / `hashtag Y` walks further back in that timeline.
//!
//! The loop is generic over [`ReplSession`] so it can be tested without a network.

use std::io::Write;

use anyhow::{anyhow, Result};
use clap::{CommandFactory, Parser};
use hashiverse_client_rust::Hashiverse;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use crate::cli_args::{CliArgs, Command, ENV_PASSWORD};
use crate::commands::run_command;
use crate::unlock::{is_guest, resolve_unlock_method, unlock};

/// What a single REPL line looks like to clap: a [`Command`] plus a per-line `--json`.
#[derive(Parser, Debug)]
#[command(name = "", no_binary_name = true, disable_help_subcommand = true, disable_version_flag = true)]
struct ReplCommandLine {
    /// Emit machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReplLine {
    Empty,
    Help,
    Quit,
    Login { client_id_hex: String },
    Command { command: Command, json: bool },
}

/// Splits a line into words, shell-style: whitespace separates words, `"..."` and `'...'` group
/// them, and a backslash escapes the next character (outside single quotes). Unlike a real
/// shell there is no comment syntax, so `#hashtag` is an ordinary word.
pub fn split_repl_words(line: &str) -> std::result::Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current_word = String::new();
    let mut in_word = false;
    let mut characters = line.chars();

    while let Some(character) = characters.next() {
        match character {
            character if character.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current_word));
                    in_word = false;
                }
            }
            '\\' => {
                current_word.push(characters.next().ok_or("trailing backslash")?);
                in_word = true;
            }
            '\'' => {
                in_word = true;
                loop {
                    match characters.next() {
                        Some('\'') => break,
                        Some(quoted_character) => current_word.push(quoted_character),
                        None => return Err("unterminated single quote".to_string()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match characters.next() {
                        Some('"') => break,
                        Some('\\') => current_word.push(characters.next().ok_or("unterminated double quote")?),
                        Some(quoted_character) => current_word.push(quoted_character),
                        None => return Err("unterminated double quote".to_string()),
                    }
                }
            }
            character => {
                current_word.push(character);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current_word);
    }
    Ok(words)
}

/// Parses one line. `Err` carries a message ready to show the user (including clap's usage
/// text, or a command's `--help` output).
pub fn parse_repl_line(line: &str) -> std::result::Result<ReplLine, String> {
    let words = split_repl_words(line).map_err(|error| format!("could not parse line: {}", error))?;
    let Some(first_word) = words.first() else {
        return Ok(ReplLine::Empty);
    };

    match first_word.as_str() {
        "help" => Ok(ReplLine::Help),
        "quit" | "exit" => Ok(ReplLine::Quit),
        "login" => match words.as_slice() {
            [_, client_id_hex] => Ok(ReplLine::Login { client_id_hex: client_id_hex.trim().to_string() }),
            _ => Err("usage: login <client_id_hex>".to_string()),
        },
        _ => ReplCommandLine::try_parse_from(&words)
            .map(|repl_command_line| ReplLine::Command { command: repl_command_line.command, json: repl_command_line.json })
            .map_err(|error| error.render().to_string()),
    }
}

/// The list shown by `help`, built from the clap definitions so it never drifts from them.
pub fn help_text() -> String {
    let mut text = String::from("Commands:\n");
    for subcommand in ReplCommandLine::command().get_subcommands() {
        let about = subcommand.get_about().map(|about| about.to_string()).unwrap_or_default();
        text.push_str(&format!("  {:<10} {}\n", subcommand.get_name(), about));
    }
    text.push_str(&format!("  {:<10} {}\n", "login", "Switch to a stored profile: login <client_id_hex> (uses HASHIVERSE_PASSWORD)"));
    text.push_str(&format!("  {:<10} {}\n", "help", "Show this list"));
    text.push_str(&format!("  {:<10} {}\n", "quit", "Leave (also: exit, or end-of-input)"));
    text.push_str("\nUse <command> --help for a command's options; add --json to any command for JSON output.");
    text
}

pub fn format_prompt(guest: bool, client_id_hex: &str) -> String {
    if guest {
        "guest> ".to_string()
    } else {
        format!("{}> ", &client_id_hex[..client_id_hex.len().min(8)])
    }
}

/// The state the REPL drives: the current identity and how to run commands against it.
#[allow(async_fn_in_trait)]
pub trait ReplSession {
    fn prompt(&self) -> String;
    async fn run_command(&mut self, command: &Command, json: bool) -> Result<String>;
    /// Switches identity. On failure the current identity must be kept.
    async fn login(&mut self, client_id_hex: &str) -> Result<String>;
}

pub struct LiveReplSession<'a> {
    cli_args: &'a CliArgs,
    password_env: Option<String>,
    hashiverse: Hashiverse,
}

impl<'a> LiveReplSession<'a> {
    pub fn new(cli_args: &'a CliArgs, password_env: Option<String>, hashiverse: Hashiverse) -> Self {
        Self { cli_args, password_env, hashiverse }
    }
}

impl ReplSession for LiveReplSession<'_> {
    fn prompt(&self) -> String {
        format_prompt(is_guest(&self.hashiverse), &self.hashiverse.client_id().id_hex())
    }

    async fn run_command(&mut self, command: &Command, json: bool) -> Result<String> {
        run_command(&self.hashiverse, command, json).await
    }

    async fn login(&mut self, client_id_hex: &str) -> Result<String> {
        if self.password_env.is_none() {
            return Err(anyhow!("login needs {} in the environment (it may be empty)", ENV_PASSWORD));
        }
        let unlock_method = resolve_unlock_method(Some(client_id_hex.to_string()), None, self.password_env.clone())?;
        // Only replace the current client once the new one is fully built.
        let hashiverse = unlock(self.cli_args, unlock_method).await?;
        let message = format!("logged in as {}", hashiverse.client_id().id_hex());
        self.hashiverse = hashiverse;
        Ok(message)
    }
}

/// Runs the prompt loop until `quit`/`exit` or end of input. Command output goes to `output`,
/// errors to `error_output`; a failing line never ends the loop.
pub async fn run_repl<TReplSession: ReplSession, TInput: AsyncBufRead + Unpin>(
    repl_session: &mut TReplSession,
    input: TInput,
    output: &mut impl Write,
    error_output: &mut impl Write,
    session_json: bool,
    show_prompt: bool,
) -> Result<()> {
    let mut lines = input.lines();
    loop {
        if show_prompt {
            write!(output, "{}", repl_session.prompt())?;
            output.flush()?;
        }

        let Some(line) = lines.next_line().await? else {
            if show_prompt {
                writeln!(output)?;
            }
            break;
        };

        match parse_repl_line(&line) {
            Ok(ReplLine::Empty) => {}
            Ok(ReplLine::Help) => writeln!(output, "{}", help_text())?,
            Ok(ReplLine::Quit) => break,
            Ok(ReplLine::Login { client_id_hex }) => match repl_session.login(&client_id_hex).await {
                Ok(message) => writeln!(output, "{}", message)?,
                Err(error) => writeln!(error_output, "error: {:#}", error)?,
            },
            Ok(ReplLine::Command { command, json }) => match repl_session.run_command(&command, json || session_json).await {
                Ok(command_output) => writeln!(output, "{}", command_output)?,
                Err(error) => writeln!(error_output, "error: {:#}", error)?,
            },
            Err(message) => writeln!(error_output, "{}", message.trim_end())?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_args::DEFAULT_TRENDING_LIMIT;
    use std::io::Cursor;

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn split_plain_words_and_hashtags() {
        assert_eq!(split_repl_words("  post hello  #rust @abc "), Ok(words(&["post", "hello", "#rust", "@abc"])));
    }

    #[test]
    fn split_quotes_and_escapes() {
        assert_eq!(split_repl_words("post \"a  b\" 'c \\ d' e\\ f"), Ok(words(&["post", "a  b", "c \\ d", "e f"])));
        assert_eq!(split_repl_words("post \"say \\\"hi\\\"\""), Ok(words(&["post", "say \"hi\""])));
        assert_eq!(split_repl_words("post \"\""), Ok(words(&["post", ""])));
        assert_eq!(split_repl_words("ab\"c d\"e"), Ok(words(&["abc de"])));
    }

    #[test]
    fn split_errors() {
        assert!(split_repl_words("post \"open").is_err());
        assert!(split_repl_words("post 'open").is_err());
        assert!(split_repl_words("post trailing\\").is_err());
    }

    #[test]
    fn parse_empty_and_whitespace() {
        assert_eq!(parse_repl_line(""), Ok(ReplLine::Empty));
        assert_eq!(parse_repl_line("   "), Ok(ReplLine::Empty));
    }

    #[test]
    fn parse_repl_only_commands() {
        assert_eq!(parse_repl_line("help"), Ok(ReplLine::Help));
        assert_eq!(parse_repl_line("quit"), Ok(ReplLine::Quit));
        assert_eq!(parse_repl_line("exit"), Ok(ReplLine::Quit));
        assert_eq!(parse_repl_line("login abcd"), Ok(ReplLine::Login { client_id_hex: "abcd".to_string() }));
    }

    #[test]
    fn parse_login_needs_exactly_one_id() {
        assert!(parse_repl_line("login").unwrap_err().contains("usage: login"));
        assert!(parse_repl_line("login a b").unwrap_err().contains("usage: login"));
    }

    #[test]
    fn parse_regular_commands() {
        assert_eq!(parse_repl_line("trending"), Ok(ReplLine::Command { command: Command::Trending { limit: DEFAULT_TRENDING_LIMIT }, json: false }));
        assert_eq!(parse_repl_line("hashtag #rust --pages 2 --json"), Ok(ReplLine::Command { command: Command::Hashtag { hashtag: "#rust".to_string(), pages: 2 }, json: true }));
    }

    #[test]
    fn parse_post_with_quotes_and_bare_words() {
        assert_eq!(parse_repl_line("post \"hello   world\" #rust"), Ok(ReplLine::Command { command: Command::Post { text: vec!["hello   world".to_string(), "#rust".to_string()] }, json: false }));
    }

    #[test]
    fn parse_errors_are_messages() {
        assert!(parse_repl_line("frobnicate").is_err());
        assert!(parse_repl_line("post").is_err());
        assert!(parse_repl_line("post \"unterminated").unwrap_err().contains("could not parse line"));
        assert!(parse_repl_line("user abcd --help").unwrap_err().contains("--pages"));
    }

    #[test]
    fn help_lists_every_command() {
        let text = help_text();
        for command_name in ["post", "user", "hashtag", "trending", "login", "help", "quit"] {
            assert!(text.contains(&format!("  {}", command_name)), "help missing {}: {}", command_name, text);
        }
    }

    #[test]
    fn prompts() {
        assert_eq!(format_prompt(true, "fe050cc21479"), "guest> ");
        assert_eq!(format_prompt(false, "04bfbfacadd0b89c"), "04bfbfac> ");
    }

    struct FakeReplSession {
        client_id_hex: Option<String>,
        commands_run: Vec<(Command, bool)>,
    }

    impl ReplSession for FakeReplSession {
        fn prompt(&self) -> String {
            match &self.client_id_hex {
                Some(client_id_hex) => format_prompt(false, client_id_hex),
                None => format_prompt(true, ""),
            }
        }

        async fn run_command(&mut self, command: &Command, json: bool) -> Result<String> {
            self.commands_run.push((command.clone(), json));
            match command {
                Command::Post { .. } if self.client_id_hex.is_none() => Err(anyhow!("posting requires an identity")),
                _ => Ok(format!("ran {:?}", command)),
            }
        }

        async fn login(&mut self, client_id_hex: &str) -> Result<String> {
            if client_id_hex.starts_with("bad") {
                return Err(anyhow!("Key file not found for {}", client_id_hex));
            }
            self.client_id_hex = Some(client_id_hex.to_string());
            Ok(format!("logged in as {}", client_id_hex))
        }
    }

    async fn run_script(script: &str, session_json: bool, show_prompt: bool) -> (FakeReplSession, String, String) {
        let mut fake_repl_session = FakeReplSession { client_id_hex: None, commands_run: Vec::new() };
        let mut output = Vec::new();
        let mut error_output = Vec::new();
        run_repl(&mut fake_repl_session, Cursor::new(script.as_bytes().to_vec()), &mut output, &mut error_output, session_json, show_prompt).await.unwrap();
        (fake_repl_session, String::from_utf8(output).unwrap(), String::from_utf8(error_output).unwrap())
    }

    #[tokio::test]
    async fn errors_do_not_end_the_loop() {
        let (fake_repl_session, output, error_output) = run_script("frobnicate\npost hi\ntrending\n", false, false).await;
        assert_eq!(fake_repl_session.commands_run.len(), 2, "both post and trending should run");
        assert!(error_output.contains("posting requires an identity"));
        assert!(output.contains("ran Trending"));
    }

    #[tokio::test]
    async fn login_switches_identity_and_failed_login_keeps_it() {
        let (fake_repl_session, output, error_output) = run_script("login 04bfbfacadd0\nlogin badbad\npost hi\n", false, true).await;
        assert_eq!(fake_repl_session.client_id_hex.as_deref(), Some("04bfbfacadd0"));
        assert!(error_output.contains("Key file not found for badbad"));
        assert!(output.contains("ran Post"), "post should succeed after login: {}", output);
        assert!(output.starts_with("guest> logged in as 04bfbfacadd0\n04bfbfac> "), "prompt should follow identity: {:?}", output);
    }

    #[tokio::test]
    async fn quit_stops_reading() {
        let (fake_repl_session, _, _) = run_script("trending\nquit\ntrending\n", false, false).await;
        assert_eq!(fake_repl_session.commands_run.len(), 1);
    }

    #[tokio::test]
    async fn end_of_input_without_trailing_newline() {
        let (fake_repl_session, _, _) = run_script("trending", false, false).await;
        assert_eq!(fake_repl_session.commands_run.len(), 1);
    }

    #[tokio::test]
    async fn json_comes_from_session_or_line() {
        let (fake_repl_session, _, _) = run_script("trending\ntrending --json\n", false, false).await;
        assert_eq!(fake_repl_session.commands_run.iter().map(|(_, json)| *json).collect::<Vec<_>>(), vec![false, true]);
        let (fake_repl_session, _, _) = run_script("trending\n", true, false).await;
        assert!(fake_repl_session.commands_run[0].1);
    }

    #[tokio::test]
    async fn help_and_no_prompt_when_not_interactive() {
        let (_, output, _) = run_script("help\n", false, false).await;
        assert!(output.starts_with("Commands:"));
        assert!(!output.contains("guest> "));
    }
}
