//! Structured extraction compaction strategy.
//!
//! An LLM-backed strategy that processes conversation history and produces
//! typed records: decisions, constraints, action items, key facts — each
//! with source turn references. When the LLM call fails, the caller falls
//! back to truncation.

use serde::{Deserialize, Serialize};
use swage_core::{ChatMessage, CompletionRequest, CompletionResponse, ProviderError, Role};
use swage_provider::LlmProvider;

use crate::tokeniser::Tokeniser;

// ─────────────────────────────────────────────────────────────────────────────
// Output types
// ─────────────────────────────────────────────────────────────────────────────

/// Complete output of a structured extraction pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionOutput {
    /// Formatted text content suitable for inclusion as a system message.
    pub content: String,

    /// Categorised extraction records.
    pub categories: Vec<ExtractionCategory>,

    /// Estimated token count of the formatted content.
    pub token_count: usize,

    /// Whether the output was truncated to fit within budget.
    pub truncated: bool,
}

/// A named category of extracted records (e.g. "decisions", "constraints").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionCategory {
    /// Category name, e.g. `"decisions"`, `"constraints"`, `"action_items"`,
    /// `"key_facts"`.
    pub name: String,

    /// Individual items extracted under this category.
    pub items: Vec<ExtractionItem>,
}

/// A single extracted record with source provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionItem {
    /// The extracted content.
    pub content: String,

    /// Indices of the source turns (message positions) this item was derived
    /// from. Enables traceability back to the original conversation.
    pub source_turns: Vec<usize>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Error type
// ─────────────────────────────────────────────────────────────────────────────

/// Errors from structured extraction — the caller is expected to fall back
/// to truncation when any of these occur.
#[derive(Debug, thiserror::Error)]
pub enum ExtractionError {
    /// The LLM provider returned an error.
    #[error("LLM provider error: {0}")]
    Provider(#[from] ProviderError),

    /// The LLM response could not be parsed as the expected JSON structure.
    #[error("failed to parse extraction response: {0}")]
    ParseFailure(String),
}

// ─────────────────────────────────────────────────────────────────────────────
// Compactor
// ─────────────────────────────────────────────────────────────────────────────

/// LLM-backed compactor that extracts structured records from conversation
/// history. Produces a `ChatMessage(Role::System)` with categorised content
/// instead of raw truncated messages.
pub struct StructuredExtractionCompactor;

impl StructuredExtractionCompactor {
    /// Construct a compactor. Currently stateless; reserved for future options.
    pub fn new() -> Self {
        Self
    }

