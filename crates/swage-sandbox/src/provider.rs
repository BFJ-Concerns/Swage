//! IPC-backed completion provider for sandboxed agents.
//!
//! Sends LLM completion requests over IPC to the host process, which executes
//! them through the real provider chain. Credentials never enter the agent
//! process. The interface uses swage-core request/response types, making it
//! interchangeable with in-process providers at the type level.

use swage_core::{
    CompletionRequest, CompletionResponse, SandboxError, ToolCompletionRequest,
    ToolCompletionResponse,
};
use tokio::sync::Mutex;

use crate::ipc::agent::AgentConnection;

// ---------------------------------------------------------------------------
// IpcCompletionProvider
// ---------------------------------------------------------------------------

/// Agent-side completion provider that proxies LLM requests through the host.
///
/// Wraps an `AgentConnection` in a `Mutex` so `complete` and `complete_with_tools`
/// can take `&self`, making the provider composable without passing mutable
/// references through every call site.
///
/// The host executes requests through the full provider chain (retry, circuit
/// breaker, failover). Credentials never cross the sandbox boundary.
pub struct IpcCompletionProvider {
    connection: Mutex<AgentConnection>,
}

impl IpcCompletionProvider {
    /// Wrap an `AgentConnection` in a completion provider.
    pub fn new(connection: AgentConnection) -> Self {
        Self {
            connection: Mutex::new(connection),
        }
    }

    /// Request a text-only LLM completion through the host.
    ///
    /// Converts `CompletionRequest` to an IPC `LlmRequest`, sends it via the
    /// agent connection, waits for the matching `LlmResponse`, and maps it
    /// back to a `CompletionResponse`.
    ///
    /// `stop_sequences` and `metadata` are provider-specific fields that are
    /// not represented in the IPC protocol — they are silently dropped.
    pub async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, SandboxError> {
        let mut conn = self.connection.lock().await;

        let response = conn
            .send_llm_request(
                request.messages,
                request.model,
                request.max_tokens,
                request.temperature,
                None, // no tool definitions for a plain completion
            )
            .await?;

        Ok(CompletionResponse {
            content: response.content,
            input_tokens: response.token_usage.prompt_tokens,
            output_tokens: response.token_usage.completion_tokens,
            finish_reason: response.finish_reason,
            cost: None, // cost information is not forwarded over IPC
        })
    }

