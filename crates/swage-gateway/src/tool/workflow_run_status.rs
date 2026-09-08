//! What the space's workflow runs are doing and waiting on, for the agent
//! steering them.
//!
//! A lead that wakes into a fresh session has no transcript to tell it which
//! pipelines are parked on a decision. This tool answers that from the record:
//! every run in the caller's space that has not ended, with the events it has
//! posted that nobody has yet acted on — a pipeline's hold is such an event,
//! carrying the hold's content and the verdicts it accepts.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_storage::{WorkflowRunEventRecord, WorkflowRunRecord};
use swage_tool::context::ToolContext;
use uuid::Uuid;

use crate::{state::AppState, tool::Tool, tool_executor::GatewayToolContext};

pub const WORKFLOW_RUN_STATUS_TOOL: &str = "workflow_run_status";

/// Run states after which a run posts nothing and answers to nothing.
pub fn is_terminal_run_state(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "cancelled")
}

fn event_value(event: &WorkflowRunEventRecord) -> Value {
    json!({
        "seq": event.seq,
        "kind": event.kind,
        "payload": event.payload,
        "posted_at": event.posted_at.to_rfc3339(),
        "source": {
            "surface": event.source_surface,
            "agent_id": event.source_agent_id,
            "session_id": event.source_session_id,
        },
    })
}

async fn run_value(state: &AppState, run: &WorkflowRunRecord) -> Result<Value, String> {
    let pending = state
        .storage
        .list_pending_workflow_run_events(run.id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "run_id": run.id,
        "script_name": run.script_name,
        "carrying_skill": run.carrying_skill,
        "task": run.task,
        "state": run.state,
        "ended": is_terminal_run_state(&run.state),
        "wake_at": run.wake_at.map(|at| at.to_rfc3339()),
        "created_at": run.created_at.to_rfc3339(),
        "error_message": run.error_message,
        "pending_events": pending.iter().map(event_value).collect::<Vec<_>>(),
    }))
}

/// The open runs of the caller's space, or one named run in any state.
///
/// A run in another space reads as absent rather than forbidden: the caller
/// has no way to know it exists, so naming it would be the disclosure.
pub async fn status_for_session(
    state: &AppState,
    session_id: Uuid,
    run_id: Option<Uuid>,
) -> Result<Value, String> {
    let session = state
        .storage
        .get_session(session_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("session {session_id} was not found"))?;

    let runs = match run_id {
        Some(run_id) => {
            let record = state
                .storage
                .get_workflow_run(run_id)
                .await
                .map_err(|error| error.to_string())?
                .filter(|record| record.space_id == session.space_id)
                .ok_or_else(|| format!("workflow run {run_id} was not found"))?;
            vec![record]
        }
        None => state
            .storage
            .list_workflow_runs(session.space_id)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|record| !is_terminal_run_state(&record.state))
            .collect(),
    };

    let mut values = Vec::with_capacity(runs.len());
    for run in &runs {
        values.push(run_value(state, run).await?);
    }
    Ok(json!({ "runs": values }))
}

pub struct WorkflowRunStatusTool;

#[async_trait]
impl Tool for WorkflowRunStatusTool {
    fn name(&self) -> &str {
        WORKFLOW_RUN_STATUS_TOOL
    }

