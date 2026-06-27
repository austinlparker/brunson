use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};

use crate::api::*;
use crate::config::Config;
use crate::daemon::poller::PollState;
use crate::daemon::store::SharedStore;
use crate::github::types::parse_slug;
use crate::llm::classifier::Classifier;

/// Shared application state for all handlers.
pub struct AppState {
    pub store: SharedStore,
    pub config: Config,
    pub poll_state: Arc<PollState>,
    pub classifier: Option<Arc<Classifier>>,
}

/// Build the axum router with all routes.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(handle_health))
        .route("/prs", get(handle_prs))
        .route("/prs/refresh", post(handle_refresh))
        .route("/prs/:id", get(handle_pr_detail))
        .route("/prs/:id/diff", get(handle_pr_diff))
        .route("/prs/:id/classify", post(handle_classify))
        .route("/config", get(handle_config))
        .with_state(state)
}

async fn handle_health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let s = state.store.read().await;
    Json(HealthResponse {
        service: crate::daemon::SERVICE_NAME.to_string(),
        version: crate::daemon::VERSION.to_string(),
        status: "ok".to_string(),
        current_user: s.current_user.clone(),
        last_poll_at: s.last_poll_at.map(|dt| dt.to_rfc3339()),
        last_poll_error: s.last_poll_error.clone(),
        rate_limit_remaining: s.rate_limit_remaining,
        refresh_in_progress: s.refresh_in_progress,
    })
}

async fn handle_prs(State(state): State<Arc<AppState>>) -> Json<PrListResponse> {
    let s = state.store.read().await;
    let grouped = s.group_prs();

    let mut groups: HashMap<String, Vec<PrSummary>> = HashMap::new();

    for (group, prs) in grouped {
        let key = group_key(&group);
        let summaries: Vec<PrSummary> = prs
            .iter()
            .map(|pr| PrSummary {
                id: pr.slug(),
                node_id: pr.node_id.clone(),
                owner: pr.owner.clone(),
                repo: pr.repo.clone(),
                number: pr.number,
                title: pr.title.clone(),
                author: pr.author.clone(),
                group: key.clone(),
                next_action: s.next_action(pr).to_string(),
                check_status: check_status_string(&pr.check_status),
                llm_priority: pr.llm_priority,
                updated_at: pr.updated_at.clone(),
                url: pr.url.clone(),
                comments: s.comment_count(pr),
            })
            .collect();
        groups.insert(key, summaries);
    }

    let updated_at = s.last_poll_at.map(|dt| dt.to_rfc3339()).unwrap_or_default();

    Json(PrListResponse { groups, updated_at })
}

async fn handle_pr_detail(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    // Validate slug format
    let (owner, repo, number) = match parse_slug(&id) {
        Some(v) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiError::bad_request(format!(
                    "Invalid PR slug '{}': expected '{{owner}}~{{repo}}~{{number}}'",
                    id
                ))),
            )
                .into_response();
        }
    };

    let s = state.store.read().await;
    let pr = match s.get_by_slug(&id) {
        Some(pr) => pr,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiError::not_found(format!(
                    "PR {}/{}/{} not found",
                    owner, repo, number
                ))),
            )
                .into_response();
        }
    };

    let detail = PrDetailResponse {
        id: pr.slug(),
        node_id: pr.node_id.clone(),
        owner: pr.owner.clone(),
        repo: pr.repo.clone(),
        number: pr.number,
        title: pr.title.clone(),
        body: pr.body.clone(),
        url: pr.url.clone(),
        author: pr.author.clone(),
        is_draft: pr.is_draft,
        updated_at: pr.updated_at.clone(),
        head_ref: pr.head_ref.clone(),
        base_ref: pr.base_ref.clone(),
        mergeable: serde_json::to_value(pr.mergeable)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_default(),
        review_decision: pr.review_decision.as_ref().map(|rd| {
            serde_json::to_value(rd)
                .unwrap_or_default()
                .to_string()
                .trim_matches('"')
                .to_string()
        }),
        review_requests: pr.review_requests.clone(),
        viewer_latest_review: pr.viewer_latest_review.clone(),
        latest_reviews: pr
            .latest_reviews
            .iter()
            .map(|r| LatestReviewDto {
                author: r.author.clone(),
                state: r.state.clone(),
            })
            .collect(),
        check_status: check_status_string(&pr.check_status),
        checks: pr
            .checks
            .iter()
            .map(|c| CheckEntryDto {
                name: c.name.clone(),
                status: c.status.clone(),
                conclusion: c.conclusion.clone(),
                url: c.url.clone(),
            })
            .collect(),
        review_threads: pr
            .review_threads
            .iter()
            .map(|t| ReviewThreadDto {
                is_resolved: t.is_resolved,
                is_outdated: t.is_outdated,
                comments: t
                    .comments
                    .iter()
                    .map(|c| ReviewCommentDto {
                        author: c.author.clone(),
                        body: c.body.clone(),
                        path: c.path.clone(),
                        line: c.line,
                    })
                    .collect(),
            })
            .collect(),
        files: pr
            .files
            .iter()
            .map(|f| FileDto {
                path: f.path.clone(),
                additions: f.additions,
                deletions: f.deletions,
                status: f.status,
            })
            .collect(),
        timeline: pr
            .timeline
            .iter()
            .map(|e| TimelineEventDto {
                event_type: serde_json::to_value(&e.event_type)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| "other".to_string()),
                actor: e.actor.clone(),
                created_at: e.created_at.clone(),
                detail: e.detail.clone(),
            })
            .collect(),
        llm_priority: pr.llm_priority,
        llm_summary: pr.llm_summary.clone(),
    };

    Json(detail).into_response()
}

