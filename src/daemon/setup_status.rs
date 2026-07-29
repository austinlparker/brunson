use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::future::BoxFuture;

use crate::api::{AuthStatus, LlmSetupStatus, SetupStatusResponse};
use crate::config::{config_file_path, default_endpoint_for_provider, Config, LlmConfig};
use crate::github::auth::{resolve_host, resolve_token};
use crate::github::client::GitHubClient;
use crate::github::graphql::fetch_viewer_login;
use crate::llm::classifier::Classifier;

/// TTL for cached setup diagnostics.
pub const SETUP_CACHE_TTL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default)]
pub struct SetupCache {
    pub cached_at: Option<Instant>,
    pub result: SetupStatusResponse,
}

impl SetupCache {
    pub fn is_fresh(&self) -> bool {
        match self.cached_at {
            Some(t) => t.elapsed() < SETUP_CACHE_TTL,
            None => false,
        }
    }
}

/// Injectable auth dependency used by setup diagnostics.
pub trait SetupAuth: Send + Sync {
    fn resolve_token(&self) -> Result<String>;
    fn viewer_login<'a>(&'a self, token: &'a str, host: &'a str) -> BoxFuture<'a, Result<String>>;
}

pub struct SetupAuthImpl;

impl SetupAuth for SetupAuthImpl {
    fn resolve_token(&self) -> Result<String> {
        resolve_token()
    }

    fn viewer_login<'a>(&'a self, token: &'a str, host: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let client = GitHubClient::new(token.to_string(), Some(host.to_string()))?;
            fetch_viewer_login(&client).await
        })
    }
}

/// Build an easy-to-understand label for where the token came from.
fn token_source() -> Option<String> {
    if std::env::var("GH_TOKEN").is_ok_and(|v| !v.is_empty()) {
        return Some("GH_TOKEN".to_string());
    }
    if std::env::var("GITHUB_TOKEN").is_ok_and(|v| !v.is_empty()) {
        return Some("GITHUB_TOKEN".to_string());
    }
    Some("gh".to_string())
}

/// One actionable setup problem. This single structured model feeds every
/// setup surface: the `next_steps` strings of `/setup/status` and the
/// `prompts`/`advice` fields of `brunson setup --json` are both derived
/// from it.
#[derive(Debug, Clone)]
pub struct SetupIssue {
    /// Config-shaped field identifier, e.g. "github.auth", "llm.api_key".
    pub field: String,
    pub description: String,
    pub current_value: Option<String>,
    pub example: String,
    /// One actionable sentence.
    pub advice: String,
}

/// Result of setup diagnostics: the wire-shaped status plus the structured
/// issues it was derived from. `/setup/status` serializes only `status`.
#[derive(Debug, Clone)]
pub struct SetupDiagnostics {
    pub status: SetupStatusResponse,
    pub issues: Vec<SetupIssue>,
}

/// One next-step line per issue, plus the start hint when everything is ready.
fn next_steps_from(issues: &[SetupIssue], ready: bool) -> Vec<String> {
    let mut steps: Vec<String> = issues.iter().map(|issue| issue.advice.clone()).collect();
    if ready && steps.is_empty() {
        steps.push(
            "Run `brunson daemon` (if it is not already running) and `brunson tui` to start."
                .to_string(),
        );
    }
    steps
}

fn config_file_diagnostics(issue: SetupIssue) -> SetupDiagnostics {
    let issues = vec![issue];
    SetupDiagnostics {
        status: SetupStatusResponse {
            ready: false,
            status: "missing_config".to_string(),
            auth: AuthStatus::default(),
            llm: LlmSetupStatus::default(),
            next_steps: next_steps_from(&issues, false),
        },
        issues,
    }
}

