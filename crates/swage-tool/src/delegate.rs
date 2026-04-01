//! Built-in delegate tool — triggers inter-agent delegation from within
//! the turn loop.
//!
//! When the LLM calls this tool, it returns [`ToolError::RequiresDelegation`]
//! rather than producing output directly. The agent turn loop catches this
//! error variant and orchestrates the delegation via a [`DelegationHandler`],
//! following the same pattern as `RequiresApproval` for HITL gates.

use async_trait::async_trait;
use serde_json::{json, Value};
use swage_core::ToolError;

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
        "Delegate a task to another agent. The target agent executes the \
         task and returns its result as this tool's output."
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
                }
            },
            "required": ["target_agent", "task"]
        })
    }

    async fn execute(
        &self,
        params: Value,
        _ctx: &dyn ToolContext,
    ) -> Result<Value, ToolError> {
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

        // Signal the agent turn loop to handle delegation.
        Err(ToolError::RequiresDelegation {
            name: DELEGATE_TOOL_NAME.to_string(),
            target_agent_id,
            task,
            context,
        })
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
                ..
            } => {
                assert_eq!(target_agent_id, "reviewer");
                assert_eq!(task, "Review this code for correctness");
                assert!(context.is_none());
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
        assert_eq!(params["required"], json!(["target_agent", "task"]));
    }
}
