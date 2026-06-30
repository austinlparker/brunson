use std::io::{self, Write};
use std::process::Command;

use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::SetupArgs;
use crate::config::{
    config_dir, config_file_path, default_endpoint_for_provider, Config, DaemonConfig,
    GithubConfig, LlmConfig, TuiConfig,
};
use crate::daemon::setup_status::{evaluate_setup, SetupAuthImpl};
use crate::github::auth::resolve_token;

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

    println!("Brunson interactive setup");
    println!("=========================");

    // 1. Confirm config path.
    let config_path = prompt(
        &format!("Config file path [{}]", default_path.display()),
        default_path.to_string_lossy().to_string(),
    )?;
    let config_path = std::path::PathBuf::from(config_path);
    if config_path.parent().is_some_and(|p| !p.exists()) {
        std::fs::create_dir_all(config_path.parent().unwrap())?;
    }

    // 2. Check GitHub auth.
    let mut auth_ok = resolve_token().is_ok();
    if !auth_ok {
        println!("\nGitHub auth not detected. You need `gh auth login` or a GH_TOKEN env var.");
        let run_login = prompt_y_n("Run `gh auth login` now?", true)?;
        if run_login {
            let status = Command::new("gh")
                .args(["auth", "login"])
                .status()
                .context("Failed to run `gh auth login`. Is GitHub CLI installed?")?;
            if !status.success() {
                println!("`gh auth login` did not complete successfully; continuing anyway.");
            }
            auth_ok = resolve_token().is_ok();
        }
    }
    if auth_ok {
        println!("GitHub auth detected.");
    } else {
        println!("\nWarning: GitHub auth is still missing. The daemon will start but \n         will report `missing_auth` until you authenticate.");
    }

    // 3. Watch list.
    let watch_all = prompt_y_n("Watch all repos involving you?", true)?;
    let watch = if watch_all {
        Vec::new()
    } else {
        let raw = prompt(
            "Enter repositories to watch (comma-separated, e.g. myorg,myorg/repo)",
            String::new(),
        )?;
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    };

    // 4. LLM configuration.
    let enable_llm = prompt_y_n("Enable LLM classification?", false)?;
    let llm = if enable_llm {
        let provider_choice = prompt(
            "LLM provider [1=lm_studio (local), 2=openai_compatible]",
            "1".to_string(),
        )?;
        let provider = match provider_choice.trim() {
            "2" => "openai_compatible".to_string(),
            _ => "lm_studio".to_string(),
        };

        let default_endpoint = default_endpoint_for_provider(&provider).to_string();
        let endpoint = prompt(
            &format!("LLM endpoint [{}]", default_endpoint),
            default_endpoint,
        )?;

        let api_key = if provider == "openai_compatible" {
            let env_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
            let key = prompt(
                &format!(
                    "API key [{}]",
                    if env_key.is_empty() {
                        "required".to_string()
                    } else {
                        "from OPENAI_API_KEY".to_string()
                    }
                ),
                env_key,
            )?;
            key
        } else {
            prompt("API key (optional for LM Studio)", String::new())?
        };

        let model = prompt("Model name (empty = auto-detect)", String::new())?;

        LlmConfig {
            enabled: true,
            provider,
            endpoint,
            api_key,
            model,
            classify_on_change: true,
            max_output_tokens: 4096,
        }
    } else {
        LlmConfig::default()
    };

    let config = Config {
        github: GithubConfig {
            watch,
            targets: Vec::new(),
            poll_interval: 300,
        },
        daemon: DaemonConfig::default(),
        llm,
        tui: TuiConfig::default(),
    };

    if let Err(e) = config.validate() {
        anyhow::bail!("Generated config is invalid: {}", e);
    }

    // 5. Write config atomically.
    let temp_path = config_path.with_extension("toml.tmp");
    {
        let mut file = std::fs::File::create(&temp_path)
            .with_context(|| format!("Failed to create temp file at {}", temp_path.display()))?;
        file.write_all(toml::to_string_pretty(&config)?.as_bytes())
            .with_context(|| format!("Failed to write temp config to {}", temp_path.display()))?;
    }
    std::fs::rename(&temp_path, &config_path).with_context(|| {
        format!(
            "Failed to move {} to {}",
            temp_path.display(),
            config_path.display()
        )
    })?;
    println!("\nWrote config to {}", config_path.display());

    // 6. Run diagnostics on the written config.
    let status = evaluate_setup(
        &Config::load(Some(&config_path))?,
        Some(&config_path),
        &SetupAuthImpl,
    )
    .await;
    println!("Setup status: {}", status.status);
    if !status.next_steps.is_empty() {
        println!("Next steps:");
        for step in &status.next_steps {
            println!("  - {}", step);
        }
    }

    // 7. If a daemon is already running on the configured port, ask it to reload.
    if daemon_running(config.daemon.port).await {
        println!("Daemon detected; sending /config/reload...");
        if let Err(e) = reload_daemon(config.daemon.port).await {
            println!(
                "Reload failed: {}. Start or restart the daemon manually.",
                e
            );
        } else {
            println!("Daemon reloaded successfully.");
        }
    } else {
        println!("No daemon detected. Run `brunson daemon` to start it.");
    }

    Ok(())
}

fn prompt(prompt_text: &str, default: String) -> Result<String> {
    print!("{}: ", prompt_text);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(default)
    } else {
        Ok(trimmed.to_string())
    }
}

fn prompt_y_n(prompt_text: &str, default_yes: bool) -> Result<bool> {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    loop {
        print!("{} {}: ", prompt_text, hint);
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        match input.trim().to_lowercase().as_str() {
            "" => return Ok(default_yes),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("Please answer y or n."),
        }
    }
}

async fn daemon_running(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{}/health", port);
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(800))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(&url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn reload_daemon(port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{}/config/reload", port);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client.post(&url).send().await?;
    if resp.status().is_success() {
        Ok(())
    } else {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Daemon returned: {}", body)
    }
}
