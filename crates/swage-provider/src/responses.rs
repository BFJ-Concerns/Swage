//! OpenAI Responses API provider.
//!
//! Speaks `POST /v1/responses` — the envelope GPT-family models are addressed
//! with by their native harness. Configure `protocol = "openai-responses"` on
//! an endpoint carrying GPT-family models (the operator-run proxy is the type
//! case) so requests reach the model in its trained shape; generic
//! OpenAI-compatible endpoints (Ollama, Azure, custom) stay on the Chat
//! Completions adapter in `openai.rs`, which remains the right envelope for
//! them.

use std::pin::Pin;

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use swage_core::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart, FinishReason, ProviderError,
    Role, StreamChunk, ToolCall, ToolCompletionRequest, ToolCompletionResponse, ToolReplayPart,
};

use crate::http::{HttpClient, SseEvent};
use crate::provider::{LlmProvider, ProviderConfig, spawn_stream_forwarder};

/// Enumerated divergences from the native GPT harness's Responses request,
/// recorded in the provider snapshot so every capture comparison reads them
/// beside the body. Each entry names its permitted category.
const RESPONSES_REQUEST_DIVERGENCES: &[(&str, &str)] = &[
    (
        "vendor_dynamic_context",
        "client_metadata and prompt_cache_key carry the native harness's per-session \
         identifiers, injected at run time",
    ),
    (
        "vendor_dynamic_context",
        "include: [reasoning.encrypted_content] and reasoning.context serve the native \
         harness's encrypted reasoning replay, which Swage has no counterpart to consume",
    ),
];

// ---------------------------------------------------------------------------
// Request wire types
// ---------------------------------------------------------------------------

/// Request body for POST /responses.
#[derive(Serialize)]
struct ResponsesRequest {
    model: String,
    input: Vec<ResponseItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ResponsesTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    /// The native GPT harness pins one tool call per step
    /// (native-gpt-5.6-luna.json in the parity evidence); matching it
    /// addresses the model in its trained shape.
    parallel_tool_calls: bool,
    /// Never persist responses vendor-side — the local record is the record.
    store: bool,
    /// The native harness requests low-verbosity output text.
    text: ResponsesTextOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ResponsesReasoning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Serialize)]
struct ResponsesTextOptions {
    verbosity: &'static str,
}

/// Reasoning controls. Only `effort` is sent: `context` pairs with the
/// native harness's encrypted reasoning replay, which Swage does not
/// consume, so sending it would be a guess (see the divergence register).
#[derive(Serialize)]
struct ResponsesReasoning {
    effort: String,
}

/// A tool definition in the Responses format — flattened, not nested under a
/// `function` object as Chat Completions has it.
#[derive(Serialize)]
struct ResponsesTool {
    #[serde(rename = "type")]
    tool_type: &'static str,
    name: String,
    description: String,
    parameters: Value,
}

/// One item of the request `input` list.
#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseItem {
    Message {
        role: &'static str,
        content: Vec<ResponseContentPart>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        /// Arguments as a JSON string, as the API round-trips them.
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

#[derive(Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseContentPart {
    InputText { text: String },
    OutputText { text: String },
}

// ---------------------------------------------------------------------------
// Response wire types
// ---------------------------------------------------------------------------

/// Top-level response from POST /responses. Output items are kept as raw
/// values because the API adds item types freely (reasoning, built-in tool
/// calls); unknown kinds must pass through harmlessly rather than fail the
/// whole parse.
#[derive(Deserialize, Debug)]
struct ResponsesResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    incomplete_details: Option<IncompleteDetails>,
    #[serde(default)]
    output: Vec<Value>,
    #[serde(default)]
    usage: Option<ResponsesUsage>,
    #[serde(default)]
    error: Option<ResponsesError>,
}

#[derive(Deserialize, Debug)]
struct IncompleteDetails {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

#[derive(Deserialize, Debug)]
struct ResponsesError {
    #[serde(default)]
    message: String,
}

// ---------------------------------------------------------------------------
// ResponsesProvider
// ---------------------------------------------------------------------------

/// LLM provider for the OpenAI Responses API.
pub struct ResponsesProvider {
    config: ProviderConfig,
}

impl ResponsesProvider {
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

