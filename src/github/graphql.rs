use anyhow::{anyhow, Result};
use serde_json::json;
use tracing::{debug, warn};

use super::client::GitHubClient;
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

const MAX_CONNECTION_PAGES: usize = 20;

/// Build the variable-driven PR detail query.
pub fn build_pr_detail_query() -> &'static str {
    PR_DETAIL_QUERY
}

const PR_DETAIL_QUERY: &str = r#"query(
  $owner: String!,
  $repo: String!,
  $number: Int!,
  $reviewRequestsAfter: String,
  $reviewThreadsAfter: String,
  $timelineAfter: String,
  $filesAfter: String
) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      id
    number
    title
    body
    url
    isDraft
    updatedAt
    author { login }
    headRefName
    baseRefName
    mergeable
    reviewDecision
    reviewRequests(first: 100, after: $reviewRequestsAfter) {
      pageInfo {
        hasNextPage
        endCursor
      }
      nodes {
        requestedReviewer {
          ... on User { login }
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
            contexts(first: 30) {
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
    reviewThreads(first: 100, after: $reviewThreadsAfter) {
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
          }
        }
      }
    }
    timelineItems(first: 100, after: $timelineAfter) {
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
        }
        ... on PullRequestReview {
          author { login }
          body
          state
          submittedAt
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
          requestedReviewer { ... on User { login } }
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
    files(first: 100, after: $filesAfter) {
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
        }
      }
    }
  }
}"#;

/// Parse a GraphQL response into PullRequest structs.
/// Returns PRs keyed by their GraphQL node ID.
pub fn parse_batch_response(
    resp: &serde_json::Value,
    prs: &[(String, String, u64)],
) -> Vec<PullRequest> {
    let data = match resp.get("data") {
        Some(d) => d,
        None => {
            warn!("GraphQL response missing 'data' field");
            return Vec::new();
        }
    };

    let mut results = Vec::new();

    for (i, (owner, repo, number)) in prs.iter().enumerate() {
        let alias = format!("pr{}", i);
        let pr_data = &data[&alias]["pullRequest"];
        if pr_data.is_null() {
            debug!(
                "PR {}/{}/{} not found in GraphQL response",
                owner, repo, number
            );
            continue;
        }

        let pr = parse_single_pr(pr_data, owner, repo);
        results.push(pr);
    }

    results
}

