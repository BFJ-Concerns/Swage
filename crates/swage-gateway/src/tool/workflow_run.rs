use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_protocol::client::workflow::WorkflowBudget;
use swage_tool::context::ToolContext;
use uuid::Uuid;

use crate::{
    tool::Tool,
    tool_executor::GatewayToolContext,
    workflow::{GatewayWorkflowLauncher, WorkflowLauncher},
};

pub struct WorkflowRunTool;

#[async_trait]
impl Tool for WorkflowRunTool {
    fn name(&self) -> &str {
        "workflow_run"
    }
    fn description(&self) -> &str {
        "Start either an available workflow by name or a newly authored workflow from JavaScript source. Provide exactly one of name or source. Inline source inherits this session's dispatch bounds and cannot deliver outside the run; use wait=true only when this turn needs the final result."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "description": "Provide exactly one of name or source.",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The available workflow's name in this space. Use this or source, but not both."
                },
                "source": {
                    "type": "string",
                    "description": "JavaScript source for a one-off workflow authored in this session. Use this or name, but not both."
                },
                "args": {
                    "description": "Arguments handed to the script as `args`; any JSON value the workflow expects."
                },
                "budget": {
                    "type": "object",
                    "description": "Optional run limits.",
                    "properties": {
                        "max_agent_calls": {"type": "integer", "description": "Most agent calls the run may make."},
                        "max_total_tokens": {"type": "integer", "description": "Token ceiling across all the run's agent calls."},
                        "reserved_closing_tokens": {"type": "integer", "description": "Tokens held back from ordinary work and released when the run enters wind-down."},
                        "max_concurrent_agent_calls": {"type": "integer", "description": "Most agent calls in flight at once."}
                    }
                },
                "wait": {
                    "type": "boolean",
                    "default": false,
                    "description": "Wait for the run to finish and return its final output. Leave false to start it detached and get the run reference."
                }
            },
            "additionalProperties": false
        })
    }
    fn requires_device_execution(&self) -> bool {
        true
    }
    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let name = params.get("name").and_then(Value::as_str);
        let source = params.get("source").and_then(Value::as_str);
        if matches!((name, source), (Some(_), Some(_)) | (None, None)) {
            return Err(ToolError::InvalidParameters {
                name: self.name().into(),
                message: "provide exactly one of name or source".into(),
            });
        }
        let args = params.get("args").cloned().unwrap_or(Value::Null);
        let budget = params
            .get("budget")
            .cloned()
            .map(serde_json::from_value::<WorkflowBudget>)
            .transpose()
            .map_err(|error| ToolError::InvalidParameters {
                name: self.name().into(),
                message: error.to_string(),
            })?;
        let wait = params.get("wait").and_then(Value::as_bool).unwrap_or(false);
        let session_id =
            Uuid::parse_str(ctx.session_id()).map_err(|error| ToolError::InvalidParameters {
                name: self.name().into(),
                message: error.to_string(),
            })?;
        let gateway = ctx
            .as_any()
            .downcast_ref::<GatewayToolContext>()
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().into(),
                message: "workflow_run requires gateway execution".into(),
            })?;
        let state = gateway
            .app_state()
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().into(),
                message: "workflow gateway state is unavailable".into(),
            })?;
        let launcher = GatewayWorkflowLauncher::new(state.clone());
        let result = match (name, source) {
            (Some(name), None) => {
                launcher
                    .start_by_name(session_id, name, args, budget, wait)
                    .await
            }
            (None, Some(source)) => {
                launcher
                    .start_inline(session_id, source, args, budget, wait)
                    .await
            }
            _ => unreachable!("name/source validation is exhaustive"),
        };
        result.map_err(|message| ToolError::ExecutionFailed {
            name: self.name().into(),
            message,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::state::AppState;
    use sha2::{Digest, Sha256};
    use swage_core::{CompletionRequest, ToolError};
    use swage_provider::{LlmProvider, ProviderConfig, openai::OpenAiProvider};
    use swage_storage::{
        NewSession, SessionStore, SpaceStore, SqliteStorage, Storage, WorkflowStore,
    };
    use swage_tool::context::MockToolContext;

    #[tokio::test]
    async fn dispatcher_returns_actionable_error_for_ambiguous_workflow_input() {
        let dispatcher =
            crate::tool::ToolDispatcher::allow_all(
                vec![Arc::new(WorkflowRunTool) as Arc<dyn Tool>],
            );
        let context = MockToolContext::new("agent", Uuid::new_v4().to_string());

        for params in [json!({}), json!({"name": "saved", "source": "return null"})] {
            let error = dispatcher
                .dispatch("workflow_run", params, &context, "agent")
                .await
                .expect_err("ambiguous input should be rejected through the dispatcher");
            assert!(
                matches!(
                    error,
                    ToolError::InvalidParameters { ref message, .. }
                        if message == "provide exactly one of name or source"
                ),
                "unexpected dispatcher error: {error}"
            );
        }

        for params in [json!({"name": "saved"}), json!({"source": "return null"})] {
            let error = dispatcher
                .dispatch("workflow_run", params, &context, "agent")
                .await
                .expect_err("one workflow input should pass schema validation and reach execution");
            assert!(
                matches!(
                    error,
                    ToolError::ExecutionFailed { ref message, .. }
                        if message == "workflow_run requires gateway execution"
                ),
                "one workflow input did not reach execution: {error}"
            );
        }
    }

    #[test]
    fn provider_projection_describes_the_workflow_input_choice() {
        let dispatcher =
            crate::tool::ToolDispatcher::allow_all(
                vec![Arc::new(WorkflowRunTool) as Arc<dyn Tool>],
            );
        let provider =
            OpenAiProvider::from_config(ProviderConfig::new("key", "model", "http://localhost"));
        let snapshot = provider.provider_request_snapshot(
            &CompletionRequest {
                messages: vec![],
                model: None,
                effort: None,
                max_tokens: None,
                temperature: None,
                stop_sequences: None,
                metadata: None,
                tool_definitions: dispatcher.tool_definitions(),
            },
            false,
        );
        let schema = &snapshot["body"]["tools"][0]["function"]["parameters"];

        assert_eq!(
            schema["description"],
            "Provide exactly one of name or source."
        );
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["name"]["type"], "string");
        assert_eq!(schema["properties"]["source"]["type"], "string");
        assert!(schema.get("oneOf").is_none());
        assert!(schema.get("not").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn inline_source_runs_with_session_bounds_without_delivery_and_is_inspectable() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let space = storage.get_or_create_default().await.expect("space");
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "current-agent".into(),
                metadata: Some(json!({
                    "dispatch_bound_agent_ids": ["ancestor-agent"],
                    "dispatch_bound_task_definition_ids": ["ancestor-task"]
                })),
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session");
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let base = MockToolContext::new("current-agent", session.id.to_string());
        let (ws_tx, _ws_rx) = tokio::sync::mpsc::unbounded_channel();
        let context =
            GatewayToolContext::new(&base, ws_tx, Arc::clone(&storage) as Arc<dyn Storage>)
                .with_app_state(Some(state.clone()));
        let source = "return { answer: 42, delivery: typeof deliver };";

        let result = WorkflowRunTool
            .execute(json!({"source": source, "wait": true}), &context)
            .await
            .expect("inline workflow completes");
        let run_id: Uuid = serde_json::from_value(result["id"].clone()).expect("run id");
        assert_eq!(result["state"], "completed");
        assert_eq!(
            result["final_output"],
            json!({"answer": 42, "delivery": "undefined"})
        );
        assert_eq!(result["source"], source);

        let record = storage
            .get_workflow_run(run_id)
            .await
            .expect("run query")
            .expect("run record");
        assert_eq!(
            record.dispatch_bound_agent_ids,
            vec!["ancestor-agent", "current-agent"]
        );
        assert_eq!(
            record.dispatch_bound_task_definition_ids,
            vec!["ancestor-task"]
        );
        assert_eq!(record.source.as_deref(), Some(source));
        let expected_hash = Sha256::digest(source.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(record.source_hash, expected_hash);
        assert_eq!(result["source_hash"], expected_hash);

        let axum::Json(detail) = crate::workflow::get_run(
            axum::extract::State(state.clone()),
            axum::extract::Path(run_id),
        )
        .await
        .expect("run detail");
        assert_eq!(detail.run.source.as_deref(), Some(source));
        assert_eq!(detail.run.source_hash, expected_hash);

        let axum::Json(list) =
            crate::workflow::list_runs(axum::extract::State(state), axum::extract::Path(space.id))
                .await
                .expect("list runs");
        let listed = list
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .expect("listed run");
        assert_eq!(listed.source.as_deref(), Some(source));
        assert_eq!(listed.source_hash, expected_hash);
    }
}
