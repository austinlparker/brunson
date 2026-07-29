use std::collections::{HashMap, HashSet};

use crate::config::{GithubConfig, GithubTarget};

use super::types::{PullRequest, SearchResult};

/// Scope attached to a generated search query.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SearchScope {
    Org(String),
    Repo { owner: String, repo: String },
}

impl SearchScope {
    fn qualifier(&self) -> String {
        match self {
            Self::Org(org) => format!("org:{org}"),
            Self::Repo { owner, repo } => format!("repo:{owner}/{repo}"),
        }
    }

    fn matches_result(&self, result: &SearchResult) -> bool {
        match self {
            Self::Org(org) => org.eq_ignore_ascii_case(&result.repo_owner),
            Self::Repo { owner, repo } => {
                owner.eq_ignore_ascii_case(&result.repo_owner)
                    && repo.eq_ignore_ascii_case(&result.repo_name)
            }
        }
    }

    fn matches_pr(&self, pr: &PullRequest) -> bool {
        match self {
            Self::Org(org) => org.eq_ignore_ascii_case(&pr.owner),
            Self::Repo { owner, repo } => {
                owner.eq_ignore_ascii_case(&pr.owner) && repo.eq_ignore_ascii_case(&pr.repo)
            }
        }
    }
}

/// Why a concrete query matched a PR.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SearchReason {
    WatchReviewRequested { scope: Option<SearchScope> },
    WatchAuthor { scope: Option<SearchScope> },
    WatchInvolves { scope: Option<SearchScope> },
    TargetDirectReview { scope: SearchScope },
    TargetTeamReview { scope: SearchScope, team: String },
    TargetAuthor { scope: SearchScope },
    TargetInvolves { scope: SearchScope },
}

/// Rendered GitHub search query plus the reason it represents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub query: String,
    pub reason: SearchReason,
}

/// Search result with all query reasons that matched it.
#[derive(Debug, Clone)]
pub struct ProvenancedSearchResult {
    pub result: SearchResult,
    pub reasons: Vec<SearchReason>,
}

impl ProvenancedSearchResult {
    pub fn new(result: SearchResult, reason: SearchReason) -> Self {
        Self {
            result,
            reasons: vec![reason],
        }
    }
}

/// Normalize GitHub team identifiers to `org/team-slug` for stable comparison.
pub fn normalize_team_identifier(team: &str) -> Option<String> {
    let (org, slug) = team.split_once('/')?;
    let org = org.trim();
    let slug = slug.trim();
    if org.is_empty() || slug.is_empty() || slug.contains('/') {
        return None;
    }
    Some(format!(
        "{}/{}",
        org.to_ascii_lowercase(),
        slug.to_ascii_lowercase()
    ))
}

pub fn configured_team_review_requests(config: &GithubConfig) -> HashSet<String> {
    config
        .targets
        .iter()
        .flat_map(|target| target.team_review_requests.iter())
        .filter_map(|team| normalize_team_identifier(team))
        .collect()
}

/// Search type: which relationship the user has with the PR.
#[derive(Debug, Clone, Copy)]
pub enum SearchType {
    ReviewRequested,
    Author,
    Involves,
}

impl SearchType {
    fn qualifier(&self) -> &'static str {
        match self {
            Self::ReviewRequested => "review-requested:@me",
            Self::Author => "author:@me",
            Self::Involves => "involves:@me",
        }
    }
}

/// Build all search queries for a poll cycle with provenance.
pub fn build_query_specs(watch: &[String]) -> Vec<SearchQuery> {
    let search_types = [
        SearchType::ReviewRequested,
        SearchType::Author,
        SearchType::Involves,
    ];

    let mut queries = Vec::new();

    if watch.is_empty() {
        for st in &search_types {
            queries.push(SearchQuery {
                query: format!("{} is:pr is:open", st.qualifier()),
                reason: watch_reason(*st, None),
            });
        }
    } else {
        for entry in watch {
            let Some(scope) = scope_from_entry(entry) else {
                continue;
            };
            let qualifier = scope.qualifier();
            for st in &search_types {
                queries.push(SearchQuery {
                    query: format!("{} is:pr is:open {}", st.qualifier(), qualifier),
                    reason: watch_reason(*st, Some(scope.clone())),
                });
            }
        }
    }

    queries
}

