use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::github::types::{
    CheckStatus, MergeableState, PrGroup, Priority, ReviewDecision, TimelineEventType,
};

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
    /// Modification time (unix seconds) of the daemon's own executable at the
    /// time it started. Lets a client detect that it's talking to a daemon
    /// running an older build of the same binary (e.g. left over from before
    /// a `cargo build`) and restart it. `None` if the mtime couldn't be read.
    #[serde(default)]
    pub binary_mtime: Option<u64>,
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

/// GET /setup/memberships
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MembershipsResponse {
    pub orgs: Vec<OrgMemberships>,
    /// True if the viewer belongs to more orgs/teams than fit in one page —
    /// the list is incomplete and the wizard should offer a manual-entry
    /// fallback.
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgMemberships {
    pub login: String,
    pub teams: Vec<TeamMembership>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMembership {
    pub slug: String,
    pub name: String,
}

/// POST /config/preview_counts
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigPreviewCountsResponse {
    pub queries: Vec<String>,
    /// Distinct PRs matched across all queries (deduplicated), i.e. what
    /// you'd actually see in the inbox with this config.
    pub total_matched_prs: usize,
    /// Per-query errors (e.g. a single bad search qualifier) that didn't
    /// prevent the rest of the queries from running.
    pub errors: Vec<String>,
}

/// GET /prs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrListResponse {
    pub groups: HashMap<PrGroup, Vec<PrSummary>>,
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
    /// Whether the author is a GitHub App/bot (see `PullRequest::author_is_bot`).
    #[serde(default)]
    pub author_is_bot: bool,
    pub group: PrGroup,
    /// Short action label computed by the daemon (e.g. "Review now", "Respond", "Merge").
    pub next_action: String,
    pub check_status: CheckStatus,
    pub llm_priority: Option<Priority>,
    pub updated_at: String,
    pub url: String,
    #[serde(default)]
    pub comments: u32,
    /// Head branch name, so the TUI can copy a branch before the detail
    /// response is loaded. Defaults to empty for older daemons.
    #[serde(default)]
    pub head_ref: String,
    /// One-line LLM summary for the inbox preview strip. Defaults to `None`
    /// for older daemons.
    #[serde(default)]
    pub llm_one_line: Option<String>,
}

impl PrSummary {
    /// Return true when `query` matches any of the searchable summary fields.
    /// An empty query matches every PR.
    pub fn matches_filter(&self, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let q = query.to_lowercase();
        self.title.to_lowercase().contains(&q)
            || self.author.to_lowercase().contains(&q)
            || self.repo.to_lowercase().contains(&q)
            || self.owner.to_lowercase().contains(&q)
            || self.id.to_lowercase().contains(&q)
            || self.next_action.to_lowercase().contains(&q)
            || self.number.to_string().contains(&q)
    }
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
    pub mergeable: MergeableState,
    pub review_decision: Option<ReviewDecision>,
    pub review_requests: Vec<String>,
    #[serde(default)]
    pub team_review_requests: Vec<String>,
    pub viewer_latest_review: Option<String>,
    pub latest_reviews: Vec<LatestReviewDto>,
    pub check_status: CheckStatus,
    pub checks: Vec<CheckEntryDto>,
    pub review_threads: Vec<ReviewThreadDto>,
    pub files: Vec<FileDto>,
    pub timeline: Vec<TimelineEventDto>,
    pub llm_priority: Option<Priority>,
    pub llm_summary: Option<String>,
    #[serde(default)]
    pub llm_rich_summary: Option<crate::github::types::LlmRichSummary>,
    #[serde(default)]
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
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
    /// ISO 8601 timestamp; empty when talking to an older daemon.
    #[serde(default)]
    pub created_at: String,
    /// Web URL of the comment; empty when talking to an older daemon.
    #[serde(default)]
    pub url: String,
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
    pub event_type: TimelineEventType,
    pub actor: String,
    pub created_at: String,
    pub detail: String,
    /// Web URL of the event (comments/reviews only); empty when talking to
    /// an older daemon or for event types with no stable URL.
    #[serde(default)]
    pub url: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A `PrSummary` payload as an old daemon (before `author_is_bot`,
    /// `comments`, `head_ref`, and `llm_one_line` existed) would emit it.
    const OLD_DAEMON_SUMMARY_JSON: &str = r#"{
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

    fn sample_summary() -> PrSummary {
        PrSummary {
            id: "org~repo~1".into(),
            node_id: "n1".into(),
            owner: "org".into(),
            repo: "repo".into(),
            number: 1,
            title: "T".into(),
            author: "a".into(),
            author_is_bot: false,
            group: PrGroup::ReviewNeeded,
            next_action: "Review now".into(),
            check_status: CheckStatus::None,
            llm_priority: None,
            updated_at: "2024-01-01T00:00:00Z".into(),
            url: "https://example.com".into(),
            comments: 0,
            head_ref: String::new(),
            llm_one_line: None,
        }
    }

    // Wire-format lock: `PrGroup`/`CheckStatus` are serde enums erased to
    // their `rename_all = "snake_case"` string form on the wire. The TUI
    // (and any older daemon build) depends on those exact tokens, so this
    // must never silently change when the enums are edited.
    #[test]
    fn pr_list_wire_format_uses_snake_case_tokens() {
        let mut groups = HashMap::new();
        groups.insert(PrGroup::ReviewNeeded, vec![sample_summary()]);
        let resp = PrListResponse {
            groups,
            updated_at: "2024-01-01T00:00:00Z".into(),
        };
        let value = serde_json::to_value(&resp).unwrap();
        assert!(value["groups"]
            .as_object()
            .unwrap()
            .contains_key("review_needed"));

        let value = serde_json::to_value(sample_summary()).unwrap();
        assert_eq!(value["group"], "review_needed");
        assert_eq!(value["check_status"], "none");
    }

    // Version-skew tolerance (AGENTS.md contract): payloads from an older
    // daemon omit fields added later; every such field must default cleanly.
    #[test]
    fn wire_types_default_missing_fields_from_old_daemon_payloads() {
        let summary: PrSummary =
            serde_json::from_str(OLD_DAEMON_SUMMARY_JSON).expect("deserializes");
        assert_eq!(summary.group, PrGroup::ReviewNeeded);
        assert_eq!(summary.check_status, CheckStatus::None);
        assert!(!summary.author_is_bot);
        assert_eq!(summary.comments, 0);
        assert_eq!(summary.head_ref, "");
        assert_eq!(summary.llm_one_line, None);

        let json = r#"{"author":"bob","body":"fix","path":"src/main.rs","line":3}"#;
        let comment: ReviewCommentDto = serde_json::from_str(json).expect("deserializes");
        assert_eq!(comment.created_at, "");
        assert_eq!(comment.url, "");

        let json = r#"{"event_type":"comment","actor":"bob","created_at":"2024-01-01T00:00:00Z","detail":"hi"}"#;
        let event: TimelineEventDto = serde_json::from_str(json).expect("deserializes");
        assert_eq!(event.url, "");

        let json = r#"{"path": "src/main.rs", "additions": 10, "deletions": 2}"#;
        let file: FileDto = serde_json::from_str(json).expect("deserializes");
        assert_eq!(file.status, '?');
    }
}
