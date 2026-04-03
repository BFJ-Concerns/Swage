//! OpenAI-compatible LLM provider.
//!
//! Handles any endpoint speaking the OpenAI chat completions format — OpenAI
//! itself, Azure OpenAI, Ollama, or custom endpoints — via configurable
//! `base_url`.

use std::pin::Pin;

use async_trait::async_trait;
use futures::channel::mpsc;
use futures::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use swage_core::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart, FinishReason, ProviderError,
    Role, StreamChunk, ToolCall, ToolCompletionRequest, ToolCompletionResponse,
};

use crate::http::{HttpClient, SseEvent};
use crate::provider::{LlmProvider, ProviderConfig};

// ---------------------------------------------------------------------------
// OpenAI wire types
// ---------------------------------------------------------------------------

/// Request body for POST /chat/completions.
#[derive(Serialize)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool>,
}

/// Wire representation of a message in the OpenAI format.
#[derive(Serialize, Deserialize, Debug)]
struct OpenAiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

/// Wire representation of a tool call from the OpenAI response.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct OpenAiToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OpenAiFunctionCall,
}

/// The function name and arguments within a tool call.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct OpenAiFunctionCall {
    name: String,
    /// Arguments as a JSON string — we normalise to structured JSON on parse.
    arguments: String,
}

/// Wire representation of a tool definition for the OpenAI API.
#[derive(Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiFunctionDef,
}

/// Function definition within an OpenAI tool.
#[derive(Serialize)]
struct OpenAiFunctionDef {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// Top-level response from POST /chat/completions.
#[derive(Deserialize, Debug)]
struct ChatCompletionsResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
    #[allow(dead_code)]
    model: String,
}

/// A single completion choice.
#[derive(Deserialize, Debug)]
struct Choice {
    message: ResponseMessage,
    finish_reason: Option<String>,
}

/// The assistant message within a choice.
#[derive(Deserialize, Debug)]
struct ResponseMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

/// Token usage from the API response.
#[derive(Deserialize, Debug)]
struct Usage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

// ---------------------------------------------------------------------------
// Streaming response types
// ---------------------------------------------------------------------------

/// A single SSE chunk from a streaming response.
#[derive(Deserialize, Debug)]
struct StreamingChunk {
    choices: Vec<StreamingChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

/// A streaming choice delta.
#[derive(Deserialize, Debug)]
struct StreamingChoice {
    delta: Delta,
    finish_reason: Option<String>,
}

/// The delta object within a streaming choice.
#[derive(Deserialize, Debug)]
struct Delta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<StreamingToolCall>>,
}

/// A streaming tool call — may arrive across multiple chunks.
#[derive(Deserialize, Debug)]
struct StreamingToolCall {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamingFunction>,
}

/// Partial function data within a streaming tool call.
#[derive(Deserialize, Debug)]
struct StreamingFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

// ---------------------------------------------------------------------------
// OpenAiProvider
// ---------------------------------------------------------------------------

/// LLM provider for OpenAI-compatible endpoints.
///
/// Supports OpenAI, Azure OpenAI, Ollama, and any endpoint implementing the
/// `/chat/completions` API with optional tool calling.
pub struct OpenAiProvider {
    config: ProviderConfig,
}

impl OpenAiProvider {
    /// Create a provider from a resolved configuration.
    pub fn from_config(config: ProviderConfig) -> Self {
        Self { config }
    }