fn parse_pr_response(
    resp: &serde_json::Value,
    owner: &str,
    repo: &str,
    number: u64,
) -> Option<PullRequest> {
    let pr_data = &resp["data"]["repository"]["pullRequest"];
    if pr_data.is_null() {
        debug!(
            "PR {}/{}/{} not found in GraphQL response",
            owner, repo, number
        );
        return None;
    }
    Some(parse_single_pr(pr_data, owner, repo))
}

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

    let review_requests: Vec<String> = pr["reviewRequests"]["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| n["requestedReviewer"]["login"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

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
        owner: owner.to_string(),
        repo: repo.to_string(),
        is_draft,
        updated_at,
        head_ref,
        base_ref,
        mergeable,
        review_decision,
        review_requests,
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
    }
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
    nodes
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .map(|n| {
                    let is_resolved = n["isResolved"].as_bool().unwrap_or(false);
                    let is_outdated = n["isOutdated"].as_bool().unwrap_or(false);
                    let comments: Vec<ReviewComment> = n["comments"]["nodes"]
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
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    ReviewThread {
                        is_resolved,
                        is_outdated,
                        comments,
                    }
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
                                .unwrap_or("someone");
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

                    Some(TimelineEvent {
                        event_type,
                        actor,
                        created_at,
                        detail,
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

/// Fetch detail for search results.
pub async fn fetch_pr_details(
    client: &GitHubClient,
    results: &[SearchResult],
) -> Result<Vec<PullRequest>> {
    let pr_keys: Vec<(String, String, u64)> = results
        .iter()
        .map(|r| (r.repo_owner.clone(), r.repo_name.clone(), r.number))
        .collect();

    let mut all_prs = Vec::new();

    for (owner, repo, number) in pr_keys {
        if let Some(pr) = fetch_pr_detail(client, &owner, &repo, number).await? {
            all_prs.push(pr);
        }
    }

    Ok(all_prs)
}

async fn fetch_pr_detail(
    client: &GitHubClient,
    owner: &str,
    repo: &str,
    number: u64,
) -> Result<Option<PullRequest>> {
    let mut resp = client
        .graphql_with_variables(
            PR_DETAIL_QUERY,
            pr_detail_variables(owner, repo, number, None, None, None, None),
        )
        .await?;

    if pr_value(&resp).is_none() {
        return Ok(None);
    }

    paginate_pr_connection(client, &mut resp, owner, repo, number, "reviewRequests").await?;
    paginate_pr_connection(client, &mut resp, owner, repo, number, "reviewThreads").await?;
    paginate_pr_connection(client, &mut resp, owner, repo, number, "timelineItems").await?;
    paginate_pr_connection(client, &mut resp, owner, repo, number, "files").await?;
    paginate_review_thread_comments(client, &mut resp).await?;

    Ok(parse_pr_response(&resp, owner, repo, number))
}

fn pr_detail_variables(
    owner: &str,
    repo: &str,
    number: u64,
    review_requests_after: Option<String>,
    review_threads_after: Option<String>,
    timeline_after: Option<String>,
    files_after: Option<String>,
) -> serde_json::Value {
    json!({
        "owner": owner,
        "repo": repo,
        "number": number,
        "reviewRequestsAfter": review_requests_after,
        "reviewThreadsAfter": review_threads_after,
        "timelineAfter": timeline_after,
        "filesAfter": files_after,
    })
}

async fn paginate_pr_connection(
    client: &GitHubClient,
    merged: &mut serde_json::Value,
    owner: &str,
    repo: &str,
    number: u64,
    connection: &str,
) -> Result<()> {
    let mut pages = 1usize;
    while let Some(cursor) = next_cursor(pr_connection(merged, connection)) {
        if pages >= MAX_CONNECTION_PAGES {
            return Err(anyhow!(
                "GraphQL {} pagination exceeded {} pages for {}/{}/{}",
                connection,
                MAX_CONNECTION_PAGES,
                owner,
                repo,
                number
            ));
        }

        let vars = match connection {
            "reviewRequests" => {
                pr_detail_variables(owner, repo, number, Some(cursor), None, None, None)
            }
            "reviewThreads" => {
                pr_detail_variables(owner, repo, number, None, Some(cursor), None, None)
            }
            "timelineItems" => {
                pr_detail_variables(owner, repo, number, None, None, Some(cursor), None)
            }
            "files" => pr_detail_variables(owner, repo, number, None, None, None, Some(cursor)),
            _ => return Err(anyhow!("unknown PR connection '{}'", connection)),
        };
        let page = client.graphql_with_variables(PR_DETAIL_QUERY, vars).await?;
        append_connection_page(merged, &page, connection)?;
        pages += 1;
    }
    Ok(())
}

async fn paginate_review_thread_comments(
    client: &GitHubClient,
    merged: &mut serde_json::Value,
) -> Result<()> {
    let Some(nodes) = pr_connection_mut(merged, "reviewThreads")
        .and_then(|v| v.get_mut("nodes"))
        .and_then(|v| v.as_array_mut())
    else {
        return Ok(());
    };

    for thread in nodes {
        let Some(thread_id) = thread.get("id").and_then(|v| v.as_str()).map(String::from) else {
            continue;
        };
        let mut pages = 1usize;
        while let Some(cursor) = next_cursor(thread.get("comments")) {
            if pages >= MAX_CONNECTION_PAGES {
                return Err(anyhow!(
                    "GraphQL review thread comments pagination exceeded {} pages for thread {}",
                    MAX_CONNECTION_PAGES,
                    thread_id
                ));
            }
            let page = client
                .graphql_with_variables(
                    REVIEW_THREAD_COMMENTS_QUERY,
                    json!({ "threadId": thread_id, "after": cursor }),
                )
                .await?;
            append_thread_comments_page(thread, &page)?;
            pages += 1;
        }
    }

    Ok(())
}

fn pr_value(resp: &serde_json::Value) -> Option<&serde_json::Value> {
    let pr = &resp["data"]["repository"]["pullRequest"];
    if pr.is_null() {
        None
    } else {
        Some(pr)
    }
}

fn pr_value_mut(resp: &mut serde_json::Value) -> Option<&mut serde_json::Value> {
    let pr = &mut resp["data"]["repository"]["pullRequest"];
    if pr.is_null() {
        None
    } else {
        Some(pr)
    }
}

fn pr_connection<'a>(resp: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    pr_value(resp).and_then(|pr| pr.get(name))
}

fn pr_connection_mut<'a>(
    resp: &'a mut serde_json::Value,
    name: &str,
) -> Option<&'a mut serde_json::Value> {
    pr_value_mut(resp).and_then(|pr| pr.get_mut(name))
}

fn next_cursor(connection: Option<&serde_json::Value>) -> Option<String> {
    let page_info = connection?.get("pageInfo")?;
    if !page_info
        .get("hasNextPage")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return None;
    }
    page_info
        .get("endCursor")
        .and_then(|v| v.as_str())
        .map(String::from)
}

