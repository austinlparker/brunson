use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::github::types::*;

/// Central in-memory PR store, shared between poller and HTTP handlers.
pub struct PrStore {
    pub current_user: String,
    pub prs: HashMap<String, PullRequest>,
    pub cached_diffs: HashMap<String, String>,
    pub last_poll_at: Option<DateTime<Utc>>,
    pub last_poll_error: Option<String>,
    pub rate_limit_remaining: Option<u32>,
    pub refresh_in_progress: bool,
}

impl PrStore {
    pub fn new(current_user: String) -> Self {
        Self {
            current_user,
            prs: HashMap::new(),
            cached_diffs: HashMap::new(),
            last_poll_at: None,
            last_poll_error: None,
            rate_limit_remaining: None,
            refresh_in_progress: false,
        }
    }

    /// Update or insert PRs from a poll cycle.
    /// Returns the list of PRs that changed (for LLM classification trigger).
    pub fn update_prs(&mut self, new_prs: Vec<PullRequest>) -> Vec<PullRequest> {
        let mut changed = Vec::new();
        let seen_keys: std::collections::HashSet<String> =
            new_prs.iter().map(|p| p.node_id.clone()).collect();

        for mut pr in new_prs {
            // Preserve LLM classification from previous version
            if let Some(existing) = self.prs.get(&pr.node_id) {
                pr.llm_priority = existing.llm_priority;
                pr.llm_summary = existing.llm_summary.clone();

                // Check if updated_at changed
                if existing.updated_at != pr.updated_at {
                    self.cached_diffs.remove(&existing.slug());
                    self.cached_diffs.remove(&pr.slug());
                    // Rich catch-up/next-steps summaries are scoped to a specific
                    // point in time; invalidate them when the PR changes.
                    pr.llm_rich_summary = None;
                    changed.push(pr.clone());
                }
            } else {
                // New PR
                changed.push(pr.clone());
            }
            self.prs.insert(pr.node_id.clone(), pr);
        }

        // Remove PRs that are no longer in the search results, and drop their
        // cached diffs so a future PR with the same slug cannot inherit stale data.
        let removed_slugs: Vec<String> = self
            .prs
            .iter()
            .filter_map(|(key, pr)| {
                if seen_keys.contains(key) {
                    None
                } else {
                    Some(pr.slug())
                }
            })
            .collect();
        self.prs.retain(|key, _| seen_keys.contains(key));
        for slug in removed_slugs {
            self.cached_diffs.remove(&slug);
        }

        changed
    }

    /// Hydrate LLM classifications and `last_seen_at` from the on-disk cache.
    pub fn hydrate_llm_cache(&mut self, cache: &crate::llm::LlmClassificationCache) {
        for pr in self.prs.values_mut() {
            cache.apply_to_pr(pr);
        }
    }

