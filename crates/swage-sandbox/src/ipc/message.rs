//! Wire-format message types for the IPC protocol.
//!
//! Every message carries a `kind` discriminator. Request-response pairs are
//! correlated by `request_id`. Lifecycle signals have no request ID.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use swage_core::{ChatMessage, FinishReason, ToolCall};

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Token usage reported by the LLM provider for a completion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Category of IPC error for diagnostics and recovery routing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    /// Malformed message or unexpected sequence.
    Protocol,
    /// Operation exceeded its deadline.
    Timeout,
    /// Upstream LLM provider returned an error.
    Provider,
    /// Tool execution failed in the agent process.
    ToolExecution,
    /// Transport-level failure (socket closed, broken pipe).
    Connection,
}

// ---------------------------------------------------------------------------
// Top-level message enum
// ---------------------------------------------------------------------------

/// All messages exchanged between the host and the sandboxed agent process.
///
/// Serialised as newline-delimited JSON with a `"kind"` discriminator field.
/// Request-response pairs share a `request_id` for correlation across async
/// boundaries. Lifecycle signals carry no `request_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcMessage {
    // --- Tool dispatch (host → agent → host) --------------------------------
    /// Host dispatches a tool invocation to the agent.
    ToolCallRequest {
        request_id: String,
        tool_name: String,
        parameters: Value,
    },

    /// Agent returns the outcome of a tool invocation.
    ToolCallResult {
        request_id: String,
        output: Value,
        /// Wall-clock execution time in milliseconds.
        duration_ms: u64,
        /// Present when the tool execution itself failed.
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    // --- LLM proxying (agent → host → agent) --------------------------------
    /// Agent requests an LLM completion through the host (credentials stay
    /// in the host process; the agent never sees API keys).
    LlmRequest {
        request_id: String,
        messages: Vec<ChatMessage>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        temperature: Option<f64>,
        /// JSON Schema tool definitions forwarded to the provider.
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_definitions: Option<Vec<Value>>,
    },

    /// Host returns the provider's completion to the agent.
    LlmResponse {
        request_id: String,
        content: String,
        token_usage: TokenUsage,
        finish_reason: FinishReason,
        /// Tool calls the model wants to invoke, if any.
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
    },

    // --- Lifecycle signals --------------------------------------------------
    /// Agent signals that it has initialised and is ready to receive requests.
    Ready,

    /// Host requests a graceful shutdown.
    Shutdown,

    /// Agent acknowledges the shutdown request before exiting.
    ShutdownAck,

    // --- Error reporting ----------------------------------------------------
    /// Either side reports a protocol or execution error.
    ///
    /// `request_id` is absent for connection-level errors that cannot be
    /// correlated with a specific in-flight request.
    ErrorMessage {
        #[serde(skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
        category: ErrorCategory,
        description: String,
    },
}

// ---------------------------------------------------------------------------
// Protocol-level request/response structs
//
// These carry the data extracted from IpcMessage variants. Using dedicated
// structs (rather than matching on the enum everywhere) gives the host,
// agent, and LLM handler a stable, typed API independent of wire framing.
// ---------------------------------------------------------------------------

/// Return value of `HostConnection::send_tool_call` and the corresponding
/// data received by the agent on a `ToolCallResult` message.
#[derive(Debug, Clone)]
pub struct ToolCallResult {
    /// JSON value returned by the tool (null on error).
    pub output: Value,
    /// Wall-clock execution time in milliseconds.
    pub duration_ms: u64,
    /// Present when the tool execution itself failed.
    pub error: Option<String>,
}

/// IPC-level LLM request.
///
/// Extracted from `IpcMessage::LlmRequest` by the host connection and
/// consumed by `HostLlmHandler`. The `request_id` must be preserved and
/// echoed back in the corresponding `LlmResponse` for correlation.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    /// Correlation ID — echoed back in the `LlmResponse`.
    pub request_id: String,
    pub messages: Vec<ChatMessage>,
    #[allow(dead_code)]
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    /// JSON Schema tool definitions forwarded to the provider.
    pub tool_definitions: Option<Vec<Value>>,
}

