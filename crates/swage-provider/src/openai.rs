//! OpenAI-compatible LLM provider.
//!
//! Handles any endpoint speaking the OpenAI chat completions format — OpenAI
//! itself, Azure OpenAI, Ollama, or custom endpoints — via configurable
//! `base_url`.

use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use swage_core::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart, Effort, EffortCapability,
    EffortControl, FinishReason, ProviderError, Role, StreamChunk, ToolCall, ToolCompletionRequest,
    ToolCompletionResponse, ToolReplayPart,
};

use crate::http::{HttpClient, SseEvent};
use crate::provider::{LlmProvider, ModelOutputWitness, ProviderConfig, spawn_stream_forwarder};

/// Known native-harness mismatch retained while this provider deliberately
/// remains on Chat Completions. Recording it does not make it an admitted
/// harness-parity divergence: a Responses API implementation is still needed.
const OPENAI_REQUEST_DIVERGENCES: &[(&str, &str)] = &[(
    "unresolved_product_defect",
    "native GPT uses /v1/responses with client_metadata, include, input, parallel_tool_calls, prompt_cache_key, reasoning, store, and text; Swage uses Chat Completions with messages, stream_options, and tools",
)];

// ---------------------------------------------------------------------------
// OpenAI wire types
// ---------------------------------------------------------------------------

/// The effort control OpenAI documents for its GPT reasoning models, shared
/// by Chat Completions (`reasoning_effort`) and the Responses API
/// (`reasoning.effort`): `none` through `xhigh`, where `none` is the
/// model's own off. `max` is not a level the control accepts.
pub(crate) fn openai_effort_capability() -> EffortCapability {
    EffortCapability::Levels {
        levels: vec![
            Effort::Minimal,
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
        ],
        can_disable: true,
    }
}

/// Whether a model id names one of OpenAI's own reasoning models — the
/// models the documented control is known to accept. The Chat Completions
/// adapter also serves arbitrary OpenAI-compatible endpoints, whose models
/// the adapter cannot vouch for: those answer `None`, and the catalogue's
/// own declaration governs.
pub(crate) fn is_openai_reasoning_model(model: &str) -> bool {
    let model = model.trim().to_ascii_lowercase();
    model.starts_with("gpt-5")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
}

/// The `reasoning_effort` / `reasoning.effort` value for a resolved control.
/// Expresses the control only — a level here is one the wire accepts.
pub(crate) fn openai_reasoning_effort(control: EffortControl) -> Option<String> {
    match control {
        EffortControl::Level { level } => Some(level.as_str().to_string()),
        EffortControl::Disabled => Some("none".to_string()),
        EffortControl::Budget { .. } | EffortControl::Omitted => None,
    }
}

/// Request body for POST /chat/completions.
#[derive(Serialize)]
struct ChatCompletionsRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<OpenAiStreamOptions>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

/// Streaming options for OpenAI-compatible chat completions.
#[derive(Serialize)]
struct OpenAiStreamOptions {
    include_usage: bool,
}

/// Wire representation of a message in the OpenAI format.
#[derive(Serialize, Deserialize, Debug)]
struct OpenAiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<OpenAiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

/// A message's content on the Chat Completions wire: a bare string when the
/// message is text only, an array of typed parts once it carries an image.
/// The string form is kept for text so a text-only request's body is
/// byte-identical to what it was before image parts existed.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(untagged)]
enum OpenAiContent {
    Text(String),
    Parts(Vec<OpenAiContentPart>),
}

