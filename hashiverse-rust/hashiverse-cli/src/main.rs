use std::io::IsTerminal;
use std::sync::Arc;

use clap::Parser;
use hashiverse_cli::cli_args::{CliArgs, ENV_KEYPHRASE, ENV_PASSWORD};
use hashiverse_cli::commands::run_command;
use hashiverse_cli::repl::{run_repl, LiveReplSession};
use hashiverse_cli::unlock::{is_guest, resolve_unlock_method, unlock};
use hashiverse_lib::tools::time_provider::time_provider::RealTimeProvider;
use hashiverse_lib::tools::tools::CustomTimeFormatter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Like `hashiverse_lib::tools::tools::configure_logging_with_time_provider`, but logs go to
/// stderr: stdout carries command output (possibly JSON) and must stay clean.
fn configure_cli_logging(level: &str) {
    let filter = format!("{},hyper=off,warp=off,reqwest=off,rustls=off,h2=off,html5ever=off,selectors=off,fjall=off,lsm_tree=off,sfa=off,hickory_resolver=off,hickory_proto=off", level);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_timer(CustomTimeFormatter::new(Arc::new(RealTimeProvider)));
    tracing_subscriber::registry().with(fmt_layer).with(tracing_subscriber::EnvFilter::new(&filter)).init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli_args = CliArgs::parse();

    configure_cli_logging(&cli_args.log_level);
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let password_env = std::env::var(ENV_PASSWORD).ok();
    let unlock_method = resolve_unlock_method(cli_args.profile.clone(), std::env::var(ENV_KEYPHRASE).ok(), password_env.clone())?;
    let hashiverse = unlock(&cli_args, unlock_method).await?;

    // Status chatter is for humans; in JSON mode only the data (and errors) are emitted.
    let chatty = !cli_args.json;
    if chatty {
        if is_guest(&hashiverse) {
            eprintln!("browsing as guest");
        } else {
            eprintln!("unlocked as {}", hashiverse.client_id().id_hex());
        }
    }

    match &cli_args.command {
        Some(command) => {
            let output = run_command(&hashiverse, command, cli_args.json).await?;
            println!("{}", output);
        }
        None => {
            let interactive = std::io::stdin().is_terminal();
            if interactive && chatty {
                eprintln!("type help for commands, quit to leave");
            }
            let mut live_repl_session = LiveReplSession::new(&cli_args, password_env, hashiverse);
            let input = tokio::io::BufReader::new(tokio::io::stdin());
            run_repl(&mut live_repl_session, input, &mut std::io::stdout(), &mut std::io::stderr(), cli_args.json, interactive).await?;
        }
    }

    Ok(())
}
