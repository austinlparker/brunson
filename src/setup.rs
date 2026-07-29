use std::io::Write;
use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::cli::SetupArgs;
use crate::config::{config_dir, config_file_path};
use crate::daemon::setup_status::{evaluate_setup, SetupAuthImpl, SetupDiagnostics};

/// JSON summary emitted when `--json` is requested. The `prompts` and
/// `advice` fields are derived from the shared `SetupIssue` model produced
/// by `evaluate_setup` — the same model that feeds `/setup/status`.
fn json_summary(config_path: &Path, diagnostics: &SetupDiagnostics) -> String {
    let status = &diagnostics.status;
    let prompts: Vec<serde_json::Value> = diagnostics
        .issues
        .iter()
        .map(|issue| {
            json!({
                "field": issue.field,
                "description": issue.description,
                "current_value": issue.current_value,
                "example": issue.example,
            })
        })
        .collect();
    let advice = if status.ready {
        "Configuration is ready. Start the daemon with `brunson daemon` and the TUI with `brunson tui`.".to_string()
    } else {
        diagnostics
            .issues
            .first()
            .map(|issue| issue.advice.clone())
            .unwrap_or_else(|| {
                "Configuration incomplete. Review the next_steps and prompts, then update the config and call `brunson setup --json` again.".to_string()
            })
    };
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
        // Evaluate unconditionally: a malformed config must still produce a
        // JSON summary (with a config_file prompt carrying the parse error)
        // instead of aborting before any output.
        let diagnostics = evaluate_setup(Some(&default_path), &SetupAuthImpl).await;
        println!("{}", json_summary(&default_path, &diagnostics));
        return Ok(());
    }

    eprintln!(
        "Interactive setup has moved into the TUI. Run `brunson tui` — it launches the \
         wizard automatically on first run, or press 'w' from inside it to reopen it any \
         time. For non-interactive/scripted setup, pass --yes or --json."
    );
    anyhow::bail!("interactive setup is no longer available via the CLI");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::SetupStatusResponse;
    use crate::daemon::setup_status::SetupIssue;

    const JSON_KEYS: [&str; 6] = [
        "ready",
        "status",
        "config_path",
        "next_steps",
        "advice",
        "prompts",
    ];

    #[test]
    fn setup_json_keys_and_prompts_derive_from_issues() {
        let issues = vec![
            SetupIssue {
                field: "github.auth".to_string(),
                description: "GitHub personal access token or gh CLI authentication.".to_string(),
                current_value: None,
                example: "gh auth login  (or export GH_TOKEN=ghp_xxx)".to_string(),
                advice: "GitHub auth is missing. Run `gh auth login`.".to_string(),
            },
            SetupIssue {
                field: "llm.endpoint".to_string(),
                description: "OpenAI-compatible endpoint URL.".to_string(),
                current_value: Some("https://api.openai.com/v1".to_string()),
                example: "https://api.openai.com/v1".to_string(),
                advice: "LLM is enabled but unreachable.".to_string(),
            },
        ];
        let diagnostics = SetupDiagnostics {
            status: SetupStatusResponse {
                ready: false,
                status: "missing_auth".to_string(),
                next_steps: issues.iter().map(|i| i.advice.clone()).collect(),
                ..Default::default()
            },
            issues,
        };

        let summary = json_summary(Path::new("/tmp/config.toml"), &diagnostics);
        let value: serde_json::Value = serde_json::from_str(&summary).unwrap();

        for key in JSON_KEYS {
            assert!(value.get(key).is_some(), "missing top-level key {}", key);
        }
        assert_eq!(value["ready"], false);
        assert_eq!(value["status"], "missing_auth");
        // Advice is the first issue's advice.
        assert_eq!(value["advice"], "GitHub auth is missing. Run `gh auth login`.");

        // Prompts correspond 1:1 to issues, with the four prompt keys.
        let prompts = value["prompts"].as_array().unwrap();
        assert_eq!(prompts.len(), diagnostics.issues.len());
        for (prompt, issue) in prompts.iter().zip(&diagnostics.issues) {
            assert_eq!(prompt["field"], issue.field.as_str());
            assert_eq!(prompt["description"], issue.description.as_str());
            assert_eq!(
                prompt["current_value"],
                issue
                    .current_value
                    .as_deref()
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null)
            );
            assert_eq!(prompt["example"], issue.example.as_str());
        }
    }

    #[tokio::test]
    async fn setup_json_reports_malformed_config() {
        struct NoAuth;
        impl crate::daemon::setup_status::SetupAuth for NoAuth {
            fn resolve_token(&self) -> Result<String> {
                anyhow::bail!("no auth in test")
            }
            fn viewer_login<'a>(
                &'a self,
                _token: &'a str,
                _host: &'a str,
            ) -> futures::future::BoxFuture<'a, Result<String>> {
                Box::pin(async { anyhow::bail!("no auth in test") })
            }
        }

        let dir = std::env::temp_dir().join(format!(
            "brunson-setup-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        std::fs::write(&config_path, "this is [not valid toml").unwrap();

        let diagnostics = evaluate_setup(Some(&config_path), &NoAuth).await;
        let summary = json_summary(&config_path, &diagnostics);
        let value: serde_json::Value = serde_json::from_str(&summary).unwrap();

        for key in JSON_KEYS {
            assert!(value.get(key).is_some(), "missing top-level key {}", key);
        }
        assert_eq!(value["ready"], false);
        assert_eq!(value["status"], "missing_config");

        // A config_file prompt exists and the advice carries the parse error.
        let prompts = value["prompts"].as_array().unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0]["field"], "config_file");
        let advice = value["advice"].as_str().unwrap();
        assert!(
            advice.contains("invalid"),
            "advice should carry the parse failure: {}",
            advice
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
