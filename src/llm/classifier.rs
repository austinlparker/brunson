use std::fmt;
use std::str::FromStr;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
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

/// Maximum PRs classified in a single batched LLM call. Chunking keeps each
/// prompt/response a bounded size regardless of how many PRs need
/// classification in a given poll cycle.
pub const CLASSIFY_BATCH_SIZE: usize = 20;

const BATCH_SYSTEM_PROMPT: &str = "You are a PR triage assistant. You will be given a numbered list of \
pull requests (PR 1, PR 2, ...). For EACH PR, classify its urgency. Consider: is the author blocked \
waiting on review? Is CI failing? Are there unresolved change requests? \
Respond ONLY with a JSON array containing exactly one object per PR, in this exact format: \
[{\"index\": 1, \"priority\": \"high|medium|low\", \"summary\": \"one sentence summary\", \"reasoning\": \"brief explanation\"}, ...]. \
The \"index\" field MUST match the PR number given in the input (PR 1 -> index 1, PR 2 -> index 2, etc). \
Include exactly one entry per PR; items may be returned in any order.";

/// Version identifier for the rich classification prompt/schema. Bumping this
/// invalidates cached rich summaries so they are re-generated with the new
/// prompt on the next Overview view.
pub const RICH_PROMPT_VERSION: u32 = 2;

const RICH_SYSTEM_PROMPT: &str = "You are a PR triage assistant. The user has just opened this pull request in their review tool. \
Give them a focused orientation so they can act quickly. \
If a 'last_seen_at' timestamp is provided, focus on commits, comments, reviews, and CI status changes since that time. \
If no 'last_seen_at' timestamp is provided, summarize the most important recent context as if this is their first deep look. \
Respond ONLY with JSON in this exact format: \
{\"priority\": \"high|medium|low\", \"one_line\": \"one sentence tl;dr\", \"catch_up\": \"2-4 sentences on what changed, who did what, and any notable new comments, reviews, or CI events\", \"next_steps\": \"concrete, prioritized actions the user should take next, e.g. '1) Review the latest commit... 2) Reply to Alice's question...'\"}";

/// Result of a cheap LLM classification (used during polling).
#[derive(Debug, Clone)]
pub struct ClassificationResult {
    pub priority: Priority,
    pub summary: String,
    #[allow(dead_code)]
    pub reasoning: String,
}

/// Result of a richer, on-demand LLM classification for the Overview blade.
#[derive(Debug, Clone)]
pub struct RichClassificationResult {
    pub priority: Priority,
    pub one_line: String,
    pub catch_up: String,
    pub next_steps: String,
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
    /// Build the rich user content for the catch-up / next-steps classifier.
    fn build_rich_pr_content(pr: &PullRequest, last_seen_at: Option<DateTime<Utc>>) -> String {
        let mut content = format!("Title: {}\n", pr.title);
        content.push_str(&format!("Author: {}\n", pr.author));
        content.push_str(&format!("Draft: {}\n", pr.is_draft));

        let body_excerpt: String = pr.body.chars().take(800).collect();
        if !body_excerpt.is_empty() {
            content.push_str(&format!("Body: {}\n", body_excerpt));
        }

        content.push_str(&format!("CI Status: {:?}\n", pr.check_status));
        if let Some(rd) = &pr.review_decision {
            content.push_str(&format!("Review Decision: {:?}\n", rd));
        }

        if let Some(ts) = last_seen_at {
            content.push_str(&format!("Last seen by user: {}\n", ts.to_rfc3339()));
            content.push_str("Focus on timeline events and comments after this timestamp.\n");
        } else {
            content.push_str("User has not viewed this PR before. Summarize the most important recent context.\n");
        }

        // Recent timeline events
        let recent_events: Vec<_> = pr
            .timeline
            .iter()
            .rev()
            .take(10)
            .collect();
        if !recent_events.is_empty() {
            content.push_str("Recent activity:\n");
            for event in recent_events {
                content.push_str(&format!(
                    "- {:?} by {} at {}: {}\n",
                    event.event_type, event.actor, event.created_at, event.detail
                ));
            }
        }

        // Unresolved review threads
        let unresolved_threads: Vec<_> = pr
            .review_threads
            .iter()
            .filter(|t| !t.is_resolved)
            .take(3)
            .collect();
        if !unresolved_threads.is_empty() {
            content.push_str("Unresolved review threads:\n");
            for thread in unresolved_threads {
                if let Some(comment) = thread.comments.first() {
                    let excerpt: String = comment.body.chars().take(300).collect();
                    content.push_str(&format!(
                        "- {} on {}: {}\n",
                        comment.author, comment.path, excerpt
                    ));
                }
            }
        }

        content
    }

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

