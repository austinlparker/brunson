use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::github::types::{CheckStatus, PrGroup, Priority};

fn default_setup_status() -> String {
    "unknown".to_string()
}

/// GET /health
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub service: String,
    pub version: String,
    pub status: String,
    pub current_user: String,
    pub last_poll_at: Option<String>,
    pub last_poll_error: Option<String>,
    pub rate_limit_remaining: Option<u32>,
    pub refresh_in_progress: bool,
    #[serde(default = "default_setup_status")]
    pub setup_status: String,
    #[serde(default)]
    pub setup_message: Option<String>,
}

/// GET /setup/status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupStatusResponse {
    pub ready: bool,
    pub status: String,
    pub auth: AuthStatus,
    pub llm: LlmSetupStatus,
    pub next_steps: Vec<String>,
}

impl Default for SetupStatusResponse {
    fn default() -> Self {
        Self {
            ready: false,
            status: "missing_config".to_string(),
            auth: AuthStatus::default(),
            llm: LlmSetupStatus::default(),
            next_steps: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthStatus {
    pub resolved: bool,
    pub source: Option<String>,
    pub user: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LlmSetupStatus {
    pub enabled: bool,
    pub reachable: Option<bool>,
    pub model: Option<String>,
    pub message: Option<String>,
}

/// POST /config/reload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigReloadResponse {
    pub reloaded: bool,
    pub error: Option<String>,
}

/// GET /config/preview and POST /config/validate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigPreviewResponse {
    pub queries: Vec<String>,
}

/// POST /config/validate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigValidateResponse {
    pub valid: bool,
    pub error: Option<String>,
    pub preview: ConfigPreviewResponse,
}

/// GET /prs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrListResponse {
    pub groups: HashMap<String, Vec<PrSummary>>,
    pub updated_at: String,
}

/// Summary of a PR in the list response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrSummary {
    pub id: String,
    pub node_id: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub group: String,
    /// Short action label computed by the daemon (e.g. "Review now", "Respond", "Merge").
    pub next_action: String,
    pub check_status: String,
    pub llm_priority: Option<Priority>,
    pub updated_at: String,
    pub url: String,
    #[serde(default)]
    pub comments: u32,
}

/// GET /prs/{id}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrDetailResponse {
    pub id: String,
    pub node_id: String,
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub author: String,
    pub is_draft: bool,
    pub updated_at: String,
    pub head_ref: String,
    pub base_ref: String,
    pub mergeable: String,
    pub review_decision: Option<String>,
    pub review_requests: Vec<String>,
    pub viewer_latest_review: Option<String>,
    pub latest_reviews: Vec<LatestReviewDto>,
    pub check_status: String,
    pub checks: Vec<CheckEntryDto>,
    pub review_threads: Vec<ReviewThreadDto>,
    pub files: Vec<FileDto>,
    pub timeline: Vec<TimelineEventDto>,
    pub llm_priority: Option<Priority>,
    pub llm_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestReviewDto {
    pub author: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckEntryDto {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewThreadDto {
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub comments: Vec<ReviewCommentDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCommentDto {
    pub author: String,
    pub body: String,
    pub path: String,
    pub line: Option<i64>,
}

fn default_file_status() -> char {
    '?'
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDto {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    #[serde(default = "default_file_status")]
    pub status: char,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEventDto {
    pub event_type: String,
    pub actor: String,
    pub created_at: String,
    pub detail: String,
}

/// GET /prs/{id}/diff
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffResponse {
    pub diff: String,
    pub cached: bool,
}

/// POST /prs/refresh
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefreshResponse {
    pub refresh_in_progress: bool,
}

/// POST /prs/{id}/classify
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassifyResponse {
    pub status: String,
}

/// Error response body
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl ApiError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            code: "not_found".into(),
            message: msg.into(),
            retryable: false,
        }
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            code: "bad_request".into(),
            message: msg.into(),
            retryable: false,
        }
    }

    pub fn service_unavailable(msg: impl Into<String>) -> Self {
        Self {
            code: "service_unavailable".into(),
            message: msg.into(),
            retryable: true,
        }
    }
}

/// Helper: convert PrGroup enum to string key for JSON serialization.
pub fn group_key(g: &PrGroup) -> String {
    serde_json::to_string(g)
        .unwrap_or_else(|_| "\"unknown\"".to_string())
        .trim_matches('"')
        .to_string()
}

/// Helper: convert CheckStatus enum to string for JSON.
pub fn check_status_string(cs: &CheckStatus) -> String {
    serde_json::to_value(cs)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "none".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pr_summary_deserializes_without_comments_field() {
        let json = r#"{
            "id": "org~repo~1",
            "node_id": "n1",
            "owner": "org",
            "repo": "repo",
            "number": 1,
            "title": "T",
            "author": "a",
            "group": "review_needed",
            "next_action": "Review now",
            "check_status": "none",
            "updated_at": "2024-01-01T00:00:00Z",
            "url": "https://example.com"
        }"#;
        let summary: PrSummary = serde_json::from_str(json).expect("deserializes");
        assert_eq!(summary.comments, 0);
    }

    #[test]
    fn test_file_dto_deserializes_without_status_field() {
        let json = r#"{"path": "src/main.rs", "additions": 10, "deletions": 2}"#;
        let file: FileDto = serde_json::from_str(json).expect("deserializes");
        assert_eq!(file.status, '?');
    }

    #[test]
    fn test_file_dto_deserializes_with_status_field() {
        let json = r#"{"path": "src/main.rs", "additions": 10, "deletions": 2, "status": "A"}"#;
        let file: FileDto = serde_json::from_str(json).expect("deserializes");
        assert_eq!(file.status, 'A');
    }
}
