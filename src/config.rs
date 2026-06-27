use anyhow::{Context, Result};
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
    #[serde(default = "default_poll_interval")]
    pub poll_interval: u64,
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
    #[serde(default = "default_llm_endpoint")]
    pub endpoint: String,
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
    #[serde(default = "default_osc8_links")]
    pub osc8_links: bool,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            watch: Vec::new(),
            poll_interval: default_poll_interval(),
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
            endpoint: default_llm_endpoint(),
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
            osc8_links: default_osc8_links(),
        }
    }
}

fn default_poll_interval() -> u64 {
    300
}
fn default_port() -> u16 {
    17890
}
fn default_llm_endpoint() -> String {
    "http://localhost:1234/v1".to_string()
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
fn default_osc8_links() -> bool {
    true
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let config_path = match path {
            Some(p) => p.to_path_buf(),
            None => config_file_path()?,
        };

        if !config_path.exists() {
            return Ok(Config::default());
        }

        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", config_path.display()))?;

        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        for entry in &self.github.watch {
            if entry.is_empty() {
                anyhow::bail!("Watch entry cannot be empty");
            }
            // Validate: either "org" or "org/repo"
            let parts: Vec<&str> = entry.split('/').collect();
            if parts.len() > 2 {
                anyhow::bail!(
                    "Invalid watch entry '{}': expected 'org' or 'org/repo'",
                    entry
                );
            }
            for part in &parts {
                if part.is_empty() {
                    anyhow::bail!("Invalid watch entry '{}': empty segment", entry);
                }
            }
        }
        Ok(())
    }
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
# Poll interval in seconds
poll_interval = 300

[daemon]
# Local HTTP API port
port = 17890
# If the TUI spawned the daemon, kill it on TUI exit
kill_on_tui_exit = false

[llm]
# Enable LLM classification via LM Studio
enabled = false
# LM Studio OpenAI-compatible endpoint
endpoint = "http://localhost:1234/v1"
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
# Emit OSC 8 terminal hyperlinks for PR/file titles
osc8_links = true
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
        assert!(config.tui.osc8_links);
    }

    #[test]
    fn test_parse_example_toml() {
        let config: Config = toml::from_str(example_config()).unwrap();
        assert_eq!(config.github.poll_interval, 300);
        assert_eq!(config.daemon.port, 17890);
        assert!(!config.llm.enabled);
        assert_eq!(config.llm.endpoint, "http://localhost:1234/v1");
        assert!(config.github.watch.is_empty());
        assert!(config.tui.osc8_links);
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
}
