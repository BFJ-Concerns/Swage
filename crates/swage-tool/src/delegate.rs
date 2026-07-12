//! Built-in delegate tool — triggers inter-agent delegation from within
//! the turn loop.
//!
//! When the LLM calls this tool, it returns [`ToolError::RequiresDelegation`]
//! rather than producing output directly. The agent turn loop catches this
//! error variant and orchestrates the delegation via a [`DelegationHandler`],
//! following the same pattern as `RequiresApproval` for HITL gates.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::{DelegateCompletionMode, ToolError};

use crate::context::ToolContext;
use crate::tool::Tool;

/// Tool name used for delegation — reserved in the protected namespace.
pub const DELEGATE_TOOL_NAME: &str = "delegate";

/// Built-in tool that triggers inter-agent delegation.
///
/// The tool's `execute()` never produces a result directly — it always
/// returns `RequiresDelegation`, which the agent turn loop handles by
/// running the target agent and returning its output as this tool's result.
pub struct DelegateTool;

#[async_trait]
impl Tool for DelegateTool {
    fn name(&self) -> &str {
        DELEGATE_TOOL_NAME
    }

    fn description(&self) -> &str {
        "Delegate a task to another agent. Use completion_mode=\"wait\" when \
         the next parent step needs the delegate's answer. Use \
         completion_mode=\"detach\" to start independent work and continue \
         this turn with a durable reference to the delegated work."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target_agent": {
                    "type": "string",
                    "description": "The agent ID to delegate work to."
                },
                "task": {
                    "type": "string",
                    "description": "Task description for the delegate agent."
                },
                "context": {
                    "type": "array",
                    "description": "Optional context messages to transfer to the delegate.",
                    "items": {
                        "type": "object"
                    }
                },
                "completion_mode": {
                    "type": "string",
                    "enum": ["wait", "detach"],
                    "default": "wait",
                    "description": "Choose \"wait\" when this turn needs the delegate's result before continuing. Choose \"detach\" for independent work; the tool returns a durable delegate_message_id and child session/thread references while the delegate continues."
                }
            },
            "required": ["target_agent", "task"]
        })
    }

    fn requires_host_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        // Parse required fields — the schema validator has already checked
        // these, but defensive parsing avoids panics if called directly.
        let target_agent_id = params
            .get("target_agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParameters {
                name: DELEGATE_TOOL_NAME.to_string(),
                message: "missing required field: target_agent".to_string(),
            })?
            .to_string();

        let task = params
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParameters {
                name: DELEGATE_TOOL_NAME.to_string(),
                message: "missing required field: task".to_string(),
            })?
            .to_string();

        let context = params.get("context").cloned();
        let completion_mode = parse_completion_mode(params.get("completion_mode"))?;

        // Signal the agent turn loop to handle delegation.
        Err(ToolError::RequiresDelegation {
            name: DELEGATE_TOOL_NAME.to_string(),
            target_agent_id,
            task,
            context,
            completion_mode,
        })
    }
}

fn parse_completion_mode(value: Option<&Value>) -> Result<DelegateCompletionMode, ToolError> {
    match value.and_then(Value::as_str).unwrap_or("wait") {
        "wait" => Ok(DelegateCompletionMode::Wait),
        "detach" => Ok(DelegateCompletionMode::Detach),
        other => Err(ToolError::InvalidParameters {
            name: DELEGATE_TOOL_NAME.to_string(),
            message: format!("completion_mode must be \"wait\" or \"detach\", got {other:?}"),
        }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::MockToolContext;

    #[tokio::test]
    async fn delegate_tool_returns_requires_delegation() {
        let tool = DelegateTool;
        let ctx = MockToolContext::new("test-agent", "session-1");
        let params = json!({
            "target_agent": "reviewer",
            "task": "Review this code for correctness"
        });

        let result = tool.execute(params, &ctx).await;
        assert!(result.is_err());

        match result.unwrap_err() {
            ToolError::RequiresDelegation {
                target_agent_id,
                task,
                context,
                completion_mode,
                ..
            } => {
                assert_eq!(target_agent_id, "reviewer");
                assert_eq!(task, "Review this code for correctness");
                assert!(context.is_none());
                assert_eq!(completion_mode, DelegateCompletionMode::Wait);
            }
            other => panic!("expected RequiresDelegation, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delegate_tool_accepts_detached_completion_mode() {
        let tool = DelegateTool;
        let ctx = MockToolContext::new("test-agent", "session-1");
        let params = json!({
            "target_agent": "reviewer",
            "task": "Review independently",
            "completion_mode": "detach"
        });

        let result = tool.execute(params, &ctx).await;

        match result.unwrap_err() {
            ToolError::RequiresDelegation {
                completion_mode, ..
            } => {
                assert_eq!(completion_mode, DelegateCompletionMode::Detach);
            }
            other => panic!("expected RequiresDelegation, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delegate_tool_with_context() {
        let tool = DelegateTool;
        let ctx = MockToolContext::new("test-agent", "session-1");
        let params = json!({
            "target_agent": "analyzer",
            "task": "Analyze this data",
            "context": [{"role": "user", "content": "some context"}]
        });

        let result = tool.execute(params, &ctx).await;
        match result.unwrap_err() {
            ToolError::RequiresDelegation { context, .. } => {
                assert!(context.is_some());
            }
            other => panic!("expected RequiresDelegation, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delegate_tool_missing_target_returns_invalid_params() {
        let tool = DelegateTool;
        let ctx = MockToolContext::new("test-agent", "session-1");
        let params = json!({ "task": "do something" });

        let result = tool.execute(params, &ctx).await;
        assert!(matches!(
            result.unwrap_err(),
            ToolError::InvalidParameters { .. }
        ));
    }

    #[tokio::test]
    async fn delegate_tool_missing_task_returns_invalid_params() {
        let tool = DelegateTool;
        let ctx = MockToolContext::new("test-agent", "session-1");
        let params = json!({ "target_agent": "reviewer" });

        let result = tool.execute(params, &ctx).await;
        assert!(matches!(
            result.unwrap_err(),
            ToolError::InvalidParameters { .. }
        ));
    }

    #[test]
    fn delegate_tool_metadata() {
        let tool = DelegateTool;
        assert_eq!(tool.name(), "delegate");
        assert!(!tool.description().is_empty());

        let params = tool.parameters();
        assert!(params["properties"]["target_agent"].is_object());
        assert!(params["properties"]["task"].is_object());
        assert_eq!(
            params["properties"]["completion_mode"]["enum"],
            json!(["wait", "detach"])
        );
        assert_eq!(params["required"], json!(["target_agent", "task"]));
    }
}
