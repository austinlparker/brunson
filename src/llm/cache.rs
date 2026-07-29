use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::task;

use crate::config::data_dir;
use crate::github::types::{LlmRichSummary, Priority, PullRequest};
use crate::llm::classifier::RICH_PROMPT_VERSION;

const CACHE_FILE: &str = "llm_cache.json";
const CACHE_VERSION: u32 = 1;

/// Disk-persisted LLM classification cache.
///
/// Classifications are keyed by PR `node_id` and are only considered valid when
/// the cached `updated_at` matches the PR's current `updated_at`. This lets the
/// daemon avoid re-classifying unchanged PRs after a restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmClassificationCache {
    version: u32,
    entries: HashMap<String, LlmCacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmCacheEntry {
    /// The PR's `updated_at` value at the time of classification.
    pub updated_at: String,
    pub llm_priority: Option<Priority>,
    pub llm_summary: Option<String>,
    pub llm_rich_summary: Option<LlmRichSummary>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

impl LlmClassificationCache {
    /// Load the cache from disk, returning an empty cache if the file is missing.
    pub fn load() -> Result<Self> {
        let path = cache_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read LLM cache from {}", path.display()))?;
        let mut cache: Self = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse LLM cache from {}", path.display()))?;
        if cache.version != CACHE_VERSION {
            // Version mismatch: treat as stale and start fresh.
            cache.entries.clear();
            cache.version = CACHE_VERSION;
        }
        Ok(cache)
    }

    /// Hydrate a PR from the cache if the cached data is still valid.
    pub fn apply_to_pr(&self, pr: &mut PullRequest) {
        let Some(entry) = self.entries.get(&pr.node_id) else {
            return;
        };
        if entry.updated_at != pr.updated_at {
            return;
        }
        pr.llm_priority = entry.llm_priority;
        pr.llm_summary = entry.llm_summary.clone();
        pr.last_seen_at = entry.last_seen_at;

        // Only restore rich summaries produced by the current prompt version.
        // Older versions are discarded so the next Overview view re-classifies.
        pr.llm_rich_summary = entry.llm_rich_summary.as_ref().and_then(|rich| {
            if rich.prompt_version == RICH_PROMPT_VERSION {
                Some(rich.clone())
            } else {
                None
            }
        });
    }

    /// Update the cache entry from a PR in memory.
    pub fn update_from_pr(&mut self, pr: &PullRequest) {
        let entry = LlmCacheEntry {
            updated_at: pr.updated_at.clone(),
            llm_priority: pr.llm_priority,
            llm_summary: pr.llm_summary.clone(),
            llm_rich_summary: pr.llm_rich_summary.clone(),
            last_seen_at: pr.last_seen_at,
        };
        self.entries.insert(pr.node_id.clone(), entry);
    }

    /// Drop entries for PRs that are no longer in the current poll snapshot
    /// (merged/closed, no longer assigned, access revoked, etc.), so the
    /// on-disk cache doesn't grow unbounded and doesn't retain stale PR
    /// content indefinitely. Returns whether any entries were removed.
    pub fn prune_missing(&mut self, known_node_ids: &std::collections::HashSet<String>) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|node_id, _| known_node_ids.contains(node_id));
        self.entries.len() != before
    }

    /// Whether an entry exists for the given PR node ID.
    pub fn contains(&self, node_id: &str) -> bool {
        self.entries.contains_key(node_id)
    }

    /// Record that the user has seen a PR in memory. `updated_at` is the PR's
    /// current `updated_at`, so a stub entry (when no classification exists
    /// yet) still matches on the next poll instead of being silently dropped.
    pub fn record_seen(&mut self, node_id: &str, updated_at: &str, seen_at: DateTime<Utc>) {
        if let Some(entry) = self.entries.get_mut(node_id) {
            entry.last_seen_at = Some(seen_at);
        } else {
            // We may not have a cached classification yet; store the seen marker
            // anyway so the first classification can use it.
            self.entries.insert(
                node_id.to_string(),
                LlmCacheEntry {
                    updated_at: updated_at.to_string(),
                    llm_priority: None,
                    llm_summary: None,
                    llm_rich_summary: None,
                    last_seen_at: Some(seen_at),
                },
            );
        }
    }

    /// Synchronously persist the cache to disk. Useful for tests and one-off
    /// sync contexts.
    pub fn save_sync(&self) -> Result<()> {
        let path = cache_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create LLM cache dir {}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).context("Failed to serialize LLM cache")?;
        std::fs::write(&path, text)
            .with_context(|| format!("Failed to write LLM cache to {}", path.display()))?;
        Ok(())
    }

    /// Asynchronously persist the cache to disk without blocking the runtime.
    pub async fn flush(&self) -> Result<()> {
        let cache = self.clone();
        task::spawn_blocking(move || cache.save_sync()).await?
    }
}

