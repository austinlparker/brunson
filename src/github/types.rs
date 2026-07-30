use serde::{Deserialize, Serialize};

/// The priority group a PR belongs to, organized into two lanes:
/// authored PRs (things I created) and review-requested PRs (things I need to review).
/// Groups within each lane are ordered by urgency — "what's the most important next action for me?"
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrGroup {
    // ── Authored lane (PRs I created) ──
    /// Ball is in my court: CI failed, changes requested, or a human (non-bot)
    /// comment or non-approval review is newer than any of my comments,
    /// reviews, or pushes. See `PrStore::classify_authored` /
    /// `PrStore::ball_in_my_court` for the full semantics.
    AuthoredActionNeeded,
    /// Approved + CI green + mergeable — I should merge.
    AuthoredReadyToMerge,
    /// Waiting for reviewers or CI — nothing for me to do right now.
    AuthoredWaiting,

    // ── Review lane (PRs I've been asked to review) ──
    /// Haven't reviewed yet — they're blocked on me.
    ReviewNeeded,
    /// New commits since my last review — need to re-review.
    ReviewUpdate,
    /// Already reviewed, no new activity — I've done my part.
    ReviewDone,

    // ── Other ──
    /// Draft PR (not ready for review).
    Draft,
    /// Involved but neither author nor requested reviewer.
    Other,
}

impl PrGroup {
    /// All groups in display order: authored lane, then review lane, then other.
    pub fn all_in_priority_order() -> &'static [PrGroup] {
        &[
            Self::AuthoredActionNeeded,
            Self::AuthoredReadyToMerge,
            Self::AuthoredWaiting,
            Self::ReviewNeeded,
            Self::ReviewUpdate,
            Self::ReviewDone,
            Self::Draft,
            Self::Other,
        ]
    }
}

/// Check/CI status for the head commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum CheckStatus {
    Success,
    Failure,
    Pending,
    Neutral,
    #[default]
    None,
}

/// Overall review decision from GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReviewDecision {
    Approved,
    ReviewRequired,
    ChangesRequested,
}

/// Mergeable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MergeableState {
    Mergeable,
    Conflicting,
    Unknown,
}

/// LLM-assigned priority level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    High,
    Medium,
    Low,
}

/// Richer LLM-generated orientation for a PR: what changed since the user last
/// looked at it, and what they should do next.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRichSummary {
    /// One-line tl;dr shown in the inbox and at the top of the Overview.
    pub one_line: String,
    /// What has changed since the user's `last_seen_at` timestamp.
    pub catch_up: String,
    /// Concrete next action the user should take.
    pub next_steps: String,
    /// When this summary was generated.
    pub generated_at: chrono::DateTime<chrono::Utc>,
    /// Prompt/version identifier used to invalidate stale cached summaries.
    pub prompt_version: u32,
}

/// A single review thread (group of inline comments).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewThread {
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub comments: Vec<ReviewComment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
    pub author: String,
    pub body: String,
    pub path: String,
    pub line: Option<i64>,
    /// ISO 8601 timestamp of the comment. Defaults to empty so payloads from
    /// older daemon versions still deserialize (documented version-skew
    /// tolerance).
    #[serde(default)]
    pub created_at: String,
    /// Web URL of the comment. Defaults to empty for older daemon payloads.
    #[serde(default)]
    pub url: String,
}

fn default_file_status() -> char {
    '?'
}

/// A changed file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    #[serde(default = "default_file_status")]
    pub status: char,
}

/// A CI check entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckEntry {
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub url: String,
}

/// A latest review entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestReview {
    pub author: String,
    pub state: String,
}

/// The type of a timeline event, derived from GitHub's `__typename`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineEventType {
    /// General PR comment (`IssueComment`).
    Comment,
    /// Review submitted (`PullRequestReview`) — approval, changes requested, or comment review.
    Review,
    /// New commits pushed (`PullRequestCommit`).
    Commit,
    /// Force push to head ref (`HeadRefForcePushedEvent`).
    ForcePush,
    /// Draft marked ready for review (`ReadyForReviewEvent`).
    ReadyForReview,
    /// Review requested on someone (`ReviewRequestedEvent`).
    ReviewRequested,
    /// PR merged (`MergedEvent`).
    Merged,
    /// PR closed (`ClosedEvent`).
    Closed,
    /// PR reopened (`ReopenedEvent`).
    Reopened,
    /// Any other event type we don't specifically model.
    Other,
}

/// A single event in a PR's activity timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub event_type: TimelineEventType,
    /// GitHub login of whoever performed the action.
    pub actor: String,
    /// ISO 8601 timestamp of the event.
    pub created_at: String,
    /// Human-readable detail: review state, commit message headline, comment excerpt, etc.
    pub detail: String,
    /// Web URL of the event (comments and reviews only; empty for other
    /// event types and for payloads from older daemon versions).
    #[serde(default)]
    pub url: String,
    /// Whether the actor is a GitHub App/bot (GraphQL author `__typename ==
    /// "Bot"`). Only populated for comment/review events; defaults to false
    /// for other event types and for payloads from older daemon versions.
    #[serde(default)]
    pub actor_is_bot: bool,
    /// Raw GraphQL review state (`"APPROVED"`, `"CHANGES_REQUESTED"`,
    /// `"COMMENTED"`, `"DISMISSED"`, `"PENDING"`) for review events; `None`
    /// for all other event types and for payloads from older daemon versions.
    #[serde(default)]
    pub review_state: Option<String>,
}

