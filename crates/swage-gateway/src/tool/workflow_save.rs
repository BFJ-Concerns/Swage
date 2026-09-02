//! `workflow_save` — keeping a workflow the conversation authored.
//!
//! `workflow_run` can already start a script written in the turn, but that
//! script lives only as long as the message carrying it. This tool is how
//! orchestration invented in a conversation becomes a named, versioned thing
//! the space keeps and can run again by name.
//!
//! Host-only (`requires_device_execution`): the save reaches live gateway
//! state, and it lands as a recorded configuration change (see
//! `service::workflow_script`) naming the conversation that authored it. The
//! authoring session comes from the turn's own context, never from a
//! parameter, so the record cannot name anyone else.
//!
//! The tool has no delivery-grant parameter, and it cannot acquire one: the
//! saved row is marked with its authoring session, and a run resolved from
//! such a script is handed no grant whatever the row's column says. Authoring
//! therefore never widens what the authoring session could already do.

use async_trait::async_trait;
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::service::{WorkflowScriptDraft, WorkflowScriptEdit};
use crate::state::AppState;
use crate::tool::Tool;
use crate::tool_executor::GatewayToolContext;

pub const WORKFLOW_SAVE_TOOL: &str = "workflow_save";

pub struct WorkflowSaveTool;

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidParameters {
        name: WORKFLOW_SAVE_TOOL.to_owned(),
        message: message.into(),
    }
}

fn failed(message: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed {
        name: WORKFLOW_SAVE_TOOL.to_owned(),
        message: message.to_string(),
    }
}

/// Reach the live gateway state through the host tool context. A call that
/// never reached the host path is a wiring bug, reported as an execution
/// failure rather than silently degrading.
fn gateway_state(ctx: &dyn ToolContext) -> Result<&AppState, ToolError> {
    super::require_gateway_state(
        ctx,
        WORKFLOW_SAVE_TOOL,
        "workflow_save runs only inside a gateway host turn",
    )
}

#[async_trait]
impl Tool for WorkflowSaveTool {
    fn name(&self) -> &str {
        WORKFLOW_SAVE_TOOL
    }