impl Default for LlmClassificationCache {
    fn default() -> Self {
        Self {
            version: CACHE_VERSION,
            entries: HashMap::new(),
        }
    }
}

fn cache_path() -> Result<std::path::PathBuf> {
    Ok(data_dir()?.join(CACHE_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pr(node_id: &str, updated_at: &str) -> PullRequest {
        PullRequest {
            node_id: node_id.to_string(),
            number: 1,
            title: "Test".to_string(),
            body: String::new(),
            url: String::new(),
            author: "author".to_string(),
            author_is_bot: false,
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            is_draft: false,
            updated_at: updated_at.to_string(),
            head_ref: "feature".to_string(),
            base_ref: "main".to_string(),
            mergeable: crate::github::types::MergeableState::Unknown,
            review_decision: None,
            review_requests: Vec::new(),
            team_review_requests: Vec::new(),
            viewer_latest_review: None,
            latest_reviews: Vec::new(),
            check_status: crate::github::types::CheckStatus::None,
            checks: Vec::new(),
            review_threads: Vec::new(),
            timeline: Vec::new(),
            files: Vec::new(),
            comments: 0,
            llm_priority: None,
            llm_summary: None,
            llm_rich_summary: None,
            last_seen_at: None,
        }
    }

    #[test]
    fn cache_applies_only_when_updated_at_matches() {
        let mut cache = LlmClassificationCache::default();
        let mut pr = make_pr("node1", "2024-01-01T00:00:00Z");
        pr.llm_summary = Some("summary".to_string());
        cache.update_from_pr(&pr);

        let mut stale = make_pr("node1", "2024-02-01T00:00:00Z");
        cache.apply_to_pr(&mut stale);
        assert!(stale.llm_summary.is_none());

        let mut fresh = make_pr("node1", "2024-01-01T00:00:00Z");
        cache.apply_to_pr(&mut fresh);
        assert_eq!(fresh.llm_summary.as_deref(), Some("summary"));
    }

    #[test]
    fn record_seen_without_classification_stores_marker() {
        let mut cache = LlmClassificationCache::default();
        let seen = Utc::now();
        cache.record_seen("node2", "2024-01-01T00:00:00Z", seen);
        assert_eq!(cache.entries["node2"].last_seen_at, Some(seen));

        // The stub entry's updated_at must match the PR so a later poll
        // (which restores last_seen_at via apply_to_pr) doesn't drop it.
        let mut pr = make_pr("node2", "2024-01-01T00:00:00Z");
        cache.apply_to_pr(&mut pr);
        assert_eq!(pr.last_seen_at, Some(seen));
    }

    #[test]
    fn prune_missing_drops_entries_not_in_known_set() {
        let mut cache = LlmClassificationCache::default();
        cache.update_from_pr(&make_pr("keep", "2024-01-01T00:00:00Z"));
        cache.update_from_pr(&make_pr("drop", "2024-01-01T00:00:00Z"));

        let known: std::collections::HashSet<String> = ["keep".to_string()].into_iter().collect();
        let pruned = cache.prune_missing(&known);

        assert!(pruned);
        assert!(cache.contains("keep"));
        assert!(!cache.contains("drop"));

        // Nothing left to prune on a second pass.
        assert!(!cache.prune_missing(&known));
    }

    #[test]
    fn cache_drops_rich_summary_with_stale_prompt_version() {
        let mut cache = LlmClassificationCache::default();
        let mut pr = make_pr("node1", "2024-01-01T00:00:00Z");
        pr.llm_rich_summary = Some(LlmRichSummary {
            one_line: "old".to_string(),
            catch_up: "old catch up".to_string(),
            next_steps: "old steps".to_string(),
            generated_at: Utc::now(),
            prompt_version: RICH_PROMPT_VERSION - 1,
        });
        cache.update_from_pr(&pr);

        let mut restored = make_pr("node1", "2024-01-01T00:00:00Z");
        cache.apply_to_pr(&mut restored);
        assert!(restored.llm_rich_summary.is_none());
    }
}
