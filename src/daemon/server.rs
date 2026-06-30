use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use tokio::sync::watch;

use crate::api::*;
use crate::config::{default_endpoint_for_provider, Config};
use crate::daemon::poller::PollState;
use crate::daemon::setup_status::{evaluate_setup, SetupAuth, SetupCache};
use crate::daemon::store::SharedStore;
use crate::github::client::GitHubClient;
use crate::github::types::parse_slug;
use crate::llm::classifier::Classifier;

/// Shared application state for all handlers.
pub struct AppState {
    pub store: SharedStore,
    pub config_rx: watch::Receiver<Config>,
    pub config_tx: watch::Sender<Config>,
    pub config_path: Option<PathBuf>,
    pub poll_state: Arc<PollState>,
    pub classifier: Arc<tokio::sync::RwLock<Option<Arc<Classifier>>>>,
    pub gh_client: Arc<tokio::sync::RwLock<Option<GitHubClient>>>,
    pub setup_cache: Arc<tokio::sync::RwLock<SetupCache>>,
    pub auth: Box<dyn SetupAuth + Send + Sync>,
}

impl AppState {
    pub fn latest_config(&self) -> Config {
        self.config_rx.borrow().clone()
    }
}

/// Build the axum router with all routes.
pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(handle_health))
        .route("/setup/status", get(handle_setup_status))
        .route("/config", get(handle_config))
        .route("/config/preview", get(handle_config_preview))
        .route("/config/validate", post(handle_config_validate))
        .route("/config/reload", post(handle_config_reload))
        .route("/prs", get(handle_prs))
        .route("/prs/refresh", post(handle_refresh))
        .route("/prs/:id", get(handle_pr_detail))
        .route("/prs/:id/diff", get(handle_pr_diff))
        .route("/prs/:id/classify", post(handle_classify))
        .with_state(state)
}

async fn handle_health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let s = state.store.read().await;
    let (setup_status, setup_message) = {
        let cache = state.setup_cache.read().await;
        (
            cache.result.status.clone(),
            cache.result.next_steps.first().cloned(),
        )
    };
    Json(HealthResponse {
        service: crate::daemon::SERVICE_NAME.to_string(),
        version: crate::daemon::VERSION.to_string(),
        status: "ok".to_string(),
        current_user: s.current_user.clone(),
        last_poll_at: s.last_poll_at.map(|dt| dt.to_rfc3339()),
        last_poll_error: s.last_poll_error.clone(),
        rate_limit_remaining: s.rate_limit_remaining,
        refresh_in_progress: s.refresh_in_progress,
        setup_status,
        setup_message,
    })
}

async fn handle_setup_status(State(state): State<Arc<AppState>>) -> Json<SetupStatusResponse> {
    {
        let cache = state.setup_cache.read().await;
        if cache.is_fresh() {
            return Json(cache.result.clone());
        }
    }

    let result = evaluate_setup(
        &state.latest_config(),
        state.config_path.as_deref(),
        state.auth.as_ref(),
    )
    .await;

    {
        let mut cache = state.setup_cache.write().await;
        cache.cached_at = Some(std::time::Instant::now());
        cache.result = result.clone();
    }

    Json(result)
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

    let client = {
        let gh = state.gh_client.read().await;
        match gh.as_ref() {
            Some(client) => client.clone(),
            None => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ApiError::service_unavailable(
                        "Cannot fetch diff: GitHub auth/client is not available",
                    )),
                )
                    .into_response();
            }
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

async fn handle_refresh(State(state): State<Arc<AppState>>) -> Response {
    {
        let gh = state.gh_client.read().await;
        if gh.is_none() {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ApiError::service_unavailable(
                    "Cannot refresh: GitHub auth/client is not available",
                )),
            )
                .into_response();
        }
    }

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
        )
            .into_response();
    }

    {
        let mut s = state.store.write().await;
        s.refresh_in_progress = true;
    }
    state.poll_state.trigger_refresh();

    (
        StatusCode::ACCEPTED,
        Json(RefreshResponse {
            refresh_in_progress: true,
        }),
    )
        .into_response()
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

    // Check LLM is enabled in the latest config.
    if !state.latest_config().llm.enabled {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiError::bad_request("LLM classification is disabled")),
        )
            .into_response();
    }

    let classifier = {
        let c = state.classifier.read().await;
        match c.as_ref() {
            Some(classifier) => classifier.clone(),
            None => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(ApiError::service_unavailable(
                        "LLM classification is not available",
                    )),
                )
                    .into_response();
            }
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
    let mut config = state.latest_config();
    if !config.llm.api_key.is_empty() {
        config.llm.api_key = "***".to_string();
    }
    if config.llm.endpoint.is_empty() {
        config.llm.endpoint = default_endpoint_for_provider(&config.llm.provider).to_string();
    }
    Json(serde_json::to_value(&config).unwrap_or_default())
}

