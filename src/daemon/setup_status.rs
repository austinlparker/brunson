use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::future::BoxFuture;

use crate::api::{AuthStatus, LlmSetupStatus, SetupStatusResponse};
use crate::config::{config_file_path, Config, LlmConfig};
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

/// Run lightweight setup diagnostics against the current configuration.
pub async fn evaluate_setup(
    _config: &Config,
    config_path: Option<&Path>,
    auth: &dyn SetupAuth,
) -> SetupStatusResponse {
    let resolved_path: PathBuf = config_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| config_file_path().unwrap_or_default());

    if resolved_path.as_os_str().is_empty() || !resolved_path.exists() {
        return SetupStatusResponse {
            ready: false,
            status: "missing_config".to_string(),
            auth: AuthStatus::default(),
            llm: LlmSetupStatus::default(),
            next_steps: vec![format!(
                "Run `brunson setup` to create {}",
                resolved_path.display()
            )],
        };
    }

    // Validate that the on-disk config can be parsed.
    let config = match Config::load(Some(&resolved_path)) {
        Ok(c) => c,
        Err(e) => {
            return SetupStatusResponse {
                ready: false,
                status: "missing_config".to_string(),
                auth: AuthStatus::default(),
                llm: LlmSetupStatus::default(),
                next_steps: vec![format!(
                    "Config file at {} is invalid: {}. Run `brunson setup` to fix it.",
                    resolved_path.display(),
                    e
                )],
            };
        }
    };

    // Check GitHub auth.
    let (auth, mut next_steps) = match auth.resolve_token() {
        Ok(token) => {
            let host = resolve_host();
            match auth.viewer_login(&token, &host).await {
                Ok(user) => {
                    let auth = AuthStatus {
                        resolved: true,
                        source: token_source(),
                        user: Some(user),
                    };
                    (auth, Vec::new())
                }
                Err(e) => {
                    let mut steps = Vec::new();
                    steps.push(format!(
                        "GitHub token resolved but viewer login failed: {}. Check GH_HOST and token scopes.",
                        e
                    ));
                    (
                        AuthStatus {
                            resolved: true,
                            source: token_source(),
                            user: None,
                        },
                        steps,
                    )
                }
            }
        }
        Err(e) => {
            let mut steps = Vec::new();
            steps.push(format!(
                "GitHub auth is missing or invalid: {}. Run `gh auth login` or set GH_TOKEN.",
                e
            ));
            (AuthStatus::default(), steps)
        }
    };

    let mut status = if auth.resolved && auth.user.is_some() {
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
                if config.llm.enabled {
                    status = "llm_misconfigured".to_string();
                    next_steps.push(
                        "LLM is enabled but unreachable. Check endpoint and api_key in [llm]."
                            .to_string(),
                    );
                }
            }
        }
    } else {
        llm.reachable = Some(true);
    }

    let ready = status == "ready"
        && auth.resolved
        && auth.user.is_some()
        && (!config.llm.enabled || llm.reachable == Some(true));

    if ready && next_steps.is_empty() {
        next_steps.push(
            "Run `brunson daemon` (if it is not already running) and `brunson tui` to start."
                .to_string(),
        );
    }

    SetupStatusResponse {
        ready,
        status: if ready { "ready".to_string() } else { status },
        auth,
        llm,
        next_steps,
    }
}

async fn check_llm_reachable(config: &LlmConfig) -> Result<String> {
    let mut classifier = Classifier::new(config)?;
    classifier.resolve_model().await?;
    Ok(classifier.model().unwrap_or_default().to_string())
}
