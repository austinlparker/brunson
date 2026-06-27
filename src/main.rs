use anyhow::Result;
use brunson::{config, daemon, tui};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "brunson")]
#[command(about = "Terminal PR manager with daemon/TUI split")]
#[command(version)]
struct Cli {
    /// Path to config file (default: ~/.config/brunson/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the background daemon (polls GitHub, serves HTTP API)
    Daemon,
    /// Run the TUI client (auto-spawns daemon if needed)
    Tui,
    /// Initialize config file with defaults
    Init,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon => {
            let config = config::Config::load(cli.config.as_deref())?;
            daemon::run_daemon(config).await?;
        }
        Commands::Tui => {
            let config = config::Config::load(cli.config.as_deref())?;
            tui::app::run_tui_with_config_path(config, cli.config).await?;
        }
        Commands::Init => {
            let config_dir = config::config_dir()?;
            std::fs::create_dir_all(&config_dir)?;
            let config_path = config_dir.join("config.toml");
            if config_path.exists() {
                eprintln!("Config already exists at {}", config_path.display());
            } else {
                std::fs::write(&config_path, config::example_config())?;
                println!("Created config at {}", config_path.display());
            }
        }
    }

    Ok(())
}
