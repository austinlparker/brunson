use std::sync::Arc;

use std::collections::HashSet;

use tokio::sync::watch;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::daemon::store::SharedStore;
use crate::github::client::GitHubClient;
use crate::github::graphql::fetch_pr_details;
use crate::github::search::{build_queries_for_config, dedup_results, filter_results_for_config};
use crate::github::types::PullRequest;
use crate::llm::classifier::Classifier;

/// Shared poll state used for refresh signaling.
pub struct PollState {
    /// Notify used to wake up the poller for an immediate refresh.
    refresh_notify: Arc<Notify>,
}

impl PollState {
    pub fn new(refresh_notify: Arc<Notify>) -> Self {
        Self { refresh_notify }
    }

    /// Signal the poller to run an immediate poll cycle.
    pub fn trigger_refresh(&self) {
        self.refresh_notify.notify_one();
    }
}

/// Run the background polling loop.
pub async fn run_poll_loop(
    gh_client: Arc<tokio::sync::RwLock<Option<GitHubClient>>>,
    store: SharedStore,
    config_rx: watch::Receiver<Config>,
    poll_state: Arc<PollState>,
    classifier: Arc<tokio::sync::RwLock<Option<Arc<Classifier>>>>,
    shutdown: CancellationToken,
) {
    info!("Poller started");

    loop {
        // Read the latest config at the top of every cycle. This means a
        // `POST /config/reload` takes effect on the next poll without a restart.
        let config = config_rx.borrow().clone();
        let poll_interval = std::time::Duration::from_secs(config.github.poll_interval.max(60));

        // A missing GitHub client is recoverable (the daemon may start before
        // auth is configured and recover later via `/config/reload`).
        let client = {
            let lock = gh_client.read().await;
            match lock.as_ref() {
                Some(c) => c.clone(),
                None => {
                    warn!("No GitHub client configured; skipping poll cycle");
                    {
                        let mut s = store.write().await;
                        s.refresh_in_progress = false;
                        s.last_poll_error = Some("GitHub auth/client is not available".to_string());
                    }
                    tokio::select! {
                        _ = shutdown.cancelled() => {
                            info!("Poller shutting down");
                            return;
                        }
                        _ = poll_state.refresh_notify.notified() => {
                            info!("Immediate refresh triggered");
                        }
                        _ = tokio::time::sleep(poll_interval) => {}
                    }
                    continue;
                }
            }
        };

        // Run a poll cycle
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("Poller shutting down");
                return;
            }
            result = run_poll_cycle(&client, &store, &config, &classifier) => {
                match result {
                    Ok(()) => {}
                    Err(e) => {
                        error!("Poll cycle failed: {}", e);
                        // Record error in store
                        {
                            let mut s = store.write().await;
                            s.last_poll_error = Some(e.to_string());
                        }
                    }
                }
            }
        }

        // Wait for next interval or immediate refresh
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("Poller shutting down");
                return;
            }
            _ = poll_state.refresh_notify.notified() => {
                info!("Immediate refresh triggered");
                // Update refresh_in_progress
                {
                    let mut s = store.write().await;
                    s.refresh_in_progress = true;
                }
            }
            _ = tokio::time::sleep(poll_interval) => {
                // Regular interval elapsed
            }
        }
    }
}

