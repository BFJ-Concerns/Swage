//! Workflow discovery for agent turns.

use async_trait::async_trait;
use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_protocol::client::workflow::WorkflowScriptListQuery;
use swage_tool::context::ToolContext;
use uuid::Uuid;

use crate::{state::AppState, tool::Tool, tool_executor::GatewayToolContext};

pub const WORKFLOW_LIST_TOOL: &str = "workflow_list";

/// Project the established effective workflow listing for one caller session.
///
/// The HTTP listing owns precedence between authored, carried, and shipped
/// workflows. Keeping this helper as a projection means agent discovery and
/// operator listing cannot drift into separate catalogue rules.
pub async fn workflows_for_session(state: &AppState, session_id: Uuid) -> Result<Value, String> {
    let session = state
        .storage
        .get_session(session_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("session {session_id} was not found"))?;
    let Json(response) = crate::workflow::list_scripts(
        State(state.clone()),
        Path(session.space_id),
        Query(WorkflowScriptListQuery {
            agent_id: Some(session.agent_id),
        }),
    )
    .await
    .map_err(|error| error.0.to_string())?;

    Ok(json!({
        "workflows": response.scripts.into_iter().map(|script| json!({
            "name": script.name,
            "description": script.description,
        })).collect::<Vec<_>>(),
    }))
}

pub struct WorkflowListTool;

#[async_trait]
impl Tool for WorkflowListTool {
    fn name(&self) -> &str {
        WORKFLOW_LIST_TOOL
    }

    fn description(&self) -> &str {
        "List the workflows available in this session. Each entry gives its name and purpose. Workflow scripts do not declare argument schemas; pass any JSON value in workflow_run.args that the workflow expects."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, _params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let session_id =
            Uuid::parse_str(ctx.session_id()).map_err(|error| ToolError::InvalidParameters {
                name: self.name().into(),
                message: error.to_string(),
            })?;
        let state = ctx
            .as_any()
            .downcast_ref::<GatewayToolContext>()
            .and_then(GatewayToolContext::app_state)
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: self.name().into(),
                message: "workflow_list requires gateway execution".into(),
            })?;
        workflows_for_session(state, session_id)
            .await
            .map_err(|message| ToolError::ExecutionFailed {
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
    use swage_core::CompletionRequest;
    use swage_provider::{LlmProvider, ProviderConfig, openai::OpenAiProvider};
    use swage_storage::{
        NewSession, NewSpace, NewWorkflowScript, SessionStore, SpaceStore, SqliteStorage, Storage,
        WorkflowStore,
    };
    use swage_tool::context::MockToolContext;

    fn state(storage: Arc<SqliteStorage>) -> AppState {
        AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            storage as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        )
    }

    async fn context_for(
        state: AppState,
        storage: Arc<SqliteStorage>,
        session_id: Uuid,
    ) -> GatewayToolContext {
        let base = MockToolContext::new("gateway", session_id.to_string());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        GatewayToolContext::new(&base, tx, storage as Arc<dyn Storage>).with_app_state(Some(state))
    }

    #[test]
    fn provider_projection_advertises_an_empty_request_schema() {
        let dispatcher = crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(WorkflowListTool) as Arc<dyn crate::tool::Tool>
        ]);
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
            snapshot["body"]["tools"][0]["function"]["name"],
            WORKFLOW_LIST_TOOL
        );
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lists_shipped_and_saved_workflows_for_the_calling_session() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let space = storage.get_or_create_default().await.expect("space");
        storage
            .create_workflow_script(NewWorkflowScript {
                space_id: space.id,
                name: "operator-report".into(),
                description: "Produce an operator report.".into(),
                source: "return {};".into(),
                delivery_grant: None,
            })
            .await
            .expect("saved workflow");
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "gateway".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session");
        let context = context_for(
            state(Arc::clone(&storage)),
            Arc::clone(&storage),
            session.id,
        )
        .await;

        let result = WorkflowListTool
            .execute(json!({}), &context)
            .await
            .expect("workflow listing");
        let workflows = result["workflows"].as_array().expect("workflow array");
        assert!(
            workflows
                .iter()
                .any(|workflow| workflow["name"] == "research-sweep"),
            "shipped workflow missing: {workflows:?}"
        );
        let saved = workflows
            .iter()
            .find(|workflow| workflow["name"] == "operator-report")
            .expect("saved workflow");
        assert_eq!(saved["description"], "Produce an operator report.");
        assert!(saved.get("argument_guidance").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn listing_uses_the_callers_space() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let first_space = storage.get_or_create_default().await.expect("first space");
        let second_space = SpaceStore::create(
            &*storage,
            NewSpace {
                name: "Other space".into(),
                slug: "other-space".into(),
            },
        )
        .await
        .expect("second space");
        storage
            .create_workflow_script(NewWorkflowScript {
                space_id: first_space.id,
                name: "private-first-space-workflow".into(),
                description: "Only in the first space.".into(),
                source: "return {};".into(),
                delivery_grant: None,
            })
            .await
            .expect("saved workflow");
        let second_session = storage
            .create_session(NewSession {
                space_id: second_space.id,
                agent_id: "gateway".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("second session");
        let context = context_for(
            state(Arc::clone(&storage)),
            Arc::clone(&storage),
            second_session.id,
        )
        .await;

        let result = WorkflowListTool
            .execute(json!({}), &context)
            .await
            .expect("workflow listing");
        assert!(
            !result["workflows"]
                .as_array()
                .expect("workflow array")
                .iter()
                .any(|workflow| workflow["name"] == "private-first-space-workflow")
        );
        assert!(
            result["workflows"]
                .as_array()
                .expect("workflow array")
                .iter()
                .any(|workflow| workflow["name"] == "research-sweep")
        );
    }
}
