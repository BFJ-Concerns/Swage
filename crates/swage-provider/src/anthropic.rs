use std::pin::Pin;

use async_trait::async_trait;
use futures::channel::mpsc;
use futures::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use swage_core::{ChatMessage, LlmRequest, LlmResponse, ProviderError, Role, StreamChunk, ToolCall, ToolResult, TokenUsage};

use crate::http::{HttpClient, SseEvent};
use crate::provider::{LlmProvider, ProviderConfig};

const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

// ---------------------------------------------------------------------------
// Anthropic wire types
// ---------------------------------------------------------------------------

/// Request body for POST /v1/messages.
#[derive(Serialize)]
struct MessagesRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
}

/// Wire representation of a tool definition for the Anthropic API.
#[derive(Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

/// Message content — either plain text or a sequence of typed content blocks.
///
/// Simple messages use `Text`; tool result messages require `Blocks` because
/// Anthropic expects `content: [{ type: "tool_result", ... }]`.
#[derive(Serialize)]
#[serde(untagged)]
enum MessageContent {
    /// Plain text content — serialises as a JSON string.
    Text(String),
    /// Multi-block content — serialises as a JSON array.
    Blocks(Vec<serde_json::Value>),
}

#[derive(Serialize)]
pub(crate) struct AnthropicMessage {
    role: String,
    content: MessageContent,
}

/// Successful response body from POST /v1/messages.
#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
    usage: ApiUsage,
    model: String,
}

#[derive(Deserialize)]
struct ContentBlock {
    text: String,
}

#[derive(Deserialize)]
struct ApiUsage {
    input_tokens: u32,
    output_tokens: u32,
}

// ---------------------------------------------------------------------------
// SSE event payload shapes (streaming)
// ---------------------------------------------------------------------------

/// `content_block_start` event — announces a new content block (text or tool_use).
#[derive(Deserialize)]
struct ContentBlockStartPayload {
    content_block: ContentBlockInfo,
}

/// Block metadata within `content_block_start`.
#[derive(Deserialize)]
struct ContentBlockInfo {
    #[serde(rename = "type")]
    block_type: String,
    /// Present when block_type is "tool_use" — the provider-assigned call ID.
    id: Option<String>,
    /// Present when block_type is "tool_use" — the tool name.
    name: Option<String>,
}

/// `content_block_delta` event — carries a text delta or partial tool input.
#[derive(Deserialize)]
struct ContentBlockDeltaPayload {
    delta: DeltaBlock,
}

#[derive(Deserialize)]
struct DeltaBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
    /// Present for `input_json_delta` — a fragment of the tool's JSON arguments.
    partial_json: Option<String>,
}

/// `message_delta` event — carries output token count.
#[derive(Deserialize)]
struct MessageDeltaPayload {
    usage: Option<MessageDeltaUsage>,
}

#[derive(Deserialize)]
struct MessageDeltaUsage {
    output_tokens: u32,
}

/// `error` event payload.
#[derive(Deserialize)]
struct ErrorPayload {
    error: ErrorDetail,
}

#[derive(Deserialize)]
struct ErrorDetail {
    message: String,
}

// ---------------------------------------------------------------------------
// Tool-use accumulator
// ---------------------------------------------------------------------------

/// Accumulates tool_use content blocks across multiple SSE events.
///
/// Anthropic streams tool calls as a three-event sequence:
///   1. `content_block_start` with `type: "tool_use"` — carries call_id and name
///   2. `content_block_delta` with `type: "input_json_delta"` — partial JSON fragments
///   3. `content_block_stop` — signals the block is complete
///
/// The accumulator gathers the JSON fragments and emits a single
/// `StreamChunk::ToolUse` when the block completes.
struct ToolUseAccumulator {
    /// (call_id, tool_name, accumulated_json)
    pending: Option<(String, String, String)>,
}

impl ToolUseAccumulator {
    fn new() -> Self {
        Self { pending: None }
    }

