use clap::{Parser, Subcommand};
use std::path::PathBuf;

const LONG_ABOUT: &str = r#"brunson is a terminal PR manager with a daemon/TUI split.

Quick start:
  brunson setup              # interactive first-time setup
  brunson daemon             # start the background daemon
  brunson tui                # start the TUI (auto-spawns daemon if needed)

The daemon exposes a local HTTP API on http://127.0.0.1:17890 by default.
Query /health for status and /setup/status for diagnostics.

For agents or non-interactive installs:
  brunson setup --yes --json # write a default config and print a
                             # machine-readable summary with config
                             # advice and prompts

Run `brunson setup --help` for details.
"#;

const SETUP_LONG_ABOUT: &str = r#"Interactive setup wizard.

Creates ~/.config/brunson/config.toml, validates GitHub authentication,
and tests LLM reachability if enabled.

Examples:
  brunson setup              # interactive prompts
  brunson setup --yes        # non-interactive: ensure default config exists
  brunson setup --yes --json # non-interactive, machine-readable summary

`--json` is intended for agents. It returns a JSON object with:
  - ready/status: a quick readiness check
  - config_path: the config file location
  - next_steps: actionable messages
  - advice: what to tell or ask the user
  - prompts: a list of missing config fields, with descriptions and examples

If a daemon is already running on the configured port, `brunson setup`
sends it /config/reload when it finishes so the new config takes effect
immediately.
"#;

#[derive(Parser)]
#[command(name = "brunson")]
#[command(about = "Terminal PR manager with daemon/TUI split")]
#[command(long_about = LONG_ABOUT)]
#[command(version)]
pub struct Cli {
    /// Path to config file (default: ~/.config/brunson/config.toml)
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run the background daemon (polls GitHub, serves HTTP API)
    Daemon,
    /// Run the TUI client (auto-spawns daemon if needed)
    Tui,
    /// Interactive setup: write config, validate auth, and test LLM reachability
    #[command(long_about = SETUP_LONG_ABOUT)]
    Setup(SetupArgs),
}

#[derive(Debug, Clone, Default, clap::Args)]
pub struct SetupArgs {
    /// Non-interactive mode: ensure config directory and default config exist only
    #[arg(long)]
    pub yes: bool,
    /// Output a machine-readable JSON summary with advice and config prompts for agents
    #[arg(long)]
    pub json: bool,
}