/// Build all search queries for a poll cycle.
/// Returns a list of query strings.
///
/// If `watch` is empty, one query per search type (3 total).
/// If `watch` has entries, 3 × len(watch) queries.
pub fn build_queries(watch: &[String]) -> Vec<String> {
    build_query_specs(watch)
        .into_iter()
        .map(|spec| spec.query)
        .collect()
}

fn watch_reason(search_type: SearchType, scope: Option<SearchScope>) -> SearchReason {
    match search_type {
        SearchType::ReviewRequested => SearchReason::WatchReviewRequested { scope },
        SearchType::Author => SearchReason::WatchAuthor { scope },
        SearchType::Involves => SearchReason::WatchInvolves { scope },
    }
}

/// Build search queries from the full GitHub config.
///
/// `watch` remains the broad shorthand:
/// - `review-requested:@me`, which includes the viewer's teams
/// - `author:@me`
/// - `involves:@me`
///
/// `targets` are the precise form. They use `user-review-requested:@me`
/// for direct user requests plus any explicit `team-review-requested:org/team`
/// entries configured on that target.
pub fn build_queries_for_config(config: &GithubConfig) -> Vec<String> {
    build_query_specs_for_config(config)
        .into_iter()
        .map(|spec| spec.query)
        .collect()
}

/// Build search queries from the full GitHub config with provenance.
pub fn build_query_specs_for_config(config: &GithubConfig) -> Vec<SearchQuery> {
    let mut queries = if config.watch.is_empty() && !config.targets.is_empty() {
        Vec::new()
    } else {
        build_query_specs(&config.watch)
    };

    for target in &config.targets {
        queries.extend(build_target_query_specs(target));
    }

    queries
}

fn build_target_query_specs(target: &GithubTarget) -> Vec<SearchQuery> {
    let Some(scope) = target_scope(target) else {
        return Vec::new();
    };
    let qualifier = scope.qualifier();

    let mut queries = Vec::new();
    if target.direct_review_requests {
        queries.push(SearchQuery {
            query: format!("user-review-requested:@me is:pr is:open {qualifier}"),
            reason: SearchReason::TargetDirectReview {
                scope: scope.clone(),
            },
        });
    }
    for team in &target.team_review_requests {
        if let Some(normalized_team) = normalize_team_identifier(team) {
            queries.push(SearchQuery {
                query: format!("team-review-requested:{team} is:pr is:open {qualifier}"),
                reason: SearchReason::TargetTeamReview {
                    scope: scope.clone(),
                    team: normalized_team,
                },
            });
        }
    }
    if target.include_authored {
        queries.push(SearchQuery {
            query: format!("author:@me is:pr is:open {qualifier}"),
            reason: SearchReason::TargetAuthor {
                scope: scope.clone(),
            },
        });
    }
    if target.include_involved {
        queries.push(SearchQuery {
            query: format!("involves:@me is:pr is:open {qualifier}"),
            reason: SearchReason::TargetInvolves { scope },
        });
    }
    queries
}

fn target_scope(target: &GithubTarget) -> Option<SearchScope> {
    if let Some(repo) = &target.repo {
        scope_from_entry(repo)
    } else {
        target.org.as_ref().map(|org| SearchScope::Org(org.clone()))
    }
}

fn scope_from_entry(entry: &str) -> Option<SearchScope> {
    if let Some((owner, repo)) = entry.split_once('/') {
        if owner.is_empty() || repo.is_empty() || repo.contains('/') {
            None
        } else {
            Some(SearchScope::Repo {
                owner: owner.to_string(),
                repo: repo.to_string(),
            })
        }
    } else if entry.is_empty() {
        None
    } else {
        Some(SearchScope::Org(entry.to_string()))
    }
}

/// De-duplicate search results by (owner, repo, number).
pub fn dedup_results(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for r in results {
        let key = search_result_key(&r);
        if seen.insert(key) {
            deduped.push(r);
        }
    }
    deduped
}

