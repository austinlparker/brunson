use anyhow::{anyhow, Result};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tracing::{debug, warn};

use super::auth::resolve_host;
use super::types::SearchResult;

/// GitHub API rate limit info extracted from response headers.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct RateLimitInfo {
    pub remaining: Option<u32>,
}

/// Wrapper around reqwest::Client with GitHub auth and base URL resolution.
#[derive(Clone)]
pub struct GitHubClient {
    client: Client,
    token: String,
    rest_base: String,
    graphql_base: String,
    rate_limit_remaining: Arc<AtomicU32>,
}

impl GitHubClient {
    pub fn new(token: String, host: Option<String>) -> Result<Self> {
        let host = host.unwrap_or_else(resolve_host);
        let (rest_base, graphql_base) = resolve_api_urls(&host);

        let client = Client::builder()
            .user_agent(format!(
                "{}/{}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION")
            ))
            .default_headers({
                let mut headers = reqwest::header::HeaderMap::new();
                let auth_value =
                    reqwest::header::HeaderValue::from_str(&format!("Bearer {}", token))
                        .map_err(|e| anyhow!("Invalid token for header: {}", e))?;
                headers.insert(reqwest::header::AUTHORIZATION, auth_value);
                headers
            })
            .build()?;

        Ok(Self {
            client,
            token,
            rest_base,
            graphql_base,
            rate_limit_remaining: Arc::new(AtomicU32::new(5000)),
        })
    }

    /// Returns a reference to the underlying token (for internal use only).
    #[allow(dead_code)]
    pub fn token(&self) -> &str {
        &self.token
    }

    #[allow(dead_code)]
    pub fn rest_base(&self) -> &str {
        &self.rest_base
    }

    #[allow(dead_code)]
    pub fn graphql_base(&self) -> &str {
        &self.graphql_base
    }

    pub fn rate_limit_remaining(&self) -> u32 {
        self.rate_limit_remaining.load(Ordering::Relaxed)
    }

    fn update_rate_limit(&self, resp: &reqwest::Response) {
        if let Some(remaining) = resp.headers().get("X-RateLimit-Remaining") {
            if let Ok(val_str) = remaining.to_str() {
                if let Ok(val) = val_str.parse::<u32>() {
                    self.rate_limit_remaining.store(val, Ordering::Relaxed);
                    if val < 100 {
                        warn!("GitHub rate limit low: {} remaining", val);
                    }
                }
            }
        }
    }

    /// Search PRs using the GitHub search API.
    pub async fn search_prs(&self, query: &str, page: u32) -> Result<SearchResponse> {
        let url = format!("{}/search/issues", self.rest_base);
        debug!("Search query: {}", query);

        let resp = self
            .client
            .get(&url)
            .query(&[
                ("q", query),
                ("sort", "updated"),
                ("order", "desc"),
                ("per_page", "100"),
                ("page", &page.to_string()),
            ])
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?;

        self.update_rate_limit(&resp);

        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            return Err(anyhow!("GitHub search rate limit exceeded"));
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Search failed ({}): {}", status, body));
        }

        let search_resp: SearchResponse = resp.json().await?;
        Ok(search_resp)
    }

    /// Get a raw diff for a PR.
    pub async fn get_pr_diff(&self, owner: &str, repo: &str, number: u64) -> Result<String> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            self.rest_base, owner, repo, number
        );

        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github.v3.diff")
            .send()
            .await?;

        self.update_rate_limit(&resp);

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Diff fetch failed ({}): {}", status, body));
        }

        Ok(resp.text().await?)
    }

    /// Send a GraphQL query.
    pub async fn graphql(&self, query: &str) -> Result<serde_json::Value> {
        self.graphql_with_variables(query, serde_json::json!({}))
            .await
    }

    /// Send a GraphQL query with variables.
    pub async fn graphql_with_variables(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let url = self.graphql_base.clone();
        let body = serde_json::json!({ "query": query, "variables": variables });

        let resp = self
            .client
            .post(&url)
            .header("Accept", "application/vnd.github+json")
            .json(&body)
            .send()
            .await?;

        self.update_rate_limit(&resp);

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("GraphQL failed ({}): {}", status, text));
        }

        let json: serde_json::Value = resp.json().await?;

        if let Some(errors) = json.get("errors") {
            if !errors.is_null() {
                return Err(anyhow!("GraphQL errors: {}", errors));
            }
        }

        Ok(json)
    }
}

/// Resolve REST and GraphQL base URLs from a host string.
pub fn resolve_api_urls(host: &str) -> (String, String) {
    if host == "github.com" {
        (
            "https://api.github.com".to_string(),
            "https://api.github.com/graphql".to_string(),
        )
    } else {
        (
            format!("https://{}/api/v3", host),
            format!("https://{}/api/graphql", host),
        )
    }
}

/// GitHub search API response.
#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    #[allow(dead_code)]
    pub total_count: u64,
    pub items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
pub struct SearchItem {
    pub number: u64,
    #[allow(dead_code)]
    pub title: String,
    pub updated_at: String,
    pub repository_url: String,
    pub user: SearchUser,
}

#[derive(Debug, Deserialize)]
pub struct SearchUser {
    pub login: String,
}

impl SearchResponse {
    /// Convert search items into SearchResult, extracting owner/repo from repository_url.
    pub fn to_results(&self) -> Vec<SearchResult> {
        self.items
            .iter()
            .filter_map(|item| {
                // repository_url looks like: https://api.github.com/repos/{owner}/{repo}
                let parts: Vec<&str> = item.repository_url.split('/').collect();
                if parts.len() >= 2 {
                    let repo_name = parts[parts.len() - 1];
                    let repo_owner = parts[parts.len() - 2];
                    Some(SearchResult {
                        repo_owner: repo_owner.to_string(),
                        repo_name: repo_name.to_string(),
                        number: item.number,
                        title: item.title.clone(),
                        author: item.user.login.clone(),
                        updated_at: item.updated_at.clone(),
                    })
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_api_urls_github_com() {
        let (rest, graphql) = resolve_api_urls("github.com");
        assert_eq!(rest, "https://api.github.com");
        assert_eq!(graphql, "https://api.github.com/graphql");
    }

    #[test]
    fn test_resolve_api_urls_ghes() {
        let (rest, graphql) = resolve_api_urls("github.company.com");
        assert_eq!(rest, "https://github.company.com/api/v3");
        assert_eq!(graphql, "https://github.company.com/api/graphql");
    }
}
