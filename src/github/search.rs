use std::collections::HashSet;

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
}
