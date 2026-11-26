use async_trait::async_trait;
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::ToolError;
use swage_protocol::client::workflow::PostWorkflowRunEventRequest;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::workflow::{RunEventSource, post_external_run_event};

#[cfg(test)]
use crate::tool_executor::GatewayToolContext;

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
                "kind": {"type": "string", "description": "The event kind accepted by the target workflow. To answer an Assembly decision.needed event, use decision.answer."},
                "payload": {"description": "JSON data matching the workflow's event contract. For an Assembly decision: {nonce, verdict, note?, dispositions?: [{finding: zero-based finding index, disposition: fix|contract-question|accept-with-note, note?: string}]}. Echo the pending nonce and choose an offered verdict. Each accept-with-note disposition requires a non-empty note."}
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
        let gateway = super::require_gateway_state(
            ctx,
            TOOL,
            "this tool runs only inside a gateway host turn",
        )?;
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
    use swage_storage::{NewWorkflowRun, SqliteStorage, Storage, TopicStore, WorkflowStore};

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_derives_agent_attribution_from_its_context() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = storage.general_scope().await.unwrap();
        let run = storage
            .create_workflow_run(NewWorkflowRun {
                source_hash: "test".into(),
                ..swage_storage::fixtures::new_workflow_run(topic.id)
            })
            .await
            .unwrap();
        let state = crate::state::AppState::for_test(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
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
