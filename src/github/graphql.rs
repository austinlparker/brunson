use anyhow::{anyhow, Result};
use serde_json::json;
use tracing::{debug, warn};

use super::client::{GitHubClient, GraphqlTransport};
use super::search::normalize_team_identifier;
use super::types::*;

/// Fetch the current viewer's login.
pub async fn fetch_viewer_login(client: &GitHubClient) -> Result<String> {
    let query = "query { viewer { login } }";
    let resp = client.graphql(query).await?;
    let login = resp["data"]["viewer"]["login"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Could not extract viewer login from GraphQL response"))?
        .to_string();
    Ok(login)
}

/// Fetch which configured teams currently include the viewer.
pub async fn fetch_viewer_team_memberships(
    client: &GitHubClient,
    configured_teams: &std::collections::HashSet<String>,
    viewer_login: &str,
) -> Result<std::collections::HashSet<String>> {
    let checks = configured_teams.iter().map(|team| async move {
        let (org, slug) = team
            .split_once('/')
            .ok_or_else(|| anyhow!("invalid normalized team identifier: {}", team))?;
        let is_member = viewer_is_team_member(client, org, slug, viewer_login).await?;
        Ok::<_, anyhow::Error>(is_member.then(|| team.clone()))
    });
    let results = futures::future::try_join_all(checks).await?;
    Ok(results.into_iter().flatten().collect())
}

/// Fetch every org the viewer belongs to, and their teams within each org.
/// Unlike `fetch_viewer_team_memberships` (which checks membership of
/// specific, already-configured teams), this enumerates everything so a
/// setup UI can offer them as choices instead of requiring the user to type
/// team slugs from memory.
pub async fn fetch_viewer_org_team_memberships(
    client: &GitHubClient,
) -> Result<(Vec<OrgMembership>, bool)> {
    let viewer_login = fetch_viewer_login(client).await?;
    let query = r#"query($viewer: [String!]) {
      viewer {
        organizations(first: 100) {
          pageInfo { hasNextPage }
          nodes {
            login
            teams(first: 100, userLogins: $viewer) {
              pageInfo { hasNextPage }
              nodes { slug name }
            }
          }
        }
      }
    }"#;
    let resp = client
        .graphql_with_variables(query, json!({ "viewer": [viewer_login] }))
        .await?;
    Ok(parse_memberships_json(&resp))
}

/// Parse the response of `fetch_viewer_org_team_memberships`'s query,
/// separated from the network call so it's testable against fixture JSON.
fn parse_memberships_json(json: &serde_json::Value) -> (Vec<OrgMembership>, bool) {
    let orgs_conn = &json["data"]["viewer"]["organizations"];
    let mut truncated = orgs_conn["pageInfo"]["hasNextPage"]
        .as_bool()
        .unwrap_or(false);

    let orgs = orgs_conn["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|org| {
                    let login = org["login"].as_str()?.to_string();
                    let teams_conn = &org["teams"];
                    if teams_conn["pageInfo"]["hasNextPage"]
                        .as_bool()
                        .unwrap_or(false)
                    {
                        truncated = true;
                    }
                    let teams = teams_conn["nodes"]
                        .as_array()
                        .map(|team_nodes| {
                            team_nodes
                                .iter()
                                .filter_map(|team| {
                                    Some(TeamMembership {
                                        slug: team["slug"].as_str()?.to_string(),
                                        name: team["name"].as_str().unwrap_or("").to_string(),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(OrgMembership { login, teams })
                })
                .collect()
        })
        .unwrap_or_default();

    (orgs, truncated)
}

async fn viewer_is_team_member(
    client: &GitHubClient,
    org: &str,
    slug: &str,
    viewer_login: &str,
) -> Result<bool> {
    let query = r#"query($org: String!, $slug: String!, $viewer: String!) {
      organization(login: $org) {
        team(slug: $slug) {
          members(first: 100, query: $viewer) {
            nodes { login }
          }
        }
      }
    }"#;
    let resp = client
        .graphql_with_variables(
            query,
            json!({ "org": org, "slug": slug, "viewer": viewer_login }),
        )
        .await?;
    let team = &resp["data"]["organization"]["team"];
    if team.is_null() {
        return Ok(false);
    }
    Ok(team["members"]["nodes"]
        .as_array()
        .map(|nodes| {
            nodes.iter().any(|node| {
                node["login"]
                    .as_str()
                    .map(|login| login.eq_ignore_ascii_case(viewer_login))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false))
}

const MAX_CONNECTION_PAGES: usize = 20;

// The initial PR detail query. The `reviewThreads`, `timelineItems`, and
// `files` connections are paginated via the narrow per-connection
// continuation queries below; `reviewRequests` is deliberately fetched once
// with `first: 100` and never paginated (a PR with >100 requested reviewers
// does not occur in practice; worst case the reviewer list is cosmetically
// truncated).
const PR_DETAIL_QUERY: &str = r#"query($owner: String!, $repo: String!, $number: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      id
    number
    title
    body
    url
    isDraft
    updatedAt
    author { login __typename }
    headRefName
    baseRefName
    mergeable
    reviewDecision
    reviewRequests(first: 100) {
      nodes {
        requestedReviewer {
          ... on User { login }
          ... on Team { slug name organization { login } }
        }
      }
    }
    viewerLatestReview { state }
    latestReviews(first: 10) {
      nodes {
        author { login }
        state
      }
    }
    commits(last: 1) {
      nodes {
        commit {
          statusCheckRollup {
            state
            contexts(first: 30) {  # not paginated: 30 check contexts is an accepted cap
              nodes {
                ... on CheckRun {
                  name
                  status
                  conclusion
                  detailsUrl
                }
                ... on StatusContext {
                  context
                  state
                  targetUrl
                }
              }
            }
          }
        }
      }
    }
    reviewThreads(first: 100) {
      pageInfo {
        hasNextPage
        endCursor
      }
      nodes {
        id
        isResolved
        isOutdated
        comments(first: 100) {
          pageInfo {
            hasNextPage
            endCursor
          }
          nodes {
            author { login }
            body
            path
            line
            createdAt
            url
          }
        }
      }
    }
    timelineItems(first: 100) {
      pageInfo {
        hasNextPage
        endCursor
      }
      nodes {
        __typename
        ... on IssueComment {
          author { login }
          body
          createdAt
          url
        }
        ... on PullRequestReview {
          author { login }
          body
          state
          submittedAt
          url
        }
        ... on PullRequestCommit {
          commit {
            author { user { login } }
            messageHeadline
            committedDate
          }
        }
        ... on HeadRefForcePushedEvent {
          actor { login }
          createdAt
        }
        ... on ReadyForReviewEvent {
          actor { login }
          createdAt
        }
        ... on ReviewRequestedEvent {
          actor { login }
          createdAt
          requestedReviewer {
            ... on User { login }
            ... on Team { slug name organization { login } }
          }
        }
        ... on ReviewDismissedEvent {
          actor { login }
          createdAt
        }
        ... on MergedEvent {
          actor { login }
          createdAt
        }
        ... on ClosedEvent {
          actor { login }
          createdAt
        }
        ... on ReopenedEvent {
          actor { login }
          createdAt
        }
      }
    }
    files(first: 100) {
      pageInfo {
        hasNextPage
        endCursor
      }
      nodes {
        path
        additions
        deletions
        changeType
      }
    }
    }
  }
}"#;

const REVIEW_THREAD_COMMENTS_QUERY: &str = r#"query($threadId: ID!, $after: String) {
  node(id: $threadId) {
    ... on PullRequestReviewThread {
      comments(first: 100, after: $after) {
        pageInfo {
          hasNextPage
          endCursor
        }
        nodes {
          author { login }
          body
          path
          line
          createdAt
          url
        }
      }
    }
  }
}"#;

// Narrow continuation query for the `timelineItems` connection only. Node
// selection must stay in sync with `PR_DETAIL_QUERY` so `parse_timeline`
// parses both identically.
const PR_TIMELINE_ITEMS_QUERY: &str = r#"query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      timelineItems(first: 100, after: $after) {
        pageInfo {
          hasNextPage
          endCursor
        }
        nodes {
          __typename
          ... on IssueComment {
            author { login }
            body
            createdAt
            url
          }
          ... on PullRequestReview {
            author { login }
            body
            state
            submittedAt
            url
          }
          ... on PullRequestCommit {
            commit {
              author { user { login } }
              messageHeadline
              committedDate
            }
          }
          ... on HeadRefForcePushedEvent {
            actor { login }
            createdAt
          }
          ... on ReadyForReviewEvent {
            actor { login }
            createdAt
          }
          ... on ReviewRequestedEvent {
            actor { login }
            createdAt
            requestedReviewer {
              ... on User { login }
              ... on Team { slug name organization { login } }
            }
          }
          ... on ReviewDismissedEvent {
            actor { login }
            createdAt
          }
          ... on MergedEvent {
            actor { login }
            createdAt
          }
          ... on ClosedEvent {
            actor { login }
            createdAt
          }
          ... on ReopenedEvent {
            actor { login }
            createdAt
          }
        }
      }
    }
  }
}"#;

