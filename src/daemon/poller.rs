use std::sync::Arc;

use std::collections::HashSet;

use tokio::sync::watch;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::daemon::store::SharedStore;
use crate::github::client::GitHubClient;
use crate::github::graphql::{fetch_pr_details, fetch_viewer_team_memberships};
use crate::github::search::{
    build_query_specs_for_config, configured_team_review_requests, dedup_provenanced_results,
    filter_prs_by_provenance, filter_results_for_config, ProvenancedSearchResult,
};
use crate::github::types::PullRequest;
use crate::llm::classifier::{Classifier, CLASSIFY_BATCH_SIZE};
use crate::llm::LlmClassificationCache;

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
    llm_cache: Arc<tokio::sync::RwLock<LlmClassificationCache>>,
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
            result = run_poll_cycle(&client, &store, &config, &classifier, &llm_cache) => {
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
    llm_cache: &Arc<tokio::sync::RwLock<LlmClassificationCache>>,
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
        let queries = build_query_specs_for_config(&config.github);
        let mut all_results = Vec::new();
        let mut search_errors = 0;
        let total_queries = queries.len();

        for query in &queries {
            for page in 1..=5u32 {
                match client.search_prs(&query.query, page).await {
                    Ok(resp) => {
                        let results = resp.to_results().into_iter().map(|result| {
                            ProvenancedSearchResult::new(result, query.reason.clone())
                        });
                        all_results.extend(results);
                        if resp.items.len() < 100 {
                            break;
                        }
                    }
                    Err(e) => {
                        warn!("Search query '{}' page {} failed: {}", query.query, page, e);
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

        // De-duplicate while preserving query provenance.
        let provenance = dedup_provenanced_results(all_results);
        let results = filter_results_for_config(
            provenance.iter().map(|item| item.result.clone()).collect(),
            &config.github,
        );
        let result_keys: HashSet<_> = results
            .iter()
            .map(|r| {
                (
                    r.repo_owner.to_ascii_lowercase(),
                    r.repo_name.to_ascii_lowercase(),
                    r.number,
                )
            })
            .collect();
        let provenance: Vec<_> = provenance
            .into_iter()
            .filter(|item| {
                result_keys.contains(&(
                    item.result.repo_owner.to_ascii_lowercase(),
                    item.result.repo_name.to_ascii_lowercase(),
                    item.result.number,
                ))
            })
            .collect();

        info!(
            "Found {} unique PRs from {} queries",
            results.len(),
            queries.len()
        );

        // Fetch GraphQL detail, then remove stale team-only hits before store replacement.
        let prs = fetch_pr_details(client, &results).await?;
        let current_user = {
            let s = store.read().await;
            s.current_user.clone()
        };
        let configured_teams = configured_team_review_requests(&config.github);
        let team_memberships = if configured_teams.is_empty() {
            HashSet::new()
        } else {
            fetch_viewer_team_memberships(client, &configured_teams, &current_user).await?
        };
        Ok(filter_poll_snapshot(
            prs,
            &provenance,
            &config.github,
            &current_user,
            &team_memberships,
        ))
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

    // Update store and hydrate any persisted LLM classifications.
    let (changed_prs, known_node_ids) = {
        let mut s = store.write().await;
        let changed = s.update_prs(prs);
        {
            let cache = llm_cache.read().await;
            s.hydrate_llm_cache(&cache);
        }
        s.last_poll_at = Some(chrono::Utc::now());
        s.last_poll_error = None;
        s.rate_limit_remaining = Some(client.rate_limit_remaining());
        s.refresh_in_progress = false;
        let known_node_ids: HashSet<String> = s.prs.keys().cloned().collect();
        (changed, known_node_ids)
    };

    // Drop any persisted LLM cache entries for PRs no longer in this
    // snapshot (merged/closed, unassigned, access revoked, etc.) so the
    // on-disk cache stays in sync with what's actually being watched instead
    // of accumulating stale entries forever.
    let pruned = {
        let mut cache = llm_cache.write().await;
        cache.prune_missing(&known_node_ids)
    };
    if pruned {
        // Clone the cache under the read lock and flush the clone so the
        // lock isn't held across the (blocking) file write. If a concurrent
        // classification also flushes, the writes race last-writer-wins,
        // which is fine for a best-effort disk cache.
        let snapshot = { llm_cache.read().await.clone() };
        if let Err(e) = snapshot.flush().await {
            warn!(
                "Failed to persist LLM cache after pruning stale entries: {}",
                e
            );
        }
    }

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

            let mut any_classified = false;
            for chunk in classify_prs.chunks(CLASSIFY_BATCH_SIZE) {
                match classifier.classify_batch(chunk).await {
                    Ok(results) => {
                        for (pr, result) in chunk.iter().zip(results) {
                            let pr_for_cache = {
                                let mut s = store.write().await;
                                if let Some(stored_pr) = s.prs.get_mut(&pr.node_id) {
                                    stored_pr.llm_priority = Some(result.priority);
                                    stored_pr.llm_summary = Some(result.summary);
                                }
                                s.prs.get(&pr.node_id).cloned()
                            };
                            if let Some(pr) = pr_for_cache {
                                let mut cache = llm_cache.write().await;
                                cache.update_from_pr(&pr);
                                any_classified = true;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "LLM batch classification failed for {} PRs (chunk starting at PR #{}): {}",
                            chunk.len(),
                            chunk.first().map(|pr| pr.number).unwrap_or(0),
                            e
                        );
                    }
                }
            }
            if any_classified {
                let snapshot = { llm_cache.read().await.clone() };
                if let Err(e) = snapshot.flush().await {
                    warn!("Failed to persist LLM cache after classification: {}", e);
                }
            }
        } else {
            warn!("LLM classification is enabled but no classifier is available");
        }
    }

    info!("Poll cycle completed in {:?}", start.elapsed());

    Ok(())
}

fn filter_poll_snapshot(
    prs: Vec<PullRequest>,
    provenance: &[ProvenancedSearchResult],
    github_config: &crate::config::GithubConfig,
    current_user: &str,
    current_team_memberships: &HashSet<String>,
) -> Vec<PullRequest> {
    filter_prs_by_provenance(
        prs,
        provenance,
        github_config,
        current_user,
        current_team_memberships,
    )
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
    fn poll_cycle_filters_stale_team_pr_before_update_prs() {
        let mut store = crate::daemon::store::PrStore::new("me".into());
        let mut stale = make_pr("stale", None, None);
        stale.owner = "myorg".into();
        stale.repo = "repo".into();
        stale.number = 1;
        stale.team_review_requests = vec!["myorg/team-a".into()];
        let stale_slug = stale.slug();
        store.update_prs(vec![stale.clone()]);
        store.set_diff(stale_slug.clone(), "diff".into());

        let mut config = crate::config::GithubConfig::default();
        config.targets.push(crate::config::GithubTarget {
            repo: Some("myorg/repo".into()),
            team_review_requests: vec!["myorg/team-a".into()],
            include_authored: false,
            include_involved: false,
            direct_review_requests: false,
            ..Default::default()
        });
        let scope = crate::github::search::SearchScope::Repo {
            owner: "myorg".into(),
            repo: "repo".into(),
        };
        let result = crate::github::types::SearchResult {
            repo_owner: "myorg".into(),
            repo_name: "repo".into(),
            number: 1,
            title: "A".into(),
            author: "other".into(),
            updated_at: stale.updated_at.clone(),
        };
        let provenance = vec![ProvenancedSearchResult::new(
            result,
            crate::github::search::SearchReason::TargetTeamReview {
                scope,
                team: "myorg/team-a".into(),
            },
        )];

        let filtered =
            filter_poll_snapshot(vec![stale], &provenance, &config, "me", &HashSet::new());
        assert!(filtered.is_empty());
        store.update_prs(filtered);

        assert!(store.get_by_slug(&stale_slug).is_none());
        assert!(store.get_diff(&stale_slug).is_none());
    }

    // Regression test: mirrors what `run_poll_cycle` does after `update_prs` —
    // once a PR drops out of the live snapshot (left the team, merged, no
    // longer assigned, etc.), its LLM cache entry must be pruned too, so the
    // on-disk cache doesn't retain stale PR content forever.
    #[test]
    fn poll_cycle_prunes_llm_cache_for_prs_no_longer_in_snapshot() {
        let mut store = crate::daemon::store::PrStore::new("me".into());
        let kept = make_pr("kept", None, None);
        let gone = make_pr("gone", None, None);
        store.update_prs(vec![kept.clone(), gone.clone()]);

        let mut llm_cache = crate::llm::LlmClassificationCache::default();
        llm_cache.update_from_pr(&kept);
        llm_cache.update_from_pr(&gone);

        // Next poll only returns `kept` (e.g. `gone` was merged, or the user
        // lost access/left the team that requested review).
        store.update_prs(vec![kept]);
        let known_node_ids: HashSet<String> = store.prs.keys().cloned().collect();

        assert!(llm_cache.prune_missing(&known_node_ids));
        assert!(llm_cache.contains("kept"));
        assert!(!llm_cache.contains("gone"));
    }

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
            author_is_bot: false,
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            is_draft: false,
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            head_ref: "feature".to_string(),
            base_ref: "main".to_string(),
            mergeable: MergeableState::Unknown,
            review_decision: None::<ReviewDecision>,
            review_requests: Vec::new(),
            team_review_requests: Vec::new(),
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
            llm_rich_summary: None,
            last_seen_at: None,
        }
    }
}