    /// Build the user content listing multiple PRs for a batched
    /// classification call. Trimmed tighter than `build_pr_content` since
    /// the excerpt budget now divides across up to `CLASSIFY_BATCH_SIZE` PRs.
    fn build_batch_pr_content(prs: &[PullRequest]) -> String {
        let mut content = String::new();
        for (i, pr) in prs.iter().enumerate() {
            content.push_str(&format!("### PR {}\n", i + 1));
            content.push_str(&format!("Repo: {}/{}#{}\n", pr.owner, pr.repo, pr.number));
            content.push_str(&format!("Title: {}\n", pr.title));
            content.push_str(&format!("Author: {}\n", pr.author));
            content.push_str(&format!("Draft: {}\n", pr.is_draft));

            let body_excerpt: String = pr.body.chars().take(200).collect();
            if !body_excerpt.is_empty() {
                content.push_str(&format!("Body: {}\n", body_excerpt));
            }

            content.push_str(&format!("CI Status: {:?}\n", pr.check_status));
            if let Some(rd) = &pr.review_decision {
                content.push_str(&format!("Review Decision: {:?}\n", rd));
            }

            for thread in pr.review_threads.iter().take(2) {
                if !thread.is_resolved {
                    if let Some(comment) = thread.comments.first() {
                        let excerpt: String = comment.body.chars().take(150).collect();
                        content
                            .push_str(&format!("Comment from {}: {}\n", comment.author, excerpt));
                    }
                }
            }
            content.push('\n');
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

    /// Classify up to `CLASSIFY_BATCH_SIZE` PRs in a single call. Always
    /// returns exactly `prs.len()` results, in input order — any PR the
    /// model drops, mis-indexes, or mangles gets `fallback_classification()`,
    /// so callers never need to reconcile a partial result set.
    pub async fn classify_batch(&self, prs: &[PullRequest]) -> Result<Vec<ClassificationResult>> {
        if prs.is_empty() {
            return Ok(Vec::new());
        }

        let content = Self::build_batch_pr_content(prs);

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: BATCH_SYSTEM_PROMPT.into(),
                },
                ChatMessage {
                    role: "user".into(),
                    content,
                },
            ],
            temperature: 0.0,
            max_tokens: batch_max_tokens(prs.len(), self.config.max_output_tokens),
        };

        let url = format!("{}/chat/completions", self.config.endpoint);
        let resp = self.client.post(&url).json(&request).send().await?;

        if !resp.status().is_success() {
            return Err(anyhow!("LLM batch request failed: {}", resp.status()));
        }

        let chat_resp: ChatResponse = resp.json().await?;
        let response_text = chat_resp
            .choices
            .first()
            .ok_or_else(|| anyhow!("No choices in LLM response"))?
            .message
            .content
            .clone();

        Ok(parse_batch_classification(&response_text, prs.len()))
    }

    /// Classify a PR with a richer prompt that produces catch-up / next-steps text
    /// scoped to the user's `last_seen_at` timestamp.
    pub async fn classify_rich(
        &self,
        pr: &PullRequest,
        last_seen_at: Option<DateTime<Utc>>,
    ) -> Result<RichClassificationResult> {
        let content = Self::build_rich_pr_content(pr, last_seen_at);

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".into(),
                    content: RICH_SYSTEM_PROMPT.into(),
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

        parse_rich_classification(&response_text)
    }
}

