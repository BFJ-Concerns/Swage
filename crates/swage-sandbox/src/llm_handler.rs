//! Host-side LLM request handler.
//!
//! Receives LLM completion requests from the sandboxed agent via IPC and
//! dispatches them through the host's provider chain. The handler converts
//! between IPC message types and swage-core request/response types.

use std::future::Future;
use std::pin::Pin;

use swage_core::{CompletionRequest, CompletionResponse, SandboxError};

use crate::ipc::message::{LlmRequest, LlmResponse, TokenUsage};

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

/// Boxed future returned by the completion function.
type CompleteFuture =
    Pin<Box<dyn Future<Output = Result<CompletionResponse, SandboxError>> + Send>>;

/// Type-erased completion function stored on the handler.
type CompleteFn = Box<dyn Fn(CompletionRequest) -> CompleteFuture + Send + Sync>;

// ---------------------------------------------------------------------------
// HostLlmHandler
// ---------------------------------------------------------------------------

/// Host-side dispatcher for IPC LLM requests.
///
/// Accepts incoming `LlmRequest` IPC messages from sandboxed agents,
/// converts them to swage-core `CompletionRequest` types, calls the provider
/// function, and returns an `LlmResponse` with the preserved `request_id`,
/// token usage, and finish reason.
///
/// The constructor is generic over the provider function so callers can pass
/// closures or any `Fn(CompletionRequest) -> Future<...>` directly — the
/// concrete `HostLlmHandler` type is not generic, making it easy to store
/// inside sandbox implementations without propagating type parameters.
pub struct HostLlmHandler {
    complete_fn: CompleteFn,
}

