use std::sync::Arc;

use anyhow::Result;
use brunson::{cli, config, daemon, setup, tui};
use clap::Parser as _;
use tracing_subscriber::EnvFilter;

/// Wraps a shared, already-open log file so tracing's `MakeWriter` can hand
/// out cheap clones instead of reopening the file on every log event. Safe to
/// share across writers because the file is opened in append mode, so the OS
/// serializes each write to the end of the file.
#[derive(Clone)]
struct SharedLogFile(Arc<std::fs::File>);

impl std::io::Write for SharedLogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        (&*self.0).write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        (&*self.0).flush()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse();

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    match &cli.command {
        cli::Commands::Tui => {
            // Direct all tracing output to a file so log lines do not corrupt
            // the alternate-screen TUI renderer.
            let data_dir = config::data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let log_path = data_dir.join("tui.log");
            let log_file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .map_err(|e| {
                    anyhow::anyhow!("Failed to open TUI log file {}: {}", log_path.display(), e)
                })?;
            let log_file = SharedLogFile(Arc::new(log_file));

            tracing_subscriber::fmt()
                .with_env_filter(env_filter)
                .with_ansi(false)
                .with_writer(move || log_file.clone())
                .init();
        }
        _ => {
            tracing_subscriber::fmt().with_env_filter(env_filter).init();
        }
    }

    match cli.command {
        cli::Commands::Daemon => {
            let config = config::Config::load(cli.config.as_deref())?;
            daemon::run_daemon(config, cli.config).await?;
        }
        cli::Commands::Tui => {
            // Unlike `daemon`, the TUI has an interactive wizard that can
            // recover a broken config, so don't hard-exit on a parse error —
            // fall back to defaults and let the wizard fix it.
            let config = config::Config::load(cli.config.as_deref()).unwrap_or_else(|e| {
                tracing::warn!(
                    "Config failed to load ({}); falling back to defaults so the setup wizard can recover it",
                    e
                );
                config::Config::default()
            });
            tui::app::run_tui_with_config_path(config, cli.config).await?;
        }
        cli::Commands::Setup(args) => {
            setup::run_setup(&args).await?;
        }
    }

    Ok(())
}