#[cfg(test)]
impl OpenAiContent {
    /// The text-only projection of the content — what a caller that reads
    /// the message as a string sees.
    fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text.as_str()),
            Self::Parts(_) => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiContentPart {
    Text { text: String },
    ImageUrl { image_url: OpenAiImageUrl },
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
struct OpenAiImageUrl {
    url: String,
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
    reasoning: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning_details: Option<serde_json::Value>,
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
        if let Some(ref key) = self.config.api_key
            && let Ok(val) = HeaderValue::from_str(&format!("Bearer {}", key.expose_secret()))
        {
            headers.insert(reqwest::header::AUTHORIZATION, val);
        }
        headers
    }

    /// Construct the `HttpClient` for this provider.
    fn make_http_client(&self) -> HttpClient {
        HttpClient::new(self.config.base_url.clone(), self.build_headers())
    }

    /// Convert `ChatMessage` list to OpenAI wire format.
    fn convert_messages(messages: &[ChatMessage]) -> Vec<OpenAiMessage> {
        messages.iter().flat_map(Self::convert_message).collect()
    }

    fn convert_message(message: &ChatMessage) -> Vec<OpenAiMessage> {
        if let Some(replay) = message.tool_replay()
            && let Some(messages) = Self::convert_tool_replay_message(message, replay)
        {
            return messages;
        }

        let role = match message.role() {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };

        let content = Self::convert_content(message.content());

        let tool_calls = message.tool_calls().map(|calls| {
            calls
                .iter()
                .map(|tool_call| OpenAiToolCall {
                    id: tool_call.call_id.clone(),
                    call_type: "function".to_string(),
                    function: OpenAiFunctionCall {
                        name: tool_call.name.clone(),
                        arguments: serde_json::to_string(&tool_call.arguments).unwrap_or_default(),
                    },
                })
                .collect()
        });

        vec![OpenAiMessage {
            role: role.to_string(),
            content,
            name: message.name().map(ToOwned::to_owned),
            tool_call_id: message.tool_call_id().map(ToOwned::to_owned),
            tool_calls,
        }]
    }

    /// Message content on the wire. Text-only content stays a single string;
    /// an image part switches the message to the typed-parts array, every
    /// part in its original order. Empty content is omitted.
    fn convert_content(parts: &[ContentPart]) -> Option<OpenAiContent> {
        let has_image = parts
            .iter()
            .any(|part| matches!(part, ContentPart::ImageUrl { .. }));
        if has_image {
            return Some(OpenAiContent::Parts(
                parts
                    .iter()
                    .map(|part| match part {
                        ContentPart::Text { text } => {
                            OpenAiContentPart::Text { text: text.clone() }
                        }
                        ContentPart::ImageUrl { url } => OpenAiContentPart::ImageUrl {
                            image_url: OpenAiImageUrl { url: url.clone() },
                        },
                    })
                    .collect(),
            ));
        }
        let text: String = parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("");
        (!text.is_empty()).then_some(OpenAiContent::Text(text))
    }

    fn convert_tool_replay_message(
        message: &ChatMessage,
        replay: &[ToolReplayPart],
    ) -> Option<Vec<OpenAiMessage>> {
        match message.role() {
            Role::Assistant => {
                let text_content = replay
                    .iter()
                    .filter_map(|part| match part {
                        ToolReplayPart::Text { text }
                        | ToolReplayPart::Reasoning { content: text, .. } => Some(text.as_str()),
                        ToolReplayPart::ToolCall { .. } | ToolReplayPart::ToolResult { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");

                let tool_calls: Vec<OpenAiToolCall> = replay
                    .iter()
                    .filter_map(|part| match part {
                        ToolReplayPart::ToolCall {
                            call_id,
                            name,
                            arguments,
                        } => Some(OpenAiToolCall {
                            id: call_id.clone(),
                            call_type: "function".to_string(),
                            function: OpenAiFunctionCall {
                                name: name.clone(),
                                arguments: serde_json::to_string(arguments).ok()?,
                            },
                        }),
                        ToolReplayPart::Text { .. }
                        | ToolReplayPart::Reasoning { .. }
                        | ToolReplayPart::ToolResult { .. } => None,
                    })
                    .collect();

                Some(vec![OpenAiMessage {
                    role: "assistant".to_string(),
                    content: (!text_content.is_empty())
                        .then_some(OpenAiContent::Text(text_content)),
                    name: message.name().map(ToOwned::to_owned),
                    tool_call_id: None,
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                }])
            }
            Role::User => {
                let tool_result_messages: Vec<OpenAiMessage> = replay
                    .iter()
                    .filter_map(|part| match part {
                        ToolReplayPart::ToolResult {
                            call_id, content, ..
                        } => Some(OpenAiMessage {
                            role: "tool".to_string(),
                            content: Some(OpenAiContent::Text(content.clone())),
                            name: None,
                            tool_call_id: Some(call_id.clone()),
                            tool_calls: None,
                        }),
                        ToolReplayPart::Text { .. }
                        | ToolReplayPart::Reasoning { .. }
                        | ToolReplayPart::ToolCall { .. } => None,
                    })
                    .collect();

                if tool_result_messages.is_empty() {
                    None
                } else {
                    Some(tool_result_messages)
                }
            }
            Role::System | Role::Tool => None,
        }
    }

    /// Build the request body for a text-only completion.
    fn build_request(&self, request: &CompletionRequest, stream: bool) -> ChatCompletionsRequest {
        let model = request
            .model
            .as_deref()
            .unwrap_or(&self.config.model)
            .to_string();
        let tools = Self::convert_tool_definitions(&request.tool_definitions);

        ChatCompletionsRequest {
            model,
            reasoning_effort: request
                .effort
                .as_ref()
                .and_then(|effort| openai_reasoning_effort(effort.control)),
            messages: Self::convert_messages(&request.messages),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            stop: request.stop_sequences.clone(),
            stream: if stream { Some(true) } else { None },
            stream_options: Self::stream_options(stream),
            tool_choice: if tools.is_empty() {
                None
            } else {
                Some("auto".to_string())
            },
            tools,
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
        let tools = Self::convert_tool_definitions(&request.tool_definitions);

        ChatCompletionsRequest {
            model,
            // ToolCompletionRequest has no per-call effort surface.
            reasoning_effort: None,
            messages: Self::convert_messages(&request.messages),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            stop: request.stop_sequences.clone(),
            stream: if stream { Some(true) } else { None },
            stream_options: Self::stream_options(stream),
            tool_choice: if tools.is_empty() {
                None
            } else {
                Some("auto".to_string())
            },
            tools,
        }
    }

    fn convert_tool_definitions(definitions: &[serde_json::Value]) -> Vec<OpenAiTool> {
        definitions
            .iter()
            .filter_map(|def| {
                let name = def.get("name")?.as_str()?.to_string();
                let description = def
                    .get("description")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let parameters = def
                    .get("parameters")
                    .or_else(|| def.get("input_schema"))
                    .map(crate::tool_schema::provider_tool_schema)
                    .unwrap_or_else(|| serde_json::json!({"type": "object"}));

                Some(OpenAiTool {
                    tool_type: "function".to_string(),
                    function: OpenAiFunctionDef {
                        name,
                        description,
                        parameters,
                    },
                })
            })
            .collect()
    }

    fn stream_options(stream: bool) -> Option<OpenAiStreamOptions> {
        stream.then_some(OpenAiStreamOptions {
            include_usage: true,
        })
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
                    .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));

                ToolCall {
                    call_id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    arguments,
                }
            })
            .collect()
    }

    /// Parse a streaming SSE event into a StreamChunk.
    #[cfg(test)]
    fn parse_stream_event(
        event: &SseEvent,
        tool_acc: &mut ToolCallAccumulator,
    ) -> Option<StreamChunk> {
        Self::parse_stream_event_chunks(event, tool_acc)
            .into_iter()
            .next()
    }

    fn parse_stream_event_chunks(
        event: &SseEvent,
        tool_acc: &mut ToolCallAccumulator,
    ) -> Vec<StreamChunk> {
        let chunk: StreamingChunk = match serde_json::from_str(&event.data) {
            Ok(chunk) => chunk,
            Err(error) => {
                return vec![StreamChunk::Error {
                    message: format!("failed to parse OpenAI SSE payload: {error}"),
                }];
            }
        };

        // Usage information may arrive in a chunk with empty choices (e.g. the
        // final chunk from OpenAI when stream_options.include_usage is set), or
        // alongside the terminal choice.
        let usage = chunk.usage.as_ref().map(|usage| {
            StreamChunk::Usage(swage_core::TokenUsage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
            })
        });

        if let Some(choice) = chunk.choices.first() {
            // Accumulate streaming tool calls.
            if let Some(ref tool_calls) = choice.delta.tool_calls {
                for tc in tool_calls {
                    tool_acc.accumulate(tc);
                }
            }

            // Check for finish_reason — emit accumulated tool calls and Done.
            if let Some(ref reason) = choice.finish_reason {
                let finish = Self::parse_finish_reason(Some(reason));

                // The terminal choice may still carry text; emit it, then any
                // accumulated tool calls, before Done.
                let mut chunks = Vec::new();
                if let Some(chunk) = Self::parse_reasoning_delta(&choice.delta) {
                    chunks.push(chunk);
                }
                if let Some(ref content) = choice.delta.content
                    && !content.is_empty()
                {
                    chunks.push(StreamChunk::Delta {
                        content: content.clone(),
                    });
                }
                chunks.extend(
                    tool_acc
                        .take()
                        .into_iter()
                        .map(|tool_call| StreamChunk::ToolUse {
                            call_id: tool_call.call_id,
                            name: tool_call.name,
                            arguments: tool_call.arguments,
                        }),
                );
                if let Some(usage) = usage {
                    chunks.push(usage);
                }
                chunks.push(StreamChunk::Done {
                    finish_reason: Some(finish),
                });
                return chunks;
            }

            let mut chunks = Vec::new();
            if let Some(chunk) = Self::parse_reasoning_delta(&choice.delta) {
                chunks.push(chunk);
            }

            // Text delta.
            if let Some(ref content) = choice.delta.content
                && !content.is_empty()
            {
                chunks.push(StreamChunk::Delta {
                    content: content.clone(),
                });
            }

            if !chunks.is_empty() {
                return chunks;
            }
        }

        // Usage information (sent in some streaming responses, often with
        // empty choices array).
        if let Some(usage) = usage {
            return vec![usage];
        }

        Vec::new()
    }

    fn parse_reasoning_delta(delta: &Delta) -> Option<StreamChunk> {
        let content = delta
            .reasoning
            .as_deref()
            .filter(|content| !content.is_empty())
            .or_else(|| {
                delta
                    .reasoning_content
                    .as_deref()
                    .filter(|content| !content.is_empty())
            });

        if content.is_none() && delta.reasoning_details.is_none() {
            return None;
        }

        Some(StreamChunk::Reasoning {
            content: content.unwrap_or_default().to_string(),
            details: delta.reasoning_details.clone(),
        })
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
                    .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
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

    fn effort_capability(&self, model: &str) -> Option<EffortCapability> {
        is_openai_reasoning_model(model).then(openai_effort_capability)
    }

    fn provider_request_snapshot(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "provider": "openai",
            "protocol": "chat_completions",
            "divergences": OPENAI_REQUEST_DIVERGENCES,
            "stream": stream,
            "body": self.build_request(request, stream),
        })
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
        let body = self.build_request(&request, true);
        let client = self.make_http_client();

        spawn_stream_forwarder(move |tx| async move {
            let (_metadata, sse_stream) = match client
                .post_stream("/chat/completions", &body, HeaderMap::new())
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(StreamChunk::Error {
                        message: e.to_string(),
                    });
                    return;
                }
            };

            tokio::pin!(sse_stream);
            let mut tool_acc = ToolCallAccumulator::new();
            let mut output = ModelOutputWitness::default();

            'events: while let Some(result) = sse_stream.next().await {
                let event = match result {
                    Err(e) => {
                        let _ = tx.send(StreamChunk::Error {
                            message: e.to_string(),
                        });
                        return;
                    }
                    Ok(ev) => ev,
                };

                // Emit accumulated tool calls before Done.
                for chunk in OpenAiProvider::parse_stream_event_chunks(&event, &mut tool_acc) {
                    let is_done = matches!(chunk, StreamChunk::Done { .. });

                    // If we hit Done and there are remaining tool calls, emit them first.
                    if is_done {
                        let remaining = tool_acc.take();
                        for tc in remaining {
                            let tool_use = output.observe(StreamChunk::ToolUse {
                                call_id: tc.call_id,
                                name: tc.name,
                                arguments: tc.arguments,
                            });
                            let _ = tx.send(tool_use.expect("a tool call is model output"));
                        }
                    }

                    let Some(chunk) = output.observe(chunk) else {
                        let _ = tx.send(output.anomaly("Chat Completions"));
                        return;
                    };
                    let _ = tx.send(chunk);
                    if is_done {
                        break 'events;
                    }
                }
            }
            if !output.completed {
                let _ = tx.send(output.anomaly("Chat Completions"));
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{empty_completion_request, local_provider_config};
    use swage_core::{ChatMessage, Role, ToolCall};

    #[tokio::test]
    async fn captured_proxy_stream_preserves_usage_before_completion() {
        use std::io::{BufRead, BufReader, Read, Write};
        let capture = include_str!("../tests/fixtures/gpt-6.1-sol-usage.sse");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(&mut socket);
            let mut content_length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some(length) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = length.trim().parse::<usize>().unwrap();
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            drop(reader);
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", capture.len(), capture).unwrap();
        });
        let provider = OpenAiProvider::from_config(ProviderConfig::new(
            "test-key",
            "gpt-6.1-sol",
            format!("http://{address}/v1"),
        ));
        let chunks: Vec<_> = provider.stream(empty_completion_request()).collect().await;
        server.join().unwrap();
        let usage_position = chunks.iter().position(|chunk| matches!(
            chunk, StreamChunk::Usage(usage) if usage.input_tokens == 9916 && usage.output_tokens == 6
        )).expect("captured provider counts must reach the caller");
        let done_position = chunks
            .iter()
            .position(|chunk| matches!(chunk, StreamChunk::Done { .. }))
            .expect("captured response must complete");
        assert!(
            usage_position < done_position,
            "usage must reach the caller before completion"
        );
    }

    // --- Message conversion ---

    #[test]
    fn convert_system_message() {
        let messages = vec![ChatMessage::new(Role::System, "You are helpful")];
        let converted = OpenAiProvider::convert_messages(&messages);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role, "system");
        assert_eq!(
            converted[0]
                .content
                .as_ref()
                .and_then(OpenAiContent::as_text),
            Some("You are helpful")
        );
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
    fn text_only_content_stays_a_string_on_the_wire() {
        let messages = vec![ChatMessage::new(Role::User, "Hello")];
        let converted = OpenAiProvider::convert_messages(&messages);
        let wire = serde_json::to_value(&converted[0]).expect("serialise message");
        assert_eq!(wire["content"], "Hello");
    }

    #[test]
    fn an_image_part_switches_content_to_typed_parts_in_order() {
        let messages = vec![ChatMessage::new_parts(
            Role::User,
            vec![
                ContentPart::Text {
                    text: "What is in this picture?".into(),
                },
                ContentPart::ImageUrl {
                    url: "https://example.test/cat.png".into(),
                },
            ],
        )];
        let converted = OpenAiProvider::convert_messages(&messages);
        let wire = serde_json::to_value(&converted[0]).expect("serialise message");
        assert_eq!(
            wire["content"],
            serde_json::json!([
                {"type": "text", "text": "What is in this picture?"},
                {"type": "image_url", "image_url": {"url": "https://example.test/cat.png"}}
            ])
        );
    }

    #[test]
    fn a_request_carrying_an_image_puts_it_on_the_wire_body() {
        let provider = OpenAiProvider::from_config(ProviderConfig::new(
            "https://api.openai.com/v1",
            "key",
            "gpt-4o",
        ));
        let request = CompletionRequest {
            messages: vec![ChatMessage::new_parts(
                Role::User,
                vec![
                    ContentPart::Text {
                        text: "Describe it".into(),
                    },
                    ContentPart::ImageUrl {
                        url: "data:image/png;base64,AAAA".into(),
                    },
                ],
            )],
            model: None,
            effort: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        };
        let body = serde_json::to_value(provider.build_request(&request, false))
            .expect("serialise request body");
        assert_eq!(
            body["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
    }

    #[test]
    fn convert_tool_message_preserves_tool_call_id() {
        let msg = ChatMessage::tool_result("call_abc", "result text");
        let converted = OpenAiProvider::convert_messages(&[msg]);
        assert_eq!(converted[0].role, "tool");
        assert_eq!(converted[0].tool_call_id, Some("call_abc".into()));
    }

    #[test]
    fn convert_assistant_with_tool_calls() {
        let msg = ChatMessage::assistant_tool_calls(
            "",
            vec![ToolCall {
                call_id: "call_1".into(),
                name: "search".into(),
                arguments: serde_json::json!({"query": "rust"}),
            }],
        );

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
            effort: None,
            messages: vec![],
            model: Some("custom-model".into()),
            max_tokens: Some(100),
            temperature: Some(0.5),
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        };
        let body = provider.build_request(&request, false);
        assert_eq!(body.model, "custom-model");
        assert_eq!(body.max_tokens, Some(100));
    }

    #[test]
    fn every_native_level_maps_to_openai_reasoning_effort() {
        let provider = OpenAiProvider::from_config(local_provider_config());
        for (level, expected) in [
            (Effort::Minimal, "minimal"),
            (Effort::Low, "low"),
            (Effort::Medium, "medium"),
            (Effort::High, "high"),
            (Effort::Xhigh, "xhigh"),
        ] {
            let request = CompletionRequest {
                effort: Some(swage_core::resolve_effort(
                    level,
                    &openai_effort_capability(),
                )),
                messages: vec![],
                model: None,
                max_tokens: None,
                temperature: None,
                stop_sequences: None,
                metadata: None,
                tool_definitions: vec![],
            };
            let body = serde_json::to_value(provider.build_request(&request, false)).unwrap();
            assert_eq!(body["reasoning_effort"], expected);
        }
    }

    /// The adapter vouches for OpenAI's own reasoning models only; a model
    /// behind a generic OpenAI-compatible endpoint answers `None`, and the
    /// catalogue's declaration governs.
    #[test]
    fn effort_capability_is_declared_only_for_recognised_models() {
        let provider = OpenAiProvider::from_config(local_provider_config());
        assert!(provider.effort_capability("gpt-5.6-terra").is_some());
        assert!(provider.effort_capability("o3-mini").is_some());
        assert!(provider.effort_capability("llama-3.3-70b").is_none());
        assert!(provider.effort_capability("qwen-3").is_none());
    }

    /// `max` is not an OpenAI level: it arrives clamped to `xhigh`, and
    /// `off` arrives as the model's own `none`.
    #[test]
    fn a_resolved_clamp_and_a_disable_reach_the_openai_wire_as_resolved() {
        let provider = OpenAiProvider::from_config(local_provider_config());
        let mut request = CompletionRequest {
            effort: Some(swage_core::resolve_effort(
                Effort::Max,
                &openai_effort_capability(),
            )),
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        };
        assert_eq!(
            request.effort.as_ref().unwrap().clamped_to,
            Some(Effort::Xhigh)
        );
        let body = serde_json::to_value(provider.build_request(&request, false)).unwrap();
        assert_eq!(body["reasoning_effort"], "xhigh");

        request.effort = Some(swage_core::resolve_effort(
            Effort::Off,
            &openai_effort_capability(),
        ));
        let body = serde_json::to_value(provider.build_request(&request, false)).unwrap();
        assert_eq!(body["reasoning_effort"], "none");
    }

    #[test]
    fn completion_without_effort_omits_openai_reasoning_effort() {
        let provider = OpenAiProvider::from_config(local_provider_config());
        let request = empty_completion_request();
        let body = serde_json::to_value(provider.build_request(&request, false)).unwrap();
        assert!(!body.as_object().unwrap().contains_key("reasoning_effort"));
    }

    #[test]
    fn build_request_falls_back_to_config_model() {
        let provider =
            OpenAiProvider::from_config(ProviderConfig::new("key", "gpt-4o", "http://localhost"));
        let request = CompletionRequest {
            effort: None,
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        };
        let body = provider.build_request(&request, false);
        assert_eq!(body.model, "gpt-4o");
    }

    #[test]
    fn build_request_keeps_output_requirements_in_system_message() {
        let provider =
            OpenAiProvider::from_config(ProviderConfig::new("key", "gpt-4o", "http://localhost"));
        let declaration = "## Output Requirements\n\nReturn ONLY schema-compliant JSON.";
        let request = CompletionRequest {
            effort: None,
            messages: vec![
                ChatMessage::new(Role::User, "Return JSON"),
                ChatMessage::new(Role::System, declaration),
            ],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        };

        let body = serde_json::to_value(provider.build_request(&request, false)).unwrap();

        assert_eq!(body["messages"][1]["role"], "system");
        assert_eq!(body["messages"][1]["content"], declaration);
    }

    #[test]
    fn build_request_sets_stream_flag() {
        let provider = OpenAiProvider::from_config(local_provider_config());
        let request = empty_completion_request();
        let non_streaming = provider.build_request(&request, false);
        assert!(non_streaming.stream.is_none());
        assert!(non_streaming.stream_options.is_none());

        let streaming = provider.build_request(&request, true);
        assert_eq!(streaming.stream, Some(true));
        assert!(
            streaming
                .stream_options
                .as_ref()
                .is_some_and(|options| options.include_usage)
        );
    }

    #[test]
    fn build_request_serialises_stream_usage_options_only_for_streaming() {
        let provider = OpenAiProvider::from_config(local_provider_config());
        let request = empty_completion_request();

        let non_streaming = serde_json::to_value(provider.build_request(&request, false)).unwrap();
        assert!(non_streaming.get("stream").is_none());
        assert!(non_streaming.get("stream_options").is_none());

        let streaming = serde_json::to_value(provider.build_request(&request, true)).unwrap();
        assert_eq!(streaming["stream"], true);
        assert_eq!(streaming["stream_options"]["include_usage"], true);
    }

    #[test]
    fn build_request_includes_tools_and_auto_choice_when_tool_definitions_exist() {
        let provider =
            OpenAiProvider::from_config(ProviderConfig::new("key", "gpt-4o", "http://localhost"));
        let request = CompletionRequest {
            effort: None,
            messages: vec![ChatMessage::new(Role::User, "Use a tool")],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![serde_json::json!({
                "name": "ping",
                "description": "Echoes the input value",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "value": { "type": "string" }
                    },
                    "required": ["value"]
                }
            })],
        };

        let body = provider.build_request(&request, true);
        assert_eq!(body.tools.len(), 1);
        assert_eq!(body.tools[0].tool_type, "function");
        assert_eq!(body.tools[0].function.name, "ping");
        assert_eq!(body.tool_choice.as_deref(), Some("auto"));
    }

    #[test]
    fn provider_request_snapshot_uses_chat_completions_wire_body() {
        let provider =
            OpenAiProvider::from_config(ProviderConfig::new("key", "gpt-4o", "http://localhost"));
        let request = CompletionRequest {
            effort: None,
            messages: vec![ChatMessage::new(Role::User, "Use a tool")],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![serde_json::json!({
                "name": "ping",
                "description": "Echoes the input value",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "value": { "type": "string" }
                    }
                }
            })],
        };

        let snapshot = provider.provider_request_snapshot(&request, true);

        assert_eq!(snapshot["provider"], "openai");
        assert_eq!(snapshot["protocol"], "chat_completions");
        assert_eq!(
            snapshot["divergences"],
            serde_json::json!([[
                "unresolved_product_defect",
                "native GPT uses /v1/responses with client_metadata, include, input, parallel_tool_calls, prompt_cache_key, reasoning, store, and text; Swage uses Chat Completions with messages, stream_options, and tools"
            ]])
        );
        assert_eq!(snapshot["stream"], true);
        assert_eq!(snapshot["body"]["messages"][0]["role"], "user");
        assert_eq!(snapshot["body"]["tools"][0]["function"]["name"], "ping");
        assert_eq!(snapshot["body"]["tool_choice"], "auto");
    }

    #[test]
    fn provider_request_snapshot_includes_reasoning_replay_when_present() {
        let provider =
            OpenAiProvider::from_config(ProviderConfig::new("key", "gpt-4o", "http://localhost"));
        let request = CompletionRequest {
            effort: None,
            messages: vec![
                ChatMessage::new(Role::User, "Use a tool"),
                ChatMessage::with_tool_replay(
                    Role::Assistant,
                    vec![
                        ToolReplayPart::Reasoning {
                            content: "Private provider reasoning".to_string(),
                            details: Some(serde_json::json!({ "source": "test" })),
                        },
                        ToolReplayPart::ToolCall {
                            call_id: "call_1".to_string(),
                            name: "ping".to_string(),
                            arguments: serde_json::json!({ "value": "hello" }),
                        },
                    ],
                ),
            ],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        };

        let snapshot = provider.provider_request_snapshot(&request, true);

        assert_eq!(
            snapshot["body"]["messages"][1]["content"],
            "Private provider reasoning"
        );
        assert_eq!(
            snapshot["body"]["messages"][1]["tool_calls"][0]["function"]["name"],
            "ping"
        );
    }

    #[test]
    fn build_request_flattens_provider_facing_tool_schema() {
        let provider =
            OpenAiProvider::from_config(ProviderConfig::new("key", "gpt-4o", "http://localhost"));
        let request = CompletionRequest {
            effort: None,
            messages: vec![ChatMessage::new(Role::User, "Use a tool")],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![serde_json::json!({
                "name": "search",
                "description": "Searches.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "minLength": 3,
                            "pattern": "^[a-z]+$"
                        },
                        "oneOf": {
                            "type": "string",
                            "description": "A normal parameter name."
                        }
                    },
                    "required": ["query"],
                    "oneOf": [{"required": ["query"]}]
                }
            })],
        };

        let body = provider.build_request(&request, false);
        let schema = &body.tools[0].function.parameters;

        assert_eq!(schema["type"], "object");
        assert!(schema.get("oneOf").is_none());
        assert!(schema["properties"]["query"].get("minLength").is_none());
        assert!(schema["properties"]["query"].get("pattern").is_none());
        assert_eq!(schema["properties"]["oneOf"]["type"], "string");
        assert!(schema["description"].as_str().unwrap().contains("oneOf"));
        assert!(
            schema["description"]
                .as_str()
                .unwrap()
                .contains("requires `query`")
        );
    }

    #[test]
    fn convert_messages_translates_tool_round_blocks_to_openai_messages() {
        let messages = swage_core::build_tool_round_messages(
            &[ToolCall {
                call_id: "call_1".into(),
                name: "shell".into(),
                arguments: serde_json::json!({ "command": "pwd" }),
            }],
            &[swage_core::ToolResult {
                call_id: "call_1".into(),
                content: "/workspace".into(),
                is_error: false,
            }],
            "",
        );

        assert!(
            messages[0].tool_replay().is_some(),
            "fixture should exercise canonical replay before compatibility blocks"
        );
        assert!(
            messages[1].tool_replay().is_some(),
            "fixture should exercise canonical replay before compatibility blocks"
        );
        let converted = OpenAiProvider::convert_messages(&messages);
        assert_eq!(converted.len(), 2);

        assert_eq!(converted[0].role, "assistant");
        assert!(converted[0].content.is_none());
        let tool_calls = converted[0]
            .tool_calls
            .as_ref()
            .expect("assistant tool call should be present");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "shell");
        assert_eq!(tool_calls[0].function.arguments, "{\"command\":\"pwd\"}");

        assert_eq!(converted[1].role, "tool");
        assert_eq!(converted[1].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(
            converted[1]
                .content
                .as_ref()
                .and_then(OpenAiContent::as_text),
            Some("/workspace")
        );
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

    #[test]
    fn build_tool_request_serialises_stream_usage_options() {
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

        let body = serde_json::to_value(provider.build_tool_request(&request, true)).unwrap();
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
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
    fn parse_stream_event_terminal_choice_keeps_its_text() {
        let event = SseEvent {
            event_type: None,
            data: r#"{"choices":[{"delta":{"content":"done"},"finish_reason":"stop"}]}"#.into(),
        };
        let mut acc = ToolCallAccumulator::new();
        let chunks = OpenAiProvider::parse_stream_event_chunks(&event, &mut acc);
        assert!(
            matches!(
                chunks.as_slice(),
                [StreamChunk::Delta { content }, StreamChunk::Done { .. }] if content == "done"
            ),
            "{chunks:?}"
        );
    }

    #[test]
    fn parse_stream_event_reasoning_content_delta() {
        let event = SseEvent {
            event_type: None,
            data: r#"{"choices":[{"delta":{"reasoning_content":"private thought"},"finish_reason":null}]}"#.into(),
        };
        let mut acc = ToolCallAccumulator::new();
        let chunk = OpenAiProvider::parse_stream_event(&event, &mut acc);
        assert!(matches!(
            chunk,
            Some(StreamChunk::Reasoning { content, details: None }) if content == "private thought"
        ));
    }

    #[test]
    fn parse_stream_event_reasoning_details_delta() {
        let event = SseEvent {
            event_type: None,
            data: r#"{"choices":[{"delta":{"reasoning_details":[{"type":"summary","text":"trace"}]},"finish_reason":null}]}"#.into(),
        };
        let mut acc = ToolCallAccumulator::new();
        let chunk = OpenAiProvider::parse_stream_event(&event, &mut acc);
        assert!(matches!(
            chunk,
            Some(StreamChunk::Reasoning { content, details: Some(details) })
                if content.is_empty()
                    && details == serde_json::json!([{"type": "summary", "text": "trace"}])
        ));
    }

    #[test]
    fn parse_stream_event_splits_reasoning_from_visible_text() {
        let event = SseEvent {
            event_type: None,
            data: r#"{"choices":[{"delta":{"reasoning":"private","content":"visible"},"finish_reason":null}]}"#.into(),
        };
        let mut acc = ToolCallAccumulator::new();
        let chunks = OpenAiProvider::parse_stream_event_chunks(&event, &mut acc);
        assert!(matches!(
            chunks.as_slice(),
            [
                StreamChunk::Reasoning {
                    content,
                    details: None
                },
                StreamChunk::Delta { content: visible }
            ] if content == "private" && visible == "visible"
        ));
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
    fn parse_stream_event_emits_all_accumulated_tool_calls_before_done() {
        let mut acc = ToolCallAccumulator::new();
        let calls = SseEvent {
            event_type: None,
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-a","function":{"name":"first","arguments":"{\"a\":"}},{"index":1,"id":"call-b","function":{"name":"second","arguments":"{\"b\":"}}]},"finish_reason":null}]}"#.into(),
        };
        assert!(OpenAiProvider::parse_stream_event_chunks(&calls, &mut acc).is_empty());

        let completed = SseEvent {
            event_type: None,
            data: r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}},{"index":1,"function":{"arguments":"2}"}}]},"finish_reason":"tool_calls"}]}"#.into(),
        };
        let chunks = OpenAiProvider::parse_stream_event_chunks(&completed, &mut acc);

        assert!(
            matches!(
                chunks.as_slice(),
                [
                    StreamChunk::ToolUse { call_id: first_id, name: first_name, arguments: first_args },
                    StreamChunk::ToolUse { call_id: second_id, name: second_name, arguments: second_args },
                    StreamChunk::Done { finish_reason: Some(FinishReason::ToolUse) }
                ] if first_id == "call-a"
                    && first_name == "first"
                && first_args == &serde_json::json!({"a": 1})
                    && second_id == "call-b"
                    && second_name == "second"
                && second_args == &serde_json::json!({"b": 2})
            ),
            "unexpected chunks: {chunks:?}"
        );
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
    fn parse_stream_event_with_finish_reason_and_usage_emits_usage_before_done() {
        let event = SseEvent {
            event_type: None,
            data: r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#.into(),
        };
        let mut acc = ToolCallAccumulator::new();
        let chunks = OpenAiProvider::parse_stream_event_chunks(&event, &mut acc);

        assert!(matches!(
            chunks.as_slice(),
            [
                StreamChunk::Usage(usage),
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::Stop)
                }
            ] if usage.input_tokens == 10 && usage.output_tokens == 5
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