async fn handle_pr_diff(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let (owner, repo, number) = match parse_slug(&id) {
        Some(v) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiError::bad_request(format!("Invalid PR slug '{}'", id))),
            )
                .into_response();
        }
    };

    // Check cache first
    {
        let s = state.store.read().await;
        if let Some(diff) = s.get_diff(&id) {
            return Json(DiffResponse {
                diff: diff.clone(),
                cached: true,
            })
            .into_response();
        }
    }

    // Check that PR exists
    {
        let s = state.store.read().await;
        if s.get_by_slug(&id).is_none() {
            return (
                StatusCode::NOT_FOUND,
                Json(ApiError::not_found(format!(
                    "PR {}/{}/{} not found",
                    owner, repo, number
                ))),
            )
                .into_response();
        }
    }

    // Fetch diff from GitHub
    // We need the GitHub client — reconstruct from config
    let token = match crate::github::auth::resolve_token() {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ApiError::service_unavailable(format!(
                    "Cannot fetch diff: {}",
                    e
                ))),
            )
                .into_response();
        }
    };

    let host = crate::github::auth::resolve_host();
    let client = match crate::github::client::GitHubClient::new(token, Some(host)) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    code: "internal_error".to_string(),
                    message: e.to_string(),
                    retryable: false,
                }),
            )
                .into_response();
        }
    };

    match client.get_pr_diff(&owner, &repo, number).await {
        Ok(diff) => {
            // Cap diff size to prevent excessive memory usage
            let max_diff_bytes = 5_000_000; // 5 MB limit
            if diff.len() > max_diff_bytes {
                let msg = format!(
                    "Diff is {} bytes (max {}), too large to display",
                    diff.len(),
                    max_diff_bytes
                );
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    Json(ApiError {
                        code: "diff_too_large".to_string(),
                        message: msg,
                        retryable: false,
                    }),
                )
                    .into_response();
            }
            // Cache it
            {
                let mut s = state.store.write().await;
                s.set_diff(id.clone(), diff.clone());
            }
            Json(DiffResponse {
                diff,
                cached: false,
            })
            .into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(ApiError {
                code: "upstream_error".to_string(),
                message: format!("Failed to fetch diff: {}", e),
                retryable: true,
            }),
        )
            .into_response(),
    }
}

async fn handle_refresh(State(state): State<Arc<AppState>>) -> (StatusCode, Json<RefreshResponse>) {
    // Check if refresh is already running
    let already_running = {
        let s = state.store.read().await;
        s.refresh_in_progress
    };

    if already_running {
        return (
            StatusCode::ACCEPTED,
            Json(RefreshResponse {
                refresh_in_progress: true,
            }),
        );
    }

    // Trigger refresh
    state.poll_state.trigger_refresh();

    (
        StatusCode::ACCEPTED,
        Json(RefreshResponse {
            refresh_in_progress: false,
        }),
    )
}