/// Run lightweight setup diagnostics against the on-disk configuration.
pub async fn evaluate_setup(config_path: Option<&Path>, auth: &dyn SetupAuth) -> SetupDiagnostics {
    let resolved_path: PathBuf = config_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| config_file_path().unwrap_or_default());

    if resolved_path.as_os_str().is_empty() || !resolved_path.exists() {
        return config_file_diagnostics(SetupIssue {
            field: "config_file".to_string(),
            description: "The brunson configuration file path.".to_string(),
            current_value: Some(resolved_path.display().to_string()),
            example: "~/.config/brunson/config.toml".to_string(),
            advice: format!(
                "No brunson config file exists. Run `brunson setup --yes` to create {}.",
                resolved_path.display()
            ),
        });
    }

    // Validate that the on-disk config can be parsed.
    let config = match Config::load(Some(&resolved_path)) {
        Ok(c) => c,
        Err(e) => {
            return config_file_diagnostics(SetupIssue {
                field: "config_file".to_string(),
                description: "The brunson configuration file could not be parsed.".to_string(),
                current_value: Some(resolved_path.display().to_string()),
                example: "~/.config/brunson/config.toml".to_string(),
                advice: format!(
                    "Config file at {} is invalid: {}. Fix the TOML, or delete it and run `brunson setup --yes` to regenerate it.",
                    resolved_path.display(),
                    e
                ),
            });
        }
    };

    let mut issues: Vec<SetupIssue> = Vec::new();

    // Check GitHub auth.
    let auth_status = match auth.resolve_token() {
        Ok(token) => {
            let host = resolve_host();
            match auth.viewer_login(&token, &host).await {
                Ok(user) => AuthStatus {
                    resolved: true,
                    source: token_source(),
                    user: Some(user),
                },
                Err(e) => {
                    issues.push(SetupIssue {
                        field: "github.auth".to_string(),
                        description: "GitHub personal access token or gh CLI authentication."
                            .to_string(),
                        current_value: token_source(),
                        example: "gh auth login  (or export GH_TOKEN=ghp_xxx)".to_string(),
                        advice: format!(
                            "GitHub token resolved but viewer login failed: {}. Check GH_HOST and token scopes.",
                            e
                        ),
                    });
                    AuthStatus {
                        resolved: true,
                        source: token_source(),
                        user: None,
                    }
                }
            }
        }
        Err(e) => {
            issues.push(SetupIssue {
                field: "github.auth".to_string(),
                description: "GitHub personal access token or gh CLI authentication.".to_string(),
                current_value: None,
                example: "gh auth login  (or export GH_TOKEN=ghp_xxx)".to_string(),
                advice: format!(
                    "GitHub auth is missing or invalid: {}. Run `gh auth login` or set GH_TOKEN.",
                    e
                ),
            });
            AuthStatus::default()
        }
    };

    let mut status = if auth_status.resolved && auth_status.user.is_some() {
        "ready".to_string()
    } else {
        "missing_auth".to_string()
    };

    let mut llm = LlmSetupStatus {
        enabled: config.llm.enabled,
        reachable: None,
        model: Some(config.llm.model.clone()).filter(|s| !s.is_empty()),
        message: None,
    };

    if config.llm.enabled {
        match check_llm_reachable(&config.llm).await {
            Ok(model) => {
                llm.reachable = Some(true);
                if !model.is_empty() {
                    llm.model = Some(model);
                }
            }
            Err(e) => {
                llm.reachable = Some(false);
                llm.message = Some(format!("LLM check failed: {}", e));
                status = "llm_misconfigured".to_string();
                if config.llm.provider == "openai_compatible" && config.llm.api_key.is_empty() {
                    issues.push(SetupIssue {
                        field: "llm.api_key".to_string(),
                        description: "API key for the OpenAI-compatible endpoint.".to_string(),
                        current_value: None,
                        example: "export OPENAI_API_KEY=sk-xxx".to_string(),
                        advice: "LLM is enabled but [llm] api_key is empty. Set the API key for the OpenAI-compatible endpoint.".to_string(),
                    });
                }
                if config.llm.model.is_empty() {
                    issues.push(SetupIssue {
                        field: "llm.model".to_string(),
                        description: "Model name to use for classification.".to_string(),
                        current_value: Some("(auto-detect)".to_string()),
                        example: "gpt-4o-mini".to_string(),
                        advice: "No [llm] model is set and auto-detection failed. Set the model explicitly.".to_string(),
                    });
                }
                issues.push(SetupIssue {
                    field: "llm.endpoint".to_string(),
                    description: "OpenAI-compatible endpoint URL.".to_string(),
                    current_value: Some(if config.llm.endpoint.is_empty() {
                        default_endpoint_for_provider(&config.llm.provider).to_string()
                    } else {
                        config.llm.endpoint.clone()
                    }),
                    example: "https://api.openai.com/v1".to_string(),
                    advice: "LLM is enabled but unreachable. Check endpoint and api_key in [llm]."
                        .to_string(),
                });
            }
        }
    } else {
        llm.reachable = Some(true);
    }

    let ready = status == "ready"
        && auth_status.resolved
        && auth_status.user.is_some()
        && (!config.llm.enabled || llm.reachable == Some(true));

    if !ready && config.github.watch.is_empty() {
        issues.push(SetupIssue {
            field: "github.watch".to_string(),
            description:
                "Repositories/orgs to watch. Empty means every PR involving the authenticated user."
                    .to_string(),
            current_value: Some("(all repos)".to_string()),
            example: "[\"myorg\", \"myorg/important-repo\"]".to_string(),
            advice: "github.watch is empty, so every PR involving the authenticated user is tracked. Add repos/orgs to narrow the scope (optional).".to_string(),
        });
    }

    SetupDiagnostics {
        status: SetupStatusResponse {
            ready,
            status: if ready { "ready".to_string() } else { status },
            auth: auth_status,
            llm,
            next_steps: next_steps_from(&issues, ready),
        },
        issues,
    }
}

async fn check_llm_reachable(config: &LlmConfig) -> Result<String> {
    let mut classifier = Classifier::new(config)?;
    classifier.resolve_model().await?;
    Ok(classifier.model().unwrap_or_default().to_string())
}