    /// Build default headers — includes Authorization if an API key is present.
    fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        if let Some(ref key) = self.config.api_key {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {key}")) {
                headers.insert(reqwest::header::AUTHORIZATION, val);
            }
        }
        headers
    }

    /// Construct the `HttpClient` for this provider.
    fn make_http_client(&self) -> HttpClient {
        HttpClient::new(self.config.base_url.clone(), self.build_headers())
    }

    /// Convert `ChatMessage` list to OpenAI wire format.
    fn convert_messages(messages: &[ChatMessage]) -> Vec<OpenAiMessage> {
        messages
            .iter()
            .map(|msg| {
                let role = match msg.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };

                // Extract plain text from content parts.
                let content_text: String = msg
                    .content
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");

                let content = if content_text.is_empty() {
                    None
                } else {
                    Some(content_text)
                };

                // Convert tool calls if present (assistant messages).
                let tool_calls = msg.tool_calls.as_ref().map(|calls| {
                    calls
                        .iter()
                        .map(|tc| OpenAiToolCall {
                            id: tc.call_id.clone(),
                            call_type: "function".to_string(),
                            function: OpenAiFunctionCall {
                                name: tc.name.clone(),
                                arguments: serde_json::to_string(&tc.arguments).unwrap_or_default(),
                            },
                        })
                        .collect()
                });

                OpenAiMessage {
                    role: role.to_string(),
                    content,
                    name: msg.name.clone(),
                    tool_call_id: msg.tool_call_id.clone(),
                    tool_calls,
                }
            })
            .collect()
    }

    /// Build the request body for a text-only completion.
    fn build_request(&self, request: &CompletionRequest, stream: bool) -> ChatCompletionsRequest {
        let model = request
            .model
            .as_deref()
            .unwrap_or(&self.config.model)
            .to_string();

        ChatCompletionsRequest {
            model,
            messages: Self::convert_messages(&request.messages),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            stop: request.stop_sequences.clone(),
            stream: if stream { Some(true) } else { None },
            tools: Vec::new(),
        }
    }

    /// Build the request body for a tool-calling completion.
    fn build_tool_request(
        &self,
        request: &ToolCompletionRequest,
        stream: bool,
    ) -> ChatCompletionsRequest {
        let model = request
            .model
            .as_deref()
            .unwrap_or(&self.config.model)
            .to_string();

        // Convert JSON tool definitions to OpenAI wire format.
        let tools: Vec<OpenAiTool> = request
            .tool_definitions
            .iter()
            .filter_map(|def| {
                let name = def.get("name")?.as_str()?.to_string();
                let description = def.get("description")?.as_str()?.to_string();
                let parameters = def
                    .get("parameters")
                    .or_else(|| def.get("input_schema"))
                    .cloned()
                    .unwrap_or(serde_json::json!({"type": "object"}));

                Some(OpenAiTool {
                    tool_type: "function".to_string(),
                    function: OpenAiFunctionDef {
                        name,
                        description,
                        parameters,
                    },
                })
            })
            .collect();

        ChatCompletionsRequest {
            model,
            messages: Self::convert_messages(&request.messages),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            stop: request.stop_sequences.clone(),
            stream: if stream { Some(true) } else { None },
            tools,
        }
    }

    /// Parse the OpenAI finish_reason string into our FinishReason enum.
    fn parse_finish_reason(reason: Option<&str>) -> FinishReason {
        match reason {
            Some("stop") => FinishReason::Stop,
            Some("length") => FinishReason::Length,
            Some("tool_calls") => FinishReason::ToolUse,
            Some("content_filter") => FinishReason::ContentFilter,
            _ => FinishReason::Unknown,
        }
    }

    /// Parse tool calls from an OpenAI response, normalising JSON string
    /// arguments into structured JSON.
    fn parse_tool_calls(calls: &[OpenAiToolCall]) -> Vec<ToolCall> {
        calls
            .iter()
            .map(|tc| {
                // OpenAI sends arguments as a JSON string — parse it into a Value.
                let arguments = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                ToolCall {
                    call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    arguments,
                }
            })
            .collect()
    }

    /// Parse a streaming SSE event into a StreamChunk.
    fn parse_stream_event(
        event: &SseEvent,
        tool_acc: &mut ToolCallAccumulator,
    ) -> Option<StreamChunk> {
        let chunk: StreamingChunk = match serde_json::from_str(&event.data) {
            Ok(chunk) => chunk,
            Err(error) => {
                return Some(StreamChunk::Error {
                    message: format!("failed to parse OpenAI SSE payload: {error}"),
                });
            }
        };

        // Usage information may arrive in a chunk with empty choices (e.g. the
        // final chunk from OpenAI when stream_options.include_usage is set).
        if let Some(ref choice) = chunk.choices.first() {
            // Accumulate streaming tool calls.
            if let Some(ref tool_calls) = choice.delta.tool_calls {
                for tc in tool_calls {
                    tool_acc.accumulate(tc);
                }
            }

            // Check for finish_reason — emit accumulated tool calls and Done.
            if let Some(ref reason) = choice.finish_reason {
                let finish = Self::parse_finish_reason(Some(reason));

                // Emit any accumulated tool calls before Done.
                let tool_calls = tool_acc.take();
                if !tool_calls.is_empty() {
                    return Some(StreamChunk::ToolUse {
                        call_id: tool_calls[0].call_id.clone(),
                        name: tool_calls[0].name.clone(),
                        arguments: tool_calls[0].arguments.clone(),
                    });
                }

                return Some(StreamChunk::Done {
                    finish_reason: Some(finish),
                });
            }

            // Text delta.
            if let Some(ref content) = choice.delta.content {
                if !content.is_empty() {
                    return Some(StreamChunk::Delta {
                        content: content.clone(),
                    });
                }
            }
        }

        // Usage information (sent in some streaming responses, often with
        // empty choices array).
        if let Some(usage) = chunk.usage {
            return Some(StreamChunk::Usage(swage_core::TokenUsage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
            }));
        }

        None
    }
}

