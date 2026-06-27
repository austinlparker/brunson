use brunson::config::Config;
use brunson::daemon;
use brunson::github::client::{resolve_api_urls, GitHubClient};

/// Test URL resolution for github.com
#[test]
fn test_api_urls_github_com() {
    let (rest, graphql) = resolve_api_urls("github.com");
    assert_eq!(rest, "https://api.github.com");
    assert_eq!(graphql, "https://api.github.com/graphql");
}

/// Test URL resolution for GHES host
#[test]
fn test_api_urls_ghes() {
    let (rest, graphql) = resolve_api_urls("github.company.com");
    assert_eq!(rest, "https://github.company.com/api/v3");
    assert_eq!(graphql, "https://github.company.com/api/graphql");
}

/// Test that a GitHubClient can be constructed with a token
#[test]
fn test_github_client_construction() {
    let client = GitHubClient::new("test_token".to_string(), Some("github.com".to_string()));
    assert!(client.is_ok());
    let c = client.unwrap();
    assert_eq!(c.rest_base(), "https://api.github.com");
    assert_eq!(c.graphql_base(), "https://api.github.com/graphql");
}

/// Test config defaults
#[test]
fn test_config_defaults() {
    let config = Config::default();
    assert_eq!(config.daemon.port, 17890);
    assert_eq!(config.github.poll_interval, 300);
    assert!(!config.llm.enabled);
}

/// Test config parsing from TOML
#[test]
fn test_config_parse_from_toml() {
    let toml_str = r#"
[github]
watch = ["myorg"]
poll_interval = 120

[daemon]
port = 9999

[llm]
enabled = true
endpoint = "http://localhost:8080/v1"
"#;
    let config: Config = toml::from_str(toml_str).unwrap();
    assert_eq!(config.github.watch, vec!["myorg"]);
    assert_eq!(config.github.poll_interval, 120);
    assert_eq!(config.daemon.port, 9999);
    assert!(config.llm.enabled);
    assert_eq!(config.llm.endpoint, "http://localhost:8080/v1");
}

/// Test that daemon module exports VERSION
#[test]
fn test_daemon_version() {
    assert!(!daemon::VERSION.is_empty());
}