fn append_connection_page(
    merged: &mut serde_json::Value,
    page: &serde_json::Value,
    connection: &str,
) -> Result<()> {
    let page_connection = pr_connection(page, connection)
        .ok_or_else(|| anyhow!("GraphQL page missing {} connection", connection))?;
    let page_nodes = page_connection
        .get("nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let page_info = page_connection
        .get("pageInfo")
        .cloned()
        .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));

    let merged_connection = pr_connection_mut(merged, connection)
        .ok_or_else(|| anyhow!("GraphQL base response missing {} connection", connection))?;
    let merged_nodes = merged_connection
        .get_mut("nodes")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow!("GraphQL {} connection missing nodes", connection))?;
    merged_nodes.extend(page_nodes);
    merged_connection["pageInfo"] = page_info;
    Ok(())
}

fn append_thread_comments_page(
    thread: &mut serde_json::Value,
    page: &serde_json::Value,
) -> Result<()> {
    let page_comments = page
        .get("data")
        .and_then(|v| v.get("node"))
        .and_then(|v| v.get("comments"))
        .ok_or_else(|| anyhow!("GraphQL page missing review thread comments"))?;
    let page_nodes = page_comments
        .get("nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let page_info = page_comments
        .get("pageInfo")
        .cloned()
        .unwrap_or_else(|| json!({ "hasNextPage": false, "endCursor": null }));

    let comments = thread
        .get_mut("comments")
        .ok_or_else(|| anyhow!("review thread missing comments connection"))?;
    let merged_nodes = comments
        .get_mut("nodes")
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow!("review thread comments missing nodes"))?;
    merged_nodes.extend(page_nodes);
    comments["pageInfo"] = page_info;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_pr_detail_query_uses_variables_not_string_interpolation() {
        let query = build_pr_detail_query();
        assert!(query.contains("$owner: String!"));
        assert!(query.contains("repository(owner: $owner, name: $repo)"));
        assert!(!query.contains("repository(owner:\""));
    }

    #[test]
    fn test_append_connection_page_merges_nodes_and_page_info() {
        let mut merged = json!({
            "data": {
                "repository": {
                    "pullRequest": {
                        "files": {
                            "pageInfo": { "hasNextPage": true, "endCursor": "a" },
                            "nodes": [{ "path": "a.txt" }]
                        }
                    }
                }
            }
        });
        let page = json!({
            "data": {
                "repository": {
                    "pullRequest": {
                        "files": {
                            "pageInfo": { "hasNextPage": false, "endCursor": null },
                            "nodes": [{ "path": "b.txt" }]
                        }
                    }
                }
            }
        });

        append_connection_page(&mut merged, &page, "files").unwrap();

        let files = merged["data"]["repository"]["pullRequest"]["files"]["nodes"]
            .as_array()
            .unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["path"], "a.txt");
        assert_eq!(files[1]["path"], "b.txt");
        assert_eq!(
            merged["data"]["repository"]["pullRequest"]["files"]["pageInfo"]["hasNextPage"],
            false
        );
    }

    #[test]
    fn test_append_thread_comments_page_merges_comments() {
        let mut thread = json!({
            "id": "thread1",
            "comments": {
                "pageInfo": { "hasNextPage": true, "endCursor": "a" },
                "nodes": [{ "body": "first" }]
            }
        });
        let page = json!({
            "data": {
                "node": {
                    "comments": {
                        "pageInfo": { "hasNextPage": false, "endCursor": null },
                        "nodes": [{ "body": "second" }]
                    }
                }
            }
        });

        append_thread_comments_page(&mut thread, &page).unwrap();

        let comments = thread["comments"]["nodes"].as_array().unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0]["body"], "first");
        assert_eq!(comments[1]["body"], "second");
        assert_eq!(thread["comments"]["pageInfo"]["hasNextPage"], false);
    }

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
}