    /// Group all PRs into actionable state groups.
    /// Each PR appears in exactly one group (highest priority match).
    pub fn group_prs(&self) -> HashMap<PrGroup, Vec<PullRequest>> {
        let mut groups: HashMap<PrGroup, Vec<PullRequest>> = HashMap::new();

        for pr in self.prs.values() {
            let group = self.classify_pr(pr);
            groups.entry(group).or_default().push(pr.clone());
        }

        // Sort each group by updated_at descending, breaking ties by slug so
        // PRs with identical timestamps don't reorder between polls due to
        // HashMap iteration order.
        for prs in groups.values_mut() {
            prs.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.slug().cmp(&b.slug())));
        }

        groups
    }

    /// Classify a single PR into its priority group.
    /// Uses two lanes (authored, review-requested) with "ball in court" heuristics
    /// derived from the activity timeline.
    pub fn classify_pr(&self, pr: &PullRequest) -> PrGroup {
        let user = &self.current_user;
        let is_author = pr.author == *user;
        let is_reviewer = pr.review_requests.iter().any(|r| r == user);

        // Drafts are always drafts regardless of lane
        if pr.is_draft {
            return PrGroup::Draft;
        }

        if is_author {
            return self.classify_authored(pr, user);
        }

        if is_reviewer {
            return self.classify_review(pr, user);
        }

        // Involved but neither author nor requested reviewer
        PrGroup::Other
    }

    /// A short, human-readable label for the next action the user should take.
    pub fn next_action(&self, pr: &PullRequest) -> &'static str {
        match self.classify_pr(pr) {
            PrGroup::AuthoredActionNeeded => {
                if pr.check_status == CheckStatus::Failure {
                    "Fix CI"
                } else if pr.review_decision == Some(ReviewDecision::ChangesRequested) {
                    "Address feedback"
                } else {
                    "Respond"
                }
            }
            PrGroup::AuthoredReadyToMerge => "Merge",
            PrGroup::AuthoredWaiting => "Waiting",
            PrGroup::ReviewNeeded => "Review now",
            PrGroup::ReviewUpdate => "Re-review",
            PrGroup::ReviewDone => "Done",
            PrGroup::Draft => "Draft",
            PrGroup::Other => "Watch",
        }
    }

    // ── Authored lane ──

    fn classify_authored(&self, pr: &PullRequest, user: &str) -> PrGroup {
        // CI failed → highest urgency
        if pr.check_status == CheckStatus::Failure {
            return PrGroup::AuthoredActionNeeded;
        }

        // Changes requested → need to address feedback
        if pr.review_decision == Some(ReviewDecision::ChangesRequested) {
            return PrGroup::AuthoredActionNeeded;
        }

        // Ball in my court: someone commented/reviewed and I haven't responded
        if self.ball_in_my_court(pr, user) {
            return PrGroup::AuthoredActionNeeded;
        }

        // Approved (or no review required) + CI green + mergeable → ready
        if (pr.review_decision == Some(ReviewDecision::Approved) || pr.review_decision.is_none())
            && pr.check_status == CheckStatus::Success
            && pr.mergeable == MergeableState::Mergeable
        {
            return PrGroup::AuthoredReadyToMerge;
        }

        PrGroup::AuthoredWaiting
    }

    // ── Review lane ──

    fn classify_review(&self, pr: &PullRequest, user: &str) -> PrGroup {
        match pr.viewer_latest_review.as_deref() {
            None | Some("DISMISSED") => PrGroup::ReviewNeeded,
            Some(_) => {
                if self.has_new_activity_since_review(pr, user) {
                    PrGroup::ReviewUpdate
                } else {
                    PrGroup::ReviewDone
                }
            }
        }
    }

    // ── Timeline helpers ──

    /// Determine if the "ball" is in the user's court on an authored PR.
    /// True when the most recent interaction (comment or review) was performed
    /// by someone other than the current user.
    fn ball_in_my_court(&self, pr: &PullRequest, user: &str) -> bool {
        let last_interaction = pr
            .timeline
            .iter()
            .filter(|e| {
                matches!(
                    e.event_type,
                    TimelineEventType::Comment | TimelineEventType::Review
                )
            })
            .max_by(|a, b| a.created_at.cmp(&b.created_at));

        match last_interaction {
            Some(e) => e.actor != user,
            None => {
                // No timeline interactions — fall back to checking unresolved review threads
                pr.review_threads.iter().any(|t| {
                    !t.is_resolved && t.comments.last().map(|c| c.author != user).unwrap_or(false)
                })
            }
        }
    }

    /// Check if there are new commits or force-pushes since the user's last review.
    fn has_new_activity_since_review(&self, pr: &PullRequest, user: &str) -> bool {
        // Find the user's most recent review timestamp
        let my_last_review = pr
            .timeline
            .iter()
            .filter(|e| e.actor == user && matches!(e.event_type, TimelineEventType::Review))
            .max_by(|a, b| a.created_at.cmp(&b.created_at));

        match my_last_review {
            Some(review) => {
                // Check for commits or force-pushes after the review
                pr.timeline.iter().any(|e| {
                    matches!(
                        e.event_type,
                        TimelineEventType::Commit | TimelineEventType::ForcePush
                    ) && e.created_at > review.created_at
                })
            }
            None => false, // No review yet — handled by ReviewNeeded
        }
    }

    /// Get a PR by its slug (owner~repo~number).
    pub fn get_by_slug(&self, slug: &str) -> Option<&PullRequest> {
        let (owner, repo, number) = parse_slug(slug)?;
        self.prs
            .values()
            .find(|pr| pr.owner == owner && pr.repo == repo && pr.number == number)
    }

    /// Get a mutable PR by its slug.
    #[allow(dead_code)]
    pub fn get_by_slug_mut(&mut self, slug: &str) -> Option<&mut PullRequest> {
        let (owner, repo, number) = parse_slug(slug)?;
        self.prs
            .values_mut()
            .find(|pr| pr.owner == owner && pr.repo == repo && pr.number == number)
    }

    /// Get a cached diff.
    pub fn get_diff(&self, slug: &str) -> Option<&String> {
        self.cached_diffs.get(slug)
    }

    /// Cache a diff.
    pub fn set_diff(&mut self, slug: String, diff: String) {
        self.cached_diffs.insert(slug, diff);
    }

    /// Approximate top-level comment count: review threads + issue comments.
    pub fn comment_count(&self, pr: &PullRequest) -> u32 {
        let threads = pr.review_threads.len() as u32;
        let issue_comments = pr
            .timeline
            .iter()
            .filter(|e| e.event_type == TimelineEventType::Comment)
            .count() as u32;
        threads + issue_comments
    }
}