/// Aggregate per-query search results into a single deduplicated PR count
/// plus any per-query errors. A naive sum of per-query counts would
/// double-count PRs matched by more than one overlapping query (e.g. both
/// `author:@me` and a team review request on the same PR), so results are
/// deduped by `(owner, repo, number)` via `dedup_results` before counting.
pub fn aggregate_preview_counts(
    results: Vec<Result<Vec<SearchResult>, String>>,
) -> (usize, Vec<String>) {
    let mut matched = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(items) => matched.extend(items),
            Err(e) => errors.push(e),
        }
    }
    (dedup_results(matched).len(), errors)
}

/// De-duplicate search results and merge all query reasons for each PR.
pub fn dedup_provenanced_results(
    results: Vec<ProvenancedSearchResult>,
) -> Vec<ProvenancedSearchResult> {
    let mut by_key: HashMap<(String, String, u64), ProvenancedSearchResult> = HashMap::new();
    for mut item in results {
        let key = search_result_key(&item.result);
        match by_key.get_mut(&key) {
            Some(existing) => {
                for reason in item.reasons.drain(..) {
                    if !existing.reasons.contains(&reason) {
                        existing.reasons.push(reason);
                    }
                }
            }
            None => {
                by_key.insert(key, item);
            }
        }
    }

    by_key.into_values().collect()
}

fn search_result_key(result: &SearchResult) -> (String, String, u64) {
    (
        result.repo_owner.to_ascii_lowercase(),
        result.repo_name.to_ascii_lowercase(),
        result.number,
    )
}

/// Enforce repository/org scope locally after GitHub search returns.
///
/// GitHub search qualifiers should already enforce this, but the store's scope
/// should be Brunson's invariant rather than an assumption about an upstream
/// response.
pub fn filter_results_for_config(
    results: Vec<SearchResult>,
    config: &GithubConfig,
) -> Vec<SearchResult> {
    if config.watch.is_empty() && config.targets.is_empty() {
        return results;
    }

    results
        .into_iter()
        .filter(|result| result_matches_config(result, config))
        .collect()
}

fn result_matches_config(result: &SearchResult, config: &GithubConfig) -> bool {
    config
        .watch
        .iter()
        .any(|entry| result_matches_watch_entry(result, entry))
        || config
            .targets
            .iter()
            .any(|target| result_matches_target(result, target))
}

fn result_matches_watch_entry(result: &SearchResult, entry: &str) -> bool {
    scope_from_entry(entry)
        .as_ref()
        .map(|scope| scope.matches_result(result))
        .unwrap_or(false)
}

fn result_matches_target(result: &SearchResult, target: &GithubTarget) -> bool {
    target_scope(target)
        .as_ref()
        .map(|scope| scope.matches_result(result))
        .unwrap_or(false)
}

/// Keep hydrated PRs only when at least one concrete matched query reason is still valid.
pub fn filter_prs_by_provenance(
    prs: Vec<PullRequest>,
    provenance: &[ProvenancedSearchResult],
    config: &GithubConfig,
    current_user: &str,
    current_team_memberships: &HashSet<String>,
) -> Vec<PullRequest> {
    let provenance_by_key: HashMap<(String, String, u64), &[SearchReason]> = provenance
        .iter()
        .map(|item| (search_result_key(&item.result), item.reasons.as_slice()))
        .collect();

    prs.into_iter()
        .filter(|pr| {
            let key = (
                pr.owner.to_ascii_lowercase(),
                pr.repo.to_ascii_lowercase(),
                pr.number,
            );
            let Some(reasons) = provenance_by_key.get(&key) else {
                return false;
            };
            reasons.iter().any(|reason| {
                reason_keeps_pr(reason, pr, config, current_user, current_team_memberships)
            })
        })
        .collect()
}

