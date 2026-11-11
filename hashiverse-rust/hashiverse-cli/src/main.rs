use std::io::IsTerminal;
use std::sync::Arc;

use clap::Parser;
use hashiverse_cli::cli_args::{CliArgs, ENV_KEYPHRASE, ENV_PASSWORD};
use hashiverse_cli::commands::run_command;
use hashiverse_cli::repl::{run_repl, LiveReplSession};
use hashiverse_cli::unlock::{is_guest, resolve_unlock_method, unlock};
use hashiverse_lib::tools::time_provider::time_provider::RealTimeProvider;
use hashiverse_lib::tools::tools::configure_logging_with_time_provider;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli_args = CliArgs::parse();

    configure_logging_with_time_provider(&cli_args.log_level, Arc::new(RealTimeProvider));
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let password_env = std::env::var(ENV_PASSWORD).ok();
    let unlock_method = resolve_unlock_method(cli_args.profile.clone(), std::env::var(ENV_KEYPHRASE).ok(), password_env.clone())?;
    let hashiverse = unlock(&cli_args, unlock_method).await?;
    if is_guest(&hashiverse) {
        eprintln!("browsing as guest");
    } else {
        eprintln!("unlocked as {}", hashiverse.client_id().id_hex());
    }

    match &cli_args.command {
        Some(command) => {
            let output = run_command(&hashiverse, command, cli_args.json).await?;
            println!("{}", output);
        }
        None => {
            let interactive = std::io::stdin().is_terminal();
            if interactive {
                eprintln!("type help for commands, quit to leave");
            }
            let mut live_repl_session = LiveReplSession::new(&cli_args, password_env, hashiverse);
            let input = tokio::io::BufReader::new(tokio::io::stdin());
            run_repl(&mut live_repl_session, input, &mut std::io::stdout(), &mut std::io::stderr(), cli_args.json, interactive).await?;
        }
    }

    Ok(())
}
