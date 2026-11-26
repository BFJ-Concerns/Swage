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
        "Hand one self-contained task to another agent, which works on it in a \
         session of its own and answers when it is done. The child starts from \
         `task` and, unless the installation is set to pass it more, sees none \
         of this conversation, so the task carries everything the child needs. \
         `completion_mode` decides whether this turn waits for the child's \
         answer or carries on beside it. A job of many agent calls that has to \
         survive an interruption belongs to `workflow_run`. Name a \
         child to come back to it later with its context intact."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target_agent": {
                    "type": "string",
                    "description": "The agent the child runs as, by its agent definition's `id`."
                },
                "task": {
                    "type": "string",
                    "description": "The whole brief for the child: what is wanted, how the result will be judged, what to send back, and the facts it needs to do it."
                },
                "task_definition": {
                    "type": "string",
                    "description": "Optional named task definition to apply to the child."
                },
                "skills": {
                    "type": "array",
                    "description": "Skills to activate for the child, by the names the skill catalogue lists.",
                    "items": { "type": "string" }
                },
                "child": {
                    "type": "string",
                    "description": "A name for this child. Give the same name in a later delegate call to continue with that child rather than starting a fresh one: it keeps everything it was told and did before. Its agent, skills, and working directory are fixed by the first call that names it."
                },
                "context": {
                    "type": "array",
                    "description": "Messages handed to the child as background ahead of the task, in place of whatever of this conversation the installation would pass by itself. Each item has to be a complete stored message, with its `id`, `role`, `created_at` and a `content` list of parts such as `{\"type\": \"text\", \"text\": …}`; an item in any other shape is dropped without notice. What the child needs to know is more surely written into `task`.",
                    "items": {
                        "type": "object"
                    }
                },
                "completion_mode": {
                    "type": "string",
                    "enum": ["wait", "detach"],
                    "default": "wait",
                    "description": "`wait`, the default, when the next step needs the child's answer: this turn stops until the child finishes, and the call returns what it answered, with an `incomplete_reason` where the child was cut short. `detach` for work that can go on beside this turn: the call returns at once with references to the child's work (`delegate_message_id`, `child_session_id`, `child_thread_id`), and the child's result reaches this conversation when it is ready."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for the delegated worker."
                },
                "isolation": {
                    "type": "string",
                    "enum": ["worktree"],
                    "description": "Ask the runtime to provision an isolated git worktree rooted at cwd."
                },
                "elements": {
                    "type": "array",
                    "description": "Named briefing elements to compose for the delegated child.",
                    "items": { "type": "string" }
                },
                "variables": {
                    "type": "object",
                    "description": "Call-scoped string values for the delegated child's context composition.",
                    "additionalProperties": { "type": "string" }
                },
                "assembly": {
                    "type": "string",
                    "pattern": "^[a-z0-9-]+$",
                    "description": "Named context assembly to use for the delegated child."
                }
            },
            "required": ["target_agent", "task"]
        })
    }

    fn requires_device_execution(&self) -> bool {
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
        let task_definition = params
            .get("task_definition")
            .and_then(Value::as_str)
            .map(str::to_string);
        let skills = params
            .get("skills")
            .map(|value| {
                value
                    .as_array()
                    .ok_or_else(|| ToolError::InvalidParameters {
                        name: DELEGATE_TOOL_NAME.to_string(),
                        message: "skills must be an array of strings".to_string(),
                    })?
                    .iter()
                    .map(|skill| {
                        skill.as_str().map(str::to_string).ok_or_else(|| {
                            ToolError::InvalidParameters {
                                name: DELEGATE_TOOL_NAME.to_string(),
                                message: "skills must be an array of strings".to_string(),
                            }
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let child = params
            .get("child")
            .and_then(Value::as_str)
            .map(str::to_string);
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_string);
        let isolation = match params.get("isolation").and_then(Value::as_str) {
            None => None,
            Some("worktree") => Some(swage_protocol::client::workflow::WorkerIsolation::Worktree),
            Some(other) => {
                return Err(ToolError::InvalidParameters {
                    name: DELEGATE_TOOL_NAME.to_string(),
                    message: format!("isolation must be \"worktree\", got {other:?}"),
                });
            }
        };
        let composition =
            swage_core::DispatchContextComposition::from_payload(&params).map_err(|error| {
                ToolError::InvalidParameters {
                    name: DELEGATE_TOOL_NAME.to_string(),
                    message: format!("invalid context composition: {error}"),
                }
            })?;
        let completion_mode = parse_completion_mode(params.get("completion_mode"))?;

        // Signal the agent turn loop to handle delegation.
        Err(ToolError::RequiresDelegation {
            request: Box::new(swage_core::DelegationRequest {
                name: DELEGATE_TOOL_NAME.to_string(),
                target_agent_id,
                task,
                task_definition,
                context,
                cwd,
                isolation,
                skills,
                child,
                composition: Box::new(composition),
                completion_mode,
            }),
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
            ToolError::RequiresDelegation { request } => {
                assert_eq!(request.target_agent_id, "reviewer");
                assert_eq!(request.task, "Review this code for correctness");
                assert!(request.context.is_none());
                assert_eq!(request.completion_mode, DelegateCompletionMode::Wait);
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
            ToolError::RequiresDelegation { request } => {
                assert_eq!(request.completion_mode, DelegateCompletionMode::Detach);
            }
            other => panic!("expected RequiresDelegation, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delegate_tool_forwards_cwd_and_worktree_isolation() {
        let result = DelegateTool
            .execute(
                json!({
                    "target_agent": "reviewer",
                    "task": "Review independently",
                    "skills": ["code-review"],
                    "cwd": "/repo",
                    "isolation": "worktree"
                }),
                &MockToolContext::new("test-agent", "session-1"),
            )
            .await;
        match result.unwrap_err() {
            ToolError::RequiresDelegation { request } => {
                assert_eq!(request.cwd.as_deref(), Some("/repo"));
                assert_eq!(
                    request.isolation,
                    Some(swage_protocol::client::workflow::WorkerIsolation::Worktree)
                );
                assert_eq!(request.skills, Some(vec!["code-review".into()]));
            }
            other => panic!("expected RequiresDelegation, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delegate_tool_carries_context_composition_options() {
        let tool = DelegateTool;
        let ctx = MockToolContext::new("test-agent", "session-1");
        let result = tool
            .execute(
                json!({
                    "target_agent": "reviewer",
                    "task": "Review independently",
                    "skills": ["code-review"],
                    "elements": ["current-work"],
                    "variables": {"owner": "delegate-owner"},
                    "assembly": "focused"
                }),
                &ctx,
            )
            .await;

        match result.unwrap_err() {
            ToolError::RequiresDelegation { request } => {
                assert_eq!(
                    request.composition.elements,
                    Some(vec!["current-work".into()])
                );
                assert_eq!(
                    request
                        .composition
                        .variables
                        .as_ref()
                        .and_then(|values| values.get("owner")),
                    Some(&"delegate-owner".to_string())
                );
                assert_eq!(request.composition.assembly.as_deref(), Some("focused"));
                assert_eq!(request.skills, Some(vec!["code-review".into()]));
            }
            other => panic!("expected RequiresDelegation, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delegate_tool_rejects_unsafe_assembly_name() {
        let tool = DelegateTool;
        let ctx = MockToolContext::new("test-agent", "session-1");
        let result = tool
            .execute(
                json!({
                    "target_agent": "reviewer",
                    "task": "Review independently",
                    "assembly": "../../outside"
                }),
                &ctx,
            )
            .await;

        assert!(matches!(
            result,
            Err(ToolError::InvalidParameters { message, .. })
                if message.contains("invalid context assembly name")
        ));
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
            ToolError::RequiresDelegation { request } => {
                assert!(request.context.is_some());
            }
            other => panic!("expected RequiresDelegation, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delegate_tool_forwards_named_task_definition() {
        let result = DelegateTool
            .execute(
                json!({
                    "target_agent": "reviewer",
                    "task": "Review",
                    "task_definition": "strict-review"
                }),
                &MockToolContext::new("test-agent", "session-1"),
            )
            .await
            .unwrap_err();
        match result {
            ToolError::RequiresDelegation { request } => {
                assert_eq!(request.task_definition.as_deref(), Some("strict-review"))
            }
            other => panic!("expected delegation, got {other:?}"),
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
