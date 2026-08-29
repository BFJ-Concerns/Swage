//! Workflow discovery for agent turns.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;
use uuid::Uuid;

use crate::{state::AppState, tool::Tool, tool_executor::GatewayToolContext};

pub const WORKFLOW_LIST_TOOL: &str = "workflow_list";

/// Describe a catalogue in an error without relying on downstream compaction.
///
/// The count and the full-list tool always fit. Any shorter preview says exactly
/// how many names it contains, so an omitted name never implies it is absent.
pub(crate) fn workflow_catalogue_error_summary(
    missing_name: &str,
    catalogue: &Value,
    max_chars: usize,
) -> String {
    let names = catalogue
        .get("workflows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|workflow| workflow.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let count = names.len();
    let noun = if count == 1 { "workflow" } else { "workflows" };
    let guidance =
        format!("{count} {noun} are available. Use `{WORKFLOW_LIST_TOOL}` to see all workflows.");
    let named_not_found = format!("workflow script {missing_name:?} was not found.");
    let shortest_preview = format!(" Showing 0 of {count} names.");
    let introduction = if named_not_found.chars().count()
        + 1
        + guidance.chars().count()
        + shortest_preview.chars().count()
        <= max_chars
    {
        format!("{named_not_found} {guidance}")
    } else {
        format!("workflow script with the supplied name was not found. {guidance}")
    };

    if count == 0 {
        return introduction;
    }

    let mut shown = Vec::new();
    for name in names {
        shown.push(name);
        let candidate = format!(
            "{introduction} Showing {} of {count} names: {}.",
            shown.len(),
            shown.join(", ")
        );
        if candidate.chars().count() > max_chars {
            shown.pop();
            break;
        }
    }

    if shown.is_empty() {
        format!("{introduction} Showing 0 of {count} names.")
    } else {
        format!(
            "{introduction} Showing {} of {count} names: {}.",
            shown.len(),
            shown.join(", ")
        )
    }
}

/// Project the established effective workflow listing for one caller session.
///
/// The HTTP listing owns precedence between authored, carried, and shipped
/// workflows. Keeping this helper as a projection means agent discovery and
/// operator listing cannot drift into separate catalogue rules.
pub async fn workflows_for_session(state: &AppState, session_id: Uuid) -> Result<Value, String> {
    let response = crate::workflow::list_scripts_for_session(state, session_id).await?;

    Ok(json!({
        "workflows": response.scripts.into_iter().map(|script| json!({
            "name": script.name,
            "description": script.description,
            "argument_contract": script.argument_contract,
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
        "List the workflows available in this session. Each entry gives its name, purpose, and argument contract. A workflow with an unconstrained argument contract declares no schema, so pass the JSON value it expects in workflow_run.args. To author a new workflow, activate the Workflow Authoring skill, which documents the script surface."
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
        AgentDefinitionStore, NewAgentDefinition, NewSession, NewSpace, NewWorkflowScript,
        SessionStore, SpaceStore, SqliteStorage, Storage, WorkflowStore,
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

    async fn store_test_agent(storage: &SqliteStorage, space_id: Uuid, agent_id: &str) {
        storage
            .upsert_agent_definition(NewAgentDefinition {
                id: agent_id.into(),
                space_id: Some(space_id),
                display_name: agent_id.into(),
                description: String::new(),
                instructions: String::new(),
                model_id: None,
                tool_permissions: None,
                skills: vec![],
                output_schema: None,
                output_max_retries: None,
            })
            .await
            .expect("store agent definition");
    }

    fn write_agent_workflow(
        root: &std::path::Path,
        space_slug: &str,
        agent_id: &str,
        skill_name: &str,
        workflow_name: &str,
    ) {
        let skill_dir = root
            .join("spaces")
            .join(space_slug)
            .join("agents")
            .join(agent_id)
            .join("skills")
            .join(skill_name);
        std::fs::create_dir_all(skill_dir.join("workflows")).expect("workflow directory");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\ndescription: Workflows for {agent_id}.\n---\n"),
        )
        .expect("skill definition");
        std::fs::write(
            skill_dir
                .join("workflows")
                .join(format!("{workflow_name}.js")),
            "return {};",
        )
        .expect("carried workflow");
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

    #[test]
    fn catalogue_error_summary_marks_a_partial_name_preview() {
        let catalogue = json!({
            "workflows": (1..=13)
                .map(|index| json!({ "name": format!("workflow-{index:02}-for-catalogue-preview") }))
                .collect::<Vec<_>>(),
        });
        let summary = workflow_catalogue_error_summary(
            "missing-workflow",
            &catalogue,
            swage_agent::agent::MAX_MODEL_TOOL_ERROR_DETAIL_CHARS,
        );

        assert!(
            summary.chars().count() <= swage_agent::agent::MAX_MODEL_TOOL_ERROR_DETAIL_CHARS,
            "summary exceeds the model error-detail budget: {summary}"
        );
        assert!(summary.contains("13 workflows are available"));
        assert!(summary.contains("`workflow_list`"));
        assert!(summary.contains("Showing 5 of 13 names"));
        assert!(summary.contains("workflow-01-for-catalogue-preview"));
        assert!(summary.contains("workflow-05-for-catalogue-preview"));
        assert!(!summary.contains("workflow-06-for-catalogue-preview"));
    }

    #[test]
    fn catalogue_error_summary_budgets_a_long_supplied_name() {
        let catalogue = json!({
            "workflows": (1..=13)
                .map(|index| json!({ "name": format!("workflow-{index:02}") }))
                .collect::<Vec<_>>(),
        });
        for name_length in [195, 217, 800] {
            let summary = workflow_catalogue_error_summary(
                &"x".repeat(name_length),
                &catalogue,
                swage_agent::agent::MAX_MODEL_TOOL_ERROR_DETAIL_CHARS,
            );

            assert!(
                summary.chars().count() <= swage_agent::agent::MAX_MODEL_TOOL_ERROR_DETAIL_CHARS,
                "summary for a {name_length}-character name exceeds the model error-detail budget: {summary}"
            );
            assert!(summary.contains("with the supplied name was not found"));
            assert!(summary.contains("13 workflows are available"));
            assert!(summary.contains("`workflow_list`"));
            assert!(summary.contains("Showing "));
            assert!(summary.contains(" of 13 names"));
        }
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
        assert_eq!(saved["argument_contract"]["kind"], "unconstrained");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn listing_projects_declared_and_unconstrained_argument_contracts() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let space = storage.get_or_create_default().await.expect("space");
        storage
            .create_workflow_script(NewWorkflowScript {
                space_id: space.id,
                name: "constrained-report".into(),
                description: String::new(),
                source: r#"export const meta = { argumentSchema: {"type":"object","required":["topic"]} }; return {};"#.into(),
                delivery_grant: None,
            })
            .await
            .expect("constrained workflow");
        storage
            .create_workflow_script(NewWorkflowScript {
                space_id: space.id,
                name: "open-report".into(),
                description: String::new(),
                source: "return {};".into(),
                delivery_grant: None,
            })
            .await
            .expect("unconstrained workflow");
        let session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "gateway".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session");
        let result = workflows_for_session(&state(Arc::clone(&storage)), session.id)
            .await
            .expect("workflow listing");
        let workflows = result["workflows"].as_array().expect("workflow array");
        let constrained = workflows
            .iter()
            .find(|workflow| workflow["name"] == "constrained-report")
            .expect("constrained workflow");
        assert_eq!(constrained["argument_contract"]["kind"], "declared");
        assert_eq!(
            constrained["argument_contract"]["schema"]["required"],
            json!(["topic"])
        );
        let open = workflows
            .iter()
            .find(|workflow| workflow["name"] == "open-report")
            .expect("unconstrained workflow");
        assert_eq!(open["argument_contract"]["kind"], "unconstrained");
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

    #[tokio::test(flavor = "multi_thread")]
    async fn listing_uses_the_callers_agent_skills() {
        let root = tempfile::tempdir().expect("skill root");
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let space = storage.get_or_create_default().await.expect("space");
        store_test_agent(&storage, space.id, "agent-one").await;
        store_test_agent(&storage, space.id, "agent-two").await;
        write_agent_workflow(
            root.path(),
            &space.slug,
            "agent-one",
            "one-workflows",
            "only-agent-one",
        );
        write_agent_workflow(
            root.path(),
            &space.slug,
            "agent-two",
            "two-workflows",
            "only-agent-two",
        );
        let first_session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "agent-one".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("first session");
        let second_session = storage
            .create_session(NewSession {
                space_id: space.id,
                agent_id: "agent-two".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("second session");
        let _first_root =
            crate::websocket::SessionSkillRootGuard::at(first_session.id, root.path());
        let _second_root =
            crate::websocket::SessionSkillRootGuard::at(second_session.id, root.path());

        let first_context = context_for(
            state(Arc::clone(&storage)),
            Arc::clone(&storage),
            first_session.id,
        )
        .await;
        let second_context = context_for(
            state(Arc::clone(&storage)),
            Arc::clone(&storage),
            second_session.id,
        )
        .await;
        let first = WorkflowListTool
            .execute(json!({}), &first_context)
            .await
            .expect("first listing");
        let second = WorkflowListTool
            .execute(json!({}), &second_context)
            .await
            .expect("second listing");
        let names = |catalogue: &Value| {
            catalogue["workflows"]
                .as_array()
                .expect("workflow array")
                .iter()
                .map(|workflow| workflow["name"].as_str().expect("workflow name").to_owned())
                .collect::<Vec<_>>()
        };

        let first_names = names(&first);
        assert!(first_names.iter().any(|name| name == "only-agent-one"));
        assert!(!first_names.iter().any(|name| name == "only-agent-two"));
        let second_names = names(&second);
        assert!(second_names.iter().any(|name| name == "only-agent-two"));
        assert!(!second_names.iter().any(|name| name == "only-agent-one"));

        let error = crate::tool::workflow_run::WorkflowRunTool
            .execute(json!({"name": "missing-workflow"}), &first_context)
            .await
            .expect_err("missing workflow");
        let ToolError::ExecutionFailed { message, .. } = error else {
            panic!("unexpected workflow error: {error}");
        };
        assert!(message.contains("only-agent-one"));
        assert!(!message.contains("only-agent-two"));
    }
}
