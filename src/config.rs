use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub github: GithubConfig,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub tui: TuiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubConfig {
    #[serde(default)]
    pub watch: Vec<String>,
    #[serde(default)]
    pub targets: Vec<GithubTarget>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubTarget {
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default = "default_true")]
    pub direct_review_requests: bool,
    #[serde(default)]
    pub team_review_requests: Vec<String>,
    #[serde(default = "default_true")]
    pub include_authored: bool,
    #[serde(default)]
    pub include_involved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub kill_on_tui_exit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_llm_provider")]
    pub provider: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_classify_on_change")]
    pub classify_on_change: bool,
    #[serde(default = "default_llm_max_output_tokens")]
    pub max_output_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuiConfig {
    #[serde(default = "default_diff_style")]
    pub diff_style: String,
    #[serde(default = "default_show_line_numbers")]
    pub show_line_numbers: bool,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            watch: Vec::new(),
            targets: Vec::new(),
            poll_interval: default_poll_interval(),
        }
    }
}

impl Default for GithubTarget {
    fn default() -> Self {
        Self {
            org: None,
            repo: None,
            direct_review_requests: true,
            team_review_requests: Vec::new(),
            include_authored: true,
            include_involved: false,
        }
    }
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            kill_on_tui_exit: false,
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: default_llm_provider(),
            endpoint: String::new(),
            api_key: String::new(),
            model: String::new(),
            classify_on_change: default_classify_on_change(),
            max_output_tokens: default_llm_max_output_tokens(),
        }
    }
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            diff_style: default_diff_style(),
            show_line_numbers: default_show_line_numbers(),
        }
    }
}

fn default_poll_interval() -> u64 {
    300
}

fn default_port() -> u16 {
    17890
}

fn default_llm_provider() -> String {
    "lm_studio".to_string()
}

fn default_classify_on_change() -> bool {
    true
}

fn default_llm_max_output_tokens() -> u32 {
    4096
}

fn default_diff_style() -> String {
    "unified".to_string()
}

fn default_show_line_numbers() -> bool {
    true
}

fn default_true() -> bool {
    true
}

pub fn default_endpoint_for_provider(provider: &str) -> &'static str {
    match provider {
        "openai_compatible" => "https://api.openai.com/v1",
        _ => "http://localhost:1234/v1",
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = match path {
            Some(p) => p.to_path_buf(),
            None => config_file_path()?,
        };

        let mut config = if config_path.exists() {
            let content = std::fs::read_to_string(&config_path).with_context(|| {
                format!("Failed to read config file: {}", config_path.display())
            })?;
            toml::from_str(&content).with_context(|| {
                format!("Failed to parse config file: {}", config_path.display())
            })?
        } else {
            Config::default()
        };

        config.resolve_llm_defaults();
        config.validate()?;
        Ok(config)
    }

    /// Atomically write this config to `path` (temp file + rename), so a
    /// crash or concurrent read never observes a half-written file.
    pub fn write_atomic(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create config dir {}", parent.display()))?;
            }
        }
        let temp_path = path.with_extension("toml.tmp");
        let text = toml::to_string_pretty(self).context("Failed to serialize config")?;
        std::fs::write(&temp_path, text)
            .with_context(|| format!("Failed to write temp config to {}", temp_path.display()))?;
        std::fs::rename(&temp_path, path).with_context(|| {
            format!(
                "Failed to move {} to {}",
                temp_path.display(),
                path.display()
            )
        })?;
        Ok(())
    }

    pub fn resolve_llm_defaults(&mut self) {
        if self.llm.provider.is_empty() {
            self.llm.provider = default_llm_provider();
        }
        if self.llm.endpoint.is_empty() {
            self.llm.endpoint = default_endpoint_for_provider(&self.llm.provider).to_string();
        }
    }

    pub fn validate(&self) -> Result<()> {
        if !self.llm.provider.is_empty() {
            match self.llm.provider.as_str() {
                "lm_studio" | "openai_compatible" => {}
                other => {
                    bail!(
                        "Invalid llm.provider '{}': expected 'lm_studio' or 'openai_compatible'",
                        other
                    );
                }
            }
        }

        for entry in &self.github.watch {
            validate_scope_entry(entry, "watch entry")?;
        }

        for target in &self.github.targets {
            match (&target.org, &target.repo) {
                (Some(_), Some(_)) => bail!("GitHub target cannot set both org and repo"),
                (None, None) => bail!("GitHub target must set either org or repo"),
                (Some(org), None) => validate_org_entry(org, "target org")?,
                (None, Some(repo)) => validate_repo_entry(repo, "target repo")?,
            }

            for team in &target.team_review_requests {
                validate_repo_entry(team, "team review request")?;
            }

            if !target.direct_review_requests
                && target.team_review_requests.is_empty()
                && !target.include_authored
                && !target.include_involved
            {
                bail!("GitHub target must enable at least one relationship");
            }
        }
        Ok(())
    }
}