/// Run a single poll cycle.
async fn run_poll_cycle(
    client: &GitHubClient,
    store: &SharedStore,
    config: &Config,
    classifier: &Arc<tokio::sync::RwLock<Option<Arc<Classifier>>>>,
) -> anyhow::Result<()> {
    let start = std::time::Instant::now();

    // Set refresh_in_progress
    {
        let mut s = store.write().await;
        s.refresh_in_progress = true;
    }

    // Fetch the latest PR snapshot. Kept in its own Result so we can always
    // clear refresh_in_progress before returning, even on error.
    let fetch_result: anyhow::Result<Vec<PullRequest>> = async {
        // Build and run search queries
        let queries = build_queries_for_config(&config.github);
        let mut all_results = Vec::new();
        let mut search_errors = 0;
        let total_queries = queries.len();

        for query in &queries {
            for page in 1..=5u32 {
                match client.search_prs(query, page).await {
                    Ok(resp) => {
                        let results = resp.to_results();
                        all_results.extend(results);
                        if resp.items.len() < 100 {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("Search query '{}' page {} failed: {}", query, page, e);
                        search_errors += 1;
                        break;
                    }
                }
            }
        }

        // If ALL search queries failed, don't wipe the store — return an error
        if search_errors == total_queries {
            anyhow::bail!(
                "All {} search queries failed; preserving existing PR data",
                total_queries
            );
        }

        // De-duplicate
        let results = filter_results_for_config(dedup_results(all_results), &config.github);

        info!(
            "Found {} unique PRs from {} queries",
            results.len(),
            queries.len()
        );

        // Fetch GraphQL detail
        let prs = fetch_pr_details(client, &results).await?;
        Ok(prs)
    }
    .await;

    let prs = match fetch_result {
        Ok(prs) => prs,
        Err(e) => {
            // Clear the flag before propagating so the API doesn't report
            // refresh as permanently in progress.
            let mut s = store.write().await;
            s.refresh_in_progress = false;
            return Err(e);
        }
    };

    // Update store
    let changed_prs = {
        let mut s = store.write().await;
        let changed = s.update_prs(prs);
        s.last_poll_at = Some(chrono::Utc::now());
        s.last_poll_error = None;
        s.rate_limit_remaining = Some(client.rate_limit_remaining());
        s.refresh_in_progress = false;
        changed
    };

    // LLM classification for changed PRs and unclassified PRs. The latter
    // matters when LLM gets enabled after a store already has PRs loaded.
    if config.llm.enabled && config.llm.classify_on_change {
        let maybe_classifier = classifier.read().await.clone();
        if let Some(classifier) = maybe_classifier.as_ref() {
            let classify_prs = {
                let s = store.read().await;
                llm_classification_candidates(&changed_prs, s.prs.values())
            };

            if !classify_prs.is_empty() {
                info!("Classifying {} PRs with LLM", classify_prs.len());
            }

            for pr in &classify_prs {
                match classifier.classify(pr).await {
                    Ok(result) => {
                        let mut s = store.write().await;
                        if let Some(stored_pr) = s.prs.get_mut(&pr.node_id) {
                            stored_pr.llm_priority = Some(result.priority);
                            stored_pr.llm_summary = Some(result.summary);
                        }
                    }
                    Err(e) => {
                        warn!("LLM classification failed for PR {}: {}", pr.number, e);
                    }
                }
            }
        } else {
            warn!("LLM classification is enabled but no classifier is available");
        }
    }

    info!("Poll cycle completed in {:?}", start.elapsed());

    Ok(())
}

fn llm_classification_candidates<'a>(
    changed_prs: &[PullRequest],
    stored_prs: impl Iterator<Item = &'a PullRequest>,
) -> Vec<PullRequest> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();

    for pr in changed_prs {
        if seen.insert(pr.node_id.clone()) {
            candidates.push(pr.clone());
        }
    }

    for pr in stored_prs {
        let needs_classification = pr.llm_priority.is_none() || pr.llm_summary.is_none();
        if needs_classification && seen.insert(pr.node_id.clone()) {
            candidates.push(pr.clone());
        }
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::types::{
        CheckStatus, MergeableState, Priority, PullRequest, ReviewDecision,
    };

    #[test]
    fn llm_candidates_include_missing_classification() {
        let classified = make_pr("classified", Some(Priority::Low), Some("done"));
        let missing_priority = make_pr("missing-priority", None, Some("summary"));
        let missing_summary = make_pr("missing-summary", Some(Priority::High), None);

        let candidates = llm_classification_candidates(
            &[],
            [&classified, &missing_priority, &missing_summary].into_iter(),
        );

        let ids: Vec<_> = candidates.iter().map(|pr| pr.node_id.as_str()).collect();
        assert_eq!(ids, vec!["missing-priority", "missing-summary"]);
    }

    #[test]
    fn llm_candidates_include_changed_even_when_classified() {
        let changed = make_pr("changed", Some(Priority::Low), Some("done"));
        let unchanged = make_pr("unchanged", Some(Priority::Medium), Some("done"));

        let candidates = llm_classification_candidates(
            std::slice::from_ref(&changed),
            [&changed, &unchanged].into_iter(),
        );

        let ids: Vec<_> = candidates.iter().map(|pr| pr.node_id.as_str()).collect();
        assert_eq!(ids, vec!["changed"]);
    }

    fn make_pr(node_id: &str, priority: Option<Priority>, summary: Option<&str>) -> PullRequest {
        PullRequest {
            node_id: node_id.to_string(),
            number: 1,
            title: "Test".to_string(),
            body: String::new(),
            url: "https://example.com".to_string(),
            author: "author".to_string(),
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            is_draft: false,
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            head_ref: "feature".to_string(),
            base_ref: "main".to_string(),
            mergeable: MergeableState::Unknown,
            review_decision: None::<ReviewDecision>,
            review_requests: Vec::new(),
            viewer_latest_review: None,
            latest_reviews: Vec::new(),
            check_status: CheckStatus::None,
            checks: Vec::new(),
            review_threads: Vec::new(),
            timeline: Vec::new(),
            files: Vec::new(),
            comments: 0,
            llm_priority: priority,
            llm_summary: summary.map(str::to_string),
        }
    }
}