fn reason_keeps_pr(
    reason: &SearchReason,
    pr: &PullRequest,
    config: &GithubConfig,
    current_user: &str,
    current_team_memberships: &HashSet<String>,
) -> bool {
    match reason {
        SearchReason::WatchReviewRequested { scope }
        | SearchReason::WatchAuthor { scope }
        | SearchReason::WatchInvolves { scope } => {
            watch_reason_is_configured(config, scope.as_ref())
                && scope.as_ref().is_none_or(|scope| scope.matches_pr(pr))
        }
        SearchReason::TargetDirectReview { scope } => {
            target_allows(config, scope, |target| target.direct_review_requests)
                && scope.matches_pr(pr)
                && pr
                    .review_requests
                    .iter()
                    .any(|r| r.eq_ignore_ascii_case(current_user))
        }
        SearchReason::TargetTeamReview { scope, team } => {
            target_allows(config, scope, |target| {
                target
                    .team_review_requests
                    .iter()
                    .filter_map(|configured| normalize_team_identifier(configured))
                    .any(|configured| configured == *team)
            }) && scope.matches_pr(pr)
                && pr
                    .team_review_requests
                    .iter()
                    .filter_map(|requested| normalize_team_identifier(requested))
                    .any(|requested| requested == *team)
                && current_team_memberships.contains(team)
        }
        SearchReason::TargetAuthor { scope } => {
            target_allows(config, scope, |target| target.include_authored)
                && scope.matches_pr(pr)
                && pr.author.eq_ignore_ascii_case(current_user)
        }
        SearchReason::TargetInvolves { scope } => {
            target_allows(config, scope, |target| target.include_involved) && scope.matches_pr(pr)
        }
    }
}

fn watch_reason_is_configured(config: &GithubConfig, scope: Option<&SearchScope>) -> bool {
    match scope {
        None => config.watch.is_empty(),
        Some(scope) => config.watch.iter().any(|entry| {
            scope_from_entry(entry)
                .as_ref()
                .map(|configured| configured == scope)
                .unwrap_or(false)
        }),
    }
}

fn target_allows(
    config: &GithubConfig,
    scope: &SearchScope,
    allows: impl Fn(&GithubTarget) -> bool,
) -> bool {
    config
        .targets
        .iter()
        .any(|target| target_scope(target).as_ref() == Some(scope) && allows(target))
}

