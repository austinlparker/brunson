use std::sync::Arc;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::daemon::store::SharedStore;
use crate::github::client::GitHubClient;
use crate::github::graphql::fetch_pr_details;
use crate::github::search::{build_queries, dedup_results};
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
    client: GitHubClient,
    store: SharedStore,
    config: Config,
    poll_state: Arc<PollState>,
    classifier: Option<Arc<Classifier>>,
    shutdown: CancellationToken,
) {
    let poll_interval = std::time::Duration::from_secs(config.github.poll_interval.max(60));

    info!(
        "Poller started with interval {:?}, watch: {:?}",
        poll_interval, config.github.watch
    );

    loop {
        // Run a poll cycle
        tokio::select! {
            _ = shutdown.cancelled() => {
                info!("Poller shutting down");
                return;
            }
            result = run_poll_cycle(&client, &store, &config, classifier.as_ref()) => {
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
    classifier: Option<&Arc<Classifier>>,
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
        let queries = build_queries(&config.github.watch);
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
        let results = dedup_results(all_results);

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

    // LLM classification for changed PRs
    if config.llm.enabled && config.llm.classify_on_change {
        for pr in &changed_prs {
            // Classification happens inline (simple approach)
            // In production, this would go through the mpsc channel
            // but for simplicity we handle it directly
            if let Some(classifier) = classifier {
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
        }
    }

    info!("Poll cycle completed in {:?}", start.elapsed());

    Ok(())
}