async fn handle_config_preview(State(state): State<Arc<AppState>>) -> Json<ConfigPreviewResponse> {
    let config = state.latest_config();
    Json(ConfigPreviewResponse {
        queries: crate::github::search::build_queries_for_config(&config.github),
    })
}

async fn handle_config_validate(Json(mut config): Json<Config>) -> Json<ConfigValidateResponse> {
    config.resolve_llm_defaults();
    match config.validate() {
        Ok(()) => Json(ConfigValidateResponse {
            valid: true,
            error: None,
            preview: ConfigPreviewResponse {
                queries: crate::github::search::build_queries_for_config(&config.github),
            },
        }),
        Err(e) => Json(ConfigValidateResponse {
            valid: false,
            error: Some(e.to_string()),
            preview: ConfigPreviewResponse {
                queries: Vec::new(),
            },
        }),
    }
}

async fn handle_config_reload(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<ConfigReloadResponse>) {
    let reload_path = state
        .config_path
        .clone()
        .unwrap_or_else(|| crate::config::config_file_path().unwrap_or_default());

    if reload_path.as_os_str().is_empty() || !reload_path.exists() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ConfigReloadResponse {
                reloaded: false,
                error: Some("config file not found".to_string()),
            }),
        );
    }

    let new_config = match Config::load(Some(&reload_path)) {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ConfigReloadResponse {
                    reloaded: false,
                    error: Some(e.to_string()),
                }),
            );
        }
    };

    let mut errors = Vec::new();

    // Re-resolve GitHub auth in case the user just ran `brunson setup`.
    match crate::daemon::resolve_github_client_and_user(state.auth.as_ref()).await {
        Ok((client, login)) => {
            {
                let mut store = state.store.write().await;
                store.current_user = login;
            }
            let mut gh = state.gh_client.write().await;
            *gh = Some(client);
        }
        Err(e) => {
            let msg = format!("GitHub auth/client unavailable after reload: {}", e);
            {
                let mut store = state.store.write().await;
                store.current_user = "unknown".to_string();
                store.last_poll_error = Some(msg.clone());
                store.refresh_in_progress = false;
            }
            let mut gh = state.gh_client.write().await;
            *gh = None;
            errors.push(msg);
        }
    }

    // Notify all watchers (including the poller) of the new config.
    let _ = state.config_tx.send(new_config.clone());

    // Rebuild classifier against the new LLM configuration.
    let classifier = crate::daemon::build_classifier(&new_config.llm).await;
    {
        let mut c = state.classifier.write().await;
        *c = classifier.clone();
    }

    // Refresh setup cache so /health immediately reflects the new config.
    let refreshed = evaluate_setup(
        &new_config,
        state.config_path.as_deref(),
        state.auth.as_ref(),
    )
    .await;
    {
        let mut cache = state.setup_cache.write().await;
        cache.cached_at = Some(std::time::Instant::now());
        cache.result = refreshed;
    }

    if classifier.is_none() && new_config.llm.enabled {
        errors.push("config reloaded, but failed to build LLM classifier".to_string());
    }

    (
        StatusCode::OK,
        Json(ConfigReloadResponse {
            reloaded: true,
            error: if errors.is_empty() {
                None
            } else {
                Some(errors.join("; "))
            },
        }),
    )
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
        make_test_state_with_store_config(store, Config::default())
    }

    fn make_test_state_with_store_config(store: PrStore, config: Config) -> Arc<AppState> {
        let refresh_notify = Arc::new(Notify::new());
        let poll_state = Arc::new(PollState::new(refresh_notify));
        let shared = Arc::new(tokio::sync::RwLock::new(store));
        let (config_tx, config_rx) = watch::channel(config);
        Arc::new(AppState {
            store: shared,
            config_rx,
            config_tx,
            config_path: None,
            poll_state,
            classifier: Arc::new(tokio::sync::RwLock::new(None)),
            gh_client: Arc::new(tokio::sync::RwLock::new(None)),
            setup_cache: Arc::new(tokio::sync::RwLock::new(SetupCache::default())),
            auth: Box::new(crate::daemon::setup_status::SetupAuthImpl),
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
    async fn test_config_preview_endpoint() {
        let mut config = Config::default();
        config.github.targets.push(crate::config::GithubTarget {
            repo: Some("myorg/repo".to_string()),
            team_review_requests: vec!["myorg/team".to_string()],
            include_authored: false,
            ..Default::default()
        });
        let state = make_test_state_with_store_config(PrStore::new("me".into()), config);
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/config/preview")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: ConfigPreviewResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.queries.len(), 2);
        assert!(json
            .queries
            .iter()
            .any(|q| q == "user-review-requested:@me is:pr is:open repo:myorg/repo"));
        assert!(json
            .queries
            .iter()
            .any(|q| q == "team-review-requested:myorg/team is:pr is:open repo:myorg/repo"));
    }

    #[tokio::test]
    async fn test_config_validate_endpoint_rejects_invalid_target() {
        let mut config = Config::default();
        config.github.targets.push(crate::config::GithubTarget {
            org: Some("myorg".to_string()),
            repo: Some("myorg/repo".to_string()),
            ..Default::default()
        });
        let body = serde_json::to_vec(&config).unwrap();
        let state = make_test_state(PrStore::new("me".into()));
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/config/validate")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: ConfigValidateResponse = serde_json::from_slice(&body).unwrap();
        assert!(!json.valid);
        assert!(json.error.unwrap().contains("cannot set both"));
        assert!(json.preview.queries.is_empty());
    }

    #[tokio::test]
    async fn test_config_endpoint_includes_tui_and_redacts_nonempty_secret() {
        let mut config = Config::default();
        config.llm.api_key = "secret".to_string();
        config.tui.show_line_numbers = false;
        let state = make_test_state_with_store_config(PrStore::new("me".into()), config);
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/config")
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
        assert_eq!(json["llm"]["api_key"], "***");
        assert_eq!(json["tui"]["show_line_numbers"], false);
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

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_refresh_without_github_client_returns_503() {
        let store = PrStore::new("me".into());
        let state = make_test_state(store);
        let app = build_router(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/prs/refresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let store = state.store.read().await;
        assert!(!store.refresh_in_progress);
    }

    #[tokio::test]
    async fn test_refresh_with_github_client_marks_in_progress() {
        let store = PrStore::new("me".into());
        let state = make_test_state(store);
        {
            let mut gh = state.gh_client.write().await;
            *gh = Some(
                GitHubClient::new("test-token".to_string(), Some("github.com".to_string()))
                    .unwrap(),
            );
        }
        let app = build_router(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/prs/refresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["refresh_in_progress"], true);
        let store = state.store.read().await;
        assert!(store.refresh_in_progress);
    }

    use futures::future::BoxFuture;
    use std::io::Write;
    use tokio::sync::Notify;

    struct TestAuth {
        user: String,
        fail_token: bool,
        fail_login: bool,
    }

    impl SetupAuth for TestAuth {
        fn resolve_token(&self) -> anyhow::Result<String> {
            if self.fail_token {
                anyhow::bail!("test token failure");
            }
            Ok("test-token".to_string())
        }

        fn viewer_login<'a>(
            &'a self,
            _token: &'a str,
            _host: &'a str,
        ) -> BoxFuture<'a, anyhow::Result<String>> {
            let user = self.user.clone();
            let fail_login = self.fail_login;
            Box::pin(async move {
                if fail_login {
                    anyhow::bail!("test viewer failure");
                }
                Ok(user)
            })
        }
    }

    fn make_test_state_with_config(
        config: Config,
        config_path: Option<std::path::PathBuf>,
    ) -> Arc<AppState> {
        let refresh_notify = Arc::new(Notify::new());
        let poll_state = Arc::new(PollState::new(refresh_notify));
        let store = PrStore::new("me".into());
        let shared = Arc::new(tokio::sync::RwLock::new(store));
        let (config_tx, config_rx) = watch::channel(config);
        Arc::new(AppState {
            store: shared,
            config_rx,
            config_tx,
            config_path,
            poll_state,
            classifier: Arc::new(tokio::sync::RwLock::new(None)),
            gh_client: Arc::new(tokio::sync::RwLock::new(None)),
            setup_cache: Arc::new(tokio::sync::RwLock::new(SetupCache::default())),
            auth: Box::new(TestAuth {
                user: "testuser".into(),
                fail_token: false,
                fail_login: false,
            }),
        })
    }

    fn make_test_state_with_auth(
        config: Config,
        config_path: Option<std::path::PathBuf>,
        auth: Box<dyn SetupAuth + Send + Sync>,
    ) -> Arc<AppState> {
        let refresh_notify = Arc::new(Notify::new());
        let poll_state = Arc::new(PollState::new(refresh_notify));
        let store = PrStore::new("me".into());
        let shared = Arc::new(tokio::sync::RwLock::new(store));
        let (config_tx, config_rx) = watch::channel(config);
        Arc::new(AppState {
            store: shared,
            config_rx,
            config_tx,
            config_path,
            poll_state,
            classifier: Arc::new(tokio::sync::RwLock::new(None)),
            gh_client: Arc::new(tokio::sync::RwLock::new(None)),
            setup_cache: Arc::new(tokio::sync::RwLock::new(SetupCache::default())),
            auth,
        })
    }

    fn write_temp_config(toml: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("brunson-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = dir.join(format!(
            "test-config-{}-{}.toml",
            std::process::id(),
            unique
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(toml.as_bytes()).unwrap();
        path
    }

    #[tokio::test]
    async fn test_health_includes_setup_status_from_cache() {
        let store = PrStore::new("me".into());
        let state = make_test_state(store);
        {
            let mut cache = state.setup_cache.write().await;
            cache.cached_at = Some(std::time::Instant::now());
            cache.result = SetupStatusResponse {
                ready: false,
                status: "missing_auth".to_string(),
                auth: crate::api::AuthStatus::default(),
                llm: crate::api::LlmSetupStatus::default(),
                next_steps: vec!["Run gh auth login".to_string()],
            };
        }

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
        assert_eq!(json["setup_status"], "missing_auth");
        assert_eq!(json["setup_message"], "Run gh auth login");
    }

    #[tokio::test]
    async fn test_setup_status_reports_ready() {
        let toml = r#"
[github]
watch = []
poll_interval = 300

[daemon]
port = 17890

[llm]
enabled = false
"#;
        let path = write_temp_config(toml);
        let config = Config::load(Some(&path)).unwrap();
        let state = make_test_state_with_config(config, Some(path));
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/setup/status")
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
        assert_eq!(json["status"], "ready");
        assert!(json["ready"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_config_reload_valid_config() {
        let toml = r#"
[github]
watch = []
poll_interval = 120

[daemon]
port = 17890

[llm]
enabled = false
"#;
        let path = write_temp_config(toml);
        let config = Config::load(Some(&path)).unwrap();
        let initial_interval = config.github.poll_interval;
        let state = make_test_state_with_config(config, Some(path.clone()));
        let app = build_router(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/config/reload")
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
        assert_eq!(json["reloaded"], true);
        assert!(json["error"].is_null());

        // The watch channel should now reflect the re-loaded config.
        assert_eq!(state.latest_config().github.poll_interval, initial_interval);
    }

    #[tokio::test]
    async fn test_config_reload_auth_failure_clears_stale_client() {
        let toml = r#"
[github]
watch = []
poll_interval = 120

[daemon]
port = 17890

[llm]
enabled = false
"#;
        let path = write_temp_config(toml);
        let config = Config::load(Some(&path)).unwrap();
        let state = make_test_state_with_auth(
            config,
            Some(path),
            Box::new(TestAuth {
                user: "newuser".into(),
                fail_token: true,
                fail_login: false,
            }),
        );
        {
            let mut store = state.store.write().await;
            store.current_user = "olduser".to_string();
        }
        {
            let mut gh = state.gh_client.write().await;
            *gh = Some(
                GitHubClient::new("old-token".to_string(), Some("github.com".to_string())).unwrap(),
            );
        }
        let app = build_router(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/config/reload")
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
        assert_eq!(json["reloaded"], true);
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("GitHub auth/client unavailable"));
        assert!(state.gh_client.read().await.is_none());
        let store = state.store.read().await;
        assert_eq!(store.current_user, "unknown");
        assert!(store.last_poll_error.is_some());
    }

    #[tokio::test]
    async fn test_config_reload_missing_file() {
        let state = make_test_state_with_config(
            Config::default(),
            Some(std::path::PathBuf::from("/nonexistent/brunson-config.toml")),
        );
        let app = build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/config/reload")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["reloaded"], false);
        assert!(!json["error"].is_null());
    }
}
