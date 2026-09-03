use async_trait::async_trait;
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::ToolError;
use swage_protocol::client::workflow::PostWorkflowRunEventRequest;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool_executor::GatewayToolContext;
use crate::workflow::{RunEventSource, post_external_run_event};

pub struct RunEventPostTool;

const TOOL: &str = "run_event_post";

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidParameters {
        name: TOOL.into(),
        message: message.into(),
    }
}

#[async_trait]
impl Tool for RunEventPostTool {
    fn name(&self) -> &str {
        TOOL
    }

    fn description(&self) -> &str {
        "Post trusted-attribution data to a workflow run's event channel. Use action=post with \
         the target run, a namespaced kind, and a JSON payload. The event is untrusted data for \
         the workflow to inspect; it is never injected as instructions and never starts a lead turn."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["post"]},
                "run_id": {"type": "string", "description": "The target workflow run UUID."},
                "kind": {"type": "string", "description": "A namespaced event kind such as decision.operator."},
                "payload": {"description": "JSON data carried by the event."}
            },
            "required": ["action", "run_id", "kind", "payload"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        if params.get("action").and_then(Value::as_str) != Some("post") {
            return Err(invalid("'action' must be 'post'"));
        }
        let run_id = params
            .get("run_id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("'run_id' is required"))?;
        let run_id = Uuid::parse_str(run_id).map_err(|_| invalid("'run_id' must be a UUID"))?;
        let kind = params
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("'kind' is required"))?
            .to_owned();
        let payload = params
            .get("payload")
            .cloned()
            .ok_or_else(|| invalid("'payload' is required"))?;
        let gateway = ctx
            .as_any()
            .downcast_ref::<GatewayToolContext>()
            .and_then(GatewayToolContext::app_state)
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: TOOL.into(),
                message: "this tool runs only inside a gateway host turn".into(),
            })?;
        let session_id =
            Uuid::parse_str(ctx.session_id()).map_err(|_| ToolError::ExecutionFailed {
                name: TOOL.into(),
                message: "the calling session id is invalid".into(),
            })?;
        let response = post_external_run_event(
            gateway,
            run_id,
            PostWorkflowRunEventRequest { kind, payload },
            RunEventSource {
                surface: "agent",
                agent_id: Some(ctx.agent_id().to_owned()),
                session_id: Some(session_id),
                turn_id: ctx.turn_id(),
            },
        )
        .await
        .map_err(|error| ToolError::ExecutionFailed {
            name: TOOL.into(),
            message: error.message().to_owned(),
        })?;
        serde_json::to_value(response).map_err(|error| ToolError::ExecutionFailed {
            name: TOOL.into(),
            message: error.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use swage_storage::{NewWorkflowRun, SpaceStore, SqliteStorage, Storage, WorkflowStore};

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_derives_agent_attribution_from_its_context() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let space = storage.get_or_create_default().await.unwrap();
        let run = storage
            .create_workflow_run(NewWorkflowRun {
                space_id: space.id,
                parent_run_id: None,
                parent_seq: None,
                script_version_id: None,
                inline_workflow_id: None,
                inline_workflow_name: None,
                inline_workflow_version: None,
                carrying_skill: None,
                task: None,
                source: Some("return null".into()),
                source_hash: "test".into(),
                args: Value::Null,
                budget: json!({}),
                engine_version: "test".into(),
                dispatch_bound_agent_ids: vec![],
                dispatch_bound_task_definition_ids: vec![],
                workflow_call_tools: None,
                inherited_worker_placement: None,
            })
            .await
            .unwrap();
        let state = crate::state::AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let session_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let mut base =
            swage_tool::context::MockToolContext::new("trusted-agent", session_id.to_string());
        base.turn_id = Some(turn_id);
        let (ws_tx, _ws_rx) = tokio::sync::mpsc::unbounded_channel();
        let context =
            GatewayToolContext::new(&base, ws_tx, Arc::clone(&storage) as Arc<dyn Storage>)
                .with_app_state(Some(state));

        RunEventPostTool
            .execute(
                json!({
                    "action": "post",
                    "run_id": run.id,
                    "kind": "agent.review",
                    "payload": {"agent_id": "spoofed", "verdict": "accept"}
                }),
                &context,
            )
            .await
            .unwrap();

        let events = storage.list_workflow_run_events(run.id).await.unwrap();
        assert_eq!(events[0].source_surface, "agent");
        assert_eq!(events[0].source_agent_id.as_deref(), Some("trusted-agent"));
        assert_eq!(events[0].source_session_id, Some(session_id));
        assert_eq!(events[0].source_turn_id, Some(turn_id));
        assert_eq!(
            events[0].payload["agent_id"], "spoofed",
            "payload remains untrusted data"
        );
    }
}