const BATCH_TOKENS_PER_PR: u32 = 200;
/// Empirically-chosen minimum output budget for a batch call. Verified live
/// against a local reasoning ("thinking") model: unlike a plain model, a
/// reasoning model that runs low on budget doesn't shorten its
/// chain-of-thought to fit — it just spends the entire budget reasoning and
/// returns empty content (`finish_reason: "length"`, observed 4093/4096
/// reasoning tokens burned with nothing left for the answer at a 4096
/// budget). Given generous headroom it reasons efficiently and stops on its
/// own well under the limit (observed 2307 total tokens used out of 16000
/// for a realistic 20-PR batch) — unused budget costs nothing, so erring
/// generous is strictly safer than calculating a tight "just enough" number.
const BATCH_MIN_TOKENS: u32 = 16384;

/// Output token budget for a batch classification call: the largest of the
/// user's configured `max_output_tokens` (proven sufficient for one
/// classification against their model), a naive per-PR linear scale (for
/// unusually large batches), and the empirically-informed minimum above.
/// The parser is truncation-tolerant regardless, as a backstop if a
/// response is ever cut off anyway.
fn batch_max_tokens(pr_count: usize, single_item_budget: u32) -> u32 {
    let scaled = BATCH_TOKENS_PER_PR.saturating_mul(pr_count as u32);
    single_item_budget.max(scaled).max(BATCH_MIN_TOKENS)
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

#[derive(Deserialize)]
struct BatchClassificationItem {
    #[serde(default)]
    index: Option<usize>,
    priority: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    reasoning: String,
}

/// Parse a batch classification response into exactly `expected` results, in
/// input order. Never fails: any PR the model drops, mis-indexes, or
/// otherwise doesn't produce a usable item for gets `fallback_classification()`,
/// so a single malformed item (or a response truncated mid-array) never
/// sacrifices the rest of the batch.
fn parse_batch_classification(text: &str, expected: usize) -> Vec<ClassificationResult> {
    let objects = extract_json_objects(text);
    if objects.is_empty() {
        warn!("No JSON objects found in batch classification response: {}", text);
        return (0..expected).map(|_| fallback_classification()).collect();
    }

    let mut results: Vec<Option<ClassificationResult>> = (0..expected).map(|_| None).collect();
    let mut unindexed: Vec<ClassificationResult> = Vec::new();

    for obj in &objects {
        match serde_json::from_str::<BatchClassificationItem>(obj) {
            Ok(item) => {
                let result = ClassificationResult {
                    priority: match item.priority.to_lowercase().as_str() {
                        "high" => Priority::High,
                        "low" => Priority::Low,
                        _ => Priority::Medium,
                    },
                    summary: item.summary,
                    reasoning: item.reasoning,
                };
                match item.index {
                    Some(idx) if idx >= 1 && idx <= expected => {
                        results[idx - 1] = Some(result);
                    }
                    Some(idx) => {
                        warn!("Batch classification item has out-of-range index {}: {}", idx, obj);
                        unindexed.push(result);
                    }
                    None => unindexed.push(result),
                }
            }
            Err(e) => {
                warn!("Failed to parse batch classification item: {} (text: {})", e, obj);
            }
        }
    }

    // Positional backfill: covers models that ignore the "index" instruction
    // entirely, or emit an out-of-range index. Fill remaining empty slots,
    // in order, from leftover items in response order.
    let mut unindexed_iter = unindexed.into_iter();
    for slot in results.iter_mut() {
        if slot.is_none() {
            *slot = unindexed_iter.next();
        }
    }

    results
        .into_iter()
        .enumerate()
        .map(|(i, opt)| {
            opt.unwrap_or_else(|| {
                warn!(
                    "No classification recovered for batch item {} of {}, using fallback",
                    i + 1,
                    expected
                );
                fallback_classification()
            })
        })
        .collect()
}

/// Scan text for top-level balanced `{...}` objects, respecting string
/// escaping. Ignores everything outside braces (array brackets, commas,
/// prose, code fences) — this is what lets a batch response be a clean JSON
/// array, one wrapped in markdown fences, or one with surrounding prose, all
/// without special-casing each shape. A trailing object whose braces never
/// balance (the response was cut off mid-object by a token limit) is simply
/// never yielded, which is what makes truncated batches degrade gracefully
/// instead of losing the whole response.
fn extract_json_objects(text: &str) -> Vec<String> {
    let mut objects = Vec::new();
    let mut depth = 0usize;
    let mut start = None;
    let mut in_string = false;
    let mut escape = false;
    for (i, ch) in text.char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start.take() {
                        objects.push(text[s..=i].to_string());
                    }
                }
            }
            _ => {}
        }
    }
    objects
}