async fn handle_classify(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let (owner, repo, number) = match parse_slug(&id) {
        Some(v) => v,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiError::bad_request(format!("Invalid PR slug '{}'", id))),
            )
                .into_response();
        }
    };

    // Check LLM is enabled
    let classifier = match &state.classifier {
        Some(c) => c.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ApiError::service_unavailable(
                    "LLM classification is not enabled",
                )),
            )
                .into_response();
        }
    };

    // Get the PR
    let pr = {
        let s = state.store.read().await;
        match s.get_by_slug(&id) {
            Some(pr) => pr.clone(),
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(ApiError::not_found(format!(
                        "PR {}/{}/{} not found",
                        owner, repo, number
                    ))),
                )
                    .into_response();
            }
        }
    };

    // Run classification
    match classifier.classify(&pr).await {
        Ok(result) => {
            let mut s = state.store.write().await;
            if let Some(stored_pr) = s.prs.get_mut(&pr.node_id) {
                stored_pr.llm_priority = Some(result.priority);
                stored_pr.llm_summary = Some(result.summary);
            }
            drop(s);
            (
                StatusCode::OK,
                Json(ClassifyResponse {
                    status: format!("Classified as {:?}", result.priority),
                }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiError::service_unavailable(format!(
                "Classification failed: {}",
                e
            ))),
        )
            .into_response(),
    }
}

async fn handle_config(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    // Return sanitized config (no secrets)
    Json(serde_json::json!({
        "github": {
            "watch": state.config.github.watch,
            "poll_interval": state.config.github.poll_interval,
        },
        "daemon": {
            "port": state.config.daemon.port,
            "kill_on_tui_exit": state.config.daemon.kill_on_tui_exit,
        },
        "llm": {
            "enabled": state.config.llm.enabled,
            "endpoint": state.config.llm.endpoint,
            "model": state.config.llm.model,
            "classify_on_change": state.config.llm.classify_on_change,
            "max_output_tokens": state.config.llm.max_output_tokens,
        },
        "tui": {
            "diff_style": state.config.tui.diff_style,
            "show_line_numbers": state.config.tui.show_line_numbers,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::store::PrStore;
    use crate::github::types::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn make_test_prs() -> Vec<PullRequest> {
        vec![PullRequest {
            node_id: "node1".into(),
            number: 42,
            title: "Test PR".into(),
            body: "Description".into(),
            url: "https://github.com/org/repo/pull/42".into(),
            author: "other".into(),
            owner: "org".into(),
            repo: "repo".into(),
            is_draft: false,
            updated_at: chrono::Utc::now().to_rfc3339(),
            head_ref: "feature".into(),
            base_ref: "main".into(),
            mergeable: MergeableState::Unknown,
            review_decision: None,
            review_requests: vec!["me".into()],
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
        }]
    }

    fn make_test_state(store: PrStore) -> Arc<AppState> {
        let refresh_notify = Arc::new(Notify::new());
        let poll_state = Arc::new(PollState::new(refresh_notify));
        let shared = Arc::new(tokio::sync::RwLock::new(store));
        Arc::new(AppState {
            store: shared,
            config: Config::default(),
            poll_state,
            classifier: None,
        })
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let store = PrStore::new("testuser".into());
        let state = make_test_state(store);
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["service"], crate::daemon::SERVICE_NAME);
        assert_eq!(json["status"], "ok");
        assert_eq!(json["current_user"], "testuser");
    }

    #[tokio::test]
    async fn test_prs_endpoint() {
        let mut store = PrStore::new("me".into());
        store.update_prs(make_test_prs());
        let state = make_test_state(store);
        let app = build_router(state);

        let response = app
            .oneshot(Request::builder().uri("/prs").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let groups = json["groups"].as_object().unwrap();
        // The PR should be in some group
        assert!(!groups.is_empty());
    }

    #[tokio::test]
    async fn test_pr_detail_not_found() {
        let store = PrStore::new("me".into());
        let state = make_test_state(store);
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/prs/org~repo~999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_pr_detail_bad_slug() {
        let store = PrStore::new("me".into());
        let state = make_test_state(store);
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/prs/invalid")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_pr_detail_found() {
        let mut store = PrStore::new("me".into());
        store.update_prs(make_test_prs());
        let state = make_test_state(store);
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/prs/org~repo~42")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["number"], 42);
        assert_eq!(json["title"], "Test PR");
    }

    #[tokio::test]
    async fn test_classify_llm_disabled() {
        let store = PrStore::new("me".into());
        let state = make_test_state(store);
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/prs/org~repo~42/classify")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    use tokio::sync::Notify;
}
