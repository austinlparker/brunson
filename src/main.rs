use anyhow::Result;
use brunson::{cli, config, daemon, setup, tui};
use clap::Parser as _;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = cli::Cli::parse();

    match cli.command {
        cli::Commands::Daemon => {
            let config = config::Config::load(cli.config.as_deref())?;
            daemon::run_daemon(config, cli.config).await?;
        }
        cli::Commands::Tui => {
            let config = config::Config::load(cli.config.as_deref())?;
            tui::app::run_tui_with_config_path(config, cli.config).await?;
        }
        cli::Commands::Setup(args) => {
            setup::run_setup(&args).await?;
        }
    }

    Ok(())
}
