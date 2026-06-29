use std::fmt;
use std::str::FromStr;

use anyhow::{anyhow, Result};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::{default_endpoint_for_provider, LlmConfig};
use crate::github::types::{Priority, PullRequest};

const SYSTEM_PROMPT: &str = "You are a PR triage assistant. Classify the following pull request's urgency. \
Consider: is the author blocked waiting on review? Is CI failing? Are there unresolved change requests? \
Respond ONLY with JSON in this exact format: \
{\"priority\": \"high|medium|low\", \"summary\": \"one sentence summary\", \"reasoning\": \"brief explanation\"}";

/// Result of LLM classification.
#[derive(Debug, Clone)]
pub struct ClassificationResult {
    pub priority: Priority,
    pub summary: String,
    #[allow(dead_code)]
    pub reasoning: String,
}

/// Supported LLM provider targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    LmStudio,
    OpenAiCompatible,
}

impl FromStr for LlmProvider {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lm_studio" => Ok(Self::LmStudio),
            "openai_compatible" => Ok(Self::OpenAiCompatible),
            _ => Err(anyhow!("Unknown LLM provider: {}", s)),
        }
    }
}

impl fmt::Display for LlmProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LlmProvider::LmStudio => write!(f, "lm_studio"),
            LlmProvider::OpenAiCompatible => write!(f, "openai_compatible"),
        }
    }
}

/// Resolved provider-level configuration used by the classifier.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
    pub max_output_tokens: u32,
}

/// OpenAI-compatible LLM classifier client.
pub struct Classifier {
    client: Client,
    provider: LlmProvider,
    config: ProviderConfig,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f64,
    max_tokens: u32,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatResponseMessage,
}

#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelInfo>,
}

#[derive(Deserialize)]
struct ModelInfo {
    id: String,
}

impl Classifier {
    pub fn new(config: &LlmConfig) -> Result<Self> {
        let provider: LlmProvider = if config.provider.trim().is_empty() {
            LlmProvider::LmStudio
        } else {
            config.provider.parse()?
        };

        let endpoint = if config.endpoint.trim().is_empty() {
            default_endpoint_for_provider(provider.to_string().as_str()).to_string()
        } else {
            config.endpoint.clone()
        };

        let provider_config = ProviderConfig {
            endpoint,
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            max_output_tokens: config.max_output_tokens,
        };

        let mut headers = HeaderMap::new();
        if !provider_config.api_key.is_empty() {
            let value = HeaderValue::from_str(&format!("Bearer {}", provider_config.api_key))?;
            headers.insert(AUTHORIZATION, value);
        }

        let client = Client::builder().default_headers(headers).build()?;
        Ok(Self {
            client,
            provider,
            config: provider_config,
        })
    }

    pub fn provider(&self) -> LlmProvider {
        self.provider
    }

    pub fn model(&self) -> Option<&str> {
        Some(self.config.model.as_str()).filter(|s| !s.is_empty())
    }

    /// Auto-detect model name via GET /models if model is empty.
    pub async fn resolve_model(&mut self) -> Result<()> {
        if !self.config.model.is_empty() {
            return Ok(());
        }

        let url = format!("{}/models", self.config.endpoint);
        debug!("Auto-detecting model from {}", url);

        let resp = self.client.get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(anyhow!("Failed to fetch models: {}", resp.status()));
        }

        let models: ModelsResponse = resp.json().await?;
        if let Some(first) = models.data.first() {
            self.config.model = first.id.clone();
            debug!("Auto-detected model: {}", self.config.model);
            Ok(())
        } else {
            Err(anyhow!(
                "No models available at {} (/models returned empty list)",
                self.provider
            ))
        }
    }

    /// Build the user content for classification from a PR.
    fn build_pr_content(pr: &PullRequest) -> String {
        let mut content = format!("Title: {}\n", pr.title);
        content.push_str(&format!("Author: {}\n", pr.author));
        content.push_str(&format!("Draft: {}\n", pr.is_draft));

        // Body excerpt
        let body_excerpt: String = pr.body.chars().take(500).collect();
        if !body_excerpt.is_empty() {
            content.push_str(&format!("Body: {}\n", body_excerpt));
        }

        // Check status
        content.push_str(&format!("CI Status: {:?}\n", pr.check_status));

        // Review decision
        if let Some(rd) = &pr.review_decision {
            content.push_str(&format!("Review Decision: {:?}\n", rd));
        }

        // Recent comments
        for thread in pr.review_threads.iter().take(3) {
            if !thread.is_resolved {
                if let Some(comment) = thread.comments.first() {
                    let excerpt: String = comment.body.chars().take(200).collect();
                    content.push_str(&format!("Comment from {}: {}\n", comment.author, excerpt));
                }
            }
        }

        content
    }

    /// Classify a PR's urgency via the configured OpenAI-compatible endpoint.
    pub async fn classify(&self, pr: &PullRequest) -> Result<ClassificationResult> {
        let content = Self::build_pr_content(pr);

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: SYSTEM_PROMPT.into(),
                },
                ChatMessage {
                    role: "user".into(),
                    content,
                },
            ],
            temperature: 0.0,
            max_tokens: self.config.max_output_tokens,
        };

        let url = format!("{}/chat/completions", self.config.endpoint);
        let resp = self.client.post(&url).json(&request).send().await?;

        if !resp.status().is_success() {
            return Err(anyhow!("LLM request failed: {}", resp.status()));
        }

        let chat_resp: ChatResponse = resp.json().await?;
        let response_text = chat_resp
            .choices
            .first()
            .ok_or_else(|| anyhow!("No choices in LLM response"))?
            .message
            .content
            .clone();

        parse_classification(&response_text)
    }
}

