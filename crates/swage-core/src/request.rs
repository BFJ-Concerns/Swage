//! Request and response types for LLM completions.
//!
//! Two complementary APIs coexist here:
//! - `LlmRequest`/`LlmResponse` with builder pattern and streaming, kept for
//!   legacy provider entry points.
//! - `CompletionRequest`/`CompletionResponse` and
//!   `ToolCompletionRequest`/`ToolCompletionResponse` — lower-level types
//!   used by active gateway and provider paths.

use serde::{Deserialize, Serialize};

use crate::message::ChatMessage;
use crate::tool::{ToolCall, ToolDefinition};

// ---------------------------------------------------------------------------
// LLM request / response (primary API)
// ---------------------------------------------------------------------------

/// Request to an LLM provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub system_prompt: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub stream: bool,
    /// Tool definitions available to the model. Empty vec means no tools.
    pub tools: Vec<ToolDefinition>,
}

impl LlmRequest {
    /// Start building an LlmRequest with defaults (stream=true, max_tokens=4096).
    pub fn builder(model: impl Into<String>) -> LlmRequestBuilder {
        LlmRequestBuilder {
            model: model.into(),
            messages: Vec::new(),
            system_prompt: None,
            max_tokens: Some(4096),
            temperature: None,
            stream: true,
            tools: Vec::new(),
        }
    }
}

/// Builder for `LlmRequest`.
pub struct LlmRequestBuilder {
    model: String,
    messages: Vec<ChatMessage>,
    system_prompt: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    stream: bool,
    tools: Vec<ToolDefinition>,
}

impl LlmRequestBuilder {
    pub fn messages(mut self, messages: Vec<ChatMessage>) -> Self {
        self.messages = messages;
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = Some(n);
        self
    }

    pub fn temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }

    pub fn stream(mut self, s: bool) -> Self {
        self.stream = s;
        self
    }

    pub fn tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn build(self) -> LlmRequest {
        LlmRequest {
            model: self.model,
            messages: self.messages,
            system_prompt: self.system_prompt,
            max_tokens: self.max_tokens,
            temperature: self.temperature,
            stream: self.stream,
            tools: self.tools,
        }
    }
}

/// Token usage statistics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Content-free evidence describing one provider streaming response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderResponseWitness {
    pub http_status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    pub usage: TokenUsage,
    #[serde(default)]
    pub content_blocks: Vec<ProviderContentBlockWitness>,
}

/// Shape and size of provider content blocks without retaining their content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderContentBlockWitness {
    pub block_type: String,
    pub count: u32,
    pub bytes: u64,
}

/// Complete response from an LLM provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmResponse {
    pub message: ChatMessage,
    pub usage: TokenUsage,
    pub model: String,
}

/// A chunk from a streaming LLM response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StreamChunk {
    /// A piece of the assistant's response text.
    Delta { content: String },
    /// Provider reasoning side-channel content.
    ///
    /// This is not visible assistant text and must not be merged into the
    /// response transcript unless a caller explicitly opts into reasoning
    /// replay.
    Reasoning {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
    },
    /// Token usage for the request (sent with or after Done).
    Usage(TokenUsage),
    /// Redacted response metadata retained for audit and diagnosis.
    ProviderResponseWitness(ProviderResponseWitness),
    /// Stream complete — no more chunks will follow.
    Done {
        /// Reason the model stopped, if the provider reported one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<FinishReason>,
    },
    /// A provider retry is waiting before the next attempt.
    ProviderRetryStarted {
        /// 1-indexed retry attempt number.
        attempt: u32,
        /// Maximum configured retry attempts.
        max_retries: u32,
        /// Provider or model label available at the retry boundary.
        provider: String,
        /// Wait duration before the next attempt starts.
        wait_ms: u64,
    },
    /// A provider retry wait has settled and streaming is resuming or failing.
    ProviderRetrySettled {
        /// 1-indexed retry attempt number.
        attempt: u32,
        /// Maximum configured retry attempts.
        max_retries: u32,
        /// Provider or model label available at the retry boundary.
        provider: String,
        /// Whether a later attempt produced a usable stream.
        success: bool,
    },
    /// A provider-side error terminated the stream.
    Error { message: String },
    /// A completed provider response contained no usable model output.
    ProviderAnomaly { message: String },
    /// The model is invoking a tool. The turn processor uses this to trigger
    /// execution.
    ToolUse {
        /// Provider-assigned call ID; correlates with the ToolResult returned
        /// to the model.
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
}

// ---------------------------------------------------------------------------
// Completion request / response (lower-level API)
// ---------------------------------------------------------------------------

/// A completion request sent to an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// Conversation messages forming the prompt.
    pub messages: Vec<ChatMessage>,
    /// Model identifier (provider-specific). When `None`, the provider's
    /// default model is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Maximum number of tokens to generate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature (0.0–2.0 typical).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Sequences that cause the model to stop generating.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    /// Arbitrary provider-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Tool definitions available to the model during streaming or text turns.
    ///
    /// Empty means the provider should make a plain text request with no
    /// function-calling schema attached.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_definitions: Vec<serde_json::Value>,
}

