//! Google Gemini LLM provider.
//!
//! Translates Swage types to Google's `generateContent` format and handles
//! Google-specific conventions: `system_instruction` field, `parts` structure,
//! and API key as query parameter.

use std::pin::Pin;

use async_trait::async_trait;
use futures::channel::mpsc;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use swage_core::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart, FinishReason, ProviderError,
    Role, StreamChunk, ToolCall, ToolCompletionRequest, ToolCompletionResponse,
};

use crate::provider::{LlmProvider, ProviderConfig};

// ---------------------------------------------------------------------------
// Google Gemini wire types
// ---------------------------------------------------------------------------

/// Request body for POST /v1beta/models/{model}:generateContent.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct GenerateContentRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<GeminiTool>,
}

/// A content block in the Gemini format.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GeminiPart>,
}

/// A single part within a Gemini content block.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<GeminiFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_response: Option<GeminiFunctionResponse>,
}

/// A function call issued by the model.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct GeminiFunctionCall {
    name: String,
    #[serde(default)]
    args: serde_json::Value,
}

/// A function response provided by the caller.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct GeminiFunctionResponse {
    name: String,
    response: serde_json::Value,
}

/// Generation configuration (temperature, max tokens, stop sequences).
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
}

/// Tool definition for function calling.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct GeminiTool {
    function_declarations: Vec<GeminiFunctionDeclaration>,
}

/// A function declaration within a tool.
#[derive(Serialize, Debug)]
struct GeminiFunctionDeclaration {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// Response from generateContent.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct GenerateContentResponse {
    #[serde(default)]
    candidates: Vec<Candidate>,
    #[serde(default)]
    usage_metadata: Option<UsageMetadata>,
}

/// A single candidate response.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    content: Option<GeminiContent>,
    #[serde(default)]
    finish_reason: Option<String>,
}

/// Token usage metadata.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct UsageMetadata {
    #[serde(default)]
    prompt_token_count: u32,
    #[serde(default)]
    candidates_token_count: u32,
}

// ---------------------------------------------------------------------------
// GoogleProvider
// ---------------------------------------------------------------------------

/// LLM provider for the Google Gemini API.
///
/// Key differences from OpenAI:
/// - API key is passed as a URL query parameter, not a header
/// - System messages go in a dedicated `system_instruction` field
/// - Streaming uses a separate endpoint (`streamGenerateContent`)
/// - The response format uses `candidates[].content.parts[]` instead of `choices[].message`
pub struct GoogleProvider {
    config: ProviderConfig,
}

impl GoogleProvider {
    /// Create a provider from a resolved configuration.
    pub fn from_config(config: ProviderConfig) -> Self {
        Self { config }
    }

    /// Build the generateContent URL for a given model, appending the API key.
    ///
    /// CRITICAL: The returned URL contains the API key — never log it directly.
    fn build_url(&self, model: &str, stream: bool) -> String {
        let endpoint = if stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };

        let key = self.config.api_key.as_deref().unwrap_or("");

        let mut url = format!(
            "{}/v1beta/models/{}:{}?key={}",
            self.config.base_url, model, endpoint, key,
        );

        // Streaming uses SSE via the alt=sse query parameter.
        if stream {
            url.push_str("&alt=sse");
        }