/// Extract owner and repo from a watch entry.
/// Returns (org_qualifier, repo_qualifier) tuple — only one is populated.
#[allow(dead_code)]
pub fn parse_watch_entry(entry: &str) -> (Option<String>, Option<String>) {
    if entry.contains('/') {
        (None, Some(format!("repo:{}", entry)))
    } else {
        (Some(format!("org:{}", entry)), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_queries_empty_watch() {
        let queries = build_queries(&[]);
        assert_eq!(queries.len(), 3);
        assert!(queries[0].contains("review-requested:@me"));
        assert!(queries[1].contains("author:@me"));
        assert!(queries[2].contains("involves:@me"));
        for q in &queries {
            assert!(q.contains("is:pr is:open"));
        }
    }

    #[test]
    fn test_build_queries_with_watch() {
        let watch = vec!["myorg".to_string(), "myorg/repo-a".to_string()];
        let queries = build_queries(&watch);
        // 3 search types × 2 watch entries = 6 queries
        assert_eq!(queries.len(), 6);

        // Should have org:myorg queries
        assert!(queries.iter().any(|q| q.contains("org:myorg")));
        // Should have repo:myorg/repo-a queries
        assert!(queries.iter().any(|q| q.contains("repo:myorg/repo-a")));

        for q in &queries {
            assert!(q.contains("is:pr is:open"));
        }
    }

    #[test]
    fn test_build_queries_org_vs_repo_qualifier() {
        let watch = vec!["myorg".to_string()];
        let queries = build_queries(&watch);
        assert_eq!(queries.len(), 3);
        for q in &queries {
            assert!(q.contains("org:myorg"));
            assert!(!q.contains("repo:"));
        }

        let watch = vec!["myorg/repo-b".to_string()];
        let queries = build_queries(&watch);
        assert_eq!(queries.len(), 3);
        for q in &queries {
            assert!(q.contains("repo:myorg/repo-b"));
            assert!(!q.contains("org:"));
        }
    }

    #[test]
    fn test_build_queries_for_config_uses_precise_target_review_requests() {
        let mut config = crate::config::GithubConfig::default();
        config.targets.push(crate::config::GithubTarget {
            repo: Some("myorg/repo-a".to_string()),
            team_review_requests: vec!["myorg/agentic-engineering".to_string()],
            include_authored: false,
            ..Default::default()
        });

        let queries = build_queries_for_config(&config);
        assert_eq!(queries.len(), 2);
        assert!(queries
            .iter()
            .any(|q| q == "user-review-requested:@me is:pr is:open repo:myorg/repo-a"));
        assert!(queries.iter().any(|q| q
            == "team-review-requested:myorg/agentic-engineering is:pr is:open repo:myorg/repo-a"));
        assert!(!queries
            .iter()
            .any(|q| q.starts_with("review-requested:@me ")));
        assert!(!queries.iter().any(|q| q.contains("involves:@me")));
        assert!(!queries.iter().any(|q| q.contains("org:myorg")));
    }

    #[test]
    fn test_build_queries_for_config_mixes_watch_and_targets_as_union() {
        let mut config = crate::config::GithubConfig {
            watch: vec!["myorg/repo-a".to_string()],
            ..Default::default()
        };
        config.targets.push(crate::config::GithubTarget {
            repo: Some("myorg/repo-b".to_string()),
            direct_review_requests: false,
            team_review_requests: vec!["myorg/platform".to_string()],
            include_authored: false,
            include_involved: false,
            ..Default::default()
        });

        let queries = build_queries_for_config(&config);
        assert_eq!(queries.len(), 4);
        assert!(queries.iter().any(|q| q.contains("repo:myorg/repo-a")));
        assert!(queries
            .iter()
            .any(|q| q == "team-review-requested:myorg/platform is:pr is:open repo:myorg/repo-b"));
    }

    #[test]
    fn test_filter_results_for_config_enforces_repo_scope() {
        let mut config = crate::config::GithubConfig::default();
        config.targets.push(crate::config::GithubTarget {
            repo: Some("myorg/repo-a".to_string()),
            ..Default::default()
        });

        let results = filter_results_for_config(
            vec![
                search_result("myorg", "repo-a", 1),
                search_result("myorg", "repo-b", 2),
                search_result("other", "repo-a", 3),
            ],
            &config,
        );

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].repo_name, "repo-a");
    }

    #[test]
    fn test_filter_results_for_config_keeps_org_scope() {
        let config = crate::config::GithubConfig {
            watch: vec!["myorg".to_string()],
            ..Default::default()
        };

        let results = filter_results_for_config(
            vec![
                search_result("myorg", "repo-a", 1),
                search_result("myorg", "repo-b", 2),
                search_result("other", "repo-a", 3),
            ],
            &config,
        );

        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.repo_owner == "myorg"));
    }

    #[test]
    fn test_dedup_results() {
        let results = vec![
            SearchResult {
                repo_owner: "org".into(),
                repo_name: "repo".into(),
                number: 1,
                title: "A".into(),
                author: "user".into(),
                updated_at: "2024-01-01T00:00:00Z".into(),
            },
            SearchResult {
                repo_owner: "org".into(),
                repo_name: "repo".into(),
                number: 1,
                title: "A dup".into(),
                author: "user".into(),
                updated_at: "2024-01-01T00:00:00Z".into(),
            },
            SearchResult {
                repo_owner: "org".into(),
                repo_name: "repo".into(),
                number: 2,
                title: "B".into(),
                author: "user".into(),
                updated_at: "2024-01-01T00:00:00Z".into(),
            },
        ];

        let deduped = dedup_results(results);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].number, 1);
        assert_eq!(deduped[1].number, 2);
    }

    #[test]
    fn aggregate_preview_counts_dedupes_overlapping_query_results() {
        // Two "queries" both match PR #1 (e.g. author:@me and a team review
        // request); a naive sum would report 3 instead of the true 2.
        let results = vec![
            Ok(vec![
                search_result("org", "repo", 1),
                search_result("org", "repo", 2),
            ]),
            Ok(vec![search_result("org", "repo", 1)]),
        ];
        let (total, errors) = aggregate_preview_counts(results);
        assert_eq!(total, 2);
        assert!(errors.is_empty());
    }

    #[test]
    fn aggregate_preview_counts_collects_errors_without_dropping_ok_results() {
        let results = vec![
            Ok(vec![search_result("org", "repo", 1)]),
            Err("query 'bad:qualifier' failed".to_string()),
        ];
        let (total, errors) = aggregate_preview_counts(results);
        assert_eq!(total, 1);
        assert_eq!(errors, vec!["query 'bad:qualifier' failed".to_string()]);
    }

    #[test]
    fn normalizes_team_identifiers_case_insensitively() {
        assert_eq!(
            normalize_team_identifier(" MyOrg/Team-A ").as_deref(),
            Some("myorg/team-a")
        );
    }

    #[test]
    fn does_not_match_different_org_or_team_slug() {
        assert_ne!(
            normalize_team_identifier("myorg/team-a"),
            normalize_team_identifier("other/team-a")
        );
        assert_ne!(
            normalize_team_identifier("myorg/team-a"),
            normalize_team_identifier("myorg/team-b")
        );
        assert!(normalize_team_identifier("team-a").is_none());
    }

    #[test]
    fn dedup_merges_reasons_for_same_pr() {
        let scope = SearchScope::Repo {
            owner: "org".into(),
            repo: "repo".into(),
        };
        let deduped = dedup_provenanced_results(vec![
            ProvenancedSearchResult::new(
                search_result("org", "repo", 1),
                SearchReason::TargetTeamReview {
                    scope: scope.clone(),
                    team: "org/team-a".into(),
                },
            ),
            ProvenancedSearchResult::new(
                search_result("ORG", "REPO", 1),
                SearchReason::TargetDirectReview {
                    scope: scope.clone(),
                },
            ),
        ]);

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].reasons.len(), 2);
    }

    #[test]
    fn filters_out_requested_team_when_viewer_is_not_member() {
        let (config, scope) = config_with_team_target();
        let pr = pull_request("other", vec![], vec!["myorg/team-a"]);
        let filtered = filter_prs_by_provenance(
            vec![pr],
            &[provenance(
                SearchReason::TargetTeamReview {
                    scope,
                    team: "myorg/team-a".into(),
                },
                1,
            )],
            &config,
            "me",
            &HashSet::new(),
        );

        assert!(filtered.is_empty());
    }

    #[test]
    fn keeps_requested_team_when_viewer_is_member() {
        let (config, scope) = config_with_team_target();
        let mut memberships = HashSet::new();
        memberships.insert("myorg/team-a".to_string());
        let pr = pull_request("other", vec![], vec!["myorg/team-a"]);
        let filtered = filter_prs_by_provenance(
            vec![pr],
            &[provenance(
                SearchReason::TargetTeamReview {
                    scope,
                    team: "myorg/team-a".into(),
                },
                1,
            )],
            &config,
            "me",
            &memberships,
        );

        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn keeps_direct_user_request_even_with_stale_team_reason() {
        let (config, scope) = config_with_team_target();
        let pr = pull_request("other", vec!["me"], vec!["myorg/team-a"]);
        let filtered = filter_prs_by_provenance(
            vec![pr],
            &[ProvenancedSearchResult {
                result: search_result("myorg", "repo", 1),
                reasons: vec![
                    SearchReason::TargetTeamReview {
                        scope: scope.clone(),
                        team: "myorg/team-a".into(),
                    },
                    SearchReason::TargetDirectReview { scope },
                ],
            }],
            &config,
            "me",
            &HashSet::new(),
        );

        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn keeps_authored_pr_with_include_authored() {
        let (config, scope) = config_with_team_target();
        let pr = pull_request("me", vec![], vec!["myorg/team-a"]);
        let filtered = filter_prs_by_provenance(
            vec![pr],
            &[provenance(SearchReason::TargetAuthor { scope }, 1)],
            &config,
            "me",
            &HashSet::new(),
        );

        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn drops_authored_pr_when_include_authored_false_without_other_reason() {
        let mut config = crate::config::GithubConfig::default();
        config.targets.push(crate::config::GithubTarget {
            repo: Some("myorg/repo".into()),
            include_authored: false,
            ..Default::default()
        });
        let scope = SearchScope::Repo {
            owner: "myorg".into(),
            repo: "repo".into(),
        };
        let pr = pull_request("me", vec![], vec![]);
        let filtered = filter_prs_by_provenance(
            vec![pr],
            &[provenance(SearchReason::TargetAuthor { scope }, 1)],
            &config,
            "me",
            &HashSet::new(),
        );

        assert!(filtered.is_empty());
    }

    #[test]
    fn keeps_target_involved_only_when_involves_query_matched() {
        let (config, scope) = config_with_team_target();
        let pr = pull_request("other", vec![], vec![]);
        let filtered = filter_prs_by_provenance(
            vec![pr],
            &[provenance(SearchReason::TargetInvolves { scope }, 1)],
            &config,
            "me",
            &HashSet::new(),
        );

        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn does_not_keep_pr_from_scope_alone_without_involves_provenance() {
        let (config, scope) = config_with_team_target();
        let pr = pull_request("other", vec![], vec![]);
        let filtered = filter_prs_by_provenance(
            vec![pr],
            &[provenance(SearchReason::TargetDirectReview { scope }, 1)],
            &config,
            "me",
            &HashSet::new(),
        );

        assert!(filtered.is_empty());
    }

    #[test]
    fn keeps_watch_review_requested_only_with_watch_provenance() {
        let config = crate::config::GithubConfig {
            watch: vec!["myorg/repo".into()],
            ..Default::default()
        };
        let scope = SearchScope::Repo {
            owner: "myorg".into(),
            repo: "repo".into(),
        };
        let pr = pull_request("other", vec![], vec![]);
        let filtered = filter_prs_by_provenance(
            vec![pr],
            &[provenance(
                SearchReason::WatchReviewRequested { scope: Some(scope) },
                1,
            )],
            &config,
            "me",
            &HashSet::new(),
        );

        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn filter_drops_stale_team_only_but_keeps_stale_team_plus_direct() {
        let (config, scope) = config_with_team_target();
        let stale_only = pull_request("other", vec![], vec!["myorg/team-a"]);
        let stale_plus_direct = PullRequest {
            number: 2,
            review_requests: vec!["me".into()],
            ..pull_request("other", vec![], vec!["myorg/team-a"])
        };
        let filtered = filter_prs_by_provenance(
            vec![stale_only, stale_plus_direct],
            &[
                provenance(
                    SearchReason::TargetTeamReview {
                        scope: scope.clone(),
                        team: "myorg/team-a".into(),
                    },
                    1,
                ),
                ProvenancedSearchResult {
                    result: search_result("myorg", "repo", 2),
                    reasons: vec![
                        SearchReason::TargetTeamReview {
                            scope: scope.clone(),
                            team: "myorg/team-a".into(),
                        },
                        SearchReason::TargetDirectReview { scope },
                    ],
                },
            ],
            &config,
            "me",
            &HashSet::new(),
        );

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].number, 2);
    }

    fn config_with_team_target() -> (crate::config::GithubConfig, SearchScope) {
        let mut config = crate::config::GithubConfig::default();
        config.targets.push(crate::config::GithubTarget {
            repo: Some("myorg/repo".into()),
            team_review_requests: vec!["myorg/team-a".into()],
            include_authored: true,
            include_involved: true,
            ..Default::default()
        });
        let scope = SearchScope::Repo {
            owner: "myorg".into(),
            repo: "repo".into(),
        };
        (config, scope)
    }

    fn provenance(reason: SearchReason, number: u64) -> ProvenancedSearchResult {
        ProvenancedSearchResult::new(search_result("myorg", "repo", number), reason)
    }

    fn pull_request(
        author: &str,
        review_requests: Vec<&str>,
        team_requests: Vec<&str>,
    ) -> PullRequest {
        PullRequest {
            node_id: format!("node-{}", review_requests.len() + team_requests.len()),
            number: 1,
            title: "A".into(),
            body: String::new(),
            url: String::new(),
            author: author.into(),
            author_is_bot: false,
            owner: "myorg".into(),
            repo: "repo".into(),
            is_draft: false,
            updated_at: "2024-01-01T00:00:00Z".into(),
            head_ref: "feature".into(),
            base_ref: "main".into(),
            mergeable: crate::github::types::MergeableState::Unknown,
            review_decision: None,
            review_requests: review_requests.into_iter().map(String::from).collect(),
            team_review_requests: team_requests.into_iter().map(String::from).collect(),
            viewer_latest_review: None,
            latest_reviews: vec![],
            check_status: crate::github::types::CheckStatus::None,
            checks: vec![],
            review_threads: vec![],
            files: vec![],
            comments: 0,
            timeline: vec![],
            llm_priority: None,
            llm_summary: None,
            llm_rich_summary: None,
            last_seen_at: None,
        }
    }

    fn search_result(owner: &str, repo: &str, number: u64) -> SearchResult {
        SearchResult {
            repo_owner: owner.into(),
            repo_name: repo.into(),
            number,
            title: "A".into(),
            author: "user".into(),
            updated_at: "2024-01-01T00:00:00Z".into(),
        }
    }
}