/// A completion response from an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// Generated text content.
    pub content: String,
    /// Number of tokens in the input prompt.
    pub input_tokens: u32,
    /// Number of tokens in the generated output.
    pub output_tokens: u32,
    /// Reason the model stopped generating.
    pub finish_reason: FinishReason,
    /// Estimated cost in USD, if the provider reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

/// A completion request that includes tool definitions for function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCompletionRequest {
    /// Conversation messages forming the prompt.
    pub messages: Vec<ChatMessage>,
    /// Model identifier (provider-specific).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Maximum number of tokens to generate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Sequences that cause the model to stop generating.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    /// Arbitrary provider-specific metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// JSON Schema definitions for tools the model may invoke.
    pub tool_definitions: Vec<serde_json::Value>,
}

/// A completion response that may include tool call requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCompletionResponse {
    /// Generated text content (may be empty when the model only issues tool calls).
    pub content: String,
    /// Number of tokens in the input prompt.
    pub input_tokens: u32,
    /// Number of tokens in the generated output.
    pub output_tokens: u32,
    /// Reason the model stopped generating.
    pub finish_reason: FinishReason,
    /// Estimated cost in USD, if the provider reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    /// Tool calls requested by the model.
    pub tool_calls: Vec<ToolCall>,
}

/// Reason the model stopped generating tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Natural end of response.
    Stop,
    /// Hit the max_tokens limit.
    Length,
    /// Model chose to invoke a tool.
    ToolUse,
    /// Content was filtered by safety systems.
    ContentFilter,
    /// Reason not recognised or not provided.
    Unknown,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ContentPart, Role};

    // -- LlmRequest builder ---------------------------------------------------

    #[test]
    fn builder_defaults() {
        // Default builder must set stream=true and max_tokens=4096.
        let req = LlmRequest::builder("gpt-4o").build();
        assert_eq!(req.model, "gpt-4o");
        assert!(req.stream);
        assert_eq!(req.max_tokens, Some(4096));
        assert!(req.messages.is_empty());
        assert!(req.system_prompt.is_none());
        assert!(req.temperature.is_none());
    }

    #[test]
    fn builder_setters() {
        let msg = ChatMessage::new(Role::User, "hi");
        let req = LlmRequest::builder("claude-3")
            .messages(vec![msg.clone()])
            .system_prompt("be helpful")
            .max_tokens(512)
            .temperature(0.7)
            .stream(false)
            .build();

        assert_eq!(req.messages, vec![msg]);
        assert_eq!(req.system_prompt, Some("be helpful".into()));
        assert_eq!(req.max_tokens, Some(512));
        assert!((req.temperature.unwrap() - 0.7).abs() < 1e-6);
        assert!(!req.stream);
    }

    #[test]
    fn llm_request_serde_round_trip() {
        let req = LlmRequest::builder("model-x")
            .system_prompt("think")
            .build();
        let json = serde_json::to_string(&req).unwrap();
        let decoded: LlmRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn token_usage_serde_round_trip() {
        let usage = TokenUsage {
            input_tokens: 10,
            output_tokens: 20,
        };
        let json = serde_json::to_string(&usage).unwrap();
        let decoded: TokenUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(usage, decoded);
    }

    #[test]
    fn stream_chunk_serde_round_trip() {
        let chunks = vec![
            StreamChunk::Delta {
                content: "hello".into(),
            },
            StreamChunk::Reasoning {
                content: "private chain".into(),
                details: Some(serde_json::json!([{"type": "summary"}])),
            },
            StreamChunk::Usage(TokenUsage {
                input_tokens: 5,
                output_tokens: 3,
            }),
            StreamChunk::Done {
                finish_reason: None,
            },
            StreamChunk::Error {
                message: "oops".into(),
            },
        ];
        for chunk in chunks {
            let json = serde_json::to_string(&chunk).unwrap();
            let decoded: StreamChunk = serde_json::from_str(&json).unwrap();
            assert_eq!(chunk, decoded);
        }
    }

    #[test]
    fn stream_chunk_done_with_finish_reason_roundtrips() {
        let chunk = StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        };
        let json = serde_json::to_string(&chunk).unwrap();
        let decoded: StreamChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(chunk, decoded);
    }

    #[test]
    fn llm_request_tools_defaults_to_empty() {
        // tools must default to an empty vec — backward compatible with callers
        // that don't supply tool definitions.
        let req = LlmRequest::builder("claude-3").build();
        assert!(req.tools.is_empty());
    }

    #[test]
    fn llm_request_tools_builder_method() {
        let tool = ToolDefinition {
            name: "bash".into(),
            description: "Run a shell command".into(),
            input_schema: serde_json::json!({ "type": "object" }),
        };
        let req = LlmRequest::builder("claude-3").tools(vec![tool]).build();
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].name, "bash");
    }

    #[test]
    fn llm_request_with_tools_serde_round_trip() {
        let tool = ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } }
            }),
        };
        let req = LlmRequest::builder("model-x").tools(vec![tool]).build();
        let json = serde_json::to_string(&req).unwrap();
        // tools field must appear in the serialised form when non-empty
        assert!(json.contains("read_file"));
        let decoded: LlmRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn stream_chunk_tool_use_serde_round_trip() {
        // Provider emits ToolUse when the model decides to call a tool.
        let chunk = StreamChunk::ToolUse {
            call_id: "call_abc123".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "echo hello" }),
        };
        let json = serde_json::to_string(&chunk).unwrap();
        let decoded: StreamChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(chunk, decoded);
    }

    // -- CompletionRequest ----------------------------------------------------

    #[test]
    fn completion_request_roundtrips_through_json() {
        let request = CompletionRequest {
            messages: vec![ChatMessage::new_parts(
                Role::User,
                vec![ContentPart::Text {
                    text: "Hello".into(),
                }],
            )],
            model: Some("gpt-4".into()),
            max_tokens: Some(1024),
            temperature: Some(0.7),
            stop_sequences: Some(vec!["STOP".into()]),
            metadata: Some(serde_json::json!({"stream": true})),
            tool_definitions: vec![],
        };

        let json = serde_json::to_string(&request).unwrap();
        let restored: CompletionRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.messages.len(), 1);
        assert_eq!(restored.model, Some("gpt-4".into()));
        assert_eq!(restored.max_tokens, Some(1024));
    }

    #[test]
    fn completion_request_with_all_none_fields() {
        let request = CompletionRequest {
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("model"));
        assert!(!json.contains("max_tokens"));

        let restored: CompletionRequest = serde_json::from_str(&json).unwrap();
        assert!(restored.model.is_none());
    }

    // -- CompletionResponse ---------------------------------------------------

    #[test]
    fn completion_response_roundtrips_through_json() {
        let response = CompletionResponse {
            content: "Hello back!".into(),
            input_tokens: 10,
            output_tokens: 5,
            finish_reason: FinishReason::Stop,
            cost: Some(0.001),
        };

        let json = serde_json::to_string(&response).unwrap();
        let restored: CompletionResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.content, "Hello back!");
        assert_eq!(restored.input_tokens, 10);
        assert_eq!(restored.output_tokens, 5);
        assert_eq!(restored.finish_reason, FinishReason::Stop);
        assert_eq!(restored.cost, Some(0.001));
    }

    #[test]
    fn completion_response_without_cost() {
        let response = CompletionResponse {
            content: "answer".into(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Length,
            cost: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("cost"));
    }

    // -- ToolCompletionRequest ------------------------------------------------

    #[test]
    fn tool_completion_request_roundtrips_through_json() {
        let request = ToolCompletionRequest {
            messages: vec![ChatMessage::new_parts(
                Role::User,
                vec![ContentPart::Text {
                    text: "What's the weather?".into(),
                }],
            )],
            model: Some("gpt-4".into()),
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![serde_json::json!({
                "name": "get_weather",
                "description": "Get weather for a location",
                "parameters": {
                    "type": "object",
                    "properties": { "location": { "type": "string" } }
                }
            })],
        };

        let json = serde_json::to_string(&request).unwrap();
        let restored: ToolCompletionRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.tool_definitions.len(), 1);
        assert_eq!(restored.messages.len(), 1);
    }

    // -- ToolCompletionResponse -----------------------------------------------

    #[test]
    fn tool_completion_response_roundtrips_through_json() {
        let response = ToolCompletionResponse {
            content: String::new(),
            input_tokens: 50,
            output_tokens: 20,
            finish_reason: FinishReason::ToolUse,
            cost: None,
            tool_calls: vec![ToolCall {
                call_id: "call_abc".into(),
                name: "get_weather".into(),
                arguments: serde_json::json!({"location": "London"}),
            }],
        };

        let json = serde_json::to_string(&response).unwrap();
        let restored: ToolCompletionResponse = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.tool_calls.len(), 1);
        assert_eq!(restored.tool_calls[0].name, "get_weather");
        assert_eq!(restored.finish_reason, FinishReason::ToolUse);
    }

    // -- FinishReason ---------------------------------------------------------

    #[test]
    fn finish_reason_all_variants_roundtrip() {
        let variants = [
            FinishReason::Stop,
            FinishReason::Length,
            FinishReason::ToolUse,
            FinishReason::ContentFilter,
            FinishReason::Unknown,
        ];

        for reason in &variants {
            let json = serde_json::to_string(reason).unwrap();
            let restored: FinishReason = serde_json::from_str(&json).unwrap();
            assert_eq!(*reason, restored);
        }
    }

    #[test]
    fn finish_reason_serialises_snake_case() {
        assert_eq!(
            serde_json::to_string(&FinishReason::ToolUse).unwrap(),
            "\"tool_use\""
        );
        assert_eq!(
            serde_json::to_string(&FinishReason::ContentFilter).unwrap(),
            "\"content_filter\""
        );
    }
}