        url
    }

    /// Build a sanitised URL for logging (API key redacted).
    fn sanitised_url(model: &str, base_url: &str, stream: bool) -> String {
        let endpoint = if stream {
            "streamGenerateContent"
        } else {
            "generateContent"
        };
        format!(
            "{}/v1beta/models/{}:{}?key=REDACTED",
            base_url, model, endpoint
        )
    }

    fn redact_api_key_in_text(&self, text: impl AsRef<str>) -> String {
        Self::redact_api_key(text.as_ref(), self.config.api_key.as_deref())
    }

    fn redact_api_key(text: &str, api_key: Option<&str>) -> String {
        let mut redacted = text.to_string();
        if let Some(api_key) = api_key {
            if !api_key.is_empty() {
                redacted = redacted.replace(api_key, "REDACTED");
            }
        }
        redacted
    }

    /// Extract system messages from the conversation and return them as
    /// a single `system_instruction`, plus the remaining non-system messages.
    fn extract_system(messages: &[ChatMessage]) -> (Option<GeminiContent>, Vec<&ChatMessage>) {
        let mut system_texts = Vec::new();
        let mut remaining = Vec::new();

        for msg in messages {
            if msg.role == Role::System {
                let text: String = msg
                    .content
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    system_texts.push(text);
                }
            } else {
                remaining.push(msg);
            }
        }

        let system_instruction = if system_texts.is_empty() {
            None
        } else {
            Some(GeminiContent {
                role: None,
                parts: vec![GeminiPart {
                    text: Some(system_texts.join("\n\n")),
                    function_call: None,
                    function_response: None,
                }],
            })
        };

        (system_instruction, remaining)
    }

    /// Convert non-system ChatMessages to Gemini contents array.
    fn convert_messages(messages: &[&ChatMessage]) -> Vec<GeminiContent> {
        messages
            .iter()
            .map(|msg| {
                let role = match msg.role {
                    Role::User | Role::Tool => "user",
                    Role::Assistant => "model",
                    Role::System => "user", // Should not happen — system is extracted
                };

                let mut parts = Vec::new();

                // Tool result messages produce function_response parts.
                if msg.role == Role::Tool {
                    if let Some(ref call_id) = msg.tool_call_id {
                        let text: String = msg
                            .content
                            .iter()
                            .filter_map(|p| match p {
                                ContentPart::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("");
                        parts.push(GeminiPart {
                            text: None,
                            function_call: None,
                            function_response: Some(GeminiFunctionResponse {
                                name: call_id.clone(),
                                response: serde_json::json!({ "result": text }),
                            }),
                        });
                    }
                } else {
                    // Regular text parts.
                    for part in &msg.content {
                        match part {
                            ContentPart::Text { text } => {
                                if !text.is_empty() {
                                    parts.push(GeminiPart {
                                        text: Some(text.clone()),
                                        function_call: None,
                                        function_response: None,
                                    });
                                }
                            }
                            _ => {} // Image URLs not yet supported for Gemini.
                        }
                    }

                    // Assistant messages may include tool calls as function_call parts.
                    if let Some(ref calls) = msg.tool_calls {
                        for tc in calls {
                            parts.push(GeminiPart {
                                text: None,
                                function_call: Some(GeminiFunctionCall {
                                    name: tc.name.clone(),
                                    args: tc.arguments.clone(),
                                }),
                                function_response: None,
                            });
                        }
                    }
                }

                // Ensure at least one part — Gemini rejects empty content.
                if parts.is_empty() {
                    parts.push(GeminiPart {
                        text: Some(String::new()),
                        function_call: None,
                        function_response: None,
                    });
                }

                GeminiContent {
                    role: Some(role.to_string()),
                    parts,
                }
            })
            .collect()
    }

    /// Build the request body for a text-only completion.
    fn build_request(&self, request: &CompletionRequest) -> GenerateContentRequest {
        let (system_instruction, remaining) = Self::extract_system(&request.messages);
        let contents = Self::convert_messages(&remaining);

        let generation_config = if request.max_tokens.is_some()
            || request.temperature.is_some()
            || request.stop_sequences.is_some()
        {
            Some(GenerationConfig {
                max_output_tokens: request.max_tokens,
                temperature: request.temperature,
                stop_sequences: request.stop_sequences.clone(),
            })
        } else {
            None
        };

        GenerateContentRequest {
            contents,
            system_instruction,
            generation_config,
            tools: Vec::new(),
        }
    }

    /// Build the request body for a tool-calling completion.
    fn build_tool_request(&self, request: &ToolCompletionRequest) -> GenerateContentRequest {
        let (system_instruction, remaining) = Self::extract_system(&request.messages);
        let contents = Self::convert_messages(&remaining);

        let generation_config = if request.max_tokens.is_some()
            || request.temperature.is_some()
            || request.stop_sequences.is_some()
        {
            Some(GenerationConfig {
                max_output_tokens: request.max_tokens,
                temperature: request.temperature,
                stop_sequences: request.stop_sequences.clone(),
            })
        } else {
            None
        };

        // Convert tool definitions to Gemini function declarations.
        let declarations: Vec<GeminiFunctionDeclaration> = request
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
                Some(GeminiFunctionDeclaration {
                    name,
                    description,
                    parameters,
                })
            })
            .collect();

        let tools = if declarations.is_empty() {
            Vec::new()
        } else {
            vec![GeminiTool {
                function_declarations: declarations,
            }]
        };

        GenerateContentRequest {
            contents,
            system_instruction,
            generation_config,
            tools,
        }
    }

    /// Parse the Gemini finish reason string.
    fn parse_finish_reason(reason: Option<&str>) -> FinishReason {
        match reason {
            Some("STOP") => FinishReason::Stop,
            Some("MAX_TOKENS") => FinishReason::Length,
            Some("SAFETY") => FinishReason::ContentFilter,
            Some("RECITATION") => FinishReason::ContentFilter,
            _ => FinishReason::Unknown,
        }
    }

    /// Parse a Gemini response into a CompletionResponse.
    fn parse_response(
        response: &GenerateContentResponse,
    ) -> Result<(String, Vec<ToolCall>, FinishReason), ProviderError> {
        let candidate = response.candidates.first().ok_or_else(|| {
            ProviderError::InvalidResponse("no candidates in response".to_string())
        })?;

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        if let Some(ref content) = candidate.content {
            for part in &content.parts {
                if let Some(ref text) = part.text {
                    text_parts.push(text.as_str());
                }
                if let Some(ref fc) = part.function_call {
                    tool_calls.push(ToolCall {
                        // Gemini doesn't provide call IDs — generate a
                        // deterministic one from the function name.
                        call_id: format!("gemini_{}", fc.name),
                        name: fc.name.clone(),
                        arguments: fc.args.clone(),
                    });
                }
            }
        }

        let finish_reason = Self::parse_finish_reason(candidate.finish_reason.as_deref());

        // If there are tool calls but finish_reason isn't already ToolUse,
        // override it — Gemini uses STOP even when issuing function calls.
        let finish_reason = if !tool_calls.is_empty() && finish_reason != FinishReason::ToolUse {
            FinishReason::ToolUse
        } else {
            finish_reason
        };

        Ok((text_parts.join(""), tool_calls, finish_reason))
    }

    /// Parse a streaming SSE event from the Gemini API.
    fn parse_stream_event(data: &str) -> Option<StreamChunk> {
        let response: GenerateContentResponse = match serde_json::from_str(data) {
            Ok(response) => response,
            Err(error) => {
                return Some(StreamChunk::Error {
                    message: format!("failed to parse Gemini SSE payload: {error}"),
                });
            }
        };

        // Check for function calls first.
        if let Some(candidate) = response.candidates.first() {
            if let Some(ref content) = candidate.content {
                for part in &content.parts {
                    if let Some(ref fc) = part.function_call {
                        return Some(StreamChunk::ToolUse {
                            call_id: format!("gemini_{}", fc.name),
                            name: fc.name.clone(),
                            arguments: fc.args.clone(),
                        });
                    }
                }

                // Text delta.
                for part in &content.parts {
                    if let Some(ref text) = part.text {
                        if !text.is_empty() {
                            return Some(StreamChunk::Delta {
                                content: text.clone(),
                            });
                        }
                    }
                }
            }

            // Check for finish reason with no content.
            if let Some(ref reason) = candidate.finish_reason {
                let finish = Self::parse_finish_reason(Some(reason));
                return Some(StreamChunk::Done {
                    finish_reason: Some(finish),
                });
            }
        }

        // Usage metadata (may arrive without candidates).
        if let Some(ref usage) = response.usage_metadata {
            return Some(StreamChunk::Usage(swage_core::TokenUsage {
                input_tokens: usage.prompt_token_count,
                output_tokens: usage.candidates_token_count,
            }));
        }

        None
    }
}