/// Parse the LLM response text into a RichClassificationResult.
pub fn parse_rich_classification(text: &str) -> Result<RichClassificationResult> {
    let json_str = extract_json(text);

    match json_str {
        Some(json) => {
            #[derive(Deserialize)]
            struct RichClassificationJson {
                priority: String,
                #[serde(default)]
                one_line: String,
                #[serde(default)]
                catch_up: String,
                #[serde(default)]
                next_steps: String,
            }

            match serde_json::from_str::<RichClassificationJson>(&json) {
                Ok(parsed) => {
                    let priority = match parsed.priority.to_lowercase().as_str() {
                        "high" => Priority::High,
                        "low" => Priority::Low,
                        _ => Priority::Medium,
                    };
                    Ok(RichClassificationResult {
                        priority,
                        one_line: parsed.one_line,
                        catch_up: parsed.catch_up,
                        next_steps: parsed.next_steps,
                    })
                }
                Err(e) => {
                    warn!(
                        "Failed to parse rich classification JSON: {} (text: {})",
                        e, json
                    );
                    Ok(fallback_rich_classification())
                }
            }
        }
        None => {
            warn!("No JSON found in rich classification response: {}", text);
            Ok(fallback_rich_classification())
        }
    }
}

fn fallback_rich_classification() -> RichClassificationResult {
    RichClassificationResult {
        priority: Priority::Medium,
        one_line: String::new(),
        catch_up: String::new(),
        next_steps: String::new(),
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
    fn test_parse_batch_valid_json() {
        let text = r#"[
            {"index": 1, "priority": "high", "summary": "First", "reasoning": "CI red"},
            {"index": 2, "priority": "low", "summary": "Second", "reasoning": "Docs only"},
            {"index": 3, "priority": "medium", "summary": "Third", "reasoning": "Waiting"}
        ]"#;
        let results = parse_batch_classification(text, 3);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].priority, Priority::High);
        assert_eq!(results[0].summary, "First");
        assert_eq!(results[1].priority, Priority::Low);
        assert_eq!(results[1].summary, "Second");
        assert_eq!(results[2].priority, Priority::Medium);
        assert_eq!(results[2].summary, "Third");
    }

    #[test]
    fn test_parse_batch_out_of_order_index() {
        let text = r#"[
            {"index": 2, "priority": "low", "summary": "Second"},
            {"index": 1, "priority": "high", "summary": "First"},
            {"index": 3, "priority": "medium", "summary": "Third"}
        ]"#;
        let results = parse_batch_classification(text, 3);
        assert_eq!(results[0].summary, "First");
        assert_eq!(results[0].priority, Priority::High);
        assert_eq!(results[1].summary, "Second");
        assert_eq!(results[1].priority, Priority::Low);
        assert_eq!(results[2].summary, "Third");
    }

    #[test]
    fn test_parse_batch_truncated_array() {
        // Second object cut off mid-way by a token limit.
        let text = r#"[{"index": 1, "priority": "high", "summary": "First"}, {"index": 2, "priority": "lo"#;
        let results = parse_batch_classification(text, 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].priority, Priority::High);
        assert_eq!(results[0].summary, "First");
        // Truncated item falls back rather than dropping the whole batch.
        assert_eq!(results[1].priority, Priority::Medium);
        assert!(results[1].summary.is_empty());
    }

    #[test]
    fn test_parse_batch_mixed_valid_invalid() {
        let text = r#"[
            {"index": 1, "priority": "high", "summary": "First"},
            {"index": 2, "notpriority": "oops"},
            {"index": 3, "priority": "low", "summary": "Third"}
        ]"#;
        let results = parse_batch_classification(text, 3);
        assert_eq!(results[0].summary, "First");
        assert_eq!(results[1].priority, Priority::Medium);
        assert!(results[1].summary.is_empty());
        assert_eq!(results[2].summary, "Third");
    }

    #[test]
    fn test_parse_batch_empty_array() {
        let results = parse_batch_classification("[]", 3);
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| r.priority == Priority::Medium));
    }

    #[test]
    fn test_parse_batch_non_array_garbage() {
        let results = parse_batch_classification("Sorry, I cannot help with that.", 4);
        assert_eq!(results.len(), 4);
        assert!(results.iter().all(|r| r.priority == Priority::Medium));
    }

    #[test]
    fn test_parse_batch_missing_index_positional_fallback() {
        let text = r#"[
            {"priority": "high", "summary": "First"},
            {"priority": "low", "summary": "Second"}
        ]"#;
        let results = parse_batch_classification(text, 2);
        assert_eq!(results[0].summary, "First");
        assert_eq!(results[0].priority, Priority::High);
        assert_eq!(results[1].summary, "Second");
        assert_eq!(results[1].priority, Priority::Low);
    }

    #[test]
    fn test_parse_batch_with_surrounding_prose_and_fence() {
        let text = "Here you go:\n```json\n[{\"index\": 1, \"priority\": \"high\", \"summary\": \"First\"}]\n```\nLet me know if you need more.";
        let results = parse_batch_classification(text, 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].priority, Priority::High);
        assert_eq!(results[0].summary, "First");
    }

    #[test]
    fn test_batch_max_tokens_scales_with_count() {
        // Small batch with a modest single-item budget: the empirically
        // generous minimum wins (small inputs never get starved).
        assert_eq!(batch_max_tokens(1, 4096), BATCH_MIN_TOKENS);
        // A very large batch's linear per-item scale can exceed the minimum.
        assert_eq!(batch_max_tokens(1000, 200), BATCH_TOKENS_PER_PR * 1000);
        // A large configured single-item budget is never clamped below itself.
        assert_eq!(batch_max_tokens(5, 20000), 20000);
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

    fn make_pr(node_id: &str, number: u64) -> PullRequest {
        PullRequest {
            node_id: node_id.to_string(),
            number,
            title: format!("PR {}", number),
            body: String::new(),
            url: String::new(),
            author: "author".to_string(),
            author_is_bot: false,
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            is_draft: false,
            updated_at: "2024-01-01T00:00:00Z".to_string(),
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
            files: Vec::new(),
            comments: 0,
            timeline: Vec::new(),
            llm_priority: None,
            llm_summary: None,
            llm_rich_summary: None,
            last_seen_at: None,
        }
    }

    #[tokio::test]
    async fn test_classifier_classify_batch_request_and_response() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "[{\"index\": 1, \"priority\": \"high\", \"summary\": \"First\"}, \
                                     {\"index\": 2, \"priority\": \"low\", \"summary\": \"Second\"}]"
                    }
                }]
            })))
            .mount(&server)
            .await;

        let config = LlmConfig {
            provider: "openai_compatible".to_string(),
            endpoint: server.uri(),
            model: "test-model".to_string(),
            ..Default::default()
        };
        let classifier = Classifier::new(&config).unwrap();
        let prs = vec![make_pr("a", 1), make_pr("b", 2)];

        let results = classifier.classify_batch(&prs).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].priority, Priority::High);
        assert_eq!(results[0].summary, "First");
        assert_eq!(results[1].priority, Priority::Low);
        assert_eq!(results[1].summary, "Second");

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let body: serde_json::Value = received[0].body_json().unwrap();
        // LlmConfig::default().max_output_tokens is 4096, the single-item
        // floor batch_max_tokens uses.
        assert_eq!(body["max_tokens"], batch_max_tokens(2, 4096));
    }
}