    fn description(&self) -> &str {
        "Keep a workflow you have written so this space can run it again by name with \
         workflow_run, and revise one you kept earlier. Use action 'save' with the whole \
         script to store it — under a name the space already uses, this adds a new version \
         rather than replacing history; under the name of a shipped or skill-carried \
         workflow, the space's copy takes over from then on and the original is untouched. \
         Use action 'update' to change a kept workflow's source or description. A workflow \
         saved this way runs with exactly the reach this conversation already has, and \
         cannot send results anywhere outside the machine. Activate the Workflow Authoring \
         skill before writing the script: it documents the script surface and the \
         determinism rules."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["save", "update"],
                    "description": "'save' stores a workflow under a name, creating it or adding a version. 'update' revises a workflow this space already keeps."
                },
                "name": {
                    "type": "string",
                    "description": "The workflow's name in this space — letters, digits, hyphens, and underscores. A name the script's own meta header declares wins over this one."
                },
                "source": {
                    "type": "string",
                    "description": "The workflow's JavaScript source. Required for 'save'; on 'update' it adds a new version."
                },
                "description": {
                    "type": "string",
                    "description": "What the workflow does, for whoever picks it later from the list."
                }
            },
            "required": ["action", "name"],
            "additionalProperties": false
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("'action' is required: one of 'save' or 'update'"))?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| invalid("'name' is required"))?
            .to_owned();
        let source = params
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let description = params
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if action == "save" && source.is_none() {
            return Err(invalid("saving a workflow needs its 'source'"));
        }
        if action == "update" && source.is_none() && description.is_none() {
            return Err(invalid(
                "updating a workflow needs a new 'source' or 'description'",
            ));
        }
        if !matches!(action, "save" | "update") {
            return Err(invalid(format!(
                "unknown action '{action}': use 'save' or 'update'"
            )));
        }

        // The authoring session is the turn's own, never a parameter: the
        // record of who wrote this workflow is not the model's to state.
        let session_id = ctx
            .session_id()
            .parse::<Uuid>()
            .map_err(|_| failed("the calling session has no usable id"))?;
        let state = gateway_state(ctx)?;
        let session = state
            .storage
            .get_session(session_id)
            .await
            .map_err(failed)?
            .ok_or_else(|| failed("the calling session no longer exists"))?;

        let change = match action {
            "save" => {
                crate::service::save_workflow_script(
                    state,
                    WorkflowScriptDraft {
                        space_id: session.space_id,
                        name,
                        description: description.unwrap_or_default(),
                        source: source.expect("save validates its source"),
                        delivery_grant: None,
                        authoring_session_id: Some(session_id),
                    },
                    WORKFLOW_SAVE_TOOL,
                    Some(session_id),
                )
                .await
            }
            _ => {
                let existing = state
                    .storage
                    .list_workflow_scripts(session.space_id)
                    .await
                    .map_err(failed)?
                    .into_iter()
                    .find(|script| script.name == name)
                    .ok_or_else(|| {
                        invalid(format!(
                            "this space keeps no workflow named '{name}'; save it first"
                        ))
                    })?;
                crate::service::update_workflow_script(
                    state,
                    existing.id,
                    WorkflowScriptEdit {
                        description,
                        source,
                        delivery_grant: None,
                        authoring_session_id: Some(session_id),
                    },
                    WORKFLOW_SAVE_TOOL,
                    Some(session_id),
                )
                .await
            }
        }
        .map_err(failed)?;

        Ok(json!({
            "name": change.name,
            "version": change.head_version,
            "created": change.created,
            "summary": change.summary,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::state::AppState;
    use swage_storage::{
        AgentDefinitionStore, NewSession, SpaceStore, SqliteStorage, Storage, WorkflowStore,
    };
    use swage_tool::context::MockToolContext;

    fn state() -> (AppState, Arc<SqliteStorage>) {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn SpaceStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        (state, storage)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_save_off_the_host_path_fails_rather_than_degrading() {
        let context = MockToolContext::new("agent", Uuid::new_v4().to_string());
        let error = WorkflowSaveTool
            .execute(
                json!({"action": "save", "name": "sweep", "source": "return 1;"}),
                &context,
            )
            .await
            .expect_err("a save outside the gateway host must fail");
        assert!(
            matches!(error, ToolError::ExecutionFailed { ref message, .. }
                if message.contains("gateway host turn")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_parameter_surface_offers_no_delivery_grant() {
        let parameters = WorkflowSaveTool.parameters();
        let properties = parameters["properties"]
            .as_object()
            .expect("declared properties");
        assert!(
            !properties.keys().any(|key| key.contains("delivery")
                || key.contains("grant")
                || key.contains("target")),
            "workflow_save must expose no delivery surface: {:?}",
            properties.keys().collect::<Vec<_>>()
        );
        assert_eq!(parameters["additionalProperties"], false);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_grant_smuggled_into_the_parameters_is_rejected_by_the_schema() {
        let dispatcher = crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(WorkflowSaveTool) as Arc<dyn Tool>
        ]);
        let context = MockToolContext::new("agent", Uuid::new_v4().to_string());
        let error = dispatcher
            .dispatch(
                WORKFLOW_SAVE_TOOL,
                json!({
                    "action": "save",
                    "name": "sweep",
                    "source": "return 1;",
                    "delivery_grant": {"targets": ["webhook"]}
                }),
                &context,
                "agent",
            )
            .await
            .expect_err("an undeclared delivery grant must not reach execution");
        assert!(
            matches!(error, ToolError::InvalidParameters { .. }),
            "unexpected error: {error}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn saving_needs_an_action_and_a_source() {
        let context = MockToolContext::new("agent", Uuid::new_v4().to_string());
        for params in [
            json!({"name": "sweep", "source": "return 1;"}),
            json!({"action": "save", "name": "sweep"}),
            json!({"action": "save", "source": "return 1;"}),
            json!({"action": "update", "name": "sweep"}),
        ] {
            let error = WorkflowSaveTool
                .execute(params.clone(), &context)
                .await
                .expect_err("incomplete input must be rejected before any host work");
            assert!(
                matches!(error, ToolError::InvalidParameters { .. }),
                "{params} produced {error}"
            );
        }
    }

    async fn saving_session(state: &AppState, storage: &SqliteStorage) -> (Uuid, Uuid) {
        let space = storage.get_or_create_default().await.expect("space");
        let session = state
            .storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "current-agent".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session");
        storage
            .upsert_agent_definition(swage_storage::NewAgentDefinition {
                id: "current-agent".into(),
                space_id: Some(space.id),
                display_name: "Authoring agent".into(),
                description: String::new(),
                instructions: String::new(),
                model_id: None,
                tool_permissions: None,
                skills: vec![],
                output_schema: None,
                output_max_retries: None,
            })
            .await
            .expect("agent definition");
        (space.id, session.id)
    }

    fn host_context(
        state: &AppState,
        storage: &Arc<SqliteStorage>,
        session_id: Uuid,
    ) -> (
        GatewayToolContext,
        tokio::sync::mpsc::UnboundedReceiver<crate::websocket::ServerEvent>,
    ) {
        let base = MockToolContext::new("current-agent", session_id.to_string());
        let (ws_tx, ws_rx) = tokio::sync::mpsc::unbounded_channel();
        (
            GatewayToolContext::new(&base, ws_tx, Arc::clone(storage) as Arc<dyn Storage>)
                .with_app_state(Some(state.clone())),
            ws_rx,
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_saved_workflow_runs_by_name_and_can_never_deliver() {
        let (state, storage) = state();
        let (space_id, session_id) = saving_session(&state, &storage).await;
        let (context, _ws) = host_context(&state, &storage, session_id);

        let saved = WorkflowSaveTool
            .execute(
                json!({
                    "action": "save",
                    "name": "keeper",
                    "description": "Reports whether it can deliver.",
                    "source": "return { delivery: typeof deliver };"
                }),
                &context,
            )
            .await
            .expect("the workflow saves");
        assert_eq!(saved["name"], "keeper");
        assert_eq!(saved["version"], 1);
        assert_eq!(saved["created"], true);

        let script = storage
            .list_workflow_scripts(space_id)
            .await
            .unwrap()
            .into_iter()
            .find(|script| script.name == "keeper")
            .expect("the saved row");
        assert_eq!(script.authoring_session_id, Some(session_id));

        // Even with a grant written straight onto the row, the run the model
        // can trigger by name is handed none: the script is the model's.
        storage
            .update_workflow_script(
                script.id,
                None,
                Some(json!({"targets": [{"kind": "webhook", "url": "https://example.invalid/"}]})),
                None,
            )
            .await
            .expect("an operator-side column write");

        let run = crate::tool::workflow_run::WorkflowRunTool
            .execute(json!({"name": "keeper", "wait": true}), &context)
            .await
            .expect("the saved workflow runs by name");
        assert_eq!(run["state"], "completed");
        assert_eq!(
            run["final_output"],
            json!({"delivery": "undefined"}),
            "an agent-authored workflow must run without any delivery capability"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_update_adds_a_version_and_refuses_an_unsaved_name() {
        let (state, storage) = state();
        let (space_id, session_id) = saving_session(&state, &storage).await;
        let (context, _ws) = host_context(&state, &storage, session_id);

        WorkflowSaveTool
            .execute(
                json!({"action": "save", "name": "keeper", "source": "return 1;"}),
                &context,
            )
            .await
            .expect("the workflow saves");
        let updated = WorkflowSaveTool
            .execute(
                json!({"action": "update", "name": "keeper", "source": "return 2;"}),
                &context,
            )
            .await
            .expect("the workflow updates");
        assert_eq!(updated["version"], 2);
        assert_eq!(updated["created"], false);
        assert_eq!(
            storage.list_workflow_scripts(space_id).await.unwrap().len(),
            1
        );

        let error = WorkflowSaveTool
            .execute(
                json!({"action": "update", "name": "absent", "source": "return 3;"}),
                &context,
            )
            .await
            .expect_err("updating a workflow the space does not keep must fail");
        assert!(
            matches!(error, ToolError::InvalidParameters { ref message, .. }
                if message.contains("keeps no workflow")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_saved_workflow_shadows_a_same_named_shipped_workflow_in_place() {
        let (state, storage) = state();
        let (space_id, session_id) = saving_session(&state, &storage).await;
        let (context, _ws) = host_context(&state, &storage, session_id);
        let shipped = "research-sweep";

        let saved = WorkflowSaveTool
            .execute(
                json!({
                    "action": "save",
                    "name": shipped,
                    "source": "return 'the space\\'s own take';"
                }),
                &context,
            )
            .await
            .expect("a save under a shipped name creates the space's own script");
        assert_eq!(saved["created"], true);

        // The shipped workflow is compiled in with its carrying skill, not a
        // row: saving over its name cannot have edited it, and the space now
        // resolves its own.
        assert!(
            crate::shipped_skills::workflow_files("research")
                .any(|file| file.path.ends_with(&format!("/{shipped}.js")))
        );
        let rows = storage.list_workflow_scripts(space_id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, shipped);
        assert_eq!(rows[0].authoring_session_id, Some(session_id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn saving_over_an_operator_script_leaves_the_model_no_delivery() {
        let (state, storage) = state();
        let (space_id, session_id) = saving_session(&state, &storage).await;
        let (context, _ws) = host_context(&state, &storage, session_id);

        // The space already keeps an operator's script by this name, with no
        // grant on it yet — the case a save-by-name overwrites.
        let operator = crate::service::create_workflow_script(
            &state,
            crate::service::WorkflowScriptDraft {
                space_id,
                name: "keeper".into(),
                description: "The operator's own.".into(),
                source: "return { delivery: typeof deliver };".into(),
                delivery_grant: None,
                authoring_session_id: None,
            },
            "operator_http",
            None,
        )
        .await
        .expect("the operator's script");

        let saved = WorkflowSaveTool
            .execute(
                json!({
                    "action": "save",
                    "name": "keeper",
                    "source": "return { delivery: typeof deliver };"
                }),
                &context,
            )
            .await
            .expect("the model writes its own version");
        assert_eq!(saved["created"], false);

        // Writing the grant onto the row afterwards is the operator-side
        // field write the seam has to survive: the script is the model's now.
        storage
            .update_workflow_script(
                operator.script_id,
                None,
                Some(json!({"targets": [{"kind": "webhook", "url": "https://example.invalid/"}]})),
                None,
            )
            .await
            .expect("an operator-side column write");

        let run = crate::tool::workflow_run::WorkflowRunTool
            .execute(json!({"name": "keeper", "wait": true}), &context)
            .await
            .expect("the saved workflow runs by name");
        assert_eq!(run["state"], "completed");
        assert_eq!(
            run["final_output"],
            json!({"delivery": "undefined"}),
            "a script the model rewrote must run without any delivery capability"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_delivering_operator_script_cannot_be_saved_over() {
        let (state, storage) = state();
        let (space_id, session_id) = saving_session(&state, &storage).await;
        let (context, _ws) = host_context(&state, &storage, session_id);
        let grant = json!({"targets": [{"kind": "webhook", "url": "https://example.invalid/"}]});

        crate::service::create_workflow_script(
            &state,
            crate::service::WorkflowScriptDraft {
                space_id,
                name: "announce".into(),
                description: "The operator's delivering script.".into(),
                source: "return { delivery: typeof deliver };".into(),
                delivery_grant: Some(grant.clone()),
                authoring_session_id: None,
            },
            "operator_http",
            None,
        )
        .await
        .expect("the operator's delivering script");

        let error = WorkflowSaveTool
            .execute(
                json!({
                    "action": "save",
                    "name": "announce",
                    "source": "return { delivery: typeof deliver };"
                }),
                &context,
            )
            .await
            .expect_err("a delivering script is not the model's to rewrite");
        assert!(
            format!("{error}").contains("carries delivery targets"),
            "unexpected error: {error}"
        );

        let row = storage
            .list_workflow_scripts(space_id)
            .await
            .unwrap()
            .into_iter()
            .find(|script| script.name == "announce")
            .expect("the operator's row");
        assert_eq!(row.authoring_session_id, None);
        assert_eq!(row.effective_delivery_grant(), Some(grant));
    }
}
