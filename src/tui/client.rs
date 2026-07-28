use anyhow::Result;
use reqwest::Client;

use crate::api::*;

#[derive(Debug, Clone)]
pub struct ConfigReloadError {
    pub body: String,
}

impl std::fmt::Display for ConfigReloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.body)
    }
}

impl std::error::Error for ConfigReloadError {}

/// HTTP client for talking to the daemon API.
#[derive(Clone)]
pub struct DaemonClient {
    base_url: String,
    client: Client,
}

impl DaemonClient {
    pub fn new(port: u16) -> Result<Self> {
        let base_url = format!("http://127.0.0.1:{}", port);
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { base_url, client })
    }

    #[allow(dead_code)]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {} failed ({}): {}", path, status, body);
        }
        Ok(resp.json().await?)
    }

    async fn post<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.post(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("POST {} failed ({}): {}", path, status, body);
        }
        Ok(resp.json().await?)
    }

    async fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.post(&url).json(body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("POST {} failed ({}): {}", path, status, body);
        }
        Ok(resp.json().await?)
    }

    pub async fn get_health(&self) -> Result<HealthResponse> {
        self.get("/health").await
    }

    /// Ask a running daemon to shut down gracefully (no response body).
    /// Used to retire a stale daemon before spawning a fresh one.
    pub async fn request_shutdown(&self) -> Result<()> {
        let url = format!("{}/shutdown", self.base_url);
        let resp = self.client.post(&url).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("POST /shutdown failed ({}): {}", status, body);
        }
        Ok(())
    }

    /// Quick health check with short timeout — used for daemon detection.
    pub async fn check_health(&self) -> Option<HealthResponse> {
        let url = format!("{}/health", self.base_url);
        let client = match Client::builder()
            .timeout(std::time::Duration::from_millis(2000))
            .build()
        {
            Ok(c) => c,
            Err(_) => return None,
        };
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => resp.json().await.ok(),
            _ => None,
        }
    }

    pub async fn get_prs(&self) -> Result<PrListResponse> {
        self.get("/prs").await
    }

    pub async fn get_pr_detail(&self, id: &str) -> Result<PrDetailResponse> {
        self.get(&format!("/prs/{}", id)).await
    }

    pub async fn get_pr_diff(&self, id: &str) -> Result<DiffResponse> {
        self.get(&format!("/prs/{}/diff", id)).await
    }

    pub async fn refresh(&self) -> Result<RefreshResponse> {
        self.post("/prs/refresh").await
    }

    /// Classify a PR. This awaits an LLM call inline on the daemon side, and
    /// a slow local model routinely runs past the client's default 30s
    /// timeout even though the daemon completes (and caches the result)
    /// successfully — so this request gets a much longer deadline than the
    /// rest of the API.
    pub async fn classify(&self, id: &str) -> Result<ClassifyResponse> {
        let path = format!("/prs/{}/classify", id);
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .post(&url)
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("POST {} failed ({}): {}", path, status, body);
        }
        Ok(resp.json().await?)
    }

    pub async fn mark_seen(&self, id: &str) -> Result<serde_json::Value> {
        self.post(&format!("/prs/{}/seen", id)).await
    }

    pub async fn get_setup_status(&self) -> Result<SetupStatusResponse> {
        self.get("/setup/status").await
    }

    pub async fn get_config(&self) -> Result<crate::config::Config> {
        self.get("/config").await
    }

    /// Fetch the orgs/teams the authenticated viewer belongs to, for the
    /// setup wizard's target picker.
    pub async fn get_org_memberships(&self) -> Result<MembershipsResponse> {
        self.get("/setup/memberships").await
    }

    /// Validate a candidate config without writing it (server-side
    /// `Config::validate`, same rules the daemon enforces on reload).
    pub async fn validate_config(&self, config: &crate::config::Config) -> Result<ConfigValidateResponse> {
        self.post_json("/config/validate", config).await
    }

    /// Actually run a candidate config's search queries and return a live,
    /// deduplicated match count — the wizard's "here's what you'll see".
    pub async fn preview_config_counts(
        &self,
        config: &crate::config::Config,
    ) -> Result<ConfigPreviewCountsResponse> {
        self.post_json("/config/preview_counts", config).await
    }

    pub async fn reload_config(&self) -> Result<()> {
        let url = format!("{}/config/reload", self.base_url);
        let resp = self.client.post(&url).send().await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(ConfigReloadError { body }.into())
        }
    }
}