/// Convenience type alias.
pub type SharedStore = Arc<RwLock<PrStore>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn make_pr(
        node_id: &str,
        number: u64,
        author: &str,
        is_draft: bool,
        check_status: CheckStatus,
        review_requests: Vec<String>,
        viewer_latest_review: Option<&str>,
        review_decision: Option<ReviewDecision>,
        mergeable: MergeableState,
        updated_at: &str,
    ) -> PullRequest {
        PullRequest {
            node_id: node_id.into(),
            number,
            title: format!("PR {}", number),
            body: String::new(),
            url: String::new(),
            author: author.into(),
            author_is_bot: false,
            owner: "org".into(),
            repo: "repo".into(),
            is_draft,
            updated_at: updated_at.into(),
            head_ref: "feature".into(),
            base_ref: "main".into(),
            mergeable,
            review_decision,
            review_requests,
            team_review_requests: vec![],
            viewer_latest_review: viewer_latest_review.map(String::from),
            latest_reviews: vec![],
            check_status,
            checks: vec![],
            review_threads: vec![],
            timeline: vec![],
            files: vec![],
            comments: 0,
            llm_priority: None,
            llm_summary: None,
            llm_rich_summary: None,
            last_seen_at: None,
        }
    }

    fn recent_time() -> String {
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }

    #[test]
    fn test_draft_precedence() {
        let store = PrStore::new("me".into());
        let pr = make_pr(
            "1",
            1,
            "me",
            true,
            CheckStatus::Failure,
            vec!["me".into()],
            None,
            Some(ReviewDecision::ReviewRequired),
            MergeableState::Conflicting,
            &recent_time(),
        );
        assert_eq!(store.classify_pr(&pr), PrGroup::Draft);
    }

    // ── Authored lane ──

    #[test]
    fn test_authored_ci_failed() {
        let store = PrStore::new("me".into());
        let pr = make_pr(
            "1",
            1,
            "me",
            false,
            CheckStatus::Failure,
            vec![],
            None,
            Some(ReviewDecision::Approved),
            MergeableState::Mergeable,
            &recent_time(),
        );
        assert_eq!(store.classify_pr(&pr), PrGroup::AuthoredActionNeeded);
        assert_eq!(store.next_action(&pr), "Fix CI");
    }

    #[test]
    fn test_authored_changes_requested() {
        let store = PrStore::new("me".into());
        let pr = make_pr(
            "1",
            1,
            "me",
            false,
            CheckStatus::Success,
            vec![],
            None,
            Some(ReviewDecision::ChangesRequested),
            MergeableState::Mergeable,
            &recent_time(),
        );
        assert_eq!(store.classify_pr(&pr), PrGroup::AuthoredActionNeeded);
        assert_eq!(store.next_action(&pr), "Address feedback");
    }

    #[test]
    fn test_authored_ball_in_court_comment_from_other() {
        let store = PrStore::new("me".into());
        let mut pr = make_pr(
            "1",
            1,
            "me",
            false,
            CheckStatus::Success,
            vec![],
            None,
            Some(ReviewDecision::ReviewRequired),
            MergeableState::Mergeable,
            &recent_time(),
        );
        // Someone commented after the PR was created
        pr.timeline = vec![
            TimelineEvent {
                event_type: TimelineEventType::Commit,
                actor: "me".into(),
                created_at: "2024-06-01T10:00:00Z".into(),
                detail: "Initial commit".into(),
            },
            TimelineEvent {
                event_type: TimelineEventType::Comment,
                actor: "bob".into(),
                created_at: "2024-06-01T11:00:00Z".into(),
                detail: "Can you fix this?".into(),
            },
        ];
        assert_eq!(store.classify_pr(&pr), PrGroup::AuthoredActionNeeded);
        assert_eq!(store.next_action(&pr), "Respond");
    }

    #[test]
    fn test_authored_ball_not_in_court_last_was_mine() {
        let store = PrStore::new("me".into());
        let mut pr = make_pr(
            "1",
            1,
            "me",
            false,
            CheckStatus::Success,
            vec![],
            None,
            Some(ReviewDecision::ReviewRequired),
            MergeableState::Mergeable,
            &recent_time(),
        );
        // I responded after someone commented — ball is in their court
        pr.timeline = vec![
            TimelineEvent {
                event_type: TimelineEventType::Comment,
                actor: "bob".into(),
                created_at: "2024-06-01T10:00:00Z".into(),
                detail: "Question?".into(),
            },
            TimelineEvent {
                event_type: TimelineEventType::Comment,
                actor: "me".into(),
                created_at: "2024-06-01T11:00:00Z".into(),
                detail: "Answered".into(),
            },
        ];
        // Review still required, CI green, but not mergeable → Waiting
        assert_eq!(store.classify_pr(&pr), PrGroup::AuthoredWaiting);
    }

    #[test]
    fn test_authored_ready_to_merge() {
        let store = PrStore::new("me".into());
        let pr = make_pr(
            "1",
            1,
            "me",
            false,
            CheckStatus::Success,
            vec![],
            None,
            Some(ReviewDecision::Approved),
            MergeableState::Mergeable,
            &recent_time(),
        );
        assert_eq!(store.classify_pr(&pr), PrGroup::AuthoredReadyToMerge);
        assert_eq!(store.next_action(&pr), "Merge");
    }

    #[test]
    fn test_authored_waiting() {
        let store = PrStore::new("me".into());
        let pr = make_pr(
            "1",
            1,
            "me",
            false,
            CheckStatus::None,
            vec![],
            None,
            Some(ReviewDecision::ReviewRequired),
            MergeableState::Unknown,
            &recent_time(),
        );
        assert_eq!(store.classify_pr(&pr), PrGroup::AuthoredWaiting);
    }

    // ── Review lane ──

    #[test]
    fn test_review_needed() {
        let store = PrStore::new("me".into());
        let pr = make_pr(
            "1",
            1,
            "other",
            false,
            CheckStatus::None,
            vec!["me".into()],
            None,
            None,
            MergeableState::Unknown,
            &recent_time(),
        );
        assert_eq!(store.classify_pr(&pr), PrGroup::ReviewNeeded);
        assert_eq!(store.next_action(&pr), "Review now");
    }

    #[test]
    fn test_review_needed_after_dismissed() {
        let store = PrStore::new("me".into());
        let pr = make_pr(
            "1",
            1,
            "other",
            false,
            CheckStatus::None,
            vec!["me".into()],
            Some("DISMISSED"),
            None,
            MergeableState::Unknown,
            &recent_time(),
        );
        assert_eq!(store.classify_pr(&pr), PrGroup::ReviewNeeded);
    }

    #[test]
    fn test_review_done_no_new_commits() {
        let store = PrStore::new("me".into());
        let mut pr = make_pr(
            "1",
            1,
            "other",
            false,
            CheckStatus::Success,
            vec!["me".into()],
            Some("APPROVED"),
            Some(ReviewDecision::Approved),
            MergeableState::Mergeable,
            &recent_time(),
        );
        pr.timeline = vec![
            TimelineEvent {
                event_type: TimelineEventType::Commit,
                actor: "other".into(),
                created_at: "2024-06-01T09:00:00Z".into(),
                detail: "Initial work".into(),
            },
            TimelineEvent {
                event_type: TimelineEventType::Review,
                actor: "me".into(),
                created_at: "2024-06-01T10:00:00Z".into(),
                detail: "APPROVED: Looks good".into(),
            },
        ];
        assert_eq!(store.classify_pr(&pr), PrGroup::ReviewDone);
    }

    #[test]
    fn test_review_update_new_commits() {
        let store = PrStore::new("me".into());
        let mut pr = make_pr(
            "1",
            1,
            "other",
            false,
            CheckStatus::Pending,
            vec!["me".into()],
            Some("APPROVED"),
            None,
            MergeableState::Unknown,
            &recent_time(),
        );
        // I reviewed, then author pushed new commits
        pr.timeline = vec![
            TimelineEvent {
                event_type: TimelineEventType::Review,
                actor: "me".into(),
                created_at: "2024-06-01T10:00:00Z".into(),
                detail: "APPROVED: Looks good".into(),
            },
            TimelineEvent {
                event_type: TimelineEventType::Commit,
                actor: "other".into(),
                created_at: "2024-06-01T12:00:00Z".into(),
                detail: "Address feedback".into(),
            },
        ];
        assert_eq!(store.classify_pr(&pr), PrGroup::ReviewUpdate);
        assert_eq!(store.next_action(&pr), "Re-review");
    }

    // ── Other ──

    #[test]
    fn test_other_involved_not_author_or_reviewer() {
        let store = PrStore::new("me".into());
        let pr = make_pr(
            "1",
            1,
            "other",
            false,
            CheckStatus::None,
            vec![], // not a reviewer
            None,
            None,
            MergeableState::Unknown,
            &recent_time(),
        );
        assert_eq!(store.classify_pr(&pr), PrGroup::Other);
    }

    // ── Ball-in-court fallback (no timeline) ──

    #[test]
    fn test_ball_in_court_fallback_review_threads() {
        let store = PrStore::new("me".into());
        let mut pr = make_pr(
            "1",
            1,
            "me",
            false,
            CheckStatus::Success,
            vec![],
            None,
            Some(ReviewDecision::ReviewRequired),
            MergeableState::Unknown,
            &recent_time(),
        );
        // No timeline, but unresolved thread with last comment from someone else
        pr.review_threads = vec![ReviewThread {
            is_resolved: false,
            is_outdated: false,
            comments: vec![ReviewComment {
                author: "bob".into(),
                body: "Please fix".into(),
                path: "src/main.rs".into(),
                line: Some(42),
            }],
        }];
        assert_eq!(store.classify_pr(&pr), PrGroup::AuthoredActionNeeded);
    }

    #[test]
    fn test_each_pr_appears_once() {
        let mut store = PrStore::new("me".into());
        let prs = vec![
            make_pr(
                "1",
                1,
                "other",
                false,
                CheckStatus::None,
                vec!["me".into()],
                None,
                None,
                MergeableState::Unknown,
                &recent_time(),
            ),
            make_pr(
                "2",
                2,
                "me",
                false,
                CheckStatus::Success,
                vec![],
                None,
                Some(ReviewDecision::Approved),
                MergeableState::Mergeable,
                &recent_time(),
            ),
            make_pr(
                "3",
                3,
                "other",
                true,
                CheckStatus::None,
                vec!["me".into()],
                None,
                None,
                MergeableState::Unknown,
                &recent_time(),
            ),
        ];
        store.update_prs(prs);
        let groups = store.group_prs();

        let total: usize = groups.values().map(|v| v.len()).sum();
        assert_eq!(total, 3);

        // PR 3 should be in Draft
        let draft = groups.get(&PrGroup::Draft).unwrap();
        assert_eq!(draft.len(), 1);
        assert_eq!(draft[0].number, 3);

        // PR 1 should be in ReviewNeeded
        let needs_review = groups.get(&PrGroup::ReviewNeeded).unwrap();
        assert_eq!(needs_review.len(), 1);
        assert_eq!(needs_review[0].number, 1);

        // PR 2 should be in AuthoredReadyToMerge
        let ready = groups.get(&PrGroup::AuthoredReadyToMerge).unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].number, 2);
    }

    #[test]
    fn test_group_prs_ties_broken_by_slug() {
        // Two drafts with identical `updated_at` must always sort in slug
        // order, regardless of insertion order (HashMap iteration order is
        // otherwise nondeterministic).
        let same_time = "2024-01-01T00:00:00Z";
        let pr_a = make_pr(
            "a",
            1,
            "other",
            true,
            CheckStatus::None,
            vec![],
            None,
            None,
            MergeableState::Unknown,
            same_time,
        );
        let pr_b = make_pr(
            "b",
            2,
            "other",
            true,
            CheckStatus::None,
            vec![],
            None,
            None,
            MergeableState::Unknown,
            same_time,
        );

        let mut store_forward = PrStore::new("me".into());
        store_forward.update_prs(vec![pr_a.clone(), pr_b.clone()]);
        let groups_forward = store_forward.group_prs();
        let draft_forward = groups_forward.get(&PrGroup::Draft).unwrap();
        assert_eq!(
            draft_forward.iter().map(|p| p.slug()).collect::<Vec<_>>(),
            vec![pr_a.slug(), pr_b.slug()]
        );

        let mut store_reverse = PrStore::new("me".into());
        store_reverse.update_prs(vec![pr_b.clone(), pr_a.clone()]);
        let groups_reverse = store_reverse.group_prs();
        let draft_reverse = groups_reverse.get(&PrGroup::Draft).unwrap();
        assert_eq!(
            draft_reverse.iter().map(|p| p.slug()).collect::<Vec<_>>(),
            vec![pr_a.slug(), pr_b.slug()]
        );
    }

    #[test]
    fn test_get_by_slug() {
        let mut store = PrStore::new("me".into());
        let pr = make_pr(
            "1",
            42,
            "other",
            false,
            CheckStatus::None,
            vec![],
            None,
            None,
            MergeableState::Unknown,
            &recent_time(),
        );
        store.update_prs(vec![pr]);

        let found = store.get_by_slug("org~repo~42");
        assert!(found.is_some());
        assert_eq!(found.unwrap().number, 42);

        assert!(store.get_by_slug("org~repo~99").is_none());
    }

    #[test]
    fn test_changed_pr_invalidates_cached_diff() {
        let mut store = PrStore::new("me".into());
        let pr = make_pr(
            "1",
            42,
            "other",
            false,
            CheckStatus::None,
            vec![],
            None,
            None,
            MergeableState::Unknown,
            "2024-01-01T00:00:00Z",
        );
        store.update_prs(vec![pr]);
        store.set_diff("org~repo~42".to_string(), "old diff".to_string());

        let updated = make_pr(
            "1",
            42,
            "other",
            false,
            CheckStatus::None,
            vec![],
            None,
            None,
            MergeableState::Unknown,
            "2024-01-02T00:00:00Z",
        );
        store.update_prs(vec![updated]);

        assert!(store.get_diff("org~repo~42").is_none());
    }

    #[test]
    fn test_removed_pr_invalidates_cached_diff() {
        let mut store = PrStore::new("me".into());
        let pr = make_pr(
            "1",
            42,
            "other",
            false,
            CheckStatus::None,
            vec![],
            None,
            None,
            MergeableState::Unknown,
            "2024-01-01T00:00:00Z",
        );
        store.update_prs(vec![pr]);
        store.set_diff("org~repo~42".to_string(), "old diff".to_string());

        store.update_prs(vec![]);

        assert!(store.get_diff("org~repo~42").is_none());
    }

    #[test]
    fn test_unchanged_pr_preserves_cached_diff() {
        let mut store = PrStore::new("me".into());
        let pr = make_pr(
            "1",
            42,
            "other",
            false,
            CheckStatus::None,
            vec![],
            None,
            None,
            MergeableState::Unknown,
            "2024-01-01T00:00:00Z",
        );
        store.update_prs(vec![pr.clone()]);
        store.set_diff("org~repo~42".to_string(), "old diff".to_string());

        store.update_prs(vec![pr]);

        assert_eq!(
            store.get_diff("org~repo~42").map(String::as_str),
            Some("old diff")
        );
    }

    #[test]
    fn test_comment_count_counts_threads_and_issue_comments() {
        let mut store = PrStore::new("me".into());
        let mut pr = make_pr(
            "1",
            1,
            "other",
            false,
            CheckStatus::None,
            vec!["me".into()],
            None,
            None,
            MergeableState::Unknown,
            &recent_time(),
        );
        pr.review_threads = vec![ReviewThread {
            is_resolved: false,
            is_outdated: false,
            comments: vec![ReviewComment {
                author: "bob".into(),
                body: "thread".into(),
                path: "src/main.rs".into(),
                line: Some(10),
            }],
        }];
        pr.timeline = vec![
            TimelineEvent {
                event_type: TimelineEventType::Comment,
                actor: " alice ".into(),
                created_at: recent_time(),
                detail: "issue comment".into(),
            },
            TimelineEvent {
                event_type: TimelineEventType::Commit,
                actor: "bob".into(),
                created_at: recent_time(),
                detail: "commit".into(),
            },
        ];
        store.update_prs(vec![pr]);
        let pr = store.prs.values().next().unwrap();
        assert_eq!(store.comment_count(pr), 2);
    }
}
