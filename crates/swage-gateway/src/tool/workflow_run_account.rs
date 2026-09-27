//! The terminal account of a workflow run, for the agent that started it.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_protocol::client::workflow::WorkflowRunTerminalAccount;
use swage_tool::context::ToolContext;
use uuid::Uuid;

use crate::{state::AppState, tool::Tool, tool_executor::GatewayToolContext};

pub const WORKFLOW_RUN_ACCOUNT_TOOL: &str = "workflow_run_account";

/// Read one run's account, scoped to the caller's own topic.
///
/// A run in another topic reads as absent rather than forbidden: the caller has
/// no way to know it exists, so naming it would be the disclosure.
pub async fn account_for_run(
    state: &AppState,
    session_id: Uuid,
    run_id: Uuid,
) -> Result<Value, String> {
    let session = state
        .storage
        .get_session(session_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("session {session_id} was not found"))?;
    let record = state
        .storage
        .get_workflow_run(run_id)
        .await
        .map_err(|error| error.to_string())?
        .filter(|record| record.topic_id == session.topic_id)
        .ok_or_else(|| format!("workflow run {run_id} was not found"))?;

    let account = record
        .terminal_account
        .map(serde_json::from_value::<WorkflowRunTerminalAccount>)
        .transpose()
        .map_err(|error| {
            format!("workflow run {run_id} terminal account does not match the recorded contract: {error}")
        })?;

    let Some(account) = account else {
        return Ok(json!({
            "run_id": run_id,
            "script_name": record.script_name,
            "state": record.state,
            "ended": false,
            "note": "This run has not ended, so it has no account yet.",
        }));
    };

    Ok(json!({
        "run_id": run_id,
        "script_name": record.script_name,
        "state": record.state,
        "ended": true,
        "account": serde_json::to_value(&account).map_err(|error| error.to_string())?,
    }))
}

pub struct WorkflowRunAccountTool;

#[async_trait]
impl Tool for WorkflowRunAccountTool {
    fn name(&self) -> &str {
        WORKFLOW_RUN_ACCOUNT_TOOL
    }