    /// Request an LLM completion with tool definitions through the host.
    ///
    /// Works identically to `complete` but forwards the tool schema so the
    /// model can issue tool calls. Any `ToolCall`s in the response are
    /// collected into `ToolCompletionResponse::tool_calls`; if the model
    /// responded with text only, the vec is empty.
    pub async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, SandboxError> {
        let mut conn = self.connection.lock().await;

        // Forward tool definitions even if the vec is empty — the host needs
        // to know that tool-use mode was requested.
        let tool_defs = if request.tool_definitions.is_empty() {
            None
        } else {
            Some(request.tool_definitions)
        };

        let response = conn
            .send_llm_request(
                request.messages,
                request.model,
                request.max_tokens,
                request.temperature,
                tool_defs,
            )
            .await?;

        Ok(ToolCompletionResponse {
            content: response.content,
            input_tokens: response.token_usage.prompt_tokens,
            output_tokens: response.token_usage.completion_tokens,
            finish_reason: response.finish_reason,
            cost: None,
            // tool_calls is None when the model responded with text only.
            tool_calls: response.tool_calls.unwrap_or_default(),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;
    use swage_core::tool::ToolCall;
    use swage_core::{ChatMessage, CompletionRequest, FinishReason, Role, ToolCompletionRequest};
    use tokio::net::UnixStream;

    use super::IpcCompletionProvider;
    use crate::ipc::agent::AgentConnection;
    use crate::ipc::message::{IpcMessage, TokenUsage};
    use crate::ipc::transport::IpcTransport;

    fn make_pair() -> (AgentConnection, IpcTransport) {
        let (agent_stream, host_stream) = UnixStream::pair().unwrap();
        (
            AgentConnection::new(IpcTransport::new(agent_stream)),
            IpcTransport::new(host_stream),
        )
    }

    // -- complete -------------------------------------------------------------

    #[tokio::test]
    async fn complete_sends_request_and_returns_response() {
        let (agent_conn, mut host_transport) = make_pair();
        let provider = IpcCompletionProvider::new(agent_conn);

        let host_handle = tokio::spawn(async move {
            let msg = host_transport.receive().await.unwrap();
            match msg {
                IpcMessage::LlmRequest { request_id, .. } => {
                    host_transport
                        .send(&IpcMessage::LlmResponse {
                            request_id,
                            content: "Hello, world!".into(),
                            token_usage: TokenUsage {
                                prompt_tokens: 8,
                                completion_tokens: 3,
                                total_tokens: 11,
                            },
                            finish_reason: FinishReason::Stop,
                            tool_calls: None,
                        })
                        .await
                        .unwrap();
                }
                other => panic!("expected LlmRequest, got {other:?}"),
            }
        });

        let response = provider
            .complete(CompletionRequest {
                effort: None,
                messages: vec![ChatMessage::new(Role::User, "Hi")],
                model: None,
                max_tokens: Some(50),
                temperature: None,
                stop_sequences: None,
                metadata: None,
                tool_definitions: vec![],
            })
            .await
            .unwrap();

        host_handle.await.unwrap();

        assert_eq!(response.content, "Hello, world!");
        assert_eq!(response.input_tokens, 8);
        assert_eq!(response.output_tokens, 3);
        assert_eq!(response.finish_reason, FinishReason::Stop);
    }

    #[tokio::test]
    async fn complete_maps_request_fields_to_ipc_message() {
        let (agent_conn, mut host_transport) = make_pair();
        let provider = IpcCompletionProvider::new(agent_conn);

        let agent_handle = tokio::spawn(async move {
            provider
                .complete(CompletionRequest {
                    effort: None,
                    messages: vec![ChatMessage::new(Role::User, "test")],
                    model: Some("claude-3-opus".into()),
                    max_tokens: Some(512),
                    temperature: Some(0.5),
                    stop_sequences: None,
                    metadata: None,
                    tool_definitions: vec![],
                })
                .await
        });

        let received = host_transport.receive().await.unwrap();
        match received {
            IpcMessage::LlmRequest {
                request_id,
                model,
                max_tokens,
                temperature,
                messages,
                ..
            } => {
                assert!(!request_id.is_empty());
                assert_eq!(model, Some("claude-3-opus".into()));
                assert_eq!(max_tokens, Some(512));
                assert_eq!(temperature, Some(0.5));
                assert_eq!(messages.len(), 1);

                host_transport
                    .send(&IpcMessage::LlmResponse {
                        request_id,
                        content: "done".into(),
                        token_usage: TokenUsage {
                            prompt_tokens: 1,
                            completion_tokens: 1,
                            total_tokens: 2,
                        },
                        finish_reason: FinishReason::Stop,
                        tool_calls: None,
                    })
                    .await
                    .unwrap();
            }
            other => panic!("expected LlmRequest, got {other:?}"),
        }

        agent_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn complete_no_credentials_in_ipc_message() {
        // Regression guard: API keys must never appear in IPC messages.
        let (agent_conn, mut host_transport) = make_pair();
        let provider = IpcCompletionProvider::new(agent_conn);

        let agent_handle = tokio::spawn(async move {
            provider
                .complete(CompletionRequest {
                    effort: None,
                    messages: vec![ChatMessage::new(Role::User, "hi")],
                    model: None,
                    max_tokens: None,
                    temperature: None,
                    stop_sequences: None,
                    metadata: None,
                    tool_definitions: vec![],
                })
                .await
        });

        let msg = host_transport.receive().await.unwrap();
        let serialised = serde_json::to_string(&msg).unwrap();

        // Common API key prefixes must not appear in the wire message.
        assert!(
            !serialised.contains("sk-"),
            "OpenAI key prefix found in IPC message"
        );
        assert!(
            !serialised.contains("sk-ant"),
            "Anthropic key prefix found in IPC message"
        );
        assert!(
            !serialised.contains("api_key"),
            "'api_key' field found in IPC message"
        );

        match msg {
            IpcMessage::LlmRequest { request_id, .. } => {
                host_transport
                    .send(&IpcMessage::LlmResponse {
                        request_id,
                        content: "clean".into(),
                        token_usage: TokenUsage {
                            prompt_tokens: 1,
                            completion_tokens: 1,
                            total_tokens: 2,
                        },
                        finish_reason: FinishReason::Stop,
                        tool_calls: None,
                    })
                    .await
                    .unwrap();
            }
            other => panic!("expected LlmRequest, got {other:?}"),
        }

        agent_handle.await.unwrap().unwrap();
    }

    // -- complete_with_tools --------------------------------------------------

    #[tokio::test]
    async fn complete_with_tools_sends_tool_definitions() {
        let (agent_conn, mut host_transport) = make_pair();
        let provider = IpcCompletionProvider::new(agent_conn);

        let tool_def = json!({
            "type": "object",
            "name": "bash",
            "description": "Run a shell command",
            "input_schema": {
                "type": "object",
                "properties": { "command": { "type": "string" } }
            }
        });

        let agent_handle = tokio::spawn(async move {
            provider
                .complete_with_tools(ToolCompletionRequest {
                    messages: vec![ChatMessage::new(Role::User, "list files")],
                    model: None,
                    max_tokens: None,
                    temperature: None,
                    stop_sequences: None,
                    metadata: None,
                    tool_definitions: vec![tool_def],
                })
                .await
        });

        let received = host_transport.receive().await.unwrap();
        match received {
            IpcMessage::LlmRequest {
                request_id,
                tool_definitions,
                ..
            } => {
                // Tool definitions must be forwarded to the host.
                assert!(
                    tool_definitions.is_some(),
                    "tool_definitions should be present"
                );
                assert_eq!(tool_definitions.unwrap().len(), 1);

                host_transport
                    .send(&IpcMessage::LlmResponse {
                        request_id,
                        content: String::new(),
                        token_usage: TokenUsage {
                            prompt_tokens: 5,
                            completion_tokens: 2,
                            total_tokens: 7,
                        },
                        finish_reason: FinishReason::ToolUse,
                        tool_calls: Some(vec![ToolCall {
                            call_id: "call_1".into(),
                            name: "bash".into(),
                            arguments: json!({"command": "ls"}),
                        }]),
                    })
                    .await
                    .unwrap();
            }
            other => panic!("expected LlmRequest, got {other:?}"),
        }

        let response = agent_handle.await.unwrap().unwrap();
        assert_eq!(response.finish_reason, FinishReason::ToolUse);
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].name, "bash");
        assert_eq!(response.input_tokens, 5);
        assert_eq!(response.output_tokens, 2);
    }

    #[tokio::test]
    async fn complete_with_tools_empty_tool_calls_when_stop() {
        // When the model stops without calling a tool, tool_calls should be empty.
        let (agent_conn, mut host_transport) = make_pair();
        let provider = IpcCompletionProvider::new(agent_conn);

        let agent_handle = tokio::spawn(async move {
            provider
                .complete_with_tools(ToolCompletionRequest {
                    messages: vec![ChatMessage::new(Role::User, "just say hi")],
                    model: None,
                    max_tokens: None,
                    temperature: None,
                    stop_sequences: None,
                    metadata: None,
                    tool_definitions: vec![],
                })
                .await
        });

        let received = host_transport.receive().await.unwrap();
        match received {
            IpcMessage::LlmRequest { request_id, .. } => {
                host_transport
                    .send(&IpcMessage::LlmResponse {
                        request_id,
                        content: "Hi!".into(),
                        token_usage: TokenUsage {
                            prompt_tokens: 2,
                            completion_tokens: 2,
                            total_tokens: 4,
                        },
                        finish_reason: FinishReason::Stop,
                        tool_calls: None, // model responded with text, no tool calls
                    })
                    .await
                    .unwrap();
            }
            other => panic!("expected LlmRequest, got {other:?}"),
        }

        let response = agent_handle.await.unwrap().unwrap();
        assert_eq!(response.content, "Hi!");
        assert!(
            response.tool_calls.is_empty(),
            "tool_calls should be empty when model didn't call tools"
        );
    }
}