fn validate_scope_entry(entry: &str, label: &str) -> Result<()> {
    if entry.is_empty() {
        bail!("GitHub {} cannot be empty", label);
    }
    let parts: Vec<&str> = entry.split('/').collect();
    if parts.len() > 2 {
        bail!(
            "Invalid GitHub {} '{}': expected 'org' or 'org/repo'",
            label,
            entry
        );
    }
    for part in &parts {
        if part.is_empty() {
            bail!("Invalid GitHub {} '{}': empty segment", label, entry);
        }
    }
    Ok(())
}

fn validate_org_entry(entry: &str, label: &str) -> Result<()> {
    if entry.is_empty() || entry.contains('/') {
        bail!("Invalid GitHub {} '{}': expected 'org'", label, entry);
    }
    Ok(())
}

fn validate_repo_entry(entry: &str, label: &str) -> Result<()> {
    if entry.is_empty() {
        bail!("GitHub {} cannot be empty", label);
    }
    let parts: Vec<&str> = entry.split('/').collect();
    if parts.len() != 2 || parts.iter().any(|p| p.is_empty()) {
        bail!("Invalid GitHub {} '{}': expected 'org/repo'", label, entry);
    }
    Ok(())
}

pub fn config_dir() -> Result<PathBuf> {
    // XDG-style: $XDG_CONFIG_HOME/brunson, fallback to ~/.config/brunson
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("brunson"));
        }
    }
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .context("HOME environment variable not set")?;
    Ok(home.join(".config").join("brunson"))
}

pub fn data_dir() -> Result<PathBuf> {
    // XDG style: $XDG_DATA_HOME/brunson, fallback to ~/.local/share/brunson
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("brunson"));
        }
    }
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .context("HOME environment variable not set")?;
    Ok(home.join(".local").join("share").join("brunson"))
}

pub fn config_file_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn example_config() -> &'static str {
    r#"# brunson configuration

[github]
# Repositories to watch. Empty = all PRs involving you.
# Entries can be org names ("myorg") or org/repo pairs ("myorg/important-repo").
watch = []
# For more precise targeting, use [[github.targets]]. Empty watch plus targets
# means only these targets are searched.
# [[github.targets]]
# repo = "myorg/important-repo"
# direct_review_requests = true          # user-review-requested:@me
# team_review_requests = ["myorg/team"]  # team-review-requested:myorg/team
# include_authored = true
# include_involved = false
# Poll interval in seconds
poll_interval = 300

[daemon]
# Local HTTP API port
port = 17890
# If the TUI spawned the daemon, kill it on TUI exit
kill_on_tui_exit = false

[llm]
# Enable LLM classification
enabled = false
# Provider: "lm_studio" (default) or "openai_compatible"
provider = "lm_studio"
# OpenAI-compatible endpoint. Leave empty for provider-specific defaults:
# lm_studio -> http://localhost:1234/v1
# openai_compatible -> https://api.openai.com/v1
endpoint = ""
# API key. Required for most openai_compatible endpoints; optional for LM Studio.
api_key = ""
# Model name (empty = auto-detect via GET /v1/models)
model = ""
# Re-classify when a PR changes state
classify_on_change = true
# Maximum completion tokens for classification. Reasoning-heavy local models may need more than 200.
max_output_tokens = 4096

