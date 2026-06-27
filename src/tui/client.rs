use anyhow::Result;
use reqwest::Client;

use crate::api::*;

/// HTTP client for talking to the daemon API.
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

    pub async fn get_health(&self) -> Result<HealthResponse> {
        self.get("/health").await
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

    #[allow(dead_code)]
    pub async fn classify(&self, id: &str) -> Result<ClassifyResponse> {
        self.post(&format!("/prs/{}/classify", id)).await
    }
}
