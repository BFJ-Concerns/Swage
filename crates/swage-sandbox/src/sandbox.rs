//! Sandbox trait — platform-abstracted process isolation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use swage_core::SandboxError;

/// Result of a tool call dispatched to a sandboxed agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    /// Tool output as JSON.
    pub output: Value,
    /// Execution time in milliseconds.
    pub duration_ms: u64,
    /// Error description if the tool failed.
    pub error: Option<String>,
}

impl ToolCallResult {
    /// Creates a successful result with the given output and duration.
    pub fn success(output: Value, duration_ms: u64) -> Self {
        Self {
            output,
            duration_ms,
            error: None,
        }
    }

    /// Creates a failed result with the given error message and duration.
    pub fn failed(error: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            output: Value::Null,
            duration_ms,
            error: Some(error.into()),
        }
    }

    /// Returns true if the tool call completed without an error.
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

/// Platform-abstracted sandbox for agent process isolation.
///
/// Callers use the same API regardless of platform — Linux with full namespace
/// isolation or fallback mode without it. Implementations are selected at
/// runtime, not compile time.
#[async_trait]
pub trait Sandbox: Send + Sync {
    /// Launch an agent as a sandboxed child process.
    ///
    /// Sets up the IPC socket, spawns the child process, and waits for the
    /// ready signal before returning.
    async fn spawn_agent(&mut self) -> Result<(), SandboxError>;

    /// Dispatch a tool call to the sandboxed agent over IPC.
    ///
    /// Sends the tool name and parameters to the agent process; the agent
    /// executes the tool and returns the result.
    async fn execute_tool(
        &self,
        tool_name: &str,
        params: Value,
    ) -> Result<ToolCallResult, SandboxError>;

    /// Terminate the agent process and clean up resources.
    ///
    /// Sends a shutdown signal, waits for acknowledgement within the grace
    /// period, then forcefully terminates if needed. Idempotent — succeeds
    /// silently if the agent has already terminated.
    async fn shutdown(&mut self) -> Result<(), SandboxError>;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // -- Serde round-trip -----------------------------------------------------

    #[test]
    fn tool_call_result_serialises_to_json() {
        let result = ToolCallResult {
            output: json!({"count": 42}),
            duration_ms: 150,
            error: None,
        };

        let json = serde_json::to_string(&result).expect("serialisation must succeed");
        assert!(json.contains("42"));
        assert!(json.contains("150"));
    }

    #[test]
    fn tool_call_result_deserialises_from_json() {
        let json = r#"{"output":{"count":42},"duration_ms":150,"error":null}"#;
        let result: ToolCallResult =
            serde_json::from_str(json).expect("deserialisation must succeed");

        assert_eq!(result.duration_ms, 150);
        assert!(result.error.is_none());
    }

    #[test]
    fn tool_call_result_serde_round_trip() {
        let original = ToolCallResult {
            output: json!({"key": "value"}),
            duration_ms: 200,
            error: Some("something went wrong".into()),
        };

        let json = serde_json::to_string(&original).unwrap();
        let decoded: ToolCallResult = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.duration_ms, original.duration_ms);
        assert_eq!(decoded.error, original.error);
        assert_eq!(decoded.output, original.output);
    }

    // -- Constructor helpers --------------------------------------------------

    #[test]
    fn success_constructor_sets_no_error() {
        let result = ToolCallResult::success(json!({"result": "ok"}), 75);

        assert_eq!(result.output, json!({"result": "ok"}));
        assert_eq!(result.duration_ms, 75);
        assert!(result.error.is_none());
        assert!(result.is_success());
    }

    #[test]
    fn failed_constructor_sets_null_output_and_error() {
        let result = ToolCallResult::failed("tool not found", 10);

        assert_eq!(result.output, Value::Null);
        assert_eq!(result.duration_ms, 10);
        assert_eq!(result.error.as_deref(), Some("tool not found"));
        assert!(!result.is_success());
    }

    #[test]
    fn is_success_true_when_no_error() {
        let result = ToolCallResult {
            output: json!(null),
            duration_ms: 0,
            error: None,
        };
        assert!(result.is_success());
    }

    #[test]
    fn is_success_false_when_error_present() {
        let result = ToolCallResult {
            output: json!(null),
            duration_ms: 0,
            error: Some("timeout".into()),
        };
        assert!(!result.is_success());
    }

    // -- Clone + Debug --------------------------------------------------------

    #[test]
    fn tool_call_result_is_clone() {
        let original = ToolCallResult::success(json!(1), 50);
        let clone = original.clone();
        assert_eq!(clone.duration_ms, original.duration_ms);
        assert_eq!(clone.output, original.output);
    }

    #[test]
    fn tool_call_result_is_debug() {
        let result = ToolCallResult::success(json!("hello"), 10);
        let debug = format!("{result:?}");
        assert!(debug.contains("ToolCallResult"));
    }

    // -- Object safety --------------------------------------------------------

    // Verifies the trait is object-safe by constructing a Box<dyn Sandbox>.
    // This is a compile-time check — if the trait is not object-safe, this
    // function will not compile.
    fn _assert_object_safe(_sandbox: Box<dyn Sandbox>) {}
}
