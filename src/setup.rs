use std::io::Write;

use anyhow::Result;
use serde_json::json;

use crate::cli::SetupArgs;
use crate::config::{config_dir, config_file_path, default_endpoint_for_provider, Config};
use crate::daemon::setup_status::{evaluate_setup, SetupAuthImpl};

/// JSON summary emitted when `--json` is requested.
fn json_summary(
    config_path: &std::path::Path,
    config: &Config,
    status: &crate::api::SetupStatusResponse,
) -> String {
    let (advice, prompts) = build_agent_advice(config, status);
    json!({
        "ready": status.ready,
        "status": status.status,
        "config_path": config_path.to_string_lossy().to_string(),
        "next_steps": status.next_steps,
        "advice": advice,
        "prompts": prompts,
    })
    .to_string()
}

/// Build human-readable advice and structured prompts for an agent.
fn build_agent_advice(
    config: &Config,
    status: &crate::api::SetupStatusResponse,
) -> (String, Vec<serde_json::Value>) {
    let mut advice = String::new();
    let mut prompts: Vec<serde_json::Value> = Vec::new();

    if status.ready {
        return (
            "Configuration is ready. Start the daemon with `brunson daemon` and the TUI with `brunson tui`.".to_string(),
            prompts,
        );
    }

    if status.status == "missing_config" {
        advice = "No brunson config file exists. The default config has been written; edit it and call `brunson setup --json` again after fixing the required fields.".to_string();
        prompts.push(json!({
            "field": "config_file",
            "description": "The brunson configuration file path.",
            "current_value": default_config_path(),
            "example": "~/.config/brunson/config.toml"
        }));
    }

    if !status.auth.resolved {
        advice = "GitHub authentication is required. Ask the user to run `gh auth login` in a shell, or set the GH_TOKEN environment variable.".to_string();
        prompts.push(json!({
            "field": "github.auth",
            "description": "GitHub personal access token or gh CLI authentication.",
            "current_value": null,
            "example": "gh auth login  (or export GH_TOKEN=ghp_xxx)"
        }));
    }

    if status.status == "llm_misconfigured"
        || (config.llm.enabled && status.llm.reachable != Some(true))
    {
        advice = "LLM classification is enabled but not reachable. Verify the [llm] provider, endpoint, api_key, and model fields.".to_string();
        if config.llm.provider == "openai_compatible" && config.llm.api_key.is_empty() {
            prompts.push(json!({
                "field": "llm.api_key",
                "description": "API key for the OpenAI-compatible endpoint.",
                "current_value": null,
                "example": "export OPENAI_API_KEY=sk-xxx"
            }));
        }
        if config.llm.model.is_empty() {
            prompts.push(json!({
                "field": "llm.model",
                "description": "Model name to use for classification.",
                "current_value": "(auto-detect)",
                "example": "gpt-4o-mini"
            }));
        }
        prompts.push(json!({
            "field": "llm.endpoint",
            "description": "OpenAI-compatible endpoint URL.",
            "current_value": if config.llm.endpoint.is_empty() { default_endpoint_for_provider(&config.llm.provider) } else { &config.llm.endpoint },
            "example": "https://api.openai.com/v1"
        }));
    }

    if config.github.watch.is_empty() {
        prompts.push(json!({
            "field": "github.watch",
            "description": "Repositories/orgs to watch. Empty means every PR involving the authenticated user.",
            "current_value": "(all repos)",
            "example": "[\"myorg\", \"myorg/important-repo\"]"
        }));
    }

    if advice.is_empty() {
        advice = "Configuration incomplete. Review the next_steps and prompts, then update the config and call `brunson setup --json` again.".to_string();
    }

    (advice, prompts)
}

fn default_config_path() -> String {
    config_file_path()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "~/.config/brunson/config.toml".to_string())
}

pub async fn run_setup(args: &SetupArgs) -> Result<()> {
    let default_path =
        config_file_path().unwrap_or_else(|_| std::path::PathBuf::from("config.toml"));
    let config_dir = config_dir()?;
    std::fs::create_dir_all(&config_dir)?;

    if args.yes || args.json {
        if !default_path.exists() {
            let mut file = std::fs::File::create(&default_path)?;
            file.write_all(crate::config::example_config().as_bytes())?;
        }
        let config = Config::load(Some(&default_path))?;
        let status = evaluate_setup(&config, Some(&default_path), &SetupAuthImpl).await;
        println!("{}", json_summary(&default_path, &config, &status));
        return Ok(());
    }

    eprintln!(
        "Interactive setup has moved into the TUI. Run `brunson tui` — it launches the \
         wizard automatically on first run, or press 'w' from inside it to reopen it any \
         time. For non-interactive/scripted setup, pass --yes or --json."
    );
    anyhow::bail!("interactive setup is no longer available via the CLI");
}