    fn make_http_client(&self) -> HttpClient {
        HttpClient::new(self.config.base_url.clone(), self.build_headers())
    }

    /// Convert `ChatMessage` list to Responses input items.
    fn convert_messages(messages: &[ChatMessage]) -> Vec<ResponseItem> {
        messages.iter().flat_map(Self::convert_message).collect()
    }

    fn convert_message(message: &ChatMessage) -> Vec<ResponseItem> {
        if let Some(replay) = message.tool_replay()
            && let Some(items) = Self::convert_tool_replay_message(message, replay)
        {
            return items;
        }

        // The native GPT harness addresses instruction-role content as
        // "developer" (native-gpt-5.6-luna.json in the parity evidence).
        let role = match message.role() {
            Role::System => "developer",
            Role::User => "user",
            Role::Assistant => "assistant",
            // A bare tool message becomes a function-call output item below.
            Role::Tool => {
                if let Some(call_id) = message.tool_call_id() {
                    return vec![ResponseItem::FunctionCallOutput {
                        call_id: call_id.to_string(),
                        output: Self::text_content(message),
                    }];
                }
                return Vec::new();
            }
        };

        let mut items = Vec::new();
        let text = Self::text_content(message);
        if !text.is_empty() {
            let part = if message.role() == Role::Assistant {
                ResponseContentPart::OutputText { text }
            } else {
                ResponseContentPart::InputText { text }
            };
            items.push(ResponseItem::Message {
                role,
                content: vec![part],
            });
        }

        if let Some(calls) = message.tool_calls() {
            for call in calls {
                items.push(ResponseItem::FunctionCall {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    arguments: serde_json::to_string(&call.arguments).unwrap_or_default(),
                });
            }
        }

        items
    }