    /// Extract structured records from `messages`, using `provider` for the
    /// LLM call and `tokeniser` for counting.
    ///
    /// On success, returns an `ExtractionOutput` whose `content` field is a
    /// formatted system message and whose `categories` carry the structured
    /// breakdown. If the output exceeds `budget` tokens it is truncated with
    /// `truncated = true`.
    ///
    /// On failure (LLM error or unparseable response), returns an
    /// `ExtractionError` — the caller should fall back to truncation.
    pub async fn extract(
        &self,
        messages: &[ChatMessage],
        budget: usize,
        provider: &dyn LlmProvider,
        tokeniser: &dyn Tokeniser,
    ) -> Result<ExtractionOutput, ExtractionError> {
        let prompt = build_extraction_prompt(messages);

        let request = CompletionRequest {
            messages: vec![ChatMessage::new(Role::System, &prompt)],
            model: None,
            max_tokens: Some(budget as u32),
            temperature: Some(0.0),
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        };

        let response: CompletionResponse = provider.complete(request).await?;
        let categories = parse_extraction_response(&response.content)?;

        // Format the structured output as readable text.
        let content = format_extraction(&categories);
        let token_count = tokeniser.count_text(&content);

        // If the output overshoots the budget, truncate it.
        let (final_content, truncated) = if token_count > budget {
            let truncated_content = truncate_to_budget(&content, budget, tokeniser);
            (truncated_content, true)
        } else {
            (content, false)
        };

        let final_token_count = tokeniser.count_text(&final_content);

        Ok(ExtractionOutput {
            content: final_content,
            categories,
            token_count: final_token_count,
            truncated,
        })
    }
}

impl Default for StructuredExtractionCompactor {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Prompt construction
// ─────────────────────────────────────────────────────────────────────────────

/// Build the extraction prompt from conversation messages.
///
/// Numbers each message for source-turn referencing, then instructs the LLM
/// to produce a JSON array of categorised extraction records.
fn build_extraction_prompt(messages: &[ChatMessage]) -> String {
    let mut prompt = String::from(
        "Extract structured records from the following conversation. \
         Categorise each record as one of: decisions, constraints, action_items, key_facts. \
         For each item, include the source turn numbers it was derived from.\n\n\
         Respond with a JSON array of objects, each with \"name\" (category), \
         and \"items\" (array of {\"content\": string, \"source_turns\": [number]}). \
         Example:\n\
         ```json\n\
         [\n\
           {\"name\": \"decisions\", \"items\": [{\"content\": \"Use PostgreSQL\", \"source_turns\": [2, 5]}]},\n\
           {\"name\": \"key_facts\", \"items\": [{\"content\": \"Budget is 50k\", \"source_turns\": [1]}]}\n\
         ]\n\
         ```\n\n\
         Conversation:\n\n",
    );

    for (i, message) in messages.iter().enumerate() {
        let role_label = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let text = message
            .content
            .iter()
            .filter_map(|part| match part {
                swage_core::ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        prompt.push_str(&format!("[Turn {i}] ({role_label}): {text}\n"));
    }

    prompt.push_str("\nJSON output:");
    prompt
}

// ─────────────────────────────────────────────────────────────────────────────
// Response parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Parse the LLM's JSON response into extraction categories.
///
/// Accepts the raw response text, extracts a JSON array from it (tolerating
/// markdown code fences), and deserialises into `Vec<ExtractionCategory>`.
fn parse_extraction_response(response: &str) -> Result<Vec<ExtractionCategory>, ExtractionError> {
    // Strip optional markdown code fences around the JSON.
    let json_str = response
        .trim()
        .strip_prefix("```json")
        .or_else(|| response.trim().strip_prefix("```"))
        .unwrap_or(response.trim());
    let json_str = json_str.strip_suffix("```").unwrap_or(json_str).trim();

    serde_json::from_str::<Vec<ExtractionCategory>>(json_str).map_err(|e| {
        ExtractionError::ParseFailure(format!("expected JSON array of categories: {e}"))
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Formatting
// ─────────────────────────────────────────────────────────────────────────────

/// Format extraction categories into a human-readable system message.
fn format_extraction(categories: &[ExtractionCategory]) -> String {
    let mut output = String::from("## Conversation Summary (Structured Extraction)\n\n");

    for category in categories {
        output.push_str(&format!("### {}\n", capitalise_category(&category.name)));
        for item in &category.items {
            let turns: String = item
                .source_turns
                .iter()
                .map(|t| t.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            output.push_str(&format!("- {} (turns: {})\n", item.content, turns));
        }
        output.push('\n');
    }

    output
}

/// Capitalise a snake_case category name for display.
fn capitalise_category(name: &str) -> String {
    name.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    format!("{upper}{}", chars.collect::<String>())
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ─────────────────────────────────────────────────────────────────────────────
// Budget enforcement
// ─────────────────────────────────────────────────────────────────────────────

/// Truncate text to fit within a token budget by progressively removing
/// trailing lines. Appends a truncation notice.
fn truncate_to_budget(text: &str, budget: usize, tokeniser: &dyn Tokeniser) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let notice = "\n[... truncated to fit token budget]";

    // Binary-style search: try progressively fewer lines until within budget.
    for end in (1..=lines.len()).rev() {
        let candidate = format!("{}{notice}", lines[..end].join("\n"));
        if tokeniser.count_text(&candidate) <= budget {
            return candidate;
        }
    }

    // Even a single line exceeds budget — return just the notice.
    notice.trim_start().to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokeniser::Tokeniser;
    use async_trait::async_trait;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use swage_core::{
        ChatMessage, CompletionRequest, CompletionResponse, FinishReason, ProviderError, Role,
        StreamChunk, ToolCompletionRequest, ToolCompletionResponse,
    };
    use swage_provider::LlmProvider;

    // ── Mock provider ────────────────────────────────────────────────────────

    struct MockProvider {
        response: String,
        call_count: AtomicUsize,
    }

    impl MockProvider {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
                call_count: AtomicUsize::new(0),
            }
        }

        fn failing() -> Self {
            Self {
                response: String::new(),
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn model_id(&self) -> &str {
            "mock-model"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            if self.response.is_empty() {
                return Err(ProviderError::Network {
                    provider: "mock".to_string(),
                    message: "simulated failure".to_string(),
                });
            }
            Ok(CompletionResponse {
                content: self.response.clone(),
                input_tokens: 100,
                output_tokens: 50,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!("not needed for extraction tests")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            unimplemented!("not needed for extraction tests")
        }
    }

    // ── Fixed-cost tokeniser ─────────────────────────────────────────────────

    struct FixedCostTokeniser {
        cost_per_char: usize,
    }

    impl FixedCostTokeniser {
        /// Roughly 1 token per 4 characters — close enough for tests.
        fn new() -> Self {
            Self { cost_per_char: 1 }
        }
    }

    impl Tokeniser for FixedCostTokeniser {
        fn count_text(&self, text: &str) -> usize {
            // Roughly 1 token per 4 characters.
            (text.len() * self.cost_per_char).div_ceil(4)
        }

        fn count_messages(&self, messages: &[ChatMessage]) -> usize {
            messages
                .iter()
                .map(|m| {
                    let text_len: usize = m
                        .content
                        .iter()
                        .map(|p| match p {
                            swage_core::ContentPart::Text { text } => text.len(),
                            _ => 0,
                        })
                        .sum();
                    (text_len * self.cost_per_char).div_ceil(4)
                })
                .sum()
        }
    }

    // ── Test fixtures ────────────────────────────────────────────────────────

    fn sample_messages() -> Vec<ChatMessage> {
        vec![
            ChatMessage::new(Role::User, "What database should we use?"),
            ChatMessage::new(
                Role::Assistant,
                "I recommend PostgreSQL for its reliability.",
            ),
            ChatMessage::new(Role::User, "Budget is limited to 50k."),
            ChatMessage::new(
                Role::Assistant,
                "We can use the free tier. Key decision: PostgreSQL.",
            ),
        ]
    }

    fn mock_json_response() -> String {
        serde_json::to_string(&vec![
            ExtractionCategory {
                name: "decisions".to_string(),
                items: vec![ExtractionItem {
                    content: "Use PostgreSQL for the database".to_string(),
                    source_turns: vec![1, 3],
                }],
            },
            ExtractionCategory {
                name: "constraints".to_string(),
                items: vec![ExtractionItem {
                    content: "Budget limited to 50k".to_string(),
                    source_turns: vec![2],
                }],
            },
            ExtractionCategory {
                name: "key_facts".to_string(),
                items: vec![ExtractionItem {
                    content: "Free tier available for PostgreSQL".to_string(),
                    source_turns: vec![3],
                }],
            },
        ])
        .unwrap()
    }

    // ── Extraction succeeds with mock provider ───────────────────────────────

    #[tokio::test]
    async fn extract_produces_categorised_output() {
        let provider = MockProvider::new(&mock_json_response());
        let tokeniser = FixedCostTokeniser::new();
        let compactor = StructuredExtractionCompactor::new();

        let result = compactor
            .extract(&sample_messages(), 10_000, &provider, &tokeniser)
            .await;

        let output = result.expect("extraction should succeed");
        assert!(
            !output.truncated,
            "should not be truncated with generous budget"
        );
        assert_eq!(output.categories.len(), 3);

        // Verify category names.
        let names: Vec<&str> = output.categories.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"decisions"));
        assert!(names.contains(&"constraints"));
        assert!(names.contains(&"key_facts"));

        // Verify source turns are preserved.
        let decisions = &output.categories[0];
        assert_eq!(decisions.items[0].source_turns, vec![1, 3]);
    }

    // ── Output is visually distinct from raw messages ────────────────────────

    #[tokio::test]
    async fn extraction_content_is_structured_not_raw() {
        let provider = MockProvider::new(&mock_json_response());
        let tokeniser = FixedCostTokeniser::new();
        let compactor = StructuredExtractionCompactor::new();

        let output = compactor
            .extract(&sample_messages(), 10_000, &provider, &tokeniser)
            .await
            .unwrap();

        // The content should be formatted with headers, not raw conversation.
        assert!(
            output.content.contains("## Conversation Summary"),
            "output should have a structured header"
        );
        assert!(
            output.content.contains("### Decisions"),
            "output should have category sections"
        );
        assert!(
            output.content.contains("turns:"),
            "output should reference source turns"
        );
    }

    // ── LLM failure returns ExtractionError ──────────────────────────────────

    #[tokio::test]
    async fn extract_returns_error_on_provider_failure() {
        let provider = MockProvider::failing();
        let tokeniser = FixedCostTokeniser::new();
        let compactor = StructuredExtractionCompactor::new();

        let result = compactor
            .extract(&sample_messages(), 10_000, &provider, &tokeniser)
            .await;

        assert!(result.is_err(), "should return error on provider failure");
        let err = result.unwrap_err();
        assert!(
            matches!(err, ExtractionError::Provider(_)),
            "error should be Provider variant"
        );
    }

    // ── Unparseable response returns ParseFailure ────────────────────────────

    #[tokio::test]
    async fn extract_returns_parse_error_on_bad_json() {
        let provider = MockProvider::new("this is not json at all");
        let tokeniser = FixedCostTokeniser::new();
        let compactor = StructuredExtractionCompactor::new();

        let result = compactor
            .extract(&sample_messages(), 10_000, &provider, &tokeniser)
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ExtractionError::ParseFailure(_)),
            "error should be ParseFailure variant"
        );
    }

    // ── Budget enforcement truncates oversized output ────────────────────────

    #[tokio::test]
    async fn extract_truncates_when_output_exceeds_budget() {
        let provider = MockProvider::new(&mock_json_response());
        let tokeniser = FixedCostTokeniser::new();
        let compactor = StructuredExtractionCompactor::new();

        // Tiny budget — the formatted output will exceed this.
        let output = compactor
            .extract(&sample_messages(), 5, &provider, &tokeniser)
            .await
            .unwrap();

        assert!(output.truncated, "output should be marked as truncated");
        assert!(
            output.content.contains("truncated"),
            "content should mention truncation"
        );
    }

    // ── Parse tolerates markdown code fences ─────────────────────────────────

    #[tokio::test]
    async fn extract_handles_code_fenced_response() {
        let json = mock_json_response();
        let fenced = format!("```json\n{json}\n```");
        let provider = MockProvider::new(&fenced);
        let tokeniser = FixedCostTokeniser::new();
        let compactor = StructuredExtractionCompactor::new();

        let output = compactor
            .extract(&sample_messages(), 10_000, &provider, &tokeniser)
            .await
            .unwrap();

        assert_eq!(output.categories.len(), 3);
    }

    // ── Token count is populated ─────────────────────────────────────────────

    #[tokio::test]
    async fn extraction_output_has_nonzero_token_count() {
        let provider = MockProvider::new(&mock_json_response());
        let tokeniser = FixedCostTokeniser::new();
        let compactor = StructuredExtractionCompactor::new();

        let output = compactor
            .extract(&sample_messages(), 10_000, &provider, &tokeniser)
            .await
            .unwrap();

        assert!(
            output.token_count > 0,
            "token count should be positive for non-empty content"
        );
    }

    // ── Unit tests for internal helpers ───────────────────────────────────────

    #[test]
    fn parse_extraction_response_handles_plain_json() {
        let json = mock_json_response();
        let result = parse_extraction_response(&json);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 3);
    }

    #[test]
    fn parse_extraction_response_handles_fenced_json() {
        let json = mock_json_response();
        let fenced = format!("```json\n{json}\n```");
        let result = parse_extraction_response(&fenced);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 3);
    }

    #[test]
    fn parse_extraction_response_rejects_invalid_json() {
        let result = parse_extraction_response("not json");
        assert!(result.is_err());
    }

    #[test]
    fn format_extraction_produces_readable_output() {
        let categories = vec![ExtractionCategory {
            name: "action_items".to_string(),
            items: vec![ExtractionItem {
                content: "Set up CI pipeline".to_string(),
                source_turns: vec![4, 7],
            }],
        }];
        let output = format_extraction(&categories);
        assert!(output.contains("### Action Items"));
        assert!(output.contains("Set up CI pipeline"));
        assert!(output.contains("turns: 4, 7"));
    }

    #[test]
    fn capitalise_category_handles_snake_case() {
        assert_eq!(capitalise_category("action_items"), "Action Items");
        assert_eq!(capitalise_category("key_facts"), "Key Facts");
        assert_eq!(capitalise_category("decisions"), "Decisions");
    }

    #[test]
    fn build_extraction_prompt_numbers_messages() {
        let messages = vec![
            ChatMessage::new(Role::User, "Hello"),
            ChatMessage::new(Role::Assistant, "Hi there"),
        ];
        let prompt = build_extraction_prompt(&messages);
        assert!(prompt.contains("[Turn 0] (user): Hello"));
        assert!(prompt.contains("[Turn 1] (assistant): Hi there"));
    }
}
