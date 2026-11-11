use std::sync::Arc;

use clap::Parser;
use hashiverse_cli::cli_args::{CliArgs, ENV_KEYPHRASE, ENV_PASSWORD};
use hashiverse_cli::unlock::{resolve_unlock_method, unlock, TerminalSecretPrompter};
use hashiverse_lib::tools::time_provider::time_provider::RealTimeProvider;
use hashiverse_lib::tools::tools::configure_logging_with_time_provider;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli_args = CliArgs::parse();

    configure_logging_with_time_provider(&cli_args.log_level, Arc::new(RealTimeProvider));
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let unlock_method = resolve_unlock_method(cli_args.profile.clone(), std::env::var(ENV_KEYPHRASE).ok(), std::env::var(ENV_PASSWORD).ok(), &TerminalSecretPrompter)?;
    let hashiverse = unlock(&cli_args, unlock_method).await?;
    eprintln!("unlocked as {}", hashiverse.client_id().id_hex());

    Ok(())
}