[tui]
# Diff style: "unified" or "side-by-side"
diff_style = "unified"
# Show line numbers in diff view
show_line_numbers = true
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.github.poll_interval, 300);
        assert_eq!(config.daemon.port, 17890);
        assert!(!config.llm.enabled);
        assert_eq!(config.llm.max_output_tokens, 4096);
        assert_eq!(config.tui.diff_style, "unified");
        assert!(config.tui.show_line_numbers);
    }

    #[test]
    fn test_parse_example_toml() {
        let config: Config = toml::from_str(example_config()).unwrap();
        assert_eq!(config.github.poll_interval, 300);
        assert_eq!(config.daemon.port, 17890);
        assert!(!config.llm.enabled);
        assert_eq!(config.llm.provider, "lm_studio");
        // Empty endpoint resolves to the LM Studio default on load.
        assert_eq!(config.llm.endpoint, "");
        assert!(config.github.watch.is_empty());
    }

    #[test]
    fn test_load_resolves_default_endpoint() {
        let dir = std::env::temp_dir().join("brunson-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("test-config-{}.toml", std::process::id()));
        std::fs::write(
            &path,
            r#"
[llm]
enabled = false
"#,
        )
        .unwrap();
        let config = Config::load(Some(&path)).unwrap();
        assert_eq!(config.llm.provider, "lm_studio");
        assert_eq!(config.llm.endpoint, "http://localhost:1234/v1");
    }

    #[test]
    fn test_resolve_llm_defaults_openai() {
        let mut config = Config::default();
        config.llm.provider = "openai_compatible".to_string();
        config.llm.endpoint.clear();
        config.resolve_llm_defaults();
        assert_eq!(config.llm.endpoint, "https://api.openai.com/v1");
    }

    #[test]
    fn test_validate_provider() {
        let mut config = Config::default();
        config.llm.provider = "lm_studio".to_string();
        assert!(config.validate().is_ok());

        config.llm.provider = "openai_compatible".to_string();
        assert!(config.validate().is_ok());

        config.llm.provider = "unknown".to_string();
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_backward_compat_without_provider_field() {
        let toml = r#"
[llm]
enabled = false
endpoint = "http://localhost:1234/v1"
model = ""
"#;
        let mut config: Config = toml::from_str(toml).unwrap();
        config.resolve_llm_defaults();
        assert_eq!(config.llm.provider, "lm_studio");
        assert_eq!(config.llm.endpoint, "http://localhost:1234/v1");
    }

    #[test]
    fn test_validate_watch_entries() {
        let mut config = Config::default();
        config.github.watch = vec!["myorg".to_string(), "myorg/repo".to_string()];
        assert!(config.validate().is_ok());

        config.github.watch = vec!["myorg/extra/slash".to_string()];
        assert!(config.validate().is_err());

        config.github.watch = vec!["".to_string()];
        assert!(config.validate().is_err());

        config.github.watch = vec!["myorg/".to_string()];
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_github_targets() {
        let mut config = Config::default();
        config.github.targets = vec![GithubTarget {
            repo: Some("myorg/repo".to_string()),
            team_review_requests: vec!["myorg/agentic-engineering".to_string()],
            ..Default::default()
        }];
        assert!(config.validate().is_ok());

        config.github.targets = vec![GithubTarget {
            org: Some("myorg".to_string()),
            repo: Some("myorg/repo".to_string()),
            ..Default::default()
        }];
        assert!(config.validate().is_err());

        config.github.targets = vec![GithubTarget {
            repo: Some("myorg/repo".to_string()),
            team_review_requests: vec!["agentic-engineering".to_string()],
            ..Default::default()
        }];
        assert!(config.validate().is_err());

        config.github.targets = vec![GithubTarget {
            repo: Some("myorg/repo".to_string()),
            direct_review_requests: false,
            include_authored: false,
            include_involved: false,
            ..Default::default()
        }];
        assert!(config.validate().is_err());
    }
}