    fn text_content(message: &ChatMessage) -> String {
        message
            .content()
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    fn convert_tool_replay_message(
        message: &ChatMessage,
        replay: &[ToolReplayPart],
    ) -> Option<Vec<ResponseItem>> {
        match message.role() {
            Role::Assistant => {
                let text: String = replay
                    .iter()
                    .filter_map(|part| match part {
                        ToolReplayPart::Text { text }
                        | ToolReplayPart::Reasoning { content: text, .. } => Some(text.as_str()),
                        ToolReplayPart::ToolCall { .. } | ToolReplayPart::ToolResult { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");

                let mut items = Vec::new();
                if !text.is_empty() {
                    items.push(ResponseItem::Message {
                        role: "assistant",
                        content: vec![ResponseContentPart::OutputText { text }],
                    });
                }
                for part in replay {
                    if let ToolReplayPart::ToolCall {
                        call_id,
                        name,
                        arguments,
                    } = part
                    {
                        items.push(ResponseItem::FunctionCall {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            arguments: serde_json::to_string(arguments).ok()?,
                        });
                    }
                }
                Some(items)
            }
            Role::User => {
                let outputs: Vec<ResponseItem> = replay
                    .iter()
                    .filter_map(|part| match part {
                        ToolReplayPart::ToolResult {
                            call_id, content, ..
                        } => Some(ResponseItem::FunctionCallOutput {
                            call_id: call_id.clone(),
                            output: content.clone(),
                        }),
                        ToolReplayPart::Text { .. }
                        | ToolReplayPart::Reasoning { .. }
                        | ToolReplayPart::ToolCall { .. } => None,
                    })
                    .collect();

                if outputs.is_empty() {
                    None
                } else {
                    Some(outputs)
                }
            }
            Role::System | Role::Tool => None,
        }
    }

    fn convert_tool_definitions(definitions: &[Value]) -> Vec<ResponsesTool> {
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

                Some(ResponsesTool {
                    tool_type: "function",
                    name,
                    description,
                    parameters,
                })
            })
            .collect()
    }

    /// Build the request body for a completion.
    ///
    /// `stop_sequences` has no Responses API mapping and is omitted.
    fn build_request(&self, request: &CompletionRequest, stream: bool) -> ResponsesRequest {
        let model = request
            .model
            .as_deref()
            .unwrap_or(&self.config.model)
            .to_string();
        let tools = Self::convert_tool_definitions(&request.tool_definitions);

        ResponsesRequest {
            model,
            input: Self::convert_messages(&request.messages),
            tool_choice: if tools.is_empty() {
                None
            } else {
                Some("auto".to_string())
            },
            tools,
            parallel_tool_calls: false,
            store: false,
            text: ResponsesTextOptions { verbosity: "low" },
            reasoning: request
                .effort
                .clone()
                .map(|effort| ResponsesReasoning { effort }),
            max_output_tokens: request.max_tokens,
            temperature: request.temperature,
            stream: if stream { Some(true) } else { None },
        }
    }

    /// Build the request body for a tool-calling completion.
    fn build_tool_request(
        &self,
        request: &ToolCompletionRequest,
        stream: bool,
    ) -> ResponsesRequest {
        let model = request
            .model
            .as_deref()
            .unwrap_or(&self.config.model)
            .to_string();
        let tools = Self::convert_tool_definitions(&request.tool_definitions);

        ResponsesRequest {
            model,
            input: Self::convert_messages(&request.messages),
            tool_choice: if tools.is_empty() {
                None
            } else {
                Some("auto".to_string())
            },
            tools,
            parallel_tool_calls: false,
            store: false,
            text: ResponsesTextOptions { verbosity: "low" },
            // ToolCompletionRequest has no per-call effort surface.
            reasoning: None,
            max_output_tokens: request.max_tokens,
            temperature: request.temperature,
            stream: if stream { Some(true) } else { None },
        }
    }

    /// Collect visible text and complete function calls from response output
    /// items, passing unknown item kinds through harmlessly.
    fn extract_output(items: &[Value]) -> (String, Vec<ToolCall>) {
        let mut text = String::new();
        let mut calls = Vec::new();
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if let Some(parts) = item.get("content").and_then(Value::as_array) {
                        for part in parts {
                            if part.get("type").and_then(Value::as_str) == Some("output_text")
                                && let Some(part_text) = part.get("text").and_then(Value::as_str)
                            {
                                text.push_str(part_text);
                            }
                        }
                    }
                }
                Some("function_call") => {
                    if let Some(call) = Self::parse_function_call_item(item) {
                        calls.push(call);
                    }
                }
                _ => {}
            }
        }
        (text, calls)
    }

    fn parse_function_call_item(item: &Value) -> Option<ToolCall> {
        let name = item.get("name")?.as_str()?.to_string();
        let call_id = item.get("call_id")?.as_str()?.to_string();
        let arguments = item
            .get("arguments")
            .and_then(Value::as_str)
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        Some(ToolCall {
            call_id,
            name,
            arguments,
        })
    }

    fn finish_reason(
        status: Option<&str>,
        incomplete_reason: Option<&str>,
        has_tool_calls: bool,
    ) -> FinishReason {
        if has_tool_calls {
            return FinishReason::ToolUse;
        }
        match (status, incomplete_reason) {
            (Some("completed"), _) => FinishReason::Stop,
            (Some("incomplete"), Some("max_output_tokens")) => FinishReason::Length,
            (Some("incomplete"), Some("content_filter")) => FinishReason::ContentFilter,
            _ => FinishReason::Unknown,
        }
    }