// ---------------------------------------------------------------------------
// Tool call accumulator for streaming
// ---------------------------------------------------------------------------

/// Accumulates tool call fragments across streaming chunks.
///
/// OpenAI streams tool calls across multiple chunks — the first chunk
/// contains the call ID and function name, subsequent chunks append to the
/// arguments string. This accumulator gathers them into complete ToolCalls.
struct ToolCallAccumulator {
    calls: Vec<AccumulatingToolCall>,
}

struct AccumulatingToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl ToolCallAccumulator {
    fn new() -> Self {
        Self { calls: Vec::new() }
    }

    /// Process a streaming tool call fragment.
    fn accumulate(&mut self, tc: &StreamingToolCall) {
        // Grow the vec if needed.
        while self.calls.len() <= tc.index {
            self.calls.push(AccumulatingToolCall {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
        }

        let entry = &mut self.calls[tc.index];

        if let Some(ref id) = tc.id {
            entry.id = id.clone();
        }
        if let Some(ref func) = tc.function {
            if let Some(ref name) = func.name {
                entry.name = name.clone();
            }
            if let Some(ref args) = func.arguments {
                entry.arguments.push_str(args);
            }
        }
    }

    /// Drain all accumulated tool calls, returning them as complete ToolCalls.
    fn take(&mut self) -> Vec<ToolCall> {
        self.calls
            .drain(..)
            .map(|acc| {
                let arguments = serde_json::from_str(&acc.arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                ToolCall {
                    call_id: acc.id,
                    name: acc.name,
                    arguments,
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// LlmProvider implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn model_id(&self) -> &str {
        &self.config.model
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let body = self.build_request(&request, false);
        let client = self.make_http_client();

        let response: ChatCompletionsResponse =
            client.post_json("/chat/completions", &body).await?;

        let choice =
            response.choices.into_iter().next().ok_or_else(|| {
                ProviderError::InvalidResponse("no choices in response".to_string())
            })?;

        let usage = response.usage.unwrap_or(Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
        });

        Ok(CompletionResponse {
            content: choice.message.content.unwrap_or_default(),
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            finish_reason: Self::parse_finish_reason(choice.finish_reason.as_deref()),
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        let body = self.build_tool_request(&request, false);
        let client = self.make_http_client();

        let response: ChatCompletionsResponse =
            client.post_json("/chat/completions", &body).await?;

        let choice =
            response.choices.into_iter().next().ok_or_else(|| {
                ProviderError::InvalidResponse("no choices in response".to_string())
            })?;

        let usage = response.usage.unwrap_or(Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
        });

        let tool_calls = choice
            .message
            .tool_calls
            .as_deref()
            .map(Self::parse_tool_calls)
            .unwrap_or_default();

        Ok(ToolCompletionResponse {
            content: choice.message.content.unwrap_or_default(),
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            finish_reason: Self::parse_finish_reason(choice.finish_reason.as_deref()),
            cost: None,
            tool_calls,
        })
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        let (tx, rx) = mpsc::unbounded();
        let body = self.build_request(&request, true);
        let client = self.make_http_client();

        tokio::spawn(async move {
            let sse_stream = match client
                .post_stream("/chat/completions", &body, HeaderMap::new())
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
            let mut tool_acc = ToolCallAccumulator::new();

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

                // Emit accumulated tool calls before Done.
                if let Some(chunk) = OpenAiProvider::parse_stream_event(&event, &mut tool_acc) {
                    let is_done = matches!(chunk, StreamChunk::Done { .. });

                    // If we hit Done and there are remaining tool calls, emit them first.
                    if is_done {
                        let remaining = tool_acc.take();
                        for tc in remaining {
                            let _ = tx.unbounded_send(StreamChunk::ToolUse {
                                call_id: tc.call_id,
                                name: tc.name,
                                arguments: tc.arguments,
                            });
                        }
                    }

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
    use swage_core::{ChatMessage, Role};

    // --- Message conversion ---

    #[test]
    fn convert_system_message() {
        let messages = vec![ChatMessage::new(Role::System, "You are helpful")];
        let converted = OpenAiProvider::convert_messages(&messages);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "system");
        assert_eq!(converted[0].content.as_deref(), Some("You are helpful"));
    }

    #[test]
    fn convert_user_and_assistant_messages() {
        let messages = vec![
            ChatMessage::new(Role::User, "Hello"),
            ChatMessage::new(Role::Assistant, "Hi there"),
        ];
        let converted = OpenAiProvider::convert_messages(&messages);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0].role, "user");
        assert_eq!(converted[1].role, "assistant");
    }

    #[test]
    fn convert_tool_message_preserves_tool_call_id() {
        let mut msg = ChatMessage::new(Role::Tool, "result text");
        msg.tool_call_id = Some("call_abc".into());
        let converted = OpenAiProvider::convert_messages(&[msg]);
        assert_eq!(converted[0].role, "tool");
        assert_eq!(converted[0].tool_call_id, Some("call_abc".into()));
    }

    #[test]
    fn convert_assistant_with_tool_calls() {
        let mut msg = ChatMessage::new(Role::Assistant, "");
        msg.content = Vec::new();
        msg.tool_calls = Some(vec![ToolCall {
            call_id: "call_1".into(),
            name: "search".into(),
            arguments: serde_json::json!({"query": "rust"}),
        }]);

        let converted = OpenAiProvider::convert_messages(&[msg]);
        let tc = converted[0].tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "call_1");
        assert_eq!(tc[0].function.name, "search");
        assert!(tc[0].function.arguments.contains("rust"));
    }

    // --- Request building ---

    #[test]
    fn build_request_uses_model_from_request_when_present() {
        let provider = OpenAiProvider::from_config(ProviderConfig::new(
            "key",
            "default-model",
            "http://localhost",
        ));
        let request = CompletionRequest {
            messages: vec![],
            model: Some("custom-model".into()),
            max_tokens: Some(100),
            temperature: Some(0.5),
            stop_sequences: None,
            metadata: None,
        };
        let body = provider.build_request(&request, false);
        assert_eq!(body.model, "custom-model");
        assert_eq!(body.max_tokens, Some(100));
    }

    #[test]
    fn build_request_falls_back_to_config_model() {
        let provider =
            OpenAiProvider::from_config(ProviderConfig::new("key", "gpt-4o", "http://localhost"));
        let request = CompletionRequest {
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
        };
        let body = provider.build_request(&request, false);
        assert_eq!(body.model, "gpt-4o");
    }

    #[test]
    fn build_request_sets_stream_flag() {
        let provider =
            OpenAiProvider::from_config(ProviderConfig::new("key", "model", "http://localhost"));
        let request = CompletionRequest {
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
        };
        let non_streaming = provider.build_request(&request, false);
        assert!(non_streaming.stream.is_none());

        let streaming = provider.build_request(&request, true);
        assert_eq!(streaming.stream, Some(true));
    }

    // --- Tool request building ---

    #[test]
    fn build_tool_request_converts_definitions() {
        let provider =
            OpenAiProvider::from_config(ProviderConfig::new("key", "gpt-4o", "http://localhost"));
        let request = ToolCompletionRequest {
            messages: vec![ChatMessage::new(Role::User, "search for rust")],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![serde_json::json!({
                "name": "search",
                "description": "Web search",
                "parameters": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } }
                }
            })],
        };
        let body = provider.build_tool_request(&request, false);
        assert_eq!(body.tools.len(), 1);
        assert_eq!(body.tools[0].tool_type, "function");
        assert_eq!(body.tools[0].function.name, "search");
    }

    // --- Finish reason parsing ---

    #[test]
    fn parse_finish_reason_stop() {
        assert_eq!(
            OpenAiProvider::parse_finish_reason(Some("stop")),
            FinishReason::Stop
        );
    }

    #[test]
    fn parse_finish_reason_length() {
        assert_eq!(
            OpenAiProvider::parse_finish_reason(Some("length")),
            FinishReason::Length
        );
    }

    #[test]
    fn parse_finish_reason_tool_calls() {
        assert_eq!(
            OpenAiProvider::parse_finish_reason(Some("tool_calls")),
            FinishReason::ToolUse
        );
    }

    #[test]
    fn parse_finish_reason_content_filter() {
        assert_eq!(
            OpenAiProvider::parse_finish_reason(Some("content_filter")),
            FinishReason::ContentFilter,
        );
    }

    #[test]
    fn parse_finish_reason_null_maps_to_unknown() {
        assert_eq!(
            OpenAiProvider::parse_finish_reason(None),
            FinishReason::Unknown
        );
    }

    // --- Tool call parsing ---

    #[test]
    fn parse_tool_calls_normalises_json_string_arguments() {
        let calls = vec![OpenAiToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: OpenAiFunctionCall {
                name: "get_weather".into(),
                arguments: r#"{"location": "London"}"#.into(),
            },
        }];
        let parsed = OpenAiProvider::parse_tool_calls(&calls);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].call_id, "call_1");
        assert_eq!(parsed[0].name, "get_weather");
        assert_eq!(parsed[0].arguments["location"], "London");
    }