    fn description(&self) -> &str {
        "Read how a workflow run ended and where its workers stood when it did. The account names the ending (returned, threw, cancelled, wound down, or interrupted without resume) and partitions every worker the run dispatched into done, failed, cut short, and unsettled, each with the directory its work was left in. A run that ended without returning also carries the completed workers' output as partial results — recoverable work, never the run's result. Use this after a run ends, especially one that ended without a result, to find out what survived and where."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "run_id": {
                    "type": "string",
                    "description": "The id of the run to account for.",
                },
            },
            "required": ["run_id"],
            "additionalProperties": false,
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let invalid = |message: String| ToolError::InvalidParameters {
            name: WORKFLOW_RUN_ACCOUNT_TOOL.into(),
            message,
        };
        let run_id = params
            .get("run_id")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("run_id is required".into()))?;
        let run_id = Uuid::parse_str(run_id).map_err(|error| invalid(error.to_string()))?;
        let session_id =
            Uuid::parse_str(ctx.session_id()).map_err(|error| invalid(error.to_string()))?;
        let state = ctx
            .as_any()
            .downcast_ref::<GatewayToolContext>()
            .and_then(GatewayToolContext::app_state)
            .ok_or_else(|| ToolError::ExecutionFailed {
                name: WORKFLOW_RUN_ACCOUNT_TOOL.into(),
                message: format!("{WORKFLOW_RUN_ACCOUNT_TOOL} requires gateway execution"),
            })?;
        account_for_run(state, session_id, run_id)
            .await
            .map_err(|message| ToolError::ExecutionFailed {
                name: WORKFLOW_RUN_ACCOUNT_TOOL.into(),
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
        NewSession, NewTopic, NewWorkflowRun, SessionStore, SqliteStorage, Storage, TopicStore,
        WorkflowStore,
    };
    use swage_tool::context::MockToolContext;

    fn state(storage: Arc<SqliteStorage>) -> AppState {
        AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            storage as Arc<dyn TopicStore>,
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

    async fn run_in(storage: &SqliteStorage, topic_id: Uuid) -> Uuid {
        storage
            .create_workflow_run(NewWorkflowRun {
                inline_workflow_name: Some("fan-out".into()),
                source: Some("return null;".into()),
                source_hash: "hash".into(),
                budget: Value::Null,
                ..swage_storage::fixtures::new_workflow_run(topic_id)
            })
            .await
            .expect("run")
            .id
    }

    fn recorded_account() -> Value {
        json!({
            "ending": {"ending": "cancelled"},
            "workers": {
                "done": [],
                "failed": [],
                "cut_short": [{
                    "journal_entry_id": Uuid::nil(),
                    "seq": 1,
                    "label": "hanging",
                    "agent_definition_id": "default",
                    "placement": "/scratch/worktrees/hanging",
                    "termination": {
                        "termination": "cut_short",
                        "cause": "run_cancelled",
                        "detail": "WorkflowCancelledError: run cancelled"
                    },
                }],
                "unsettled": [],
            },
            "partial_results": {
                "reason": "the run was cancelled before returning",
                "results": [{
                    "journal_entry_id": Uuid::nil(),
                    "seq": 0,
                    "label": "survey",
                    "output": "survey reply",
                    "placement": "/scratch/worktrees/survey",
                }],
            },
            "settled_at": "2026-09-02T00:00:00Z",
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lead_reads_the_ending_the_partition_and_the_recovered_work() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let run_id = run_in(&storage, topic.id).await;
        storage
            .set_workflow_run_terminal_account(run_id, recorded_account())
            .await
            .expect("record the account");
        let session = storage
            .create_session(NewSession {
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .expect("session");
        let context = context_for(
            state(Arc::clone(&storage)),
            Arc::clone(&storage),
            session.id,
        );

        let result = WorkflowRunAccountTool
            .execute(json!({"run_id": run_id.to_string()}), &context)
            .await
            .expect("account");
        assert_eq!(result["ended"], true);
        assert_eq!(result["account"]["ending"]["ending"], "cancelled");
        assert_eq!(
            result["account"]["workers"]["cut_short"][0]["placement"],
            "/scratch/worktrees/hanging"
        );
        assert_eq!(
            result["account"]["partial_results"]["results"][0]["output"],
            "survey reply"
        );
        assert_eq!(
            result["account"]["partial_results"]["results"][0]["placement"],
            "/scratch/worktrees/survey"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_run_still_going_reads_as_unended_rather_than_missing() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let run_id = run_in(&storage, topic.id).await;
        let session = storage
            .create_session(NewSession {
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .expect("session");
        let context = context_for(
            state(Arc::clone(&storage)),
            Arc::clone(&storage),
            session.id,
        );

        let result = WorkflowRunAccountTool
            .execute(json!({"run_id": run_id.to_string()}), &context)
            .await
            .expect("account");
        assert_eq!(result["ended"], false);
        assert!(result.get("account").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lead_cannot_read_a_terminal_account_outside_the_recorded_contract() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let run_id = run_in(&storage, topic.id).await;
        storage
            .set_workflow_run_terminal_account(run_id, json!({"ending": "invented"}))
            .await
            .expect("record malformed account");
        let session = storage
            .create_session(NewSession {
                ..swage_storage::fixtures::new_session(topic.id)
            })
            .await
            .expect("session");
        let context = context_for(
            state(Arc::clone(&storage)),
            Arc::clone(&storage),
            session.id,
        );

        let error = WorkflowRunAccountTool
            .execute(json!({"run_id": run_id.to_string()}), &context)
            .await
            .expect_err("a malformed account must not reach the lead");
        assert!(
            error
                .to_string()
                .contains("does not match the recorded contract"),
            "{error}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_run_in_another_topic_reads_as_absent() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let other = TopicStore::create(
            &*storage,
            NewTopic {
                name: "Other topic".into(),
                slug: "other-topic".into(),
            },
        )
        .await
        .expect("second topic");
        let run_id = run_in(&storage, topic.id).await;
        storage
            .set_workflow_run_terminal_account(run_id, recorded_account())
            .await
            .expect("record the account");
        let session = storage
            .create_session(NewSession {
                ..swage_storage::fixtures::new_session(other.id)
            })
            .await
            .expect("session");
        let context = context_for(
            state(Arc::clone(&storage)),
            Arc::clone(&storage),
            session.id,
        );

        let error = WorkflowRunAccountTool
            .execute(json!({"run_id": run_id.to_string()}), &context)
            .await
            .expect_err("another topic's run is not readable");
        assert!(error.to_string().contains("was not found"), "{error}");
    }
}