// ---------------------------------------------------------------------------
// LlmProvider implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmProvider for GoogleProvider {
    fn model_id(&self) -> &str {
        &self.config.model
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let model = request.model.as_deref().unwrap_or(&self.config.model);
        let url = self.build_url(model, false);
        let body = self.build_request(&request);

        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::StreamError(self.redact_api_key_in_text(e.to_string())))?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            let body_text = self.redact_api_key_in_text(response.text().await.unwrap_or_default());
            return Err(crate::http::map_http_error(status, body_text, None));
        }

        let gemini_response: GenerateContentResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::InvalidResponse(e.to_string()))?;

        let (content, _tool_calls, finish_reason) = Self::parse_response(&gemini_response)?;

        let usage = gemini_response.usage_metadata.as_ref();
        Ok(CompletionResponse {
            content,
            input_tokens: usage.map_or(0, |u| u.prompt_token_count),
            output_tokens: usage.map_or(0, |u| u.candidates_token_count),
            finish_reason,
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        let model = request.model.as_deref().unwrap_or(&self.config.model);
        let url = self.build_url(model, false);
        let body = self.build_tool_request(&request);

        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::StreamError(self.redact_api_key_in_text(e.to_string())))?;

        let status = response.status().as_u16();
        if !response.status().is_success() {
            let body_text = self.redact_api_key_in_text(response.text().await.unwrap_or_default());
            return Err(crate::http::map_http_error(status, body_text, None));
        }

        let gemini_response: GenerateContentResponse = response
            .json()
            .await
            .map_err(|e| ProviderError::InvalidResponse(e.to_string()))?;

        let (content, tool_calls, finish_reason) = Self::parse_response(&gemini_response)?;

        let usage = gemini_response.usage_metadata.as_ref();
        Ok(ToolCompletionResponse {
            content,
            input_tokens: usage.map_or(0, |u| u.prompt_token_count),
            output_tokens: usage.map_or(0, |u| u.candidates_token_count),
            finish_reason,
            cost: None,
            tool_calls,
        })
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        let model = request
            .model
            .as_deref()
            .unwrap_or(&self.config.model)
            .to_string();
        let url = self.build_url(&model, true);
        let body = self.build_request(&request);
        let _sanitised = Self::sanitised_url(&model, &self.config.base_url, true);
        let api_key = self.config.api_key.clone();

        let (tx, rx) = mpsc::unbounded();

        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let response = match client
                .post(&url)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.unbounded_send(StreamChunk::Error {
                        message: GoogleProvider::redact_api_key(&e.to_string(), api_key.as_deref()),
                    });
                    return;
                }
            };

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let body_text = GoogleProvider::redact_api_key(
                    &response.text().await.unwrap_or_default(),
                    api_key.as_deref(),
                );
                let err = crate::http::map_http_error(status, body_text, None);
                let _ = tx.unbounded_send(StreamChunk::Error {
                    message: err.to_string(),
                });
                return;
            }

            // Parse the SSE stream. Gemini uses the standard SSE format with
            // data: lines containing JSON.
            let mut buffer = String::new();
            let mut byte_stream = response.bytes_stream();

            while let Some(chunk) = byte_stream.next().await {
                let bytes = match chunk {
                    Err(e) => {
                        let _ = tx.unbounded_send(StreamChunk::Error {
                            message: e.to_string(),
                        });
                        break;
                    }
                    Ok(b) => b,
                };

                let text = match std::str::from_utf8(&bytes) {
                    Ok(s) => s.to_owned(),
                    Err(e) => {
                        let _ = tx.unbounded_send(StreamChunk::Error {
                            message: format!("invalid UTF-8: {e}"),
                        });
                        break;
                    }
                };

                buffer.push_str(&text);

                // Process complete SSE events (delimited by blank lines).
                while let Some(idx) = buffer.find("\n\n") {
                    let block = buffer[..idx].to_string();
                    buffer = buffer[idx + 2..].to_string();

                    // Extract data lines from the SSE block.
                    let data: String = block
                        .lines()
                        .filter_map(|line| {
                            if let Some(val) = line.strip_prefix("data:") {
                                Some(val.strip_prefix(' ').unwrap_or(val))
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");

                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }

                    if let Some(chunk) = GoogleProvider::parse_stream_event(&data) {
                        let is_done = matches!(chunk, StreamChunk::Done { .. });
                        let _ = tx.unbounded_send(chunk);
                        if is_done {
                            return;
                        }
                    }
                }
            }

            // If we exit the loop without a Done chunk, emit one.
            let _ = tx.unbounded_send(StreamChunk::Done {
                finish_reason: Some(FinishReason::Unknown),
            });
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

    // --- System message extraction ---

    #[test]
    fn extract_system_separates_system_messages() {
        let messages = vec![
            ChatMessage::new(Role::System, "Be helpful"),
            ChatMessage::new(Role::User, "Hello"),
            ChatMessage::new(Role::System, "Be concise"),
        ];
        let (system, remaining) = GoogleProvider::extract_system(&messages);
        assert!(system.is_some());
        let instruction = system.unwrap();
        let text = instruction.parts[0].text.as_ref().unwrap();
        assert!(text.contains("Be helpful"));
        assert!(text.contains("Be concise"));
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].role, Role::User);
    }

    #[test]
    fn extract_system_returns_none_when_no_system_messages() {
        let messages = vec![ChatMessage::new(Role::User, "Hello")];
        let (system, remaining) = GoogleProvider::extract_system(&messages);
        assert!(system.is_none());
        assert_eq!(remaining.len(), 1);
    }

    // --- Message conversion ---

    #[test]
    fn convert_user_message_to_gemini_format() {
        let msg = ChatMessage::new(Role::User, "Hello");
        let converted = GoogleProvider::convert_messages(&[&msg]);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].role.as_deref(), Some("user"));
        assert_eq!(converted[0].parts[0].text.as_deref(), Some("Hello"));
    }

    #[test]
    fn convert_assistant_message_uses_model_role() {
        let msg = ChatMessage::new(Role::Assistant, "Hi there");
        let converted = GoogleProvider::convert_messages(&[&msg]);
        assert_eq!(converted[0].role.as_deref(), Some("model"));
    }

    #[test]
    fn convert_tool_result_to_function_response() {
        let mut msg = ChatMessage::new(Role::Tool, "result text");
        msg.tool_call_id = Some("get_weather".into());
        let converted = GoogleProvider::convert_messages(&[&msg]);
        assert_eq!(converted[0].role.as_deref(), Some("user"));
        let fr = converted[0].parts[0].function_response.as_ref().unwrap();
        assert_eq!(fr.name, "get_weather");
        assert_eq!(fr.response["result"], "result text");
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
        let converted = GoogleProvider::convert_messages(&[&msg]);
        let fc = converted[0]
            .parts
            .iter()
            .find(|p| p.function_call.is_some())
            .and_then(|p| p.function_call.as_ref())
            .unwrap();
        assert_eq!(fc.name, "search");
        assert_eq!(fc.args["query"], "rust");
    }

    // --- Request building ---

    #[test]
    fn build_request_separates_system_instruction() {
        let provider = GoogleProvider::from_config(ProviderConfig::new(
            "key",
            "gemini-pro",
            "https://generativelanguage.googleapis.com",
        ));
        let request = CompletionRequest {
            messages: vec![
                ChatMessage::new(Role::System, "Be helpful"),
                ChatMessage::new(Role::User, "Hello"),
            ],
            model: None,
            max_tokens: Some(100),
            temperature: None,
            stop_sequences: None,
            metadata: None,
        };
        let body = provider.build_request(&request);
        assert!(body.system_instruction.is_some());
        assert_eq!(body.contents.len(), 1); // Only user message.
        assert_eq!(
            body.generation_config.as_ref().unwrap().max_output_tokens,
            Some(100)
        );
    }

    #[test]
    fn build_tool_request_includes_function_declarations() {
        let provider = GoogleProvider::from_config(ProviderConfig::new(
            "key",
            "gemini-pro",
            "https://generativelanguage.googleapis.com",
        ));
        let request = ToolCompletionRequest {
            messages: vec![ChatMessage::new(Role::User, "search")],
            model: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![serde_json::json!({
                "name": "search",
                "description": "Web search",
                "parameters": { "type": "object", "properties": { "q": { "type": "string" } } }
            })],
        };
        let body = provider.build_tool_request(&request);
        assert_eq!(body.tools.len(), 1);
        assert_eq!(body.tools[0].function_declarations.len(), 1);
        assert_eq!(body.tools[0].function_declarations[0].name, "search");
    }

    // --- URL building ---

    #[test]
    fn build_url_includes_api_key_as_query_param() {
        let provider = GoogleProvider::from_config(ProviderConfig::new(
            "test-key",
            "gemini-pro",
            "https://generativelanguage.googleapis.com",
        ));
        let url = provider.build_url("gemini-pro", false);
        assert!(url.contains("key=test-key"));
        assert!(url.contains(":generateContent"));
        assert!(!url.contains("alt=sse"));
    }

    #[test]
    fn build_url_streaming_uses_stream_endpoint() {
        let provider = GoogleProvider::from_config(ProviderConfig::new(
            "test-key",
            "gemini-pro",
            "https://generativelanguage.googleapis.com",
        ));
        let url = provider.build_url("gemini-pro", true);
        assert!(url.contains(":streamGenerateContent"));
        assert!(url.contains("alt=sse"));
    }

    #[test]
    fn sanitised_url_redacts_api_key() {
        let url = GoogleProvider::sanitised_url(
            "gemini-pro",
            "https://generativelanguage.googleapis.com",
            false,
        );
        assert!(url.contains("REDACTED"));
        assert!(!url.contains("test-key"));
    }

    #[test]
    fn redact_api_key_in_text_removes_embedded_key() {
        let provider = GoogleProvider::from_config(ProviderConfig::new(
            "test-key",
            "gemini-pro",
            "https://generativelanguage.googleapis.com",
        ));
        let redacted = provider.redact_api_key_in_text(
            "request failed for https://x.test?key=test-key with body test-key",
        );
        assert!(redacted.contains("REDACTED"));
        assert!(!redacted.contains("test-key"));
    }

    // --- Finish reason parsing ---

    #[test]
    fn parse_finish_reason_stop() {
        assert_eq!(
            GoogleProvider::parse_finish_reason(Some("STOP")),
            FinishReason::Stop
        );
    }

    #[test]
    fn parse_finish_reason_max_tokens() {
        assert_eq!(
            GoogleProvider::parse_finish_reason(Some("MAX_TOKENS")),
            FinishReason::Length
        );
    }

    #[test]
    fn parse_finish_reason_safety() {
        assert_eq!(
            GoogleProvider::parse_finish_reason(Some("SAFETY")),
            FinishReason::ContentFilter
        );
    }

    #[test]
    fn parse_finish_reason_recitation() {
        assert_eq!(
            GoogleProvider::parse_finish_reason(Some("RECITATION")),
            FinishReason::ContentFilter,
        );
    }

    #[test]
    fn parse_finish_reason_unknown_variant() {
        assert_eq!(
            GoogleProvider::parse_finish_reason(Some("OTHER")),
            FinishReason::Unknown
        );
        assert_eq!(
            GoogleProvider::parse_finish_reason(None),
            FinishReason::Unknown
        );
    }

    // --- Response parsing ---

    #[test]
    fn parse_response_extracts_text() {
        let response = GenerateContentResponse {
            candidates: vec![Candidate {
                content: Some(GeminiContent {
                    role: Some("model".into()),
                    parts: vec![GeminiPart {
                        text: Some("Hello!".into()),
                        function_call: None,
                        function_response: None,
                    }],
                }),
                finish_reason: Some("STOP".into()),
            }],
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 5,
            }),
        };
        let (content, tool_calls, finish) = GoogleProvider::parse_response(&response).unwrap();
        assert_eq!(content, "Hello!");
        assert!(tool_calls.is_empty());
        assert_eq!(finish, FinishReason::Stop);
    }

    #[test]
    fn parse_response_extracts_function_calls() {
        let response = GenerateContentResponse {
            candidates: vec![Candidate {
                content: Some(GeminiContent {
                    role: Some("model".into()),
                    parts: vec![GeminiPart {
                        text: None,
                        function_call: Some(GeminiFunctionCall {
                            name: "get_weather".into(),
                            args: serde_json::json!({"location": "London"}),
                        }),
                        function_response: None,
                    }],
                }),
                finish_reason: Some("STOP".into()),
            }],
            usage_metadata: None,
        };
        let (content, tool_calls, finish) = GoogleProvider::parse_response(&response).unwrap();
        assert!(content.is_empty());
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "get_weather");
        // Gemini returns STOP for function calls — we override to ToolUse.
        assert_eq!(finish, FinishReason::ToolUse);
    }

    #[test]
    fn parse_response_errors_on_empty_candidates() {
        let response = GenerateContentResponse {
            candidates: vec![],
            usage_metadata: None,
        };
        assert!(GoogleProvider::parse_response(&response).is_err());
    }

    // --- Streaming event parsing ---

    #[test]
    fn parse_stream_event_text_delta() {
        let data = r#"{
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "Hello"}]
                }
            }]
        }"#;
        let chunk = GoogleProvider::parse_stream_event(data);
        assert!(matches!(chunk, Some(StreamChunk::Delta { content }) if content == "Hello"));
    }

    #[test]
    fn parse_stream_event_function_call() {
        let data = r#"{
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": "get_weather",
                            "args": {"location": "London"}
                        }
                    }]
                }
            }]
        }"#;
        let chunk = GoogleProvider::parse_stream_event(data);
        assert!(matches!(
            chunk,
            Some(StreamChunk::ToolUse { name, .. }) if name == "get_weather"
        ));
    }

    #[test]
    fn parse_stream_event_done() {
        let data = r#"{
            "candidates": [{
                "content": { "role": "model", "parts": [] },
                "finishReason": "STOP"
            }]
        }"#;
        let chunk = GoogleProvider::parse_stream_event(data);
        assert!(matches!(
            chunk,
            Some(StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop)
            })
        ));
    }

    #[test]
    fn parse_stream_event_usage_metadata() {
        let data = r#"{
            "usageMetadata": {
                "promptTokenCount": 20,
                "candidatesTokenCount": 10
            }
        }"#;
        let chunk = GoogleProvider::parse_stream_event(data);
        assert!(matches!(
            chunk,
            Some(StreamChunk::Usage(usage)) if usage.input_tokens == 20 && usage.output_tokens == 10
        ));
    }

    #[test]
    fn parse_stream_event_reports_malformed_payload() {
        let chunk = GoogleProvider::parse_stream_event("{not-json");
        assert!(matches!(
            chunk,
            Some(StreamChunk::Error { message }) if message.contains("failed to parse Gemini SSE payload")
        ));
    }

    // --- Wire format deserialisation ---

    #[test]
    fn deserialise_generate_content_response() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{"text": "Hi"}]
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 2
            }
        }"#;
        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.candidates.len(), 1);
        assert_eq!(
            response.candidates[0].content.as_ref().unwrap().parts[0]
                .text
                .as_deref(),
            Some("Hi"),
        );
        assert_eq!(response.usage_metadata.unwrap().prompt_token_count, 5);
    }

    #[test]
    fn deserialise_function_call_response() {
        let json = r#"{
            "candidates": [{
                "content": {
                    "role": "model",
                    "parts": [{
                        "functionCall": {
                            "name": "search",
                            "args": {"query": "rust"}
                        }
                    }]
                },
                "finishReason": "STOP"
            }]
        }"#;
        let response: GenerateContentResponse = serde_json::from_str(json).unwrap();
        let fc = &response.candidates[0].content.as_ref().unwrap().parts[0]
            .function_call
            .as_ref()
            .unwrap();
        assert_eq!(fc.name, "search");
        assert_eq!(fc.args["query"], "rust");
    }

    // --- Model ID ---

    #[test]
    fn model_id_returns_configured_model() {
        let provider = GoogleProvider::from_config(ProviderConfig::new(
            "key",
            "gemini-1.5-pro",
            "https://example.com",
        ));
        assert_eq!(provider.model_id(), "gemini-1.5-pro");
    }

    // --- Keyless operation ---

    #[test]
    fn keyless_url_has_empty_key_param() {
        let provider = GoogleProvider::from_config(ProviderConfig::keyless(
            "gemini-pro",
            "https://localhost:8080",
        ));
        let url = provider.build_url("gemini-pro", false);
        assert!(url.contains("key="));
        // No panic, just empty key.
    }
}