    /// Process an SSE event. Returns `Some(ToolUse)` when a tool_use block
    /// completes, `None` otherwise.
    fn process_event(&mut self, event: &SseEvent) -> Option<StreamChunk> {
        match event.event_type.as_deref() {
            Some("content_block_start") => {
                let payload: ContentBlockStartPayload =
                    serde_json::from_str(&event.data).ok()?;
                if payload.content_block.block_type == "tool_use" {
                    let call_id = payload.content_block.id.unwrap_or_default();
                    let name = payload.content_block.name.unwrap_or_default();
                    self.pending = Some((call_id, name, String::new()));
                }
                None
            }
            Some("content_block_delta") => {
                if let Some((_, _, ref mut json_buf)) = self.pending {
                    let payload: ContentBlockDeltaPayload =
                        serde_json::from_str(&event.data).ok()?;
                    if payload.delta.block_type == "input_json_delta" {
                        if let Some(fragment) = payload.delta.partial_json {
                            json_buf.push_str(&fragment);
                        }
                    }
                }
                None
            }
            Some("content_block_stop") => {
                if let Some((call_id, name, json_buf)) = self.pending.take() {
                    let arguments = serde_json::from_str(&json_buf)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    Some(StreamChunk::ToolUse { call_id, name, arguments })
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// AnthropicProvider
// ---------------------------------------------------------------------------

/// LlmProvider implementation for the Anthropic Messages API.
pub struct AnthropicProvider {
    config: ProviderConfig,
}

impl AnthropicProvider {
    /// Construct a provider with an explicit API key and model.
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            config: ProviderConfig::new(api_key, model, ANTHROPIC_BASE_URL),
        }
    }

    /// Construct a provider from an already-resolved `ProviderConfig`.
    pub fn from_config(config: ProviderConfig) -> Self {
        Self { config }
    }

    /// Build the default headers required by every Anthropic API request.
    fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        // API key authentication.
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&self.config.api_key).expect("api key must be valid header value"),
        );
        // Protocol version required by Anthropic.
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_API_VERSION),
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers
    }

    fn make_http_client(&self) -> HttpClient {
        HttpClient::new(self.config.base_url.clone(), self.build_headers())
    }

    /// Build the two-message follow-up sequence required after tool execution.
    ///
    /// Anthropic's protocol requires the conversation history to include:
    ///   1. An assistant message reconstructing what the model produced — text
    ///      (if any preceded the tool calls) and the `tool_use` content blocks.
    ///   2. A user message containing `tool_result` blocks, one per call,
    ///      matched by `tool_use_id`.
    ///
    /// These two messages are appended to the conversation history before the
    /// next streaming request so the model can see its own tool invocations and
    /// their results.
    pub(crate) fn build_tool_result_messages(
        tool_calls: &[ToolCall],
        tool_results: &[ToolResult],
        assistant_text: &str,
    ) -> Vec<AnthropicMessage> {
        // Build assistant content blocks.
        let mut assistant_blocks: Vec<serde_json::Value> = Vec::new();

        // Prepend any text the model produced before the tool calls.
        if !assistant_text.is_empty() {
            assistant_blocks.push(serde_json::json!({
                "type": "text",
                "text": assistant_text,
            }));
        }

        // One tool_use block per tool call.
        for call in tool_calls {
            assistant_blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": call.call_id,
                "name": call.name,
                "input": call.arguments,
            }));
        }

        let assistant_message = AnthropicMessage {
            role: "assistant".into(),
            content: MessageContent::Blocks(assistant_blocks),
        };

        // Build user content blocks — one tool_result per result.
        let user_blocks: Vec<serde_json::Value> = tool_results
            .iter()
            .map(|result| {
                let mut block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": result.call_id,
                    "content": result.content,
                });
                // Only include is_error when true — omitting it keeps the wire
                // format clean for the common success case.
                if result.is_error {
                    block["is_error"] = serde_json::Value::Bool(true);
                }
                block
            })
            .collect();

        let user_message = AnthropicMessage {
            role: "user".into(),
            content: MessageContent::Blocks(user_blocks),
        };

        vec![assistant_message, user_message]
    }

    fn build_messages_request(&self, request: &LlmRequest, stream: bool) -> MessagesRequest {
        // Convert regular conversation messages (skip system — it goes in the top-level field).
        let mut messages: Vec<AnthropicMessage> = request
            .messages
            .iter()
            .filter(|m| m.role != swage_core::Role::System)
            .map(|m| {
                // Tool messages carry structured content_blocks (tool_use / tool_result).
                // Plain text messages use the simple Text variant.
                let content = match &m.content_blocks {
                    Some(blocks) => MessageContent::Blocks(blocks.clone()),
                    None => {
                        // Extract plain text from ContentPart::Text entries.
                        // Multi-modal messages (with images) are not currently
                        // supported in this code path; image parts are dropped.
                        let text = m.content.iter()
                            .filter_map(|p| {
                                if let swage_core::ContentPart::Text { text } = p {
                                    Some(text.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("");
                        MessageContent::Text(text)
                    }
                };
                AnthropicMessage {
                    role: m.role.to_string(),
                    content,
                }
            })
            .collect();

        // Append tool-use round history.  Each round becomes an assistant message
        // (reconstructing the tool_use blocks) followed by a user message carrying
        // the tool_result blocks.  This gives the model the context it needs to
        // produce a coherent response after seeing all its tool results.
        for round in &request.tool_history {
            let round_messages = Self::build_tool_result_messages(
                &round.tool_calls,
                &round.tool_results,
                &round.assistant_text,
            );
            messages.extend(round_messages);
        }

        let tools = request
            .tools
            .iter()
            .map(|t| AnthropicTool {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect();

        MessagesRequest {
            model: self.config.model.clone(),
            messages,
            max_tokens: request.max_tokens.unwrap_or(4096),
            system: request.system_prompt.clone(),
            stream: if stream { Some(true) } else { None },
            tools,
        }
    }

    /// Parse an Anthropic SSE event into a `StreamChunk`.
    ///
    /// Returns `None` for event types that don't map to visible stream output
    /// (e.g. `message_start`, `content_block_start`). This keeps the stream
    /// forward-compatible with new event types Anthropic may add.
    pub(crate) fn parse_sse_event(event: &SseEvent) -> Option<StreamChunk> {
        match event.event_type.as_deref() {
            Some("content_block_delta") => {
                let payload: ContentBlockDeltaPayload =
                    serde_json::from_str(&event.data).ok()?;
                if payload.delta.block_type == "text_delta" {
                    Some(StreamChunk::Delta {
                        content: payload.delta.text.unwrap_or_default(),
                    })
                } else {
                    None
                }
            }
            Some("message_delta") => {
                let payload: MessageDeltaPayload =
                    serde_json::from_str(&event.data).ok()?;
                let output_tokens =
                    payload.usage.map(|u| u.output_tokens).unwrap_or(0);
                // input_tokens are reported in message_start; we default to 0 here
                // since the full usage summary comes separately.
                Some(StreamChunk::Usage(TokenUsage {
                    input_tokens: 0,
                    output_tokens,
                }))
            }
            Some("message_stop") => Some(StreamChunk::Done { finish_reason: None }),
            Some("error") => {
                let payload: ErrorPayload = serde_json::from_str(&event.data).ok()?;
                Some(StreamChunk::Error {
                    message: payload.error.message,
                })
            }
            // Unrecognised or informational events (message_start, content_block_start…).
            _ => None,
        }
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn send_message(&self, request: LlmRequest) -> Result<LlmResponse, ProviderError> {
        let body = self.build_messages_request(&request, false);
        let client = self.make_http_client();

        let response: MessagesResponse =
            client.post_json("/v1/messages", &body).await?;

        let text = response
            .content
            .into_iter()
            .map(|b| b.text)
            .collect::<Vec<_>>()
            .join("");

        Ok(LlmResponse {
            message: ChatMessage::new(Role::Assistant, text),
            usage: TokenUsage {
                input_tokens: response.usage.input_tokens,
                output_tokens: response.usage.output_tokens,
            },
            model: response.model,
        })
    }

    fn stream_message(
        &self,
        request: LlmRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        // stream_message is synchronous, so we spawn a task that drives the
        // async SSE stream and forwards chunks to the caller via a channel.
        let (tx, rx) = mpsc::unbounded();
        let body = self.build_messages_request(&request, true);
        let client = self.make_http_client();

        tokio::spawn(async move {
            let sse_stream = match client
                .post_stream("/v1/messages", &body, HeaderMap::new())
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.unbounded_send(StreamChunk::Error {
                        message: e.to_string(),
                    });
                    return;
                }
            };

            tokio::pin!(sse_stream);
            let mut tool_acc = ToolUseAccumulator::new();

            while let Some(result) = sse_stream.next().await {
                let event = match result {
                    Err(e) => {
                        let _ = tx.unbounded_send(StreamChunk::Error {
                            message: e.to_string(),
                        });
                        break;
                    }
                    Ok(ev) => ev,
                };

                // Tool_use blocks span multiple events — the accumulator
                // gathers them and emits a single ToolUse chunk on completion.
                if let Some(chunk) = tool_acc.process_event(&event) {
                    let _ = tx.unbounded_send(chunk);
                    continue;
                }

                if let Some(chunk) = AnthropicProvider::parse_sse_event(&event) {
                    let is_done = matches!(chunk, StreamChunk::Done { .. });
                    let _ = tx.unbounded_send(chunk);
                    if is_done {
                        break;
                    }
                }
            }
        });

        Box::pin(rx)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event_type: &str, data: &str) -> SseEvent {
        SseEvent {
            event_type: Some(event_type.to_owned()),
            data: data.to_owned(),
        }
    }

    // --- parse_sse_event ---

    #[test]
    fn parse_content_block_delta_text() {
        let ev = event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#,
        );
        let chunk = AnthropicProvider::parse_sse_event(&ev).unwrap();
        assert!(matches!(chunk, StreamChunk::Delta { ref content } if content == "hello"));
    }

    #[test]
    fn parse_content_block_delta_non_text_returns_none() {
        // Non-text deltas (e.g. input_json_delta for tool use) should be ignored.
        let ev = event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
        );
        assert!(AnthropicProvider::parse_sse_event(&ev).is_none());
    }

    #[test]
    fn parse_message_delta_with_usage() {
        let ev = event(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":42}}"#,
        );
        let chunk = AnthropicProvider::parse_sse_event(&ev).unwrap();
        assert!(matches!(
            chunk,
            StreamChunk::Usage(TokenUsage { output_tokens: 42, .. })
        ));
    }

    #[test]
    fn parse_message_stop_emits_done() {
        let ev = event("message_stop", r#"{"type":"message_stop"}"#);
        let chunk = AnthropicProvider::parse_sse_event(&ev).unwrap();
        assert!(matches!(chunk, StreamChunk::Done { .. }));
    }

    #[test]
    fn parse_error_event() {
        let ev = event(
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Server overloaded"}}"#,
        );
        let chunk = AnthropicProvider::parse_sse_event(&ev).unwrap();
        assert!(
            matches!(chunk, StreamChunk::Error { ref message } if message.contains("overloaded"))
        );
    }

    #[test]
    fn parse_unknown_event_type_returns_none() {
        // message_start, content_block_start are informational and not surfaced.
        let ev = event("message_start", r#"{"type":"message_start"}"#);
        assert!(AnthropicProvider::parse_sse_event(&ev).is_none());
    }

    #[test]
    fn parse_event_without_event_type_returns_none() {
        let ev = SseEvent { event_type: None, data: "data".to_owned() };
        assert!(AnthropicProvider::parse_sse_event(&ev).is_none());
    }

    // --- request building ---

    #[test]
    fn build_request_extracts_system_prompt() {
        let provider = AnthropicProvider::new("key", "model");
        let req = LlmRequest::builder("model")
            .system_prompt("be helpful")
            .build();

        let body = provider.build_messages_request(&req, false);
        assert_eq!(body.system.as_deref(), Some("be helpful"));
    }

    #[test]
    fn build_request_excludes_system_role_messages() {
        // Anthropic uses a top-level system field; system-role messages must not
        // appear in the messages array.
        use swage_core::{ChatMessage, Role};
        let provider = AnthropicProvider::new("key", "model");
        let req = LlmRequest::builder("model")
            .messages(vec![
                ChatMessage::new(Role::System, "instructions"),
                ChatMessage::new(Role::User, "hello"),
            ])
            .build();

        let body = provider.build_messages_request(&req, false);
        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.messages[0].role, "user");
    }

    #[test]
    fn build_streaming_request_sets_stream_true() {
        let provider = AnthropicProvider::new("key", "model");
        let req = LlmRequest::builder("model").build();
        let body = provider.build_messages_request(&req, true);
        assert_eq!(body.stream, Some(true));
    }

    #[test]
    fn build_non_streaming_request_omits_stream_field() {
        let provider = AnthropicProvider::new("key", "model");
        let req = LlmRequest::builder("model").build();
        let body = provider.build_messages_request(&req, false);
        assert!(body.stream.is_none());
    }

    // --- tool definitions in wire request ---

    #[test]
    fn build_request_with_tools_includes_tools_array() {
        use swage_core::ToolDefinition;

        let provider = AnthropicProvider::new("key", "model");
        let tool = ToolDefinition {
            name: "bash".into(),
            description: "Run a shell command".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "command": { "type": "string" } },
                "required": ["command"]
            }),
        };
        let req = LlmRequest::builder("model")
            .tools(vec![tool])
            .build();

        let body = provider.build_messages_request(&req, false);
        let json = serde_json::to_value(&body).unwrap();

        // tools array must be present with one entry.
        let tools = json["tools"].as_array().expect("tools should be an array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "bash");
        assert_eq!(tools[0]["description"], "Run a shell command");
        assert!(tools[0]["input_schema"]["properties"]["command"].is_object());
    }

    #[test]
    fn build_request_without_tools_omits_tools_field() {
        let provider = AnthropicProvider::new("key", "model");
        let req = LlmRequest::builder("model").build();
        let body = provider.build_messages_request(&req, false);
        let json = serde_json::to_value(&body).unwrap();

        // When no tools provided, the tools field must be omitted entirely
        // (not serialised as an empty array).
        assert!(json.get("tools").is_none());
    }

    #[test]
    fn anthropic_tool_serialises_with_correct_field_names() {
        let tool = AnthropicTool {
            name: "read_file".into(),
            description: "Read file contents".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let json = serde_json::to_value(&tool).unwrap();
        assert_eq!(json["name"], "read_file");
        assert_eq!(json["description"], "Read file contents");
        assert_eq!(json["input_schema"]["type"], "object");
    }

    #[test]
    fn build_request_maps_multiple_tools() {
        use swage_core::ToolDefinition;

        let provider = AnthropicProvider::new("key", "model");
        let tools = vec![
            ToolDefinition {
                name: "bash".into(),
                description: "Execute shell commands".into(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            ToolDefinition {
                name: "read_file".into(),
                description: "Read a file".into(),
                input_schema: serde_json::json!({"type": "object"}),
            },
        ];
        let req = LlmRequest::builder("model").tools(tools).build();
        let body = provider.build_messages_request(&req, false);

        let json = serde_json::to_value(&body).unwrap();
        let wire_tools = json["tools"].as_array().unwrap();
        assert_eq!(wire_tools.len(), 2);
        assert_eq!(wire_tools[0]["name"], "bash");
        assert_eq!(wire_tools[1]["name"], "read_file");
    }

    // --- build_tool_result_messages ---

    #[test]
    fn build_tool_result_messages_produces_two_messages() {
        let tool_calls = vec![ToolCall {
            call_id: "toolu_01".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "ls" }),
        }];
        let tool_results = vec![ToolResult {
            call_id: "toolu_01".into(),
            content: "file1.txt\nfile2.txt".into(),
            is_error: false,
        }];

        let messages = AnthropicProvider::build_tool_result_messages(
            &tool_calls,
            &tool_results,
            "",
        );

        // Must produce exactly two messages: assistant (with tool_use) + user (with tool_result).
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "assistant");
        assert_eq!(messages[1].role, "user");
    }

    #[test]
    fn build_tool_result_messages_assistant_contains_tool_use_blocks() {
        let tool_calls = vec![ToolCall {
            call_id: "toolu_01".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "ls" }),
        }];
        let tool_results = vec![ToolResult {
            call_id: "toolu_01".into(),
            content: "output".into(),
            is_error: false,
        }];

        let messages = AnthropicProvider::build_tool_result_messages(&tool_calls, &tool_results, "");
        let json = serde_json::to_value(&messages[0]).unwrap();

        let blocks = json["content"].as_array().expect("assistant content should be array");
        // Tool_use block must appear with Anthropic's required fields.
        let tool_use = blocks.iter().find(|b| b["type"] == "tool_use")
            .expect("must have a tool_use block");
        assert_eq!(tool_use["id"], "toolu_01");
        assert_eq!(tool_use["name"], "bash");
        assert_eq!(tool_use["input"]["command"], "ls");
    }

    #[test]
    fn build_tool_result_messages_user_contains_tool_result_blocks() {
        let tool_calls = vec![ToolCall {
            call_id: "toolu_01".into(),
            name: "bash".into(),
            arguments: serde_json::json!({}),
        }];
        let tool_results = vec![ToolResult {
            call_id: "toolu_01".into(),
            content: "file1.txt".into(),
            is_error: false,
        }];

        let messages = AnthropicProvider::build_tool_result_messages(&tool_calls, &tool_results, "");
        let json = serde_json::to_value(&messages[1]).unwrap();

        let blocks = json["content"].as_array().expect("user content should be array");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_result");
        // Anthropic uses tool_use_id to match result to call.
        assert_eq!(blocks[0]["tool_use_id"], "toolu_01");
        assert_eq!(blocks[0]["content"], "file1.txt");
        // is_error should not be set (false is the default)
        assert!(blocks[0].get("is_error").is_none() || blocks[0]["is_error"] == false);
    }

    #[test]
    fn build_tool_result_messages_error_result_sets_is_error() {
        let tool_calls = vec![ToolCall {
            call_id: "toolu_02".into(),
            name: "bash".into(),
            arguments: serde_json::json!({}),
        }];
        let tool_results = vec![ToolResult {
            call_id: "toolu_02".into(),
            content: "command not found".into(),
            is_error: true,
        }];

        let messages = AnthropicProvider::build_tool_result_messages(&tool_calls, &tool_results, "");
        let json = serde_json::to_value(&messages[1]).unwrap();
        let blocks = json["content"].as_array().unwrap();
        assert_eq!(blocks[0]["is_error"], true);
    }

    #[test]
    fn build_tool_result_messages_includes_text_in_assistant_message() {
        // When the model produced text before the tool calls, that text must
        // appear as a text content block in the assistant message alongside the
        // tool_use blocks. Anthropic requires this for context continuity.
        let tool_calls = vec![ToolCall {
            call_id: "toolu_01".into(),
            name: "bash".into(),
            arguments: serde_json::json!({}),
        }];
        let tool_results = vec![ToolResult {
            call_id: "toolu_01".into(),
            content: "output".into(),
            is_error: false,
        }];

        let messages = AnthropicProvider::build_tool_result_messages(
            &tool_calls,
            &tool_results,
            "Let me run that for you.",
        );
        let json = serde_json::to_value(&messages[0]).unwrap();
        let blocks = json["content"].as_array().unwrap();

        // Must contain both a text block and a tool_use block.
        let text_block = blocks.iter().find(|b| b["type"] == "text")
            .expect("must include text block when assistant_text is non-empty");
        assert_eq!(text_block["text"], "Let me run that for you.");
        assert!(blocks.iter().any(|b| b["type"] == "tool_use"));
    }

    #[test]
    fn build_tool_result_messages_multiple_tool_calls() {
        // Multiple tool calls in one turn must each get their own tool_use and
        // tool_result block, matched by call_id.
        let tool_calls = vec![
            ToolCall { call_id: "toolu_01".into(), name: "bash".into(), arguments: serde_json::json!({}) },
            ToolCall { call_id: "toolu_02".into(), name: "read_file".into(), arguments: serde_json::json!({}) },
        ];
        let tool_results = vec![
            ToolResult { call_id: "toolu_01".into(), content: "result1".into(), is_error: false },
            ToolResult { call_id: "toolu_02".into(), content: "result2".into(), is_error: false },
        ];

        let messages = AnthropicProvider::build_tool_result_messages(&tool_calls, &tool_results, "");
        let json_asst = serde_json::to_value(&messages[0]).unwrap();
        let json_user = serde_json::to_value(&messages[1]).unwrap();

        let asst_blocks = json_asst["content"].as_array().unwrap();
        let user_blocks = json_user["content"].as_array().unwrap();
        assert_eq!(asst_blocks.iter().filter(|b| b["type"] == "tool_use").count(), 2);
        assert_eq!(user_blocks.iter().filter(|b| b["type"] == "tool_result").count(), 2);
    }

    // --- multi-block content for tool results ---

    #[test]
    fn anthropic_message_with_string_content_serialises_as_string() {
        let msg = AnthropicMessage {
            role: "user".into(),
            content: MessageContent::Text("hello".into()),
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["content"], "hello");
    }

    #[test]
    fn anthropic_message_with_blocks_serialises_as_array() {
        let msg = AnthropicMessage {
            role: "user".into(),
            content: MessageContent::Blocks(vec![
                serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": "toolu_01",
                    "content": "command output"
                }),
            ]),
        };
        let json = serde_json::to_value(&msg).unwrap();
        let blocks = json["content"].as_array().expect("content should be array");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "toolu_01");
    }

    // --- tool_use accumulator ---

    #[test]
    fn accumulator_emits_tool_use_on_complete_lifecycle() {
        // Simulates the 3-event tool_use sequence: start → delta(s) → stop.
        let mut acc = ToolUseAccumulator::new();

        let start = event(
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01abc","name":"bash"}}"#,
        );
        assert!(acc.process_event(&start).is_none());

        let delta1 = event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"com"}}"#,
        );
        assert!(acc.process_event(&delta1).is_none());

        let delta2 = event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"mand\":\"ls\"}"}}"#,
        );
        assert!(acc.process_event(&delta2).is_none());

        let stop = event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":1}"#,
        );
        let chunk = acc.process_event(&stop).expect("should emit ToolUse on stop");
        match chunk {
            StreamChunk::ToolUse { call_id, name, arguments } => {
                assert_eq!(call_id, "toolu_01abc");
                assert_eq!(name, "bash");
                assert_eq!(arguments, serde_json::json!({"command": "ls"}));
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn accumulator_ignores_text_block_start() {
        // A text content_block_start should not activate tool accumulation.
        let mut acc = ToolUseAccumulator::new();

        let start = event(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        );
        assert!(acc.process_event(&start).is_none());

        // content_block_stop without pending tool should return None.
        let stop = event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        );
        assert!(acc.process_event(&stop).is_none());
    }

    #[test]
    fn accumulator_ignores_unrelated_events() {
        let mut acc = ToolUseAccumulator::new();
        let ev = event("message_start", r#"{"type":"message_start"}"#);
        assert!(acc.process_event(&ev).is_none());
    }

    #[test]
    fn accumulator_handles_empty_arguments() {
        let mut acc = ToolUseAccumulator::new();

        let start = event(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_02","name":"list_files"}}"#,
        );
        acc.process_event(&start);

        // Single delta with the complete JSON.
        let delta = event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
        );
        acc.process_event(&delta);

        let stop = event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        );
        let chunk = acc.process_event(&stop).unwrap();
        match chunk {
            StreamChunk::ToolUse { arguments, .. } => {
                assert_eq!(arguments, serde_json::json!({}));
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn accumulator_resets_after_emitting() {
        let mut acc = ToolUseAccumulator::new();

        // First tool call.
        acc.process_event(&event(
            "content_block_start",
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_01","name":"bash"}}"#,
        ));
        acc.process_event(&event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
        ));
        let first = acc.process_event(&event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ));
        assert!(first.is_some());

        // Second tool call — accumulator should handle it independently.
        acc.process_event(&event(
            "content_block_start",
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_02","name":"read_file"}}"#,
        ));
        acc.process_event(&event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"/tmp\"}"}}"#,
        ));
        let second = acc.process_event(&event(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":1}"#,
        )).unwrap();
        match second {
            StreamChunk::ToolUse { call_id, name, .. } => {
                assert_eq!(call_id, "toolu_02");
                assert_eq!(name, "read_file");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }
}
