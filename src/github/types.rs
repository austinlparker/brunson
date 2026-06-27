use serde::{Deserialize, Serialize};

/// The priority group a PR belongs to, organized into two lanes:
/// authored PRs (things I created) and review-requested PRs (things I need to review).
/// Groups within each lane are ordered by urgency — "what's the most important next action for me?"
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrGroup {
    // ── Authored lane (PRs I created) ──
    /// Ball is in my court: CI failed, changes requested, or someone commented and I haven't responded.
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
    /// Display label for the group header.
    pub fn label(&self) -> &'static str {
        match self {
            Self::AuthoredActionNeeded => "Action Needed",
            Self::AuthoredReadyToMerge => "Ready to Merge",
            Self::AuthoredWaiting => "Waiting",
            Self::ReviewNeeded => "Review Needed",
            Self::ReviewUpdate => "Re-Review",
            Self::ReviewDone => "Reviewed",
            Self::Draft => "Drafts",
            Self::Other => "Other",
        }
    }

    /// Icon/emoji for the group.
    pub fn icon(&self) -> &'static str {
        match self {
            Self::AuthoredActionNeeded => "🔴",
            Self::AuthoredReadyToMerge => "✅",
            Self::AuthoredWaiting => "⏳",
            Self::ReviewNeeded => "👀",
            Self::ReviewUpdate => "🔄",
            Self::ReviewDone => "✓",
            Self::Draft => "📝",
            Self::Other => "🔔",
        }
    }

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

    /// Which lane this group belongs to.
    pub fn lane(&self) -> PrLane {
        match self {
            Self::AuthoredActionNeeded | Self::AuthoredReadyToMerge | Self::AuthoredWaiting => {
                PrLane::Authored
            }
            Self::ReviewNeeded | Self::ReviewUpdate | Self::ReviewDone => PrLane::Review,
            Self::Draft => PrLane::Draft,
            Self::Other => PrLane::Other,
        }
    }
}

/// Top-level lane for organizing PRs in the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrLane {
    Authored,
    Review,
    Draft,
    Other,
}

impl PrLane {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Authored => "Authored",
            Self::Review => "Review Requested",
            Self::Draft => "Drafts",
            Self::Other => "Other",
        }
    }

    pub fn all_in_order() -> &'static [PrLane] {
        &[Self::Authored, Self::Review, Self::Draft, Self::Other]
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
    pub owner: String,
    pub repo: String,
    pub is_draft: bool,
    pub updated_at: String,
    pub head_ref: String,
    pub base_ref: String,
    pub mergeable: MergeableState,
    pub review_decision: Option<ReviewDecision>,
    pub review_requests: Vec<String>,
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

    #[test]
    fn test_pr_slug() {
        let pr = PullRequest {
            node_id: "x".into(),
            number: 42,
            title: "Test".into(),
            body: String::new(),
            url: String::new(),
            author: "user".into(),
            owner: "org".into(),
            repo: "repo".into(),
            is_draft: false,
            updated_at: String::new(),
            head_ref: String::new(),
            base_ref: String::new(),
            mergeable: MergeableState::Unknown,
            review_decision: None,
            review_requests: vec![],
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
        };
        assert_eq!(pr.slug(), "org~repo~42");
    }
}