/// Parse the LLM response text into a ClassificationResult.
/// Handles malformed JSON gracefully.
pub fn parse_classification(text: &str) -> Result<ClassificationResult> {
    // Try to extract JSON from the response
    let json_str = extract_json(text);

    match json_str {
        Some(json) => {
            #[derive(Deserialize)]
            struct ClassificationJson {
                priority: String,
                #[serde(default)]
                summary: String,
                #[serde(default)]
                reasoning: String,
            }

            match serde_json::from_str::<ClassificationJson>(&json) {
                Ok(parsed) => {
                    let priority = match parsed.priority.to_lowercase().as_str() {
                        "high" => Priority::High,
                        "low" => Priority::Low,
                        _ => Priority::Medium,
                    };
                    Ok(ClassificationResult {
                        priority,
                        summary: parsed.summary,
                        reasoning: parsed.reasoning,
                    })
                }
                Err(e) => {
                    warn!(
                        "Failed to parse classification JSON: {} (text: {})",
                        e, json
                    );
                    Ok(fallback_classification())
                }
            }
        }
        None => {
            warn!("No JSON found in classification response: {}", text);
            Ok(fallback_classification())
        }
    }
}

fn fallback_classification() -> ClassificationResult {
    ClassificationResult {
        priority: Priority::Medium,
        summary: String::new(),
        reasoning: String::new(),
    }
}

/// Try to extract a JSON object from the response text.
fn extract_json(text: &str) -> Option<String> {
    let trimmed = text.trim();

    // Try direct parse first
    if trimmed.starts_with('{') {
        return Some(trimmed.to_string());
    }

    // Try to find JSON within markdown code blocks
    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }

    // Try to find first { ... last }
    let first = trimmed.find('{')?;
    let last = trimmed.rfind('}')?;
    if last > first {
        Some(trimmed[first..=last].to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_valid_json() {
        let text =
            r#"{"priority": "high", "summary": "Blocking issue", "reasoning": "CI failing"}"#;
        let result = parse_classification(text).unwrap();
        assert_eq!(result.priority, Priority::High);
        assert_eq!(result.summary, "Blocking issue");
    }

    #[test]
    fn test_parse_with_surrounding_text() {
        let text = r#"Here is the classification: {"priority": "low", "summary": "Minor docs update", "reasoning": "Low impact"} Done."#;
        let result = parse_classification(text).unwrap();
        assert_eq!(result.priority, Priority::Low);
        assert_eq!(result.summary, "Minor docs update");
    }

    #[test]
    fn test_parse_markdown_code_block() {
        let text = "```json\n{\"priority\": \"medium\", \"summary\": \"Test\"}\n```";
        let result = parse_classification(text).unwrap();
        assert_eq!(result.priority, Priority::Medium);
        assert_eq!(result.summary, "Test");
    }

    #[test]
    fn test_parse_malformed_json() {
        let text = "This is not JSON at all";
        let result = parse_classification(text).unwrap();
        assert_eq!(result.priority, Priority::Medium);
        assert!(result.summary.is_empty());
    }

    #[test]
    fn test_parse_truncated_json() {
        let text = r#"{"priority": "high"#;
        let result = parse_classification(text).unwrap();
        // Should fall back to medium
        assert_eq!(result.priority, Priority::Medium);
    }

    #[test]
    fn test_provider_parsing() {
        assert_eq!(
            "lm_studio".parse::<LlmProvider>().unwrap(),
            LlmProvider::LmStudio
        );
        assert_eq!(
            "openai_compatible".parse::<LlmProvider>().unwrap(),
            LlmProvider::OpenAiCompatible
        );
        assert!("unknown".parse::<LlmProvider>().is_err());
    }

    #[test]
    fn test_classifier_applies_default_endpoint() {
        let config = LlmConfig {
            provider: String::new(),
            endpoint: String::new(),
            ..Default::default()
        };
        let classifier = Classifier::new(&config).unwrap();
        assert_eq!(classifier.provider(), LlmProvider::LmStudio);
        // The endpoint is private, but we can exercise a real request in the
        // integration tests for header injection.
        assert!(classifier.model().is_none());
    }

    #[tokio::test]
    async fn test_classifier_sends_authorization_header() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("Authorization", "Bearer secret-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "test-model"}]
            })))
            .mount(&server)
            .await;

        let config = LlmConfig {
            provider: "openai_compatible".to_string(),
            endpoint: server.uri(),
            api_key: "secret-key".to_string(),
            ..Default::default()
        };

        let mut classifier = Classifier::new(&config).unwrap();
        classifier.resolve_model().await.unwrap();
        assert_eq!(classifier.model(), Some("test-model"));
    }
}
