use std::collections::HashSet;

use crate::config::{GithubConfig, GithubTarget};

use super::types::SearchResult;

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

/// Build all search queries for a poll cycle.
/// Returns a list of query strings.
///
/// If `watch` is empty, one query per search type (3 total).
/// If `watch` has entries, 3 × len(watch) queries.
pub fn build_queries(watch: &[String]) -> Vec<String> {
    let search_types = [
        SearchType::ReviewRequested,
        SearchType::Author,
        SearchType::Involves,
    ];

    let mut queries = Vec::new();

    if watch.is_empty() {
        for st in &search_types {
            queries.push(format!("{} is:pr is:open", st.qualifier()));
        }
    } else {
        for entry in watch {
            let qualifier = if entry.contains('/') {
                format!("repo:{}", entry)
            } else {
                format!("org:{}", entry)
            };
            for st in &search_types {
                queries.push(format!("{} is:pr is:open {}", st.qualifier(), qualifier));
            }
        }
    }

    queries
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
    let mut queries = if config.watch.is_empty() && !config.targets.is_empty() {
        Vec::new()
    } else {
        build_queries(&config.watch)
    };

    for target in &config.targets {
        queries.extend(build_target_queries(target));
    }

    queries
}

fn build_target_queries(target: &GithubTarget) -> Vec<String> {
    let Some(scope) = target_scope_qualifier(target) else {
        return Vec::new();
    };

    let mut queries = Vec::new();
    if target.direct_review_requests {
        queries.push(format!("user-review-requested:@me is:pr is:open {scope}"));
    }
    for team in &target.team_review_requests {
        queries.push(format!(
            "team-review-requested:{} is:pr is:open {scope}",
            team
        ));
    }
    if target.include_authored {
        queries.push(format!("author:@me is:pr is:open {scope}"));
    }
    if target.include_involved {
        queries.push(format!("involves:@me is:pr is:open {scope}"));
    }
    queries
}

fn target_scope_qualifier(target: &GithubTarget) -> Option<String> {
    if let Some(repo) = &target.repo {
        Some(format!("repo:{repo}"))
    } else {
        target.org.as_ref().map(|org| format!("org:{org}"))
    }
}

/// De-duplicate search results by (owner, repo, number).
pub fn dedup_results(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for r in results {
        let key = (r.repo_owner.clone(), r.repo_name.clone(), r.number);
        if seen.insert(key) {
            deduped.push(r);
        }
    }
    deduped
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
    if let Some((owner, repo)) = entry.split_once('/') {
        owner.eq_ignore_ascii_case(&result.repo_owner)
            && repo.eq_ignore_ascii_case(&result.repo_name)
    } else {
        entry.eq_ignore_ascii_case(&result.repo_owner)
    }
}

fn result_matches_target(result: &SearchResult, target: &GithubTarget) -> bool {
    if let Some(repo) = &target.repo {
        result_matches_watch_entry(result, repo)
    } else if let Some(org) = &target.org {
        org.eq_ignore_ascii_case(&result.repo_owner)
    } else {
        false
    }
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