    fn description(&self) -> &str {
        "See which workflow runs in this space are still going and what each is waiting on. \
         Without a run_id, lists every run that has not ended — running, parked, or \
         interrupted — with its undelivered events: a pipeline holding for a decision shows \
         the hold's content and the verdicts it accepts, and you answer it with \
         run_event_post. With a run_id, reads that one run in any state. Use this at the \
         start of a wake to find the decisions waiting for you, and after acting to confirm \
         a run moved on."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "Optional. Read this one run (any state) instead of listing the space's open runs.",
                },
            },
            "additionalProperties": false,
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let invalid = |message: String| ToolError::InvalidParameters {
            name: WORKFLOW_RUN_STATUS_TOOL.into(),
            message,
        };
        let run_id = params
            .get("run_id")
            .and_then(Value::as_str)
            .map(|raw| Uuid::parse_str(raw).map_err(|error| invalid(error.to_string())))
            .transpose()?;
        let session_id =
            Uuid::parse_str(ctx.session_id()).map_err(|error| invalid(error.to_string()))?;
        let state = ctx
            .as_any()
            .downcast_ref::<GatewayToolContext>()
            .and_then(GatewayToolContext::app_state)
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: WORKFLOW_RUN_STATUS_TOOL.into(),
                message: format!("{WORKFLOW_RUN_STATUS_TOOL} requires gateway execution"),
            })?;
        status_for_session(state, session_id, run_id)
            .await
            .map_err(|message| ToolError::ExecutionFailed {
                name: WORKFLOW_RUN_STATUS_TOOL.into(),
                message,
            })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::state::AppState;
    use swage_storage::{
        NewSession, NewSpace, NewWorkflowRun, NewWorkflowRunEvent, SessionStore, SpaceStore,
        SqliteStorage, Storage, WorkflowStore,
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

    fn context_for(
        state: AppState,
        storage: Arc<SqliteStorage>,
        session_id: Uuid,
    ) -> GatewayToolContext {
        let base = MockToolContext::new("gateway", session_id.to_string());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        GatewayToolContext::new(&base, tx, storage as Arc<dyn Storage>).with_app_state(Some(state))
    }

    async fn run_in(storage: &SqliteStorage, space_id: Uuid, name: &str) -> Uuid {
        storage
            .create_workflow_run(NewWorkflowRun {
                space_id,
                parent_run_id: None,
                parent_seq: None,
                script_version_id: None,
                inline_workflow_id: None,
                inline_workflow_name: Some(name.into()),
                inline_workflow_version: None,
                carrying_skill: None,
                task: None,
                source: Some("return null;".into()),
                source_hash: "hash".into(),
                args: Value::Null,
                budget: Value::Null,
                engine_version: "test".into(),
                dispatch_bound_agent_ids: vec![],
                dispatch_bound_task_definition_ids: vec![],
                workflow_call_tools: None,
                inherited_worker_placement: None,
            })
            .await
            .expect("run")
            .id
    }

    async fn session_in(storage: &SqliteStorage, space_id: Uuid) -> Uuid {
        storage
            .create_session(NewSession {
                space_id,
                agent_id: "gateway".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session")
            .id
    }

    async fn post(storage: &SqliteStorage, run_id: Uuid, kind: &str, payload: Value) {
        storage
            .create_workflow_run_event(NewWorkflowRunEvent {
                run_id,
                journal_entry_id: Uuid::new_v4(),
                kind: kind.into(),
                payload,
                source_surface: "script".into(),
                source_agent_id: None,
                source_session_id: None,
                source_turn_id: None,
            })
            .await
            .expect("event");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lead_sees_open_runs_with_the_decisions_they_await_and_not_ended_ones() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let space = storage.get_or_create_default().await.expect("space");
        let holding = run_in(&storage, space.id, "build-verify").await;
        post(
            &storage,
            holding,
            "decision.needed",
            json!({"stage": "restate", "nonce": "restate-1", "options": ["accept", "correct", "kill"]}),
        )
        .await;
        let ended = run_in(&storage, space.id, "docs-update").await;
        storage
            .update_workflow_run_state(ended, "completed", None, None, None)
            .await
            .expect("end the run");
        let session = session_in(&storage, space.id).await;
        let context = context_for(state(Arc::clone(&storage)), Arc::clone(&storage), session);

        let result = WorkflowRunStatusTool
            .execute(json!({}), &context)
            .await
            .expect("status");
        let runs = result["runs"].as_array().expect("runs");
        assert_eq!(runs.len(), 1, "an ended run is not open: {result}");
        assert_eq!(runs[0]["run_id"], json!(holding));
        assert_eq!(runs[0]["state"], "running");
        assert_eq!(runs[0]["ended"], false);
        assert_eq!(runs[0]["pending_events"][0]["kind"], "decision.needed");
        assert_eq!(
            runs[0]["pending_events"][0]["payload"]["nonce"],
            "restate-1"
        );

        let one = WorkflowRunStatusTool
            .execute(json!({"run_id": ended.to_string()}), &context)
            .await
            .expect("named run");
        assert_eq!(one["runs"][0]["ended"], true);
        assert_eq!(one["runs"][0]["state"], "completed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_run_in_another_space_reads_as_absent() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let mine = storage.get_or_create_default().await.expect("space");
        let theirs = SpaceStore::create(
            storage.as_ref(),
            NewSpace {
                name: "theirs".into(),
                slug: swage_storage::SpaceSlug::new("theirs").expect("slug"),
            },
        )
        .await
        .expect("other space");
        let foreign = run_in(&storage, theirs.id, "build-verify").await;
        post(&storage, foreign, "decision.needed", json!({"nonce": "x"})).await;
        let session = session_in(&storage, mine.id).await;
        let context = context_for(state(Arc::clone(&storage)), Arc::clone(&storage), session);

        let listed = WorkflowRunStatusTool
            .execute(json!({}), &context)
            .await
            .expect("status");
        assert_eq!(listed["runs"].as_array().map(Vec::len), Some(0));
        let error = WorkflowRunStatusTool
            .execute(json!({"run_id": foreign.to_string()}), &context)
            .await
            .expect_err("foreign run");
        assert!(error.to_string().contains("was not found"), "{error}");
    }
}