impl HostLlmHandler {
    /// Create a handler wrapping the given async completion function.
    ///
    /// The function is called for every incoming LLM request. Provider errors
    /// are forwarded as `SandboxError::IpcError` to the calling sandbox.
    pub fn new<F, Fut>(complete_fn: F) -> Self
    where
        F: Fn(CompletionRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CompletionResponse, SandboxError>> + Send + 'static,
    {
        // Box the closure and pin its future so the concrete type is erased.
        Self {
            complete_fn: Box::new(move |req| Box::pin(complete_fn(req))),
        }
    }

    /// Dispatch an IPC `LlmRequest` to the provider and return an `LlmResponse`.
    ///
    /// Converts the IPC request to a `CompletionRequest`, calls the provider,
    /// and maps the response back — preserving the `request_id` for correlation,
    /// token usage, and finish reason. Provider errors become `SandboxError`.
    pub async fn handle_request(&self, request: LlmRequest) -> Result<LlmResponse, SandboxError> {
        // Convert IPC request to swage-core's CompletionRequest.
        // tool_definitions are currently not forwarded on this path — the
        // basic completion API does not carry a tool schema.
        let completion_req = CompletionRequest {
            messages: request.messages,
            model: request.model,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            stop_sequences: None,
            metadata: None,
        };

        let response = (self.complete_fn)(completion_req).await?;

        // Map CompletionResponse back to the IPC LlmResponse, preserving the
        // original request_id so the agent can correlate the response.
        Ok(LlmResponse {
            request_id: request.request_id,
            content: response.content,
            token_usage: TokenUsage {
                prompt_tokens: response.input_tokens,
                completion_tokens: response.output_tokens,
                // total = prompt + completion; providers don't always report
                // this separately, so we compute it.
                total_tokens: response.input_tokens + response.output_tokens,
            },
            finish_reason: response.finish_reason,
            // Basic completion path produces no tool calls.
            tool_calls: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use swage_core::{CompletionResponse, FinishReason, SandboxError};

    use super::HostLlmHandler;
    use crate::ipc::message::{LlmRequest, LlmResponse};

    fn minimal_request() -> LlmRequest {
        LlmRequest {
            request_id: "test-req".into(),
            messages: vec![],
            model: None,
            max_tokens: Some(100),
            temperature: None,
            tool_definitions: None,
        }
    }

    // --- handle_request: happy path ---

    #[tokio::test]
    async fn handle_request_dispatches_to_provider_and_returns_response() {
        let provider = |_req: swage_core::CompletionRequest| async {
            Ok::<_, SandboxError>(CompletionResponse {
                content: "42".into(),
                input_tokens: 8,
                output_tokens: 3,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        };

        let handler = HostLlmHandler::new(provider);
        let response: LlmResponse = handler.handle_request(minimal_request()).await.unwrap();

        assert_eq!(response.request_id, "test-req");
        assert_eq!(response.content, "42");
        assert_eq!(response.token_usage.prompt_tokens, 8);
        assert_eq!(response.token_usage.completion_tokens, 3);
        assert_eq!(response.token_usage.total_tokens, 11);
        assert!(matches!(response.finish_reason, FinishReason::Stop));
    }

    // --- handle_request: token usage and finish_reason are preserved ---

    #[tokio::test]
    async fn handle_request_preserves_token_usage_and_finish_reason() {
        let provider = |_req: swage_core::CompletionRequest| async {
            Ok::<_, SandboxError>(CompletionResponse {
                content: "long answer".into(),
                input_tokens: 100,
                output_tokens: 50,
                finish_reason: FinishReason::Length,
                cost: Some(0.002),
            })
        };

        let handler = HostLlmHandler::new(provider);
        let response = handler.handle_request(minimal_request()).await.unwrap();

        assert_eq!(response.token_usage.prompt_tokens, 100);
        assert_eq!(response.token_usage.completion_tokens, 50);
        assert_eq!(response.token_usage.total_tokens, 150);
        assert!(matches!(response.finish_reason, FinishReason::Length));
    }

    // --- handle_request: request_id is echoed in the response ---

    #[tokio::test]
    async fn handle_request_echoes_request_id() {
        let provider = |_req: swage_core::CompletionRequest| async {
            Ok::<_, SandboxError>(CompletionResponse {
                content: "hi".into(),
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        };

        let handler = HostLlmHandler::new(provider);
        let mut req = minimal_request();
        req.request_id = "correlation-id-xyz".into();

        let response = handler.handle_request(req).await.unwrap();
        assert_eq!(response.request_id, "correlation-id-xyz");
    }

    // --- handle_request: provider error is forwarded ---

    #[tokio::test]
    async fn handle_request_forwards_provider_errors() {
        let provider = |_req: swage_core::CompletionRequest| async {
            Err::<CompletionResponse, SandboxError>(SandboxError::IpcError {
                message: "provider unavailable".into(),
            })
        };

        let handler = HostLlmHandler::new(provider);
        let result = handler.handle_request(minimal_request()).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::IpcError { message } => {
                assert!(
                    message.contains("provider unavailable"),
                    "error message should contain provider error, got: {message}"
                );
            }
            other => panic!("expected IpcError, got {:?}", other),
        }
    }

    // --- handle_request: messages and model are forwarded to provider ---

    #[tokio::test]
    async fn handle_request_forwards_messages_and_model_to_provider() {
        use swage_core::{ChatMessage, ContentPart, Role};
        use chrono::Utc;
        use uuid::Uuid;

        let received = std::sync::Arc::new(std::sync::Mutex::new(
            None::<swage_core::CompletionRequest>,
        ));
        let received_clone = received.clone();

        let provider = move |req: swage_core::CompletionRequest| {
            *received_clone.lock().unwrap() = Some(req);
            async {
                Ok::<_, SandboxError>(CompletionResponse {
                    content: "ok".into(),
                    input_tokens: 1,
                    output_tokens: 1,
                    finish_reason: FinishReason::Stop,
                    cost: None,
                })
            }
        };

        let handler = HostLlmHandler::new(provider);
        let mut req = minimal_request();
        req.messages = vec![ChatMessage {
            id: Uuid::new_v4(),
            role: Role::User,
            content: vec![ContentPart::Text { text: "hello".into() }],
            created_at: Utc::now(),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            content_blocks: None,
        }];
        req.model = Some("claude-3".into());

        handler.handle_request(req).await.unwrap();

        let captured = received.lock().unwrap();
        let captured = captured.as_ref().unwrap();
        assert_eq!(captured.messages.len(), 1);
        assert_eq!(captured.model.as_deref(), Some("claude-3"));
    }
}