    /// Parse one typed SSE event into stream chunks.
    ///
    /// The Responses stream names every event; a complete function call
    /// arrives whole on `response.output_item.done`, so there is no
    /// cross-chunk accumulation. `saw_tool_call` shapes the terminal finish
    /// reason.
    fn parse_stream_event_chunks(event: &SseEvent, saw_tool_call: &mut bool) -> Vec<StreamChunk> {
        let Some(kind) = event.event_type.as_deref() else {
            return Vec::new();
        };
        match kind {
            "response.output_text.delta" => match serde_json::from_str::<Value>(&event.data) {
                Ok(payload) => payload
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|delta| !delta.is_empty())
                    .map(|delta| {
                        vec![StreamChunk::Delta {
                            content: delta.to_string(),
                        }]
                    })
                    .unwrap_or_default(),
                Err(error) => vec![Self::parse_error(error)],
            },
            "response.reasoning_summary_text.delta" => {
                match serde_json::from_str::<Value>(&event.data) {
                    Ok(payload) => payload
                        .get("delta")
                        .and_then(Value::as_str)
                        .filter(|delta| !delta.is_empty())
                        .map(|delta| {
                            vec![StreamChunk::Reasoning {
                                content: delta.to_string(),
                                details: None,
                            }]
                        })
                        .unwrap_or_default(),
                    Err(error) => vec![Self::parse_error(error)],
                }
            }
            "response.output_item.done" => match serde_json::from_str::<Value>(&event.data) {
                Ok(payload) => {
                    let Some(item) = payload.get("item") else {
                        return Vec::new();
                    };
                    if item.get("type").and_then(Value::as_str) != Some("function_call") {
                        return Vec::new();
                    }
                    match Self::parse_function_call_item(item) {
                        Some(call) => {
                            *saw_tool_call = true;
                            vec![StreamChunk::ToolUse {
                                call_id: call.call_id,
                                name: call.name,
                                arguments: call.arguments,
                            }]
                        }
                        None => Vec::new(),
                    }
                }
                Err(error) => vec![Self::parse_error(error)],
            },
            "response.completed" | "response.incomplete" | "response.failed" => {
                let response = match serde_json::from_str::<Value>(&event.data) {
                    Ok(payload) => match payload.get("response") {
                        Some(response) => response.clone(),
                        None => return vec![Self::parse_error("event carries no response")],
                    },
                    Err(error) => return vec![Self::parse_error(error)],
                };
                if kind == "response.failed" {
                    let message = response
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("response failed without a message");
                    return vec![StreamChunk::Error {
                        message: message.to_string(),
                    }];
                }
                let mut chunks = Vec::new();
                if let Some(usage) = response.get("usage") {
                    chunks.push(StreamChunk::Usage(swage_core::TokenUsage {
                        input_tokens: usage
                            .get("input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as u32,
                        output_tokens: usage
                            .get("output_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as u32,
                    }));
                }
                let status = response.get("status").and_then(Value::as_str);
                let incomplete_reason = response
                    .pointer("/incomplete_details/reason")
                    .and_then(Value::as_str);
                chunks.push(StreamChunk::Done {
                    finish_reason: Some(Self::finish_reason(
                        status,
                        incomplete_reason,
                        *saw_tool_call,
                    )),
                });
                chunks
            }
            "error" => {
                let message = serde_json::from_str::<Value>(&event.data)
                    .ok()
                    .and_then(|payload| {
                        payload
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "provider stream error".to_string());
                vec![StreamChunk::Error { message }]
            }
            _ => Vec::new(),
        }
    }

    fn parse_error(error: impl std::fmt::Display) -> StreamChunk {
        StreamChunk::Error {
            message: format!("failed to parse Responses SSE payload: {error}"),
        }
    }
}

// ---------------------------------------------------------------------------
// LlmProvider implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmProvider for ResponsesProvider {
    fn model_id(&self) -> &str {
        &self.config.model
    }

    fn provider_request_snapshot(&self, request: &CompletionRequest, stream: bool) -> Value {
        serde_json::json!({
            "provider": "openai",
            "protocol": "responses",
            "stream": stream,
            "body": self.build_request(request, stream),
            "divergences": RESPONSES_REQUEST_DIVERGENCES,
        })
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let body = self.build_request(&request, false);
        let client = self.make_http_client();

        let response: ResponsesResponse = client.post_json("/responses", &body).await?;
        if let Some(error) = response.error {
            return Err(ProviderError::InvalidResponse(error.message));
        }

        let (content, calls) = Self::extract_output(&response.output);
        let usage = response.usage.unwrap_or(ResponsesUsage {
            input_tokens: 0,
            output_tokens: 0,
        });

        Ok(CompletionResponse {
            content,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            finish_reason: Self::finish_reason(
                response.status.as_deref(),
                response
                    .incomplete_details
                    .as_ref()
                    .and_then(|details| details.reason.as_deref()),
                !calls.is_empty(),
            ),
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        let body = self.build_tool_request(&request, false);
        let client = self.make_http_client();

        let response: ResponsesResponse = client.post_json("/responses", &body).await?;
        if let Some(error) = response.error {
            return Err(ProviderError::InvalidResponse(error.message));
        }

        let (content, tool_calls) = Self::extract_output(&response.output);
        let usage = response.usage.unwrap_or(ResponsesUsage {
            input_tokens: 0,
            output_tokens: 0,
        });

        Ok(ToolCompletionResponse {
            content,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            finish_reason: Self::finish_reason(
                response.status.as_deref(),
                response
                    .incomplete_details
                    .as_ref()
                    .and_then(|details| details.reason.as_deref()),
                !tool_calls.is_empty(),
            ),
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
                .post_stream("/responses", &body, HeaderMap::new())
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
            let mut saw_tool_call = false;

            'events: while let Some(result) = sse_stream.next().await {
                let event = match result {
                    Err(e) => {
                        let _ = tx.send(StreamChunk::Error {
                            message: e.to_string(),
                        });
                        break;
                    }
                    Ok(ev) => ev,
                };

                for chunk in
                    ResponsesProvider::parse_stream_event_chunks(&event, &mut saw_tool_call)
                {
                    let is_done = matches!(chunk, StreamChunk::Done { .. });
                    let _ = tx.send(chunk);
                    if is_done {
                        break 'events;
                    }
                }
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
    use swage_core::{ChatMessage, Role, ToolCall};

    fn completion_request() -> CompletionRequest {
        CompletionRequest {
            effort: None,
            messages: vec![],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        }
    }

    // --- Message conversion ---

    #[test]
    fn convert_system_message_lands_as_developer_input_text() {
        let messages = vec![ChatMessage::new(Role::System, "You are helpful")];
        let items = ResponsesProvider::convert_messages(&messages);
        let json = serde_json::to_value(&items).unwrap();
        assert_eq!(json[0]["type"], "message");
        assert_eq!(json[0]["role"], "developer");
        assert_eq!(json[0]["content"][0]["type"], "input_text");
        assert_eq!(json[0]["content"][0]["text"], "You are helpful");
    }

    #[test]
    fn convert_assistant_message_uses_output_text() {
        let messages = vec![ChatMessage::new(Role::Assistant, "Hi there")];
        let json = serde_json::to_value(ResponsesProvider::convert_messages(&messages)).unwrap();
        assert_eq!(json[0]["role"], "assistant");
        assert_eq!(json[0]["content"][0]["type"], "output_text");
    }

    #[test]
    fn convert_tool_result_becomes_function_call_output() {
        let msg = ChatMessage::tool_result("call_abc", "result text");
        let json = serde_json::to_value(ResponsesProvider::convert_messages(&[msg])).unwrap();
        assert_eq!(json[0]["type"], "function_call_output");
        assert_eq!(json[0]["call_id"], "call_abc");
        assert_eq!(json[0]["output"], "result text");
    }

    #[test]
    fn convert_assistant_tool_calls_become_function_call_items() {
        let msg = ChatMessage::assistant_tool_calls(
            "",
            vec![ToolCall {
                call_id: "call_1".into(),
                name: "search".into(),
                arguments: serde_json::json!({"query": "rust"}),
            }],
        );
        let json = serde_json::to_value(ResponsesProvider::convert_messages(&[msg])).unwrap();
        assert_eq!(json[0]["type"], "function_call");
        assert_eq!(json[0]["call_id"], "call_1");
        assert_eq!(json[0]["name"], "search");
        assert!(json[0]["arguments"].as_str().unwrap().contains("rust"));
    }

    #[test]
    fn convert_tool_round_replay_to_items() {
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
        let json = serde_json::to_value(ResponsesProvider::convert_messages(&messages)).unwrap();
        assert_eq!(json[0]["type"], "function_call");
        assert_eq!(json[0]["name"], "shell");
        assert_eq!(json[0]["arguments"], "{\"command\":\"pwd\"}");
        assert_eq!(json[1]["type"], "function_call_output");
        assert_eq!(json[1]["call_id"], "call_1");
        assert_eq!(json[1]["output"], "/workspace");
    }

    // --- Request building ---

    #[test]
    fn build_request_pins_the_native_envelope_constants() {
        let provider =
            ResponsesProvider::from_config(ProviderConfig::new("key", "model", "http://localhost"));
        let body =
            serde_json::to_value(provider.build_request(&completion_request(), false)).unwrap();
        assert_eq!(body["parallel_tool_calls"], false);
        assert_eq!(body["store"], false);
        assert_eq!(body["text"]["verbosity"], "low");
        assert!(body.get("stop").is_none());
    }

    #[test]
    fn build_request_maps_effort_into_reasoning() {
        let provider =
            ResponsesProvider::from_config(ProviderConfig::new("key", "model", "http://localhost"));
        let request = CompletionRequest {
            effort: Some("high".into()),
            ..completion_request()
        };
        let body = serde_json::to_value(provider.build_request(&request, false)).unwrap();
        assert_eq!(body["reasoning"]["effort"], "high");
        assert!(body["reasoning"].get("context").is_none());
    }

    #[test]
    fn build_request_omits_reasoning_when_no_effort_is_configured() {
        let provider =
            ResponsesProvider::from_config(ProviderConfig::new("key", "model", "http://localhost"));
        let body =
            serde_json::to_value(provider.build_request(&completion_request(), false)).unwrap();
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn build_request_maps_max_tokens_to_max_output_tokens() {
        let provider =
            ResponsesProvider::from_config(ProviderConfig::new("key", "model", "http://localhost"));
        let request = CompletionRequest {
            max_tokens: Some(2048),
            ..completion_request()
        };
        let body = serde_json::to_value(provider.build_request(&request, false)).unwrap();
        assert_eq!(body["max_output_tokens"], 2048);
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn build_request_flattens_tools_and_sets_auto_choice() {
        let provider =
            ResponsesProvider::from_config(ProviderConfig::new("key", "model", "http://localhost"));
        let request = CompletionRequest {
            tool_definitions: vec![serde_json::json!({
                "name": "ping",
                "description": "Echoes the input value",
                "parameters": {
                    "type": "object",
                    "properties": { "value": { "type": "string" } },
                    "required": ["value"]
                }
            })],
            ..completion_request()
        };
        let body = serde_json::to_value(provider.build_request(&request, false)).unwrap();
        // Flattened: name at the tool's top level, no nested `function` object.
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "ping");
        assert!(body["tools"][0].get("function").is_none());
        assert_eq!(body["tool_choice"], "auto");
    }

    #[test]
    fn build_request_sets_stream_flag_only_when_streaming() {
        let provider =
            ResponsesProvider::from_config(ProviderConfig::new("key", "model", "http://localhost"));
        let non_streaming =
            serde_json::to_value(provider.build_request(&completion_request(), false)).unwrap();
        assert!(non_streaming.get("stream").is_none());
        let streaming =
            serde_json::to_value(provider.build_request(&completion_request(), true)).unwrap();
        assert_eq!(streaming["stream"], true);
    }

    #[test]
    fn build_request_uses_model_from_request_when_present() {
        let provider = ResponsesProvider::from_config(ProviderConfig::new(
            "key",
            "default-model",
            "http://localhost",
        ));
        let request = CompletionRequest {
            model: Some("custom-model".into()),
            ..completion_request()
        };
        let body = provider.build_request(&request, false);
        assert_eq!(body.model, "custom-model");
    }

    #[test]
    fn provider_request_snapshot_names_the_responses_protocol_and_register() {
        let provider =
            ResponsesProvider::from_config(ProviderConfig::new("key", "model", "http://localhost"));
        let snapshot = provider.provider_request_snapshot(&completion_request(), true);
        assert_eq!(snapshot["provider"], "openai");
        assert_eq!(snapshot["protocol"], "responses");
        assert_eq!(snapshot["stream"], true);
        assert!(
            snapshot["divergences"]
                .as_array()
                .is_some_and(|entries| !entries.is_empty())
        );
    }

    // --- Output extraction ---

    #[test]
    fn extract_output_collects_text_and_function_calls_and_skips_unknown_items() {
        let items = vec![
            serde_json::json!({ "type": "reasoning", "summary": [] }),
            serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": [
                    { "type": "output_text", "text": "Hello " },
                    { "type": "output_text", "text": "world" }
                ]
            }),
            serde_json::json!({
                "type": "function_call",
                "call_id": "call_1",
                "name": "search",
                "arguments": "{\"query\":\"rust\"}"
            }),
            serde_json::json!({ "type": "web_search_call", "status": "completed" }),
        ];
        let (text, calls) = ResponsesProvider::extract_output(&items);
        assert_eq!(text, "Hello world");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_id, "call_1");
        assert_eq!(calls[0].arguments["query"], "rust");
    }

    #[test]
    fn extract_output_normalises_malformed_arguments_to_empty_object() {
        let items = vec![serde_json::json!({
            "type": "function_call",
            "call_id": "call_bad",
            "name": "tool",
            "arguments": "not valid json"
        })];
        let (_, calls) = ResponsesProvider::extract_output(&items);
        assert!(calls[0].arguments.is_object());
    }

    // --- Finish reason ---

    #[test]
    fn finish_reason_maps_status_and_tool_calls() {
        assert_eq!(
            ResponsesProvider::finish_reason(Some("completed"), None, false),
            FinishReason::Stop
        );
        assert_eq!(
            ResponsesProvider::finish_reason(Some("incomplete"), Some("max_output_tokens"), false),
            FinishReason::Length
        );
        assert_eq!(
            ResponsesProvider::finish_reason(Some("incomplete"), Some("content_filter"), false),
            FinishReason::ContentFilter
        );
        assert_eq!(
            ResponsesProvider::finish_reason(Some("completed"), None, true),
            FinishReason::ToolUse
        );
        assert_eq!(
            ResponsesProvider::finish_reason(None, None, false),
            FinishReason::Unknown
        );
    }

    // --- Streaming event parsing ---

    fn event(kind: &str, data: &str) -> SseEvent {
        SseEvent {
            event_type: Some(kind.to_string()),
            data: data.to_string(),
        }
    }

    #[test]
    fn parse_stream_output_text_delta() {
        let mut saw = false;
        let chunks = ResponsesProvider::parse_stream_event_chunks(
            &event("response.output_text.delta", r#"{"delta":"Hello"}"#),
            &mut saw,
        );
        assert!(
            matches!(chunks.as_slice(), [StreamChunk::Delta { content }] if content == "Hello")
        );
    }

    #[test]
    fn parse_stream_reasoning_summary_delta() {
        let mut saw = false;
        let chunks = ResponsesProvider::parse_stream_event_chunks(
            &event(
                "response.reasoning_summary_text.delta",
                r#"{"delta":"thinking"}"#,
            ),
            &mut saw,
        );
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Reasoning { content, details: None }] if content == "thinking"
        ));
    }

    #[test]
    fn parse_stream_completed_function_call_item_emits_tool_use() {
        let mut saw = false;
        let chunks = ResponsesProvider::parse_stream_event_chunks(
            &event(
                "response.output_item.done",
                r#"{"item":{"type":"function_call","call_id":"call_1","name":"search","arguments":"{\"query\":\"rust\"}"}}"#,
            ),
            &mut saw,
        );
        assert!(saw);
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::ToolUse { call_id, name, arguments }]
                if call_id == "call_1" && name == "search" && arguments["query"] == "rust"
        ));
    }

    #[test]
    fn parse_stream_non_function_item_done_is_ignored() {
        let mut saw = false;
        let chunks = ResponsesProvider::parse_stream_event_chunks(
            &event(
                "response.output_item.done",
                r#"{"item":{"type":"message","content":[]}}"#,
            ),
            &mut saw,
        );
        assert!(chunks.is_empty());
        assert!(!saw);
    }

    #[test]
    fn parse_stream_completed_emits_usage_then_done_with_stop() {
        let mut saw = false;
        let chunks = ResponsesProvider::parse_stream_event_chunks(
            &event(
                "response.completed",
                r#"{"response":{"status":"completed","usage":{"input_tokens":10,"output_tokens":5}}}"#,
            ),
            &mut saw,
        );
        assert!(matches!(
            chunks.as_slice(),
            [
                StreamChunk::Usage(usage),
                StreamChunk::Done { finish_reason: Some(FinishReason::Stop) }
            ] if usage.input_tokens == 10 && usage.output_tokens == 5
        ));
    }

    #[test]
    fn parse_stream_completed_after_tool_call_finishes_with_tool_use() {
        let mut saw = true;
        let chunks = ResponsesProvider::parse_stream_event_chunks(
            &event(
                "response.completed",
                r#"{"response":{"status":"completed"}}"#,
            ),
            &mut saw,
        );
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Done {
                finish_reason: Some(FinishReason::ToolUse)
            }]
        ));
    }

    #[test]
    fn parse_stream_incomplete_maps_max_output_tokens_to_length() {
        let mut saw = false;
        let chunks = ResponsesProvider::parse_stream_event_chunks(
            &event(
                "response.incomplete",
                r#"{"response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}}"#,
            ),
            &mut saw,
        );
        assert!(matches!(
            chunks.last(),
            Some(StreamChunk::Done {
                finish_reason: Some(FinishReason::Length)
            })
        ));
    }

    #[test]
    fn parse_stream_failed_reports_the_error() {
        let mut saw = false;
        let chunks = ResponsesProvider::parse_stream_event_chunks(
            &event(
                "response.failed",
                r#"{"response":{"status":"failed","error":{"message":"quota exhausted"}}}"#,
            ),
            &mut saw,
        );
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Error { message }] if message == "quota exhausted"
        ));
    }

    #[test]
    fn parse_stream_error_event_reports_the_message() {
        let mut saw = false;
        let chunks = ResponsesProvider::parse_stream_event_chunks(
            &event("error", r#"{"message":"bad request"}"#),
            &mut saw,
        );
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Error { message }] if message == "bad request"
        ));
    }

    #[test]
    fn parse_stream_unknown_event_kinds_are_ignored() {
        let mut saw = false;
        for kind in [
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.function_call_arguments.delta",
            "response.content_part.added",
        ] {
            assert!(
                ResponsesProvider::parse_stream_event_chunks(&event(kind, "{}"), &mut saw)
                    .is_empty(),
                "{kind} should be ignored"
            );
        }
    }

    #[test]
    fn parse_stream_malformed_payload_reports_parse_error() {
        let mut saw = false;
        let chunks = ResponsesProvider::parse_stream_event_chunks(
            &event("response.output_text.delta", "{not-json"),
            &mut saw,
        );
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Error { message }]
                if message.contains("failed to parse Responses SSE payload")
        ));
    }

    #[test]
    fn parse_stream_untyped_event_is_ignored() {
        let mut saw = false;
        let untyped = SseEvent {
            event_type: None,
            data: r#"{"delta":"x"}"#.into(),
        };
        assert!(ResponsesProvider::parse_stream_event_chunks(&untyped, &mut saw).is_empty());
    }

    // --- Response deserialisation ---

    #[test]
    fn deserialise_response_with_message_and_usage() {
        let json = r#"{
            "status": "completed",
            "output": [
                { "type": "message", "role": "assistant",
                  "content": [{ "type": "output_text", "text": "Hello!" }] }
            ],
            "usage": { "input_tokens": 10, "output_tokens": 5 }
        }"#;
        let response: ResponsesResponse = serde_json::from_str(json).unwrap();
        let (text, calls) = ResponsesProvider::extract_output(&response.output);
        assert_eq!(text, "Hello!");
        assert!(calls.is_empty());
        assert_eq!(response.usage.unwrap().input_tokens, 10);
    }

    #[test]
    fn deserialise_response_without_usage() {
        let json = r#"{ "status": "completed", "output": [] }"#;
        let response: ResponsesResponse = serde_json::from_str(json).unwrap();
        assert!(response.usage.is_none());
    }
}