// Narrow continuation query for the `reviewThreads` connection only. Node
// selection must stay in sync with `PR_DETAIL_QUERY` so thread records
// (including the `id` used for nested comment pagination) parse identically.
const PR_REVIEW_THREADS_QUERY: &str = r#"query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $after) {
        pageInfo {
          hasNextPage
          endCursor
        }
        nodes {
          id
          isResolved
          isOutdated
          comments(first: 100) {
            pageInfo {
              hasNextPage
              endCursor
            }
            nodes {
              author { login }
              body
              path
              line
              createdAt
              url
            }
          }
        }
      }
    }
  }
}"#;

// Narrow continuation query for the `files` connection only. Node selection
// must stay in sync with `PR_DETAIL_QUERY` so `parse_files` parses both
// identically.
const PR_FILES_QUERY: &str = r#"query($owner: String!, $repo: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      files(first: 100, after: $after) {
        pageInfo {
          hasNextPage
          endCursor
        }
        nodes {
          path
          additions
          deletions
          changeType
        }
      }
    }
  }
}"#;

fn parse_single_pr(pr: &serde_json::Value, owner: &str, repo: &str) -> PullRequest {
    let node_id = pr["id"].as_str().unwrap_or("").to_string();
    let number = pr["number"].as_u64().unwrap_or(0);
    let title = pr["title"].as_str().unwrap_or("").to_string();
    let body = pr["body"].as_str().unwrap_or("").to_string();
    let url = pr["url"].as_str().unwrap_or("").to_string();
    let author = pr["author"]["login"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let author_is_bot = pr["author"]["__typename"].as_str() == Some("Bot");
    let is_draft = pr["isDraft"].as_bool().unwrap_or(false);
    let updated_at = pr["updatedAt"].as_str().unwrap_or("").to_string();
    let head_ref = pr["headRefName"].as_str().unwrap_or("").to_string();
    let base_ref = pr["baseRefName"].as_str().unwrap_or("").to_string();

    let mergeable = match pr["mergeable"].as_str() {
        Some("MERGEABLE") => MergeableState::Mergeable,
        Some("CONFLICTING") => MergeableState::Conflicting,
        _ => MergeableState::Unknown,
    };

    let review_decision = match pr["reviewDecision"].as_str() {
        Some("APPROVED") => Some(ReviewDecision::Approved),
        Some("REVIEW_REQUIRED") => Some(ReviewDecision::ReviewRequired),
        Some("CHANGES_REQUESTED") => Some(ReviewDecision::ChangesRequested),
        _ => None,
    };

    let (review_requests, team_review_requests) =
        parse_review_requests(&pr["reviewRequests"]["nodes"]);

    let viewer_latest_review = pr["viewerLatestReview"]["state"].as_str().map(String::from);

    let latest_reviews: Vec<LatestReview> = pr["latestReviews"]["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .map(|n| LatestReview {
                    author: n["author"]["login"].as_str().unwrap_or("").to_string(),
                    state: n["state"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    let (check_status, checks) = parse_checks(&pr["commits"]["nodes"]);

    let review_threads = parse_review_threads(&pr["reviewThreads"]["nodes"]);

    let timeline = parse_timeline(&pr["timelineItems"]["nodes"]);

    let files = parse_files(&pr["files"]["nodes"]);

    PullRequest {
        node_id,
        number,
        title,
        body,
        url,
        author,
        author_is_bot,
        owner: owner.to_string(),
        repo: repo.to_string(),
        is_draft,
        updated_at,
        head_ref,
        base_ref,
        mergeable,
        review_decision,
        review_requests,
        team_review_requests,
        viewer_latest_review,
        latest_reviews,
        check_status,
        checks,
        review_threads,
        timeline,
        files,
        comments: 0,
        llm_priority: None,
        llm_summary: None,
        llm_rich_summary: None,
        last_seen_at: None,
    }
}

fn parse_review_requests(nodes: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let mut users = Vec::new();
    let mut teams = Vec::new();

    if let Some(nodes) = nodes.as_array() {
        for node in nodes {
            let reviewer = &node["requestedReviewer"];
            if let Some(login) = reviewer["login"].as_str() {
                users.push(login.to_string());
                continue;
            }
            if let Some(team) = normalize_team_reviewer(reviewer) {
                teams.push(team);
            }
        }
    }

    (users, teams)
}

fn normalize_team_reviewer(reviewer: &serde_json::Value) -> Option<String> {
    let org = reviewer["organization"]["login"].as_str()?;
    let slug = reviewer["slug"].as_str()?;
    normalize_team_identifier(&format!("{org}/{slug}"))
}

fn parse_checks(commits: &serde_json::Value) -> (CheckStatus, Vec<CheckEntry>) {
    let nodes = match commits.as_array() {
        Some(a) if !a.is_empty() => &a[0]["commit"]["statusCheckRollup"],
        _ => return (CheckStatus::None, Vec::new()),
    };

    let status = match nodes["state"].as_str() {
        Some("SUCCESS") => CheckStatus::Success,
        Some("FAILURE") => CheckStatus::Failure,
        Some("ERROR") => CheckStatus::Failure,
        Some("PENDING") => CheckStatus::Pending,
        Some("EXPECTED") => CheckStatus::Pending,
        Some("NEUTRAL") => CheckStatus::Neutral,
        _ => CheckStatus::None,
    };

    let checks: Vec<CheckEntry> = nodes["contexts"]["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .map(|n| {
                    // CheckRun or StatusContext
                    let name = n
                        .get("name")
                        .and_then(|v| v.as_str())
                        .or_else(|| n.get("context").and_then(|v| v.as_str()))
                        .unwrap_or("unknown")
                        .to_string();
                    // CheckRun nodes expose `status` (QUEUED, IN_PROGRESS, COMPLETED, ...).
                    // Legacy StatusContext nodes expose `state` (ERROR, FAILURE, PENDING, SUCCESS).
                    let cs = n
                        .get("status")
                        .and_then(|v| v.as_str())
                        .or_else(|| n.get("state").and_then(|v| v.as_str()))
                        .unwrap_or("");
                    let conclusion = n
                        .get("conclusion")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let url = n
                        .get("detailsUrl")
                        .and_then(|v| v.as_str())
                        .or_else(|| n.get("targetUrl").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .to_string();
                    CheckEntry {
                        name,
                        status: cs.to_string(),
                        conclusion,
                        url,
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    (status, checks)
}

fn parse_review_threads(nodes: &serde_json::Value) -> Vec<ReviewThread> {
    parse_thread_records(nodes)
        .into_iter()
        .map(|record| record.thread)
        .collect()
}

/// Cursor state of one GraphQL connection, parsed from its `pageInfo`.
#[derive(Debug, Clone, Default)]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

impl PageInfo {
    fn parse(connection: &serde_json::Value) -> Self {
        let info = &connection["pageInfo"];
        Self {
            has_next_page: info["hasNextPage"].as_bool().unwrap_or(false),
            end_cursor: info["endCursor"].as_str().map(String::from),
        }
    }

    /// The cursor to continue from, or `None` when this connection is done.
    fn next_cursor(&self) -> Option<String> {
        if self.has_next_page {
            self.end_cursor.clone()
        } else {
            None
        }
    }
}

/// A parsed review thread plus the GraphQL-internal bits needed to paginate
/// its nested comments: the thread node `id` and the comments connection's
/// cursor state. Neither ever serializes into `PrDetailResponse`.
struct ThreadRecord {
    id: String,
    comments_page: PageInfo,
    thread: ReviewThread,
}

fn parse_thread_records(nodes: &serde_json::Value) -> Vec<ThreadRecord> {
    nodes
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .map(|n| ThreadRecord {
                    id: n["id"].as_str().unwrap_or("").to_string(),
                    comments_page: PageInfo::parse(&n["comments"]),
                    thread: ReviewThread {
                        is_resolved: n["isResolved"].as_bool().unwrap_or(false),
                        is_outdated: n["isOutdated"].as_bool().unwrap_or(false),
                        comments: parse_review_comments(&n["comments"]["nodes"]),
                    },
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_review_comments(nodes: &serde_json::Value) -> Vec<ReviewComment> {
    nodes
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .map(|c| ReviewComment {
                    author: c["author"]["login"]
                        .as_str()
                        .unwrap_or("unknown")
                        .to_string(),
                    body: c["body"].as_str().unwrap_or("").to_string(),
                    path: c["path"].as_str().unwrap_or("").to_string(),
                    line: c["line"].as_i64(),
                    created_at: c["createdAt"].as_str().unwrap_or("").to_string(),
                    url: c["url"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_timeline(nodes: &serde_json::Value) -> Vec<TimelineEvent> {
    nodes
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| {
                    let typename = n["__typename"].as_str().unwrap_or("");
                    let (event_type, actor, created_at, detail) = match typename {
                        "IssueComment" => (
                            TimelineEventType::Comment,
                            n["author"]["login"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string(),
                            n["createdAt"].as_str().unwrap_or("").to_string(),
                            n["body"].as_str().unwrap_or("").to_string(),
                        ),
                        "PullRequestReview" => {
                            let state = n["state"].as_str().unwrap_or("COMMENTED");
                            let body = n["body"].as_str().unwrap_or("").to_string();
                            (
                                TimelineEventType::Review,
                                n["author"]["login"]
                                    .as_str()
                                    .unwrap_or("unknown")
                                    .to_string(),
                                n["submittedAt"].as_str().unwrap_or("").to_string(),
                                if body.is_empty() {
                                    state.to_string()
                                } else {
                                    format!("{}: {}", state, body)
                                },
                            )
                        }
                        "PullRequestCommit" => {
                            let commit = &n["commit"];
                            (
                                TimelineEventType::Commit,
                                commit["author"]["user"]["login"]
                                    .as_str()
                                    .unwrap_or("unknown")
                                    .to_string(),
                                commit["committedDate"].as_str().unwrap_or("").to_string(),
                                commit["messageHeadline"].as_str().unwrap_or("").to_string(),
                            )
                        }
                        "HeadRefForcePushedEvent" => (
                            TimelineEventType::ForcePush,
                            n["actor"]["login"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string(),
                            n["createdAt"].as_str().unwrap_or("").to_string(),
                            "Force pushed".to_string(),
                        ),
                        "ReadyForReviewEvent" => (
                            TimelineEventType::ReadyForReview,
                            n["actor"]["login"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string(),
                            n["createdAt"].as_str().unwrap_or("").to_string(),
                            "Marked ready for review".to_string(),
                        ),
                        "ReviewRequestedEvent" => {
                            let target = n["requestedReviewer"]["login"]
                                .as_str()
                                .map(String::from)
                                .or_else(|| normalize_team_reviewer(&n["requestedReviewer"]))
                                .unwrap_or_else(|| "someone".to_string());
                            (
                                TimelineEventType::ReviewRequested,
                                n["actor"]["login"]
                                    .as_str()
                                    .unwrap_or("unknown")
                                    .to_string(),
                                n["createdAt"].as_str().unwrap_or("").to_string(),
                                format!("Requested review from {}", target),
                            )
                        }
                        "ReviewDismissedEvent" => (
                            TimelineEventType::Other,
                            n["actor"]["login"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string(),
                            n["createdAt"].as_str().unwrap_or("").to_string(),
                            "Dismissed review".to_string(),
                        ),
                        "MergedEvent" => (
                            TimelineEventType::Merged,
                            n["actor"]["login"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string(),
                            n["createdAt"].as_str().unwrap_or("").to_string(),
                            "Merged".to_string(),
                        ),
                        "ClosedEvent" => (
                            TimelineEventType::Closed,
                            n["actor"]["login"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string(),
                            n["createdAt"].as_str().unwrap_or("").to_string(),
                            "Closed".to_string(),
                        ),
                        "ReopenedEvent" => (
                            TimelineEventType::Reopened,
                            n["actor"]["login"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string(),
                            n["createdAt"].as_str().unwrap_or("").to_string(),
                            "Reopened".to_string(),
                        ),
                        _ => {
                            // Unknown event type — skip if no actor/timestamp
                            if n["createdAt"].as_str().is_none()
                                && n["submittedAt"].as_str().is_none()
                            {
                                return None;
                            }
                            (
                                TimelineEventType::Other,
                                n["actor"]["login"]
                                    .as_str()
                                    .or_else(|| n["author"]["login"].as_str())
                                    .unwrap_or("unknown")
                                    .to_string(),
                                n["createdAt"]
                                    .as_str()
                                    .or_else(|| n["submittedAt"].as_str())
                                    .unwrap_or("")
                                    .to_string(),
                                typename.to_string(),
                            )
                        }
                    };

                    // Only comments and reviews carry a stable web URL; other
                    // event types fall back to the PR URL in the TUI.
                    let url = match event_type {
                        TimelineEventType::Comment | TimelineEventType::Review => {
                            n["url"].as_str().unwrap_or("").to_string()
                        }
                        _ => String::new(),
                    };

                    Some(TimelineEvent {
                        event_type,
                        actor,
                        created_at,
                        detail,
                        url,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_files(nodes: &serde_json::Value) -> Vec<PrFile> {
    nodes
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .map(|n| {
                    let status = match n["changeType"].as_str() {
                        Some("ADDED") => 'A',
                        Some("CHANGED") => 'M',
                        Some("REMOVED") => 'D',
                        Some("RENAMED") => 'R',
                        Some("COPIED") => 'M',
                        _ => '?',
                    };
                    PrFile {
                        path: n["path"].as_str().unwrap_or("").to_string(),
                        additions: n["additions"].as_u64().unwrap_or(0),
                        deletions: n["deletions"].as_u64().unwrap_or(0),
                        status,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Maximum PR detail fetches in flight at once.
const PR_DETAIL_FETCH_CONCURRENCY: usize = 4;

/// Outcome of hydrating a batch of search results.
pub struct PrDetailFetch {
    /// Hydrated PRs, in original input order.
    pub prs: Vec<PullRequest>,
    /// `(owner, repo, number)` keys whose detail fetch failed transiently
    /// (network error, rate limit, ...). Callers should treat these as
    /// still-live PRs of unknown state — not as gone. Vanished PRs (GitHub
    /// returned `null`) are intentionally NOT listed here.
    pub failed: Vec<(String, String, u64)>,
}

/// Fetch detail for search results with bounded concurrency.
///
/// Ordering contract: survivors are emitted in original input order
/// (inputs are already deduplicated upstream); vanished PRs (deleted or
/// permission-lost between search and hydration) are discarded; failed
/// fetches are discarded from `prs` but reported via `failed` so callers
/// can avoid dropping previously known-good state.
pub async fn fetch_pr_details<C: GraphqlTransport>(
    client: &C,
    results: &[SearchResult],
) -> Result<PrDetailFetch> {
    use futures::stream::StreamExt;

    let keys: Vec<(usize, String, String, u64)> = results
        .iter()
        .enumerate()
        .map(|(index, r)| (index, r.repo_owner.clone(), r.repo_name.clone(), r.number))
        .collect();
    let fetches = keys.into_iter().map(|(index, owner, repo, number)| async move {
        let outcome = fetch_pr_detail(client, &owner, &repo, number).await;
        (index, owner, repo, number, outcome)
    });

    let mut indexed: Vec<(usize, PullRequest)> = Vec::new();
    let mut failed: Vec<(String, String, u64)> = Vec::new();
    let mut stream =
        futures::stream::iter(fetches).buffer_unordered(PR_DETAIL_FETCH_CONCURRENCY);
    while let Some((index, owner, repo, number, outcome)) = stream.next().await {
        match outcome {
            Ok(Some(pr)) => indexed.push((index, pr)),
            Ok(None) => {} // PR vanished between search and hydration.
            Err(e) => {
                warn!(
                    "Failed to fetch PR detail for {}/{}/{}: {}; keeping previously stored data",
                    owner, repo, number, e
                );
                failed.push((owner, repo, number));
            }
        }
    }
    indexed.sort_by_key(|(index, _)| *index);
    Ok(PrDetailFetch {
        prs: indexed.into_iter().map(|(_, pr)| pr).collect(),
        failed,
    })
}

async fn fetch_pr_detail<C: GraphqlTransport>(
    client: &C,
    owner: &str,
    repo: &str,
    number: u64,
) -> Result<Option<PullRequest>> {
    let resp = client
        .graphql_query(
            PR_DETAIL_QUERY,
            json!({ "owner": owner, "repo": repo, "number": number }),
        )
        .await?;

    let pr_data = &resp["data"]["repository"]["pullRequest"];
    if pr_data.is_null() {
        debug!(
            "PR {}/{}/{} not found in GraphQL response",
            owner, repo, number
        );
        return Ok(None);
    }

    let mut pr = parse_single_pr(pr_data, owner, repo);

    paginate_pr_connection(
        client,
        owner,
        repo,
        number,
        PR_TIMELINE_ITEMS_QUERY,
        "timelineItems",
        PageInfo::parse(&pr_data["timelineItems"]),
        &mut pr.timeline,
        parse_timeline,
    )
    .await?;

    paginate_pr_connection(
        client,
        owner,
        repo,
        number,
        PR_FILES_QUERY,
        "files",
        PageInfo::parse(&pr_data["files"]),
        &mut pr.files,
        parse_files,
    )
    .await?;

    // Review threads are paginated as typed records so each thread keeps its
    // GraphQL node id and comments cursor for nested comment pagination.
    let mut threads = parse_thread_records(&pr_data["reviewThreads"]["nodes"]);
    paginate_pr_connection(
        client,
        owner,
        repo,
        number,
        PR_REVIEW_THREADS_QUERY,
        "reviewThreads",
        PageInfo::parse(&pr_data["reviewThreads"]),
        &mut threads,
        parse_thread_records,
    )
    .await?;
    for record in &mut threads {
        paginate_thread_comments(client, record).await?;
    }
    pr.review_threads = threads.into_iter().map(|record| record.thread).collect();

    Ok(Some(pr))
}

/// Drive one top-level PR connection to completion via its narrow
/// continuation `query`, appending parsed items into `items` in fetch order.
/// Owns exactly one cursor and never touches another connection's data.
#[allow(clippy::too_many_arguments)]
async fn paginate_pr_connection<C, T>(
    client: &C,
    owner: &str,
    repo: &str,
    number: u64,
    query: &'static str,
    connection: &str,
    mut page: PageInfo,
    items: &mut Vec<T>,
    parse_nodes: impl Fn(&serde_json::Value) -> Vec<T>,
) -> Result<()>
where
    C: GraphqlTransport,
{
    let mut pages = 1usize;
    while let Some(cursor) = page.next_cursor() {
        if pages >= MAX_CONNECTION_PAGES {
            // Truncate this one connection rather than failing the whole PR
            // (and the whole poll cycle) over a single oversized PR.
            warn!(
                "GraphQL {} pagination exceeded {} pages for {}/{}/{}; truncating",
                connection, MAX_CONNECTION_PAGES, owner, repo, number
            );
            break;
        }

        let resp = client
            .graphql_query(
                query,
                json!({ "owner": owner, "repo": repo, "number": number, "after": cursor }),
            )
            .await?;
        let conn = &resp["data"]["repository"]["pullRequest"][connection];
        if conn.is_null() {
            return Err(anyhow!(
                "GraphQL continuation page missing {} connection",
                connection
            ));
        }
        items.extend(parse_nodes(&conn["nodes"]));
        page = PageInfo::parse(conn);
        pages += 1;
    }
    Ok(())
}

/// Drive one review thread's nested comments connection to completion via
/// `REVIEW_THREAD_COMMENTS_QUERY`, appending parsed comments to the thread.
async fn paginate_thread_comments<C: GraphqlTransport>(
    client: &C,
    record: &mut ThreadRecord,
) -> Result<()> {
    if record.id.is_empty() {
        return Ok(());
    }
    let mut pages = 1usize;
    while let Some(cursor) = record.comments_page.next_cursor() {
        if pages >= MAX_CONNECTION_PAGES {
            // Truncate this thread's comments rather than failing the
            // whole PR (and the whole poll cycle) over one big thread.
            warn!(
                "GraphQL review thread comments pagination exceeded {} pages for thread {}; truncating",
                MAX_CONNECTION_PAGES, record.id
            );
            break;
        }
        let resp = client
            .graphql_query(
                REVIEW_THREAD_COMMENTS_QUERY,
                json!({ "threadId": record.id, "after": cursor }),
            )
            .await?;
        let conn = &resp["data"]["node"]["comments"];
        if conn.is_null() {
            return Err(anyhow!("GraphQL page missing review thread comments"));
        }
        record.thread.comments.extend(parse_review_comments(&conn["nodes"]));
        record.comments_page = PageInfo::parse(conn);
        pages += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_single_pr_minimal() {
        let pr_json = json!({
            "id": "PR_node1",
            "number": 42,
            "title": "Fix bug",
            "body": "Description",
            "url": "https://github.com/org/repo/pull/42",
            "isDraft": false,
            "updatedAt": "2024-01-15T10:30:00Z",
            "author": { "login": "alice" },
            "headRefName": "feature",
            "baseRefName": "main",
            "mergeable": "MERGEABLE",
            "reviewDecision": "APPROVED",
            "reviewRequests": { "nodes": [] },
            "viewerLatestReview": null,
            "latestReviews": { "nodes": [] },
            "commits": { "nodes": [] },
            "reviewThreads": { "nodes": [] },
            "timelineItems": { "nodes": [] },
            "files": { "nodes": [] }
        });

        let pr = parse_single_pr(&pr_json, "org", "repo");
        assert_eq!(pr.node_id, "PR_node1");
        assert_eq!(pr.number, 42);
        assert_eq!(pr.title, "Fix bug");
        assert_eq!(pr.author, "alice");
        assert!(!pr.is_draft);
        assert_eq!(pr.mergeable, MergeableState::Mergeable);
        assert_eq!(pr.review_decision, Some(ReviewDecision::Approved));
        assert_eq!(pr.check_status, CheckStatus::None);
    }

    #[test]
    fn test_parse_review_requests_includes_users_and_teams() {
        let pr_json = json!({
            "id": "PR_node1",
            "number": 42,
            "title": "Fix bug",
            "body": "Description",
            "url": "https://github.com/MyOrg/repo/pull/42",
            "isDraft": false,
            "updatedAt": "2024-01-15T10:30:00Z",
            "author": { "login": "alice" },
            "headRefName": "feature",
            "baseRefName": "main",
            "mergeable": "MERGEABLE",
            "reviewDecision": "APPROVED",
            "reviewRequests": {
                "nodes": [
                    { "requestedReviewer": { "login": "bob" } },
                    { "requestedReviewer": {
                        "slug": "Team-A",
                        "name": "Team A",
                        "organization": { "login": "MyOrg" }
                    } }
                ]
            },
            "viewerLatestReview": null,
            "latestReviews": { "nodes": [] },
            "commits": { "nodes": [] },
            "reviewThreads": { "nodes": [] },
            "timelineItems": {
                "nodes": [{
                    "__typename": "ReviewRequestedEvent",
                    "actor": { "login": "alice" },
                    "createdAt": "2024-01-15T10:31:00Z",
                    "requestedReviewer": {
                        "slug": "Team-A",
                        "name": "Team A",
                        "organization": { "login": "MyOrg" }
                    }
                }]
            },
            "files": { "nodes": [] }
        });

        let pr = parse_single_pr(&pr_json, "MyOrg", "repo");
        assert_eq!(pr.review_requests, vec!["bob"]);
        assert_eq!(pr.team_review_requests, vec!["myorg/team-a"]);
        assert_eq!(pr.timeline.len(), 1);
        assert_eq!(
            pr.timeline[0].event_type,
            TimelineEventType::ReviewRequested
        );
        assert_eq!(pr.timeline[0].detail, "Requested review from myorg/team-a");
    }

    #[test]
    fn test_parse_checks_checkrun() {
        let commits = json!([{
            "commit": {
                "statusCheckRollup": {
                    "state": "FAILURE",
                    "contexts": {
                        "nodes": [
                            {
                                "name": "CI",
                                "status": "COMPLETED",
                                "conclusion": "FAILURE",
                                "detailsUrl": "https://example.com/check/1"
                            },
                            {
                                "context": "Security Check",
                                "state": "SUCCESS",
                                "targetUrl": "https://example.com/check/2"
                            }
                        ]
                    }
                }
            }
        }]);

        let (status, checks) = parse_checks(&commits);
        assert_eq!(status, CheckStatus::Failure);
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "CI");
        assert_eq!(checks[0].status, "COMPLETED");
        assert!(checks[0].conclusion.as_deref() == Some("FAILURE"));
        assert_eq!(checks[1].name, "Security Check");
        assert_eq!(checks[1].status, "SUCCESS");
    }

    #[test]
    fn test_parse_checks_status_context_uses_state() {
        let commits = json!([{
            "commit": {
                "statusCheckRollup": {
                    "state": "SUCCESS",
                    "contexts": {
                        "nodes": [
                            {
                                "context": "legacy/status",
                                "state": "PENDING",
                                "targetUrl": "https://example.com/check/1"
                            }
                        ]
                    }
                }
            }
        }]);

        let (status, checks) = parse_checks(&commits);
        assert_eq!(status, CheckStatus::Success);
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "legacy/status");
        assert_eq!(checks[0].status, "PENDING");
    }

    #[test]
    fn test_parse_checks_error_rollups_as_failure() {
        let commits = json!([{
            "commit": {
                "statusCheckRollup": {
                    "state": "ERROR",
                    "contexts": { "nodes": [] }
                }
            }
        }]);

        let (status, _) = parse_checks(&commits);
        assert_eq!(status, CheckStatus::Failure);
    }

    #[test]
    fn test_parse_checks_expected_rollups_as_pending() {
        let commits = json!([{
            "commit": {
                "statusCheckRollup": {
                    "state": "EXPECTED",
                    "contexts": { "nodes": [] }
                }
            }
        }]);

        let (status, _) = parse_checks(&commits);
        assert_eq!(status, CheckStatus::Pending);
    }

    #[test]
    fn test_parse_checks_empty() {
        let commits = json!([]);
        let (status, checks) = parse_checks(&commits);
        assert_eq!(status, CheckStatus::None);
        assert!(checks.is_empty());
    }

    #[test]
    fn test_parse_review_threads() {
        let nodes = json!([{
            "isResolved": false,
            "isOutdated": true,
            "comments": {
                "nodes": [{
                    "author": { "login": "bob" },
                    "body": "Looks good",
                    "path": "src/main.rs",
                    "line": 42
                }]
            }
        }]);

        let threads = parse_review_threads(&nodes);
        assert_eq!(threads.len(), 1);
        assert!(!threads[0].is_resolved);
        assert!(threads[0].is_outdated);
        assert_eq!(threads[0].comments.len(), 1);
        assert_eq!(threads[0].comments[0].author, "bob");
        assert_eq!(threads[0].comments[0].line, Some(42));
    }

    #[test]
    fn parse_review_threads_maps_created_at_and_url() {
        let nodes = json!([{
            "isResolved": false,
            "isOutdated": false,
            "comments": {
                "nodes": [{
                    "author": { "login": "bob" },
                    "body": "Looks good",
                    "path": "src/main.rs",
                    "line": 42,
                    "createdAt": "2024-01-15T10:00:00Z",
                    "url": "https://github.com/org/repo/pull/1#discussion_r7"
                }]
            }
        }]);

        let threads = parse_review_threads(&nodes);
        assert_eq!(threads[0].comments[0].created_at, "2024-01-15T10:00:00Z");
        assert_eq!(
            threads[0].comments[0].url,
            "https://github.com/org/repo/pull/1#discussion_r7"
        );

        // Legacy payloads without the new fields default to empty strings.
        let legacy = json!([{
            "isResolved": false,
            "isOutdated": false,
            "comments": { "nodes": [{ "author": { "login": "bob" }, "body": "b", "path": "p", "line": 1 }] }
        }]);
        let threads = parse_review_threads(&legacy);
        assert_eq!(threads[0].comments[0].created_at, "");
        assert_eq!(threads[0].comments[0].url, "");
    }

    #[test]
    fn parse_timeline_maps_comment_url() {
        let nodes = json!([
            {
                "__typename": "IssueComment",
                "author": { "login": "bob" },
                "body": "hi",
                "createdAt": "2024-01-15T10:00:00Z",
                "url": "https://github.com/org/repo/pull/1#issuecomment-9"
            },
            {
                "__typename": "PullRequestReview",
                "author": { "login": "alice" },
                "body": "ok",
                "state": "APPROVED",
                "submittedAt": "2024-01-15T11:00:00Z",
                "url": "https://github.com/org/repo/pull/1#pullrequestreview-3"
            },
            {
                "__typename": "MergedEvent",
                "actor": { "login": "alice" },
                "createdAt": "2024-01-15T12:00:00Z"
            }
        ]);

        let timeline = parse_timeline(&nodes);
        assert_eq!(
            timeline[0].url,
            "https://github.com/org/repo/pull/1#issuecomment-9"
        );
        assert_eq!(
            timeline[1].url,
            "https://github.com/org/repo/pull/1#pullrequestreview-3"
        );
        // Non-comment events carry no URL (TUI falls back to the PR URL).
        assert_eq!(timeline[2].url, "");
    }

    #[test]
    fn test_parse_timeline_mixed_events() {
        let nodes = json!([
            {
                "__typename": "IssueComment",
                "author": { "login": "bob" },
                "body": "Can you take a look?",
                "createdAt": "2024-01-15T10:00:00Z"
            },
            {
                "__typename": "PullRequestReview",
                "author": { "login": "alice" },
                "body": "Looks good",
                "state": "APPROVED",
                "submittedAt": "2024-01-15T11:00:00Z"
            },
            {
                "__typename": "PullRequestCommit",
                "commit": {
                    "author": { "user": { "login": "alice" } },
                    "messageHeadline": "Fix typo",
                    "committedDate": "2024-01-15T09:00:00Z"
                }
            },
            {
                "__typename": "ReviewRequestedEvent",
                "actor": { "login": "alice" },
                "createdAt": "2024-01-15T08:00:00Z",
                "requestedReviewer": { "login": "bob" }
            }
        ]);

        let timeline = parse_timeline(&nodes);
        assert_eq!(timeline.len(), 4);

        // Comment
        assert_eq!(timeline[0].event_type, TimelineEventType::Comment);
        assert_eq!(timeline[0].actor, "bob");
        assert!(timeline[0].detail.contains("Can you take a look?"));

        // Review
        assert_eq!(timeline[1].event_type, TimelineEventType::Review);
        assert_eq!(timeline[1].actor, "alice");
        assert!(timeline[1].detail.contains("APPROVED"));

        // Commit
        assert_eq!(timeline[2].event_type, TimelineEventType::Commit);
        assert_eq!(timeline[2].actor, "alice");
        assert_eq!(timeline[2].detail, "Fix typo");

        // Review requested
        assert_eq!(timeline[3].event_type, TimelineEventType::ReviewRequested);
        assert_eq!(timeline[3].actor, "alice");
        assert!(timeline[3].detail.contains("bob"));
    }

    #[test]
    fn test_parse_timeline_empty() {
        let nodes = json!([]);
        let timeline = parse_timeline(&nodes);
        assert!(timeline.is_empty());
    }

    #[test]
    fn test_parse_files_change_type_to_status_char() {
        let nodes = json!([
            { "path": "new.txt", "additions": 1, "deletions": 0, "changeType": "ADDED" },
            { "path": "mod.txt", "additions": 2, "deletions": 1, "changeType": "CHANGED" },
            { "path": "del.txt", "additions": 0, "deletions": 5, "changeType": "REMOVED" },
            { "path": "ren.txt", "additions": 1, "deletions": 1, "changeType": "RENAMED" },
            { "path": "cpr.txt", "additions": 1, "deletions": 0, "changeType": "COPIED" },
            { "path": "unk.txt", "additions": 0, "deletions": 0 }
        ]);
        let files = parse_files(&nodes);
        assert_eq!(files.len(), 6);
        assert_eq!(files[0].status, 'A');
        assert_eq!(files[1].status, 'M');
        assert_eq!(files[2].status, 'D');
        assert_eq!(files[3].status, 'R');
        assert_eq!(files[4].status, 'M');
        assert_eq!(files[5].status, '?');
    }

    #[test]
    fn test_parse_memberships_json_collects_orgs_and_teams() {
        let resp = json!({
            "data": {
                "viewer": {
                    "organizations": {
                        "pageInfo": { "hasNextPage": false },
                        "nodes": [
                            {
                                "login": "myorg",
                                "teams": {
                                    "pageInfo": { "hasNextPage": false },
                                    "nodes": [
                                        { "slug": "team-a", "name": "Team A" },
                                        { "slug": "team-b", "name": "Team B" }
                                    ]
                                }
                            },
                            {
                                "login": "otherorg",
                                "teams": {
                                    "pageInfo": { "hasNextPage": false },
                                    "nodes": []
                                }
                            }
                        ]
                    }
                }
            }
        });
        let (orgs, truncated) = parse_memberships_json(&resp);
        assert!(!truncated);
        assert_eq!(orgs.len(), 2);
        assert_eq!(orgs[0].login, "myorg");
        assert_eq!(orgs[0].teams.len(), 2);
        assert_eq!(orgs[0].teams[0].slug, "team-a");
        assert_eq!(orgs[0].teams[0].name, "Team A");
        assert_eq!(orgs[1].login, "otherorg");
        assert!(orgs[1].teams.is_empty());
    }

    #[test]
    fn test_parse_memberships_json_marks_truncated_on_either_page_info() {
        let org_page_truncated = json!({
            "data": { "viewer": { "organizations": {
                "pageInfo": { "hasNextPage": true },
                "nodes": []
            }}}
        });
        let (_, truncated) = parse_memberships_json(&org_page_truncated);
        assert!(truncated);

        let team_page_truncated = json!({
            "data": { "viewer": { "organizations": {
                "pageInfo": { "hasNextPage": false },
                "nodes": [{
                    "login": "myorg",
                    "teams": {
                        "pageInfo": { "hasNextPage": true },
                        "nodes": []
                    }
                }]
            }}}
        });
        let (orgs, truncated) = parse_memberships_json(&team_page_truncated);
        assert!(truncated);
        assert_eq!(orgs.len(), 1);
    }

    #[test]
    fn test_parse_memberships_json_handles_missing_data() {
        let (orgs, truncated) = parse_memberships_json(&json!({}));
        assert!(orgs.is_empty());
        assert!(!truncated);
    }

    // ── Pagination + concurrency tests (via the GraphqlTransport seam) ──

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    type Responder =
        Box<dyn Fn(&str, &serde_json::Value) -> Result<serde_json::Value> + Send + Sync>;

    /// Recording GraphQL transport: scripts responses per request, records
    /// every request, and tracks the maximum number of in-flight calls.
    struct MockTransport {
        responder: Responder,
        requests: Mutex<Vec<(String, serde_json::Value)>>,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
        delay: Option<std::time::Duration>,
    }

    impl MockTransport {
        fn new(
            responder: impl Fn(&str, &serde_json::Value) -> Result<serde_json::Value>
                + Send
                + Sync
                + 'static,
        ) -> Self {
            Self {
                responder: Box::new(responder),
                requests: Mutex::new(Vec::new()),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                delay: None,
            }
        }

        fn with_delay(mut self, delay: std::time::Duration) -> Self {
            self.delay = Some(delay);
            self
        }

        fn requests(&self) -> Vec<(String, serde_json::Value)> {
            self.requests.lock().unwrap().clone()
        }

        fn max_in_flight(&self) -> usize {
            self.max_in_flight.load(Ordering::SeqCst)
        }
    }

    impl GraphqlTransport for MockTransport {
        async fn graphql_query(
            &self,
            query: &str,
            variables: serde_json::Value,
        ) -> Result<serde_json::Value> {
            let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(current, Ordering::SeqCst);
            self.requests
                .lock()
                .unwrap()
                .push((query.to_string(), variables.clone()));
            if let Some(delay) = self.delay {
                tokio::time::sleep(delay).await;
            }
            let outcome = (self.responder)(query, &variables);
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            outcome
        }
    }

    fn done_page() -> serde_json::Value {
        json!({ "hasNextPage": false, "endCursor": null })
    }

    fn timeline_comment(body: &str) -> serde_json::Value {
        json!({
            "__typename": "IssueComment",
            "author": { "login": "alice" },
            "body": body,
            "createdAt": "2024-01-01T00:00:00Z",
            "url": "https://example.com/c"
        })
    }

    fn thread_node(id: &str, comment_bodies: &[&str], page: serde_json::Value) -> serde_json::Value {
        let comments: Vec<_> = comment_bodies
            .iter()
            .map(|b| {
                json!({
                    "author": { "login": "bob" },
                    "body": b,
                    "path": "src/main.rs",
                    "line": 1,
                    "createdAt": "2024-01-01T00:00:00Z",
                    "url": ""
                })
            })
            .collect();
        json!({
            "id": id,
            "isResolved": false,
            "isOutdated": false,
            "comments": { "pageInfo": page, "nodes": comments }
        })
    }

    /// Full initial PR detail response with all connections exhausted, then
    /// mutated by `mutate` for per-test page setups.
    fn detail_response(
        number: u64,
        mutate: impl FnOnce(&mut serde_json::Value),
    ) -> serde_json::Value {
        let mut pr = json!({
            "id": format!("PR_{}", number),
            "number": number,
            "title": "T",
            "body": "",
            "url": "https://example.com",
            "isDraft": false,
            "updatedAt": "2024-01-01T00:00:00Z",
            "author": { "login": "alice" },
            "headRefName": "feature",
            "baseRefName": "main",
            "mergeable": "MERGEABLE",
            "reviewDecision": null,
            "reviewRequests": { "nodes": [] },
            "viewerLatestReview": null,
            "latestReviews": { "nodes": [] },
            "commits": { "nodes": [] },
            "reviewThreads": { "pageInfo": done_page(), "nodes": [] },
            "timelineItems": { "pageInfo": done_page(), "nodes": [] },
            "files": { "pageInfo": done_page(), "nodes": [] }
        });
        mutate(&mut pr);
        json!({ "data": { "repository": { "pullRequest": pr } } })
    }

    fn search_result(number: u64) -> SearchResult {
        SearchResult {
            repo_owner: "org".into(),
            repo_name: "repo".into(),
            number,
            title: "T".into(),
            author: "alice".into(),
            updated_at: "2024-01-01T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn pagination_continuation_query_is_narrow() {
        let transport = MockTransport::new(|query, _vars| {
            if query == PR_DETAIL_QUERY {
                Ok(detail_response(1, |pr| {
                    pr["timelineItems"] = json!({
                        "pageInfo": { "hasNextPage": true, "endCursor": "t1" },
                        "nodes": [timeline_comment("one")]
                    });
                }))
            } else if query == PR_TIMELINE_ITEMS_QUERY {
                Ok(json!({ "data": { "repository": { "pullRequest": {
                    "timelineItems": {
                        "pageInfo": done_page(),
                        "nodes": [timeline_comment("two")]
                    }
                } } } }))
            } else {
                Err(anyhow!("unexpected query"))
            }
        });

        let pr = fetch_pr_detail(&transport, "org", "repo", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pr.timeline.len(), 2);

        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        let (continuation_query, continuation_vars) = &requests[1];
        // The continuation request fetches only its own connection: none of
        // the other four connections' field names appear in the query body.
        assert!(continuation_query.contains("timelineItems"));
        for other in ["reviewThreads", "files(", "reviewRequests", "statusCheckRollup"] {
            assert!(
                !continuation_query.contains(other),
                "continuation query for timelineItems must not fetch {}",
                other
            );
        }
        assert_eq!(continuation_vars["after"], "t1");
    }

    #[tokio::test]
    async fn pagination_appends_into_typed_collection() {
        let transport = MockTransport::new(|query, _vars| {
            if query == PR_DETAIL_QUERY {
                Ok(detail_response(1, |pr| {
                    pr["files"] = json!({
                        "pageInfo": { "hasNextPage": true, "endCursor": "f1" },
                        "nodes": [{ "path": "a.txt", "additions": 1, "deletions": 0, "changeType": "ADDED" }]
                    });
                    pr["timelineItems"] = json!({
                        "pageInfo": done_page(),
                        "nodes": [timeline_comment("only")]
                    });
                }))
            } else if query == PR_FILES_QUERY {
                Ok(json!({ "data": { "repository": { "pullRequest": {
                    "files": {
                        "pageInfo": done_page(),
                        "nodes": [{ "path": "b.txt", "additions": 2, "deletions": 1, "changeType": "CHANGED" }]
                    }
                } } } }))
            } else {
                Err(anyhow!("unexpected query"))
            }
        });

        let pr = fetch_pr_detail(&transport, "org", "repo", 1)
            .await
            .unwrap()
            .unwrap();
        // The appended page landed in files (in fetch order) and nowhere else.
        assert_eq!(pr.files.len(), 2);
        assert_eq!(pr.files[0].path, "a.txt");
        assert_eq!(pr.files[1].path, "b.txt");
        assert_eq!(pr.timeline.len(), 1);
        assert!(pr.review_threads.is_empty());
    }

    #[tokio::test]
    async fn pagination_follows_cursors_until_has_next_page_is_false() {
        let transport = MockTransport::new(|query, vars| {
            if query == PR_DETAIL_QUERY {
                Ok(detail_response(1, |pr| {
                    pr["files"] = json!({
                        "pageInfo": { "hasNextPage": true, "endCursor": "f1" },
                        "nodes": [{ "path": "p1", "additions": 0, "deletions": 0, "changeType": "ADDED" }]
                    });
                }))
            } else if query == PR_FILES_QUERY {
                let (nodes, page) = match vars["after"].as_str() {
                    Some("f1") => (
                        json!([{ "path": "p2", "additions": 0, "deletions": 0, "changeType": "ADDED" }]),
                        json!({ "hasNextPage": true, "endCursor": "f2" }),
                    ),
                    Some("f2") => (
                        json!([{ "path": "p3", "additions": 0, "deletions": 0, "changeType": "ADDED" }]),
                        done_page(),
                    ),
                    other => return Err(anyhow!("unexpected cursor {:?}", other)),
                };
                Ok(json!({ "data": { "repository": { "pullRequest": {
                    "files": { "pageInfo": page, "nodes": nodes }
                } } } }))
            } else {
                Err(anyhow!("unexpected query"))
            }
        });

        let pr = fetch_pr_detail(&transport, "org", "repo", 1)
            .await
            .unwrap()
            .unwrap();
        let paths: Vec<_> = pr.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["p1", "p2", "p3"]);

        let cursors: Vec<_> = transport
            .requests()
            .iter()
            .filter(|(q, _)| q == PR_FILES_QUERY)
            .map(|(_, v)| v["after"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(cursors, vec!["f1", "f2"]);
    }

    #[tokio::test]
    async fn pagination_truncates_at_max_connection_pages() {
        let transport = MockTransport::new(|query, _vars| {
            if query == PR_DETAIL_QUERY {
                Ok(detail_response(1, |pr| {
                    pr["files"] = json!({
                        "pageInfo": { "hasNextPage": true, "endCursor": "x" },
                        "nodes": []
                    });
                }))
            } else if query == PR_FILES_QUERY {
                // Never terminates on its own: hasNextPage stays true.
                Ok(json!({ "data": { "repository": { "pullRequest": {
                    "files": {
                        "pageInfo": { "hasNextPage": true, "endCursor": "x" },
                        "nodes": []
                    }
                } } } }))
            } else {
                Err(anyhow!("unexpected query"))
            }
        });

        fetch_pr_detail(&transport, "org", "repo", 1)
            .await
            .unwrap()
            .unwrap();

        let continuation_count = transport
            .requests()
            .iter()
            .filter(|(q, _)| q == PR_FILES_QUERY)
            .count();
        // The page counter starts at 1 (the initial full query), so the
        // continuation loop issues at most MAX_CONNECTION_PAGES - 1 requests.
        assert_eq!(continuation_count, MAX_CONNECTION_PAGES - 1);
    }

    #[tokio::test]
    async fn fetch_pr_details_bounds_concurrency_and_preserves_order() {
        let transport = MockTransport::new(|query, vars| {
            assert_eq!(query, PR_DETAIL_QUERY);
            let number = vars["number"].as_u64().unwrap();
            match number {
                3 => Ok(json!({ "data": { "repository": { "pullRequest": null } } })), // vanished
                5 => Err(anyhow!("boom")), // failed fetch
                n => Ok(detail_response(n, |_| {})),
            }
        })
        .with_delay(std::time::Duration::from_millis(10));

        let results: Vec<SearchResult> = (1..=10).map(search_result).collect();
        let fetch = fetch_pr_details(&transport, &results).await.unwrap();

        // Failed (5) and vanished (3) PRs are discarded from the hydrated
        // list; survivors keep the original input order.
        let numbers: Vec<u64> = fetch.prs.iter().map(|pr| pr.number).collect();
        assert_eq!(numbers, vec![1, 2, 4, 6, 7, 8, 9, 10]);
        // Only the transient failure is reported as failed — vanished PRs
        // are genuinely gone and must not be preserved by callers.
        assert_eq!(fetch.failed, vec![("org".to_string(), "repo".to_string(), 5)]);

        assert!(
            transport.max_in_flight() <= PR_DETAIL_FETCH_CONCURRENCY,
            "max in-flight {} exceeded the bound {}",
            transport.max_in_flight(),
            PR_DETAIL_FETCH_CONCURRENCY
        );
        assert!(
            transport.max_in_flight() > 1,
            "fetches did not overlap at all; expected concurrent hydration"
        );
    }

    #[tokio::test]
    async fn thread_ids_from_all_pages_drive_nested_comment_pagination() {
        let transport = MockTransport::new(|query, vars| {
            if query == PR_DETAIL_QUERY {
                Ok(detail_response(1, |pr| {
                    pr["reviewThreads"] = json!({
                        "pageInfo": { "hasNextPage": true, "endCursor": "rt1" },
                        "nodes": [thread_node(
                            "T1",
                            &["t1c1"],
                            json!({ "hasNextPage": true, "endCursor": "c1" }),
                        )]
                    });
                }))
            } else if query == PR_REVIEW_THREADS_QUERY {
                assert_eq!(vars["after"], "rt1");
                Ok(json!({ "data": { "repository": { "pullRequest": {
                    "reviewThreads": {
                        "pageInfo": done_page(),
                        "nodes": [thread_node(
                            "T2",
                            &["t2c1"],
                            json!({ "hasNextPage": true, "endCursor": "c2" }),
                        )]
                    }
                } } } }))
            } else if query == REVIEW_THREAD_COMMENTS_QUERY {
                let (body, expected_cursor) = match vars["threadId"].as_str() {
                    Some("T1") => ("t1c2", "c1"),
                    Some("T2") => ("t2c2", "c2"),
                    other => return Err(anyhow!("unexpected threadId {:?}", other)),
                };
                assert_eq!(vars["after"], expected_cursor);
                Ok(json!({ "data": { "node": { "comments": {
                    "pageInfo": done_page(),
                    "nodes": [{
                        "author": { "login": "bob" },
                        "body": body,
                        "path": "src/main.rs",
                        "line": 1,
                        "createdAt": "2024-01-01T00:00:00Z",
                        "url": ""
                    }]
                } } } }))
            } else {
                Err(anyhow!("unexpected query"))
            }
        });

        let pr = fetch_pr_detail(&transport, "org", "repo", 1)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(pr.review_threads.len(), 2);
        let t1: Vec<_> = pr.review_threads[0]
            .comments
            .iter()
            .map(|c| c.body.as_str())
            .collect();
        let t2: Vec<_> = pr.review_threads[1]
            .comments
            .iter()
            .map(|c| c.body.as_str())
            .collect();
        assert_eq!(t1, vec!["t1c1", "t1c2"]);
        assert_eq!(t2, vec!["t2c1", "t2c2"]);

        // Both thread ids (initial page and continuation page) drove nested
        // comment requests.
        let thread_ids: Vec<_> = transport
            .requests()
            .iter()
            .filter(|(q, _)| q == REVIEW_THREAD_COMMENTS_QUERY)
            .map(|(_, v)| v["threadId"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(thread_ids, vec!["T1", "T2"]);
    }
}