/// IPC-level LLM response.
///
/// Constructed by `HostLlmHandler` and sent back to the agent by the host
/// connection. The `request_id` must match the originating `LlmRequest`.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    /// Correlation ID from the originating `LlmRequest`.
    pub request_id: String,
    pub content: String,
    pub token_usage: TokenUsage,
    pub finish_reason: FinishReason,
    /// Tool calls requested by the model, if any.
    pub tool_calls: Option<Vec<ToolCall>>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{ErrorCategory, IpcMessage, TokenUsage};

    // --- TokenUsage roundtrip ---

    #[test]
    fn token_usage_roundtrip() {
        let usage = TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        };
        let json = serde_json::to_string(&usage).unwrap();
        let decoded: TokenUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.prompt_tokens, 10);
        assert_eq!(decoded.completion_tokens, 5);
        assert_eq!(decoded.total_tokens, 15);
    }

    // --- ErrorCategory roundtrip ---

    #[test]
    fn error_category_roundtrip() {
        for (variant, expected_json) in [
            (ErrorCategory::Protocol, "\"protocol\""),
            (ErrorCategory::Timeout, "\"timeout\""),
            (ErrorCategory::Provider, "\"provider\""),
            (ErrorCategory::ToolExecution, "\"tool_execution\""),
            (ErrorCategory::Connection, "\"connection\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json);
            let decoded: ErrorCategory = serde_json::from_str(&json).unwrap();
            // Re-serialise to confirm symmetric roundtrip.
            assert_eq!(serde_json::to_string(&decoded).unwrap(), expected_json);
        }
    }

    // --- IpcMessage: "kind" discriminator in JSON ---

    #[test]
    fn ipc_message_kind_discriminator() {
        let msg = IpcMessage::Ready;
        let json: Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(
            json["kind"], "ready",
            "lifecycle signals must carry 'kind' field"
        );
    }

    // --- ToolCallRequest roundtrip ---

    #[test]
    fn tool_call_request_roundtrip() {
        let msg = IpcMessage::ToolCallRequest {
            request_id: "req-1".into(),
            tool_name: "echo".into(),
            parameters: json!({"text": "hello"}),
        };
        let serialised = serde_json::to_value(&msg).unwrap();
        assert_eq!(serialised["kind"], "tool_call_request");
        assert_eq!(serialised["request_id"], "req-1");
        assert_eq!(serialised["tool_name"], "echo");
        assert_eq!(serialised["parameters"], json!({"text": "hello"}));

        let roundtripped: IpcMessage = serde_json::from_value(serialised).unwrap();
        match roundtripped {
            IpcMessage::ToolCallRequest {
                request_id,
                tool_name,
                parameters,
            } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(tool_name, "echo");
                assert_eq!(parameters, json!({"text": "hello"}));
            }
            other => panic!("expected ToolCallRequest, got {other:?}"),
        }
    }

    // --- ToolCallResult roundtrip (with and without error) ---

    #[test]
    fn tool_call_result_roundtrip_success() {
        let msg = IpcMessage::ToolCallResult {
            request_id: "req-1".into(),
            output: json!("hello"),
            duration_ms: 42,
            error: None,
        };
        let serialised = serde_json::to_value(&msg).unwrap();
        assert_eq!(serialised["kind"], "tool_call_result");
        assert_eq!(serialised["duration_ms"], 42);
        // skip_serializing_if means the field is absent when None.
        assert!(
            serialised.get("error").is_none(),
            "absent error should not appear in the wire format"
        );

        let roundtripped: IpcMessage = serde_json::from_value(serialised).unwrap();
        match roundtripped {
            IpcMessage::ToolCallResult { error, .. } => assert!(error.is_none()),
            other => panic!("expected ToolCallResult, got {other:?}"),
        }
    }

    #[test]
    fn tool_call_result_roundtrip_error() {
        let msg = IpcMessage::ToolCallResult {
            request_id: "req-2".into(),
            output: Value::Null,
            duration_ms: 5,
            error: Some("tool not found".into()),
        };
        let serialised = serde_json::to_value(&msg).unwrap();
        assert_eq!(serialised["error"], "tool not found");

        let roundtripped: IpcMessage = serde_json::from_value(serialised).unwrap();
        match roundtripped {
            IpcMessage::ToolCallResult { error, .. } => {
                assert_eq!(error.unwrap(), "tool not found");
            }
            other => panic!("expected ToolCallResult, got {other:?}"),
        }
    }

    // --- LlmRequest roundtrip ---

    #[test]
    fn llm_request_roundtrip() {
        let msg = IpcMessage::LlmRequest {
            request_id: "llm-1".into(),
            messages: vec![],
            model: Some("claude-3".into()),
            max_tokens: Some(512),
            temperature: Some(0.7),
            tool_definitions: None,
        };
        let serialised = serde_json::to_value(&msg).unwrap();
        assert_eq!(serialised["kind"], "llm_request");
        assert_eq!(serialised["model"], "claude-3");
        assert_eq!(serialised["max_tokens"], 512);

        let roundtripped: IpcMessage = serde_json::from_value(serialised).unwrap();
        match roundtripped {
            IpcMessage::LlmRequest {
                request_id,
                model,
                max_tokens,
                ..
            } => {
                assert_eq!(request_id, "llm-1");
                assert_eq!(model.unwrap(), "claude-3");
                assert_eq!(max_tokens.unwrap(), 512);
            }
            other => panic!("expected LlmRequest, got {other:?}"),
        }
    }

    // --- LlmResponse roundtrip ---

    #[test]
    fn llm_response_roundtrip() {
        let msg = IpcMessage::LlmResponse {
            request_id: "llm-1".into(),
            content: "The answer is 42.".into(),
            token_usage: TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
            finish_reason: swage_core::FinishReason::Stop,
            tool_calls: None,
        };
        let serialised = serde_json::to_value(&msg).unwrap();
        assert_eq!(serialised["kind"], "llm_response");
        assert_eq!(serialised["content"], "The answer is 42.");
        assert_eq!(serialised["token_usage"]["total_tokens"], 15);
        assert_eq!(serialised["finish_reason"], "stop");

        let roundtripped: IpcMessage = serde_json::from_value(serialised).unwrap();
        match roundtripped {
            IpcMessage::LlmResponse {
                content,
                token_usage,
                ..
            } => {
                assert_eq!(content, "The answer is 42.");
                assert_eq!(token_usage.total_tokens, 15);
            }
            other => panic!("expected LlmResponse, got {other:?}"),
        }
    }

    // --- Lifecycle signals roundtrip ---

    #[test]
    fn lifecycle_signals_roundtrip() {
        for msg in [
            IpcMessage::Ready,
            IpcMessage::Shutdown,
            IpcMessage::ShutdownAck,
        ] {
            let serialised = serde_json::to_value(&msg).unwrap();
            // Every lifecycle signal serialises to just {"kind": "..."} — no other fields.
            let obj = serialised.as_object().unwrap();
            assert_eq!(
                obj.len(),
                1,
                "lifecycle signals carry only 'kind', got: {obj:?}"
            );
            assert!(obj.contains_key("kind"));

            let roundtripped: IpcMessage = serde_json::from_value(serialised).unwrap();
            let re_serialised = serde_json::to_value(&roundtripped).unwrap();
            let kind = re_serialised["kind"].as_str().unwrap();
            assert!(["ready", "shutdown", "shutdown_ack"].contains(&kind));
        }
    }

    // --- ErrorMessage roundtrip ---

    #[test]
    fn error_message_roundtrip() {
        let msg = IpcMessage::ErrorMessage {
            request_id: Some("req-1".into()),
            category: ErrorCategory::Provider,
            description: "upstream timeout".into(),
        };
        let serialised = serde_json::to_value(&msg).unwrap();
        assert_eq!(serialised["kind"], "error_message");
        assert_eq!(serialised["category"], "provider");
        assert_eq!(serialised["description"], "upstream timeout");

        let roundtripped: IpcMessage = serde_json::from_value(serialised).unwrap();
        match roundtripped {
            IpcMessage::ErrorMessage {
                request_id,
                description,
                ..
            } => {
                assert_eq!(request_id.unwrap(), "req-1");
                assert_eq!(description, "upstream timeout");
            }
            other => panic!("expected ErrorMessage, got {other:?}"),
        }
    }

    #[test]
    fn error_message_no_request_id() {
        // Connection-level errors that cannot be correlated with a request omit request_id.
        let msg = IpcMessage::ErrorMessage {
            request_id: None,
            category: ErrorCategory::Connection,
            description: "socket closed".into(),
        };
        let serialised = serde_json::to_value(&msg).unwrap();
        // skip_serializing_if = None means the field is absent entirely.
        assert!(
            serialised.get("request_id").is_none(),
            "absent request_id should not appear in the wire format"
        );

        let roundtripped: IpcMessage = serde_json::from_value(serialised).unwrap();
        match roundtripped {
            IpcMessage::ErrorMessage { request_id, .. } => assert!(request_id.is_none()),
            other => panic!("expected ErrorMessage, got {other:?}"),
        }
    }
}
