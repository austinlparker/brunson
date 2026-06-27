use anyhow::{anyhow, Context, Result};
use std::process::Command;

/// Resolve the GitHub host from GH_HOST env or default to github.com.
pub fn resolve_host() -> String {
    std::env::var("GH_HOST").unwrap_or_else(|_| "github.com".to_string())
}

/// Resolve a GitHub auth token via: GH_TOKEN → GITHUB_TOKEN → `gh auth token`.
/// Returns an error with actionable guidance if all methods fail.
pub fn resolve_token() -> Result<String> {
    // 1. GH_TOKEN env var
    if let Ok(token) = std::env::var("GH_TOKEN") {
        if !token.is_empty() {
            return Ok(token);
        }
    }

    // 2. GITHUB_TOKEN env var
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.is_empty() {
            return Ok(token);
        }
    }

    // 3. gh auth token
    let output = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .context("Failed to execute `gh` command. Is GitHub CLI installed?")?;

    if output.status.success() {
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    Err(anyhow!(
        "No GitHub token found. Tried GH_TOKEN, GITHUB_TOKEN, and `gh auth token`.\n\
         Run `gh auth login` to authenticate with GitHub CLI, or set GH_TOKEN."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to serialize env-var tests
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("GITHUB_TOKEN");
    }

    #[test]
    fn test_resolve_host_default() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::remove_var("GH_HOST");
        assert_eq!(resolve_host(), "github.com");
    }

    #[test]
    fn test_resolve_host_custom() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("GH_HOST", "github.company.com");
        assert_eq!(resolve_host(), "github.company.com");
        std::env::remove_var("GH_HOST");
    }

    #[test]
    fn test_resolve_token_from_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("GH_TOKEN", "ghp_testtoken123");
        let token = resolve_token().unwrap();
        assert_eq!(token, "ghp_testtoken123");
        clear_env();
    }

    #[test]
    fn test_resolve_token_from_github_token_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var("GITHUB_TOKEN", "ghp_envtoken456");
        let token = resolve_token().unwrap();
        assert_eq!(token, "ghp_envtoken456");
        clear_env();
    }
}