    #[test]
    fn parse_tool_calls_handles_malformed_arguments() {
        // Malformed JSON string should fall back to empty object.
        let calls = vec![OpenAiToolCall {
            id: "call_bad".into(),
            call_type: "function".into(),
            function: OpenAiFunctionCall {
                name: "tool".into(),
                arguments: "not valid json".into(),
            },
        }];
        let parsed = OpenAiProvider::parse_tool_calls(&calls);
        assert!(parsed[0].arguments.is_object());
    }

    // --- Tool call accumulator ---

    #[test]
    fn accumulator_gathers_fragments_across_chunks() {
        let mut acc = ToolCallAccumulator::new();

        // First chunk: start of tool call with id and name.
        acc.accumulate(&StreamingToolCall {
            index: 0,
            id: Some("call_1".into()),
            function: Some(StreamingFunction {
                name: Some("search".into()),
                arguments: Some("{\"q".into()),
            }),
        });

        // Second chunk: continue arguments.
        acc.accumulate(&StreamingToolCall {
            index: 0,
            id: None,
            function: Some(StreamingFunction {
                name: None,
                arguments: Some("uery\":\"rust\"}".into()),
            }),
        });

        let calls = acc.take();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_id, "call_1");
        assert_eq!(calls[0].name, "search");
        assert_eq!(calls[0].arguments["query"], "rust");
    }

    #[test]
    fn accumulator_handles_multiple_concurrent_tool_calls() {
        let mut acc = ToolCallAccumulator::new();

        acc.accumulate(&StreamingToolCall {
            index: 0,
            id: Some("call_a".into()),
            function: Some(StreamingFunction {
                name: Some("tool_a".into()),
                arguments: Some("{}".into()),
            }),
        });

        acc.accumulate(&StreamingToolCall {
            index: 1,
            id: Some("call_b".into()),
            function: Some(StreamingFunction {
                name: Some("tool_b".into()),
                arguments: Some("{}".into()),
            }),
        });

        let calls = acc.take();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "tool_a");
        assert_eq!(calls[1].name, "tool_b");
    }

    // --- Headers ---

    #[test]
    fn headers_include_authorization_when_key_present() {
        let provider = OpenAiProvider::from_config(ProviderConfig::new(
            "sk-test-key",
            "model",
            "http://localhost",
        ));
        let headers = provider.build_headers();
        let auth = headers.get("authorization").unwrap().to_str().unwrap();
        assert_eq!(auth, "Bearer sk-test-key");
    }

    #[test]
    fn headers_omit_authorization_when_keyless() {
        let provider = OpenAiProvider::from_config(ProviderConfig::keyless(
            "llama3",
            "http://localhost:11434",
        ));
        let headers = provider.build_headers();
        assert!(headers.get("authorization").is_none());
    }

    // --- Model ID ---

    #[test]
    fn model_id_returns_configured_model() {
        let provider = OpenAiProvider::from_config(ProviderConfig::new(
            "key",
            "gpt-4o-mini",
            "http://localhost",
        ));
        assert_eq!(provider.model_id(), "gpt-4o-mini");
    }

    // --- Streaming event parsing ---

    #[test]
    fn parse_stream_event_text_delta() {
        let event = SseEvent {
            event_type: None,
            data: r#"{"choices":[{"delta":{"content":"Hello"},"finish_reason":null}]}"#.into(),
        };
        let mut acc = ToolCallAccumulator::new();
        let chunk = OpenAiProvider::parse_stream_event(&event, &mut acc);
        assert!(matches!(chunk, Some(StreamChunk::Delta { content }) if content == "Hello"));
    }

    #[test]
    fn parse_stream_event_done_with_stop() {
        let event = SseEvent {
            event_type: None,
            data: r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#.into(),
        };
        let mut acc = ToolCallAccumulator::new();
        let chunk = OpenAiProvider::parse_stream_event(&event, &mut acc);
        assert!(matches!(
            chunk,
            Some(StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop)
            })
        ));
    }

    #[test]
    fn parse_stream_event_ignores_empty_delta() {
        let event = SseEvent {
            event_type: None,
            data: r#"{"choices":[{"delta":{},"finish_reason":null}]}"#.into(),
        };
        let mut acc = ToolCallAccumulator::new();
        let chunk = OpenAiProvider::parse_stream_event(&event, &mut acc);
        assert!(chunk.is_none());
    }

    #[test]
    fn parse_stream_event_with_usage() {
        let event = SseEvent {
            event_type: None,
            data: r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#.into(),
        };
        let mut acc = ToolCallAccumulator::new();
        let chunk = OpenAiProvider::parse_stream_event(&event, &mut acc);
        assert!(matches!(
            chunk,
            Some(StreamChunk::Usage(usage)) if usage.input_tokens == 10 && usage.output_tokens == 5
        ));
    }

    #[test]
    fn parse_stream_event_reports_malformed_payload() {
        let event = SseEvent {
            event_type: None,
            data: "{not-json".into(),
        };
        let mut acc = ToolCallAccumulator::new();
        let chunk = OpenAiProvider::parse_stream_event(&event, &mut acc);
        assert!(matches!(
            chunk,
            Some(StreamChunk::Error { message }) if message.contains("failed to parse OpenAI SSE payload")
        ));
    }

    // --- Response parsing (via serde) ---

    #[test]
    fn deserialise_completion_response() {
        let json = r#"{
            "choices": [{
                "message": { "content": "Hello!" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 },
            "model": "gpt-4o"
        }"#;
        let response: ChatCompletionsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.choices.len(), 1);
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("Hello!")
        );
        assert_eq!(response.usage.unwrap().prompt_tokens, 10);
    }

    #[test]
    fn deserialise_response_with_tool_calls() {
        let json = r#"{
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\"London\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 20, "completion_tokens": 10 },
            "model": "gpt-4o"
        }"#;
        let response: ChatCompletionsResponse = serde_json::from_str(json).unwrap();
        let tc = response.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].function.name, "get_weather");
    }

    #[test]
    fn deserialise_response_without_usage() {
        // Some compatible endpoints omit usage entirely.
        let json = r#"{
            "choices": [{
                "message": { "content": "Hello" },
                "finish_reason": "stop"
            }],
            "model": "local-model"
        }"#;
        let response: ChatCompletionsResponse = serde_json::from_str(json).unwrap();
        assert!(response.usage.is_none());
    }

    #[test]
    fn deserialise_streaming_chunk() {
        let json = r#"{
            "choices": [{
                "delta": { "content": "Hi" },
                "finish_reason": null
            }]
        }"#;
        let chunk: StreamingChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hi"));
    }

    #[test]
    fn deserialise_streaming_tool_call() {
        let json = r#"{
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_x",
                        "function": { "name": "search", "arguments": "{" }
                    }]
                },
                "finish_reason": null
            }]
        }"#;
        let chunk: StreamingChunk = serde_json::from_str(json).unwrap();
        let tc = &chunk.choices[0].delta.tool_calls.as_ref().unwrap()[0];
        assert_eq!(tc.index, 0);
        assert_eq!(tc.id, Some("call_x".into()));
    }
}