/// The full PullRequest model stored in the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    pub node_id: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub author: String,
    /// Whether the author is a GitHub App/bot (GraphQL actor `__typename == "Bot"`,
    /// e.g. dependabot).
    #[serde(default)]
    pub author_is_bot: bool,
    pub owner: String,
    pub repo: String,
    pub is_draft: bool,
    pub updated_at: String,
    pub head_ref: String,
    pub base_ref: String,
    pub mergeable: MergeableState,
    pub review_decision: Option<ReviewDecision>,
    /// Direct user review requests, stored as GitHub user logins.
    pub review_requests: Vec<String>,
    /// Team review requests, normalized as `org/team-slug`.
    #[serde(default)]
    pub team_review_requests: Vec<String>,
    pub viewer_latest_review: Option<String>,
    pub latest_reviews: Vec<LatestReview>,
    pub check_status: CheckStatus,
    pub checks: Vec<CheckEntry>,
    pub review_threads: Vec<ReviewThread>,
    pub files: Vec<PrFile>,
    #[serde(default)]
    pub comments: u32,
    /// Chronological activity timeline (comments, reviews, commits, etc.).
    #[serde(default)]
    pub timeline: Vec<TimelineEvent>,
    /// LLM classification result, if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_priority: Option<Priority>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_summary: Option<String>,
    /// Richer LLM-generated catch-up / next-steps summary, generated on demand.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_rich_summary: Option<LlmRichSummary>,
    /// Last time the user focused this PR in the TUI. Used to scope the catch-up text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl PullRequest {
    /// URL-safe slug for this PR: `{owner}~{repo}~{number}`
    pub fn slug(&self) -> String {
        format!("{}~{}~{}", self.owner, self.repo, self.number)
    }
}

/// Parse a slug into (owner, repo, number).
pub fn parse_slug(slug: &str) -> Option<(String, String, u64)> {
    let parts: Vec<&str> = slug.split('~').collect();
    if parts.len() != 3 {
        return None;
    }
    let number = parts[2].parse::<u64>().ok()?;
    Some((parts[0].to_string(), parts[1].to_string(), number))
}

/// Search result from the GitHub search API.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub repo_owner: String,
    pub repo_name: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub updated_at: String,
}

/// An org the viewer belongs to, and the teams within it the viewer is a
/// member of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgMembership {
    pub login: String,
    pub teams: Vec<TeamMembership>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamMembership {
    pub slug: String,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_slug_valid() {
        let (owner, repo, number) = parse_slug("myorg~myrepo~123").unwrap();
        assert_eq!(owner, "myorg");
        assert_eq!(repo, "myrepo");
        assert_eq!(number, 123);
    }

    #[test]
    fn test_parse_slug_invalid() {
        assert!(parse_slug("invalid").is_none());
        assert!(parse_slug("a~b~c").is_none());
        assert!(parse_slug("a~b~c~d").is_none());
    }

    // Version-skew guard: payloads from daemons predating `actor_is_bot` /
    // `review_state` (and `url`) must still deserialize with safe defaults.
    #[test]
    fn timeline_event_defaults_for_missing_new_fields() {
        let json = r#"{
            "event_type": "comment",
            "actor": "bob",
            "created_at": "2024-06-01T10:00:00Z",
            "detail": "hi"
        }"#;
        let e: TimelineEvent = serde_json::from_str(json).unwrap();
        assert_eq!(e.event_type, TimelineEventType::Comment);
        assert!(!e.actor_is_bot);
        assert_eq!(e.review_state, None);
        assert_eq!(e.url, "");
    }

    #[test]
    fn test_pr_slug() {
        let pr = PullRequest {
            node_id: "x".into(),
            number: 42,
            title: "Test".into(),
            body: String::new(),
            url: String::new(),
            author: "user".into(),
            author_is_bot: false,
            owner: "org".into(),
            repo: "repo".into(),
            is_draft: false,
            updated_at: String::new(),
            head_ref: String::new(),
            base_ref: String::new(),
            mergeable: MergeableState::Unknown,
            review_decision: None,
            review_requests: vec![],
            team_review_requests: vec![],
            viewer_latest_review: None,
            latest_reviews: vec![],
            check_status: CheckStatus::None,
            checks: vec![],
            review_threads: vec![],
            timeline: vec![],
            files: vec![],
            comments: 0,
            llm_priority: None,
            llm_summary: None,
            llm_rich_summary: None,
            last_seen_at: None,
        };
        assert_eq!(pr.slug(), "org~repo~42");
    }
}
