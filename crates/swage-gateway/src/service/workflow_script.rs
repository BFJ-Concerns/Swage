//! Workflow-script mutations: validate, apply through the store, record.
//!
//! Saved workflows are configuration — a named, versioned thing the topic
//! keeps and later runs — so every save and update lands here rather than on
//! the store directly, and every one of them leaves a `configuration_change`
//! record naming the surface and the session behind it (see
//! `config_change.rs`).
//!
//! ## Agent-authored scripts never carry a delivery grant
//!
//! A script's delivery grant names the endpoints its run may push effects to;
//! it is how work leaves the machine. A model that could attach a grant to a
//! script it wrote and then start that script would have written itself
//! authority its own session never held, with `workflow_run` as the trigger.
//! The grant is therefore withheld structurally, not gated on a prompt: a save
//! that names an authoring session is refused if it carries a grant, the row
//! records the authoring session durably, and every run-start path reads the
//! grant through [`WorkflowScriptRecord::effective_delivery_grant`], which
//! yields nothing for an agent-authored row. An operator who wants a model's
//! script to deliver re-creates it through the operator surface, where the
//! creation is theirs and is recorded as theirs.

use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{NewWorkflowScript, WorkflowScriptRecord};

use crate::config_change::{ConfigChange, record_config_change};
use crate::state::AppState;

/// The outcome a mutation surface reports: which script changed and a
/// human-readable account of what happened. The full record is the audit
/// trail's.
#[derive(Debug)]
pub struct WorkflowScriptChange {
    pub script_id: Uuid,
    pub name: String,
    pub head_version: u32,
    pub created: bool,
    pub summary: String,
}

/// A script as a caller asks for it to be saved.
pub struct WorkflowScriptDraft {
    pub topic_id: Uuid,
    pub name: String,
    pub description: String,
    pub source: String,
    pub delivery_grant: Option<serde_json::Value>,
    pub captured_sources_grant: bool,
    /// The conversation authoring the script, when a model is authoring it.
    pub authoring_session_id: Option<Uuid>,
}

/// What an update changes; every field absent leaves the script alone.
#[derive(Default)]
pub struct WorkflowScriptEdit {
    pub description: Option<String>,
    pub source: Option<String>,
    pub delivery_grant: Option<serde_json::Value>,
    pub captured_sources_grant: Option<bool>,
    /// Set when a conversation is writing this version. The row records it,
    /// so a script a model rewrote is a model's script from then on however
    /// it began; it is never cleared here, only by an operator re-creating
    /// the script through the operator surface.
    pub authoring_session_id: Option<Uuid>,
}

/// Create a new saved workflow script in a topic, recorded.
pub async fn create_workflow_script(
    state: &AppState,
    draft: WorkflowScriptDraft,
    source_surface: &str,
    session_id: Option<Uuid>,
) -> Result<WorkflowScriptChange, GatewayError> {
    let draft = validate_draft(draft)?;
    if find_by_name(state, draft.topic_id, &draft.name)
        .await?
        .is_some()
    {
        return Err(GatewayError::TopicConflict(format!(
            "a workflow script named '{}' already exists in this topic",
            draft.name
        )));
    }

    let name = draft.name.clone();
    let topic_id = draft.topic_id;
    let (record, version) = state
        .storage
        .create_workflow_script(NewWorkflowScript {
            topic_id,
            name: draft.name,
            description: draft.description,
            source: draft.source,
            delivery_grant: draft.delivery_grant,
            captured_sources_grant: draft.captured_sources_grant,
            authoring_session_id: draft.authoring_session_id,
        })
        .await
        .map_err(GatewayError::from)?;
    let summary = format!("created workflow script '{name}' in topic {topic_id}");

    record_config_change(
        state,
        ConfigChange {
            entity_kind: "workflow_script",
            entity_id: &record.id.to_string(),
            action: "create_workflow_script",
            source: source_surface,
            session_id,
            summary: &summary,
            prior_state: None,
            new_state: Some(snapshot(&record, version.version)),
        },
    )
    .await?;

    Ok(WorkflowScriptChange {
        script_id: record.id,
        name,
        head_version: version.version,
        created: true,
        summary,
    })
}

/// Change a saved workflow script — a new source version, a new description,
/// a changed grant — recorded with the state it replaced.
pub async fn update_workflow_script(
    state: &AppState,
    id: Uuid,
    edit: WorkflowScriptEdit,
    source_surface: &str,
    session_id: Option<Uuid>,
) -> Result<WorkflowScriptChange, GatewayError> {
    let record = state
        .storage
        .get_workflow_script(id)
        .await
        .map_err(GatewayError::from)?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("no workflow script {id} exists in this topic"))
        })?;
    if edit.delivery_grant.is_some() && edit.authoring_session_id.is_some() {
        return Err(agent_authored_grant_refusal());
    }
    if edit.delivery_grant.is_some() && record.authoring_session_id.is_some() {
        return Err(GatewayError::ValidationError(
            "'{name}' was authored by a conversation and cannot be granted delivery; \
             create the workflow through the operator surface to give it delivery targets"
                .replace("{name}", &record.name),
        ));
    }
    // A delivering script is the operator's own configuration: a conversation
    // neither inherits its grant by writing a new version over it nor gets to
    // silently take delivery away from it.
    if edit.authoring_session_id.is_some() && record.delivery_grant.is_some() {
        return Err(GatewayError::ValidationError(
            "'{name}' carries delivery targets and was not authored in a conversation; \
             save your workflow under a different name"
                .replace("{name}", &record.name),
        ));
    }
    // Source headers are authoritative for both creation and updates.
    let source_meta = edit.source.as_deref().map(validate_source).transpose()?;
    let description = source_meta
        .as_ref()
        .and_then(|meta| meta.description.as_deref())
        .or(edit.description.as_deref());

    let prior_version = head_version(state, id).await?;
    let prior = Some(snapshot(&record, prior_version));

    if description.is_some()
        || edit.delivery_grant.is_some()
        || edit.captured_sources_grant.is_some()
        || edit.authoring_session_id.is_some()
    {
        state
            .storage
            .update_workflow_script(
                id,
                description,
                edit.delivery_grant,
                edit.captured_sources_grant,
                edit.authoring_session_id,
            )
            .await
            .map_err(GatewayError::from)?;
    }
    if let Some(source) = &edit.source {
        state
            .storage
            .append_workflow_script_version(id, source)
            .await
            .map_err(GatewayError::from)?;
    }

    let updated = state
        .storage
        .get_workflow_script(id)
        .await
        .map_err(GatewayError::from)?
        .ok_or_else(|| {
            GatewayError::ConfigError(format!("workflow script {id} vanished mid-update"))
        })?;
    let new_version = head_version(state, id).await?;
    let summary = format!(
        "updated workflow script '{}' to version {new_version}",
        updated.name
    );

    record_config_change(
        state,
        ConfigChange {
            entity_kind: "workflow_script",
            entity_id: &id.to_string(),
            action: "update_workflow_script",
            source: source_surface,
            session_id,
            summary: &summary,
            prior_state: prior,
            new_state: Some(snapshot(&updated, new_version)),
        },
    )
    .await?;

    Ok(WorkflowScriptChange {
        script_id: id,
        name: updated.name,
        head_version: new_version,
        created: false,
        summary,
    })
}

/// Save a script by name: a new script where the topic has none by that name,
/// a new version of the topic's own script where it does.
///
/// A shipped or skill-carried workflow of the same name is left untouched —
/// the topic's script shadows it from then on, which is the same direction an
/// operator's own copy takes.
pub async fn save_workflow_script(
    state: &AppState,
    draft: WorkflowScriptDraft,
    source_surface: &str,
    session_id: Option<Uuid>,
) -> Result<WorkflowScriptChange, GatewayError> {
    let draft = validate_draft(draft)?;
    match find_by_name(state, draft.topic_id, &draft.name).await? {
        Some(existing) => {
            update_workflow_script(
                state,
                existing.id,
                WorkflowScriptEdit {
                    description: Some(draft.description),
                    source: Some(draft.source),
                    delivery_grant: draft.delivery_grant,
                    captured_sources_grant: Some(draft.captured_sources_grant),
                    authoring_session_id: draft.authoring_session_id,
                },
                source_surface,
                session_id,
            )
            .await
        }
        None => create_workflow_script(state, draft, source_surface, session_id).await,
    }
}

/// Validate a draft before anything is written, normalising the name and
/// description the script's own `meta` header declares.
fn validate_draft(mut draft: WorkflowScriptDraft) -> Result<WorkflowScriptDraft, GatewayError> {
    let meta = validate_source(&draft.source)?;
    if let Some(name) = meta.name {
        draft.name = name;
    }
    if let Some(description) = meta.description {
        draft.description = description;
    }
    validate_name(&draft.name)?;
    if draft.delivery_grant.is_some() && draft.authoring_session_id.is_some() {
        return Err(agent_authored_grant_refusal());
    }
    Ok(draft)
}

fn validate_source(source: &str) -> Result<swage_workflow::ScriptMeta, GatewayError> {
    if source.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "a workflow script needs source".into(),
        ));
    }
    swage_workflow::script_meta(source).map_err(GatewayError::ValidationError)
}

fn validate_name(name: &str) -> Result<(), GatewayError> {
    if name.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "a workflow script needs a name, in the call or in the script's meta header".into(),
        ));
    }
    if name.len() > 64 {
        return Err(GatewayError::ValidationError(
            "workflow script names are at most 64 characters".into(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(GatewayError::ValidationError(
            "workflow script names are ASCII letters, digits, hyphens, and underscores".into(),
        ));
    }
    Ok(())
}

fn agent_authored_grant_refusal() -> GatewayError {
    GatewayError::ValidationError(
        "a workflow authored in a conversation cannot carry a delivery grant".into(),
    )
}

async fn find_by_name(
    state: &AppState,
    topic_id: Uuid,
    name: &str,
) -> Result<Option<WorkflowScriptRecord>, GatewayError> {
    Ok(state
        .storage
        .list_workflow_scripts(topic_id)
        .await
        .map_err(GatewayError::from)?
        .into_iter()
        .find(|script| script.name == name))
}

async fn head_version(state: &AppState, id: Uuid) -> Result<u32, GatewayError> {
    Ok(state
        .storage
        .get_workflow_script_head(id)
        .await
        .map_err(GatewayError::from)?
        .map_or(0, |head| head.version))
}

/// The script's whole state as the audit record carries it. The source lives
/// in versions rather than the row, so the snapshot names the version a reader
/// can fetch rather than inlining a script body into the audit trail.
fn snapshot(record: &WorkflowScriptRecord, head_version: u32) -> serde_json::Value {
    serde_json::json!({
        "id": record.id,
        "topic_id": record.topic_id,
        "name": record.name,
        "description": record.description,
        "head_version": head_version,
        "delivery_grant": &record.delivery_grant,
        "captured_sources_grant": record.captured_sources_grant,
        "authoring_session_id": record.authoring_session_id,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use swage_storage::{NewSession, SqliteStorage, Storage, TopicStore};

    const SOURCE: &str = "return 1;";

    fn draft(topic_id: Uuid, name: &str) -> WorkflowScriptDraft {
        WorkflowScriptDraft {
            topic_id,
            name: name.into(),
            description: "A saved workflow.".into(),
            source: SOURCE.into(),
            delivery_grant: None,
            captured_sources_grant: Default::default(),
            authoring_session_id: None,
        }
    }

    fn state() -> AppState {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        )
    }

    /// A real session row: the audit trail's session reference is a foreign key,
    /// so a save naming a session only records against one that exists.
    async fn session(state: &AppState, topic_id: Uuid) -> Uuid {
        state
            .storage
            .create_session(NewSession {
                topic_id,
                agent_id: "current-agent".into(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session")
            .id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refuses_a_delivery_grant_on_an_agent_authored_save() {
        let state = state();
        let topic = state.topic_store.get_or_create_default().await.unwrap();
        let mut draft = draft(topic.id, "escalate");
        draft.delivery_grant = Some(serde_json::json!({"targets": ["webhook"]}));
        draft.authoring_session_id = Some(Uuid::new_v4());

        let error = save_workflow_script(&state, draft, "workflow_save", None)
            .await
            .expect_err("an agent-authored grant must be refused");
        assert!(
            matches!(&error, GatewayError::ValidationError(message)
                if message.contains("cannot carry a delivery grant")),
            "unexpected error: {error}"
        );
        assert!(
            state
                .storage
                .list_workflow_scripts(topic.id)
                .await
                .unwrap()
                .is_empty(),
            "the refused save must not have written a row"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_authored_script_resolves_no_delivery_grant() {
        let state = state();
        let topic = state.topic_store.get_or_create_default().await.unwrap();
        let session = session(&state, topic.id).await;
        let mut draft = draft(topic.id, "authored");
        draft.authoring_session_id = Some(session);
        let change = save_workflow_script(&state, draft, "workflow_save", Some(session))
            .await
            .unwrap();

        // An operator-side field write on the row must not re-arm delivery.
        state
            .storage
            .update_workflow_script(
                change.script_id,
                None,
                Some(serde_json::json!({"targets": ["webhook"]})),
                None,
                None,
            )
            .await
            .unwrap();
        let record = state
            .storage
            .get_workflow_script(change.script_id)
            .await
            .unwrap()
            .expect("the saved script");
        assert!(record.delivery_grant.is_some(), "the column holds the ask");
        assert_eq!(
            record.effective_delivery_grant(),
            None,
            "an agent-authored script must resolve no delivery grant"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_operator_script_keeps_its_delivery_grant() {
        let state = state();
        let topic = state.topic_store.get_or_create_default().await.unwrap();
        let mut draft = draft(topic.id, "operator-authored");
        draft.delivery_grant = Some(serde_json::json!({"targets": ["webhook"]}));
        let change = save_workflow_script(&state, draft, "operator_http", None)
            .await
            .unwrap();
        let record = state
            .storage
            .get_workflow_script(change.script_id)
            .await
            .unwrap()
            .expect("the saved script");
        assert!(record.effective_delivery_grant().is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_save_appends_a_version_rather_than_a_row() {
        let state = state();
        let topic = state.topic_store.get_or_create_default().await.unwrap();
        let first = save_workflow_script(&state, draft(topic.id, "sweep"), "workflow_save", None)
            .await
            .unwrap();
        assert!(first.created);
        assert_eq!(first.head_version, 1);

        let mut second = draft(topic.id, "sweep");
        second.source = "return 2;".into();
        let second = save_workflow_script(&state, second, "workflow_save", None)
            .await
            .unwrap();
        assert!(!second.created);
        assert_eq!(second.script_id, first.script_id);
        assert_eq!(second.head_version, 2);
        assert_eq!(
            state
                .storage
                .list_workflow_scripts(topic.id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            state
                .storage
                .get_workflow_script_head(first.script_id)
                .await
                .unwrap()
                .unwrap()
                .source,
            "return 2;"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_meta_header_names_the_saved_script() {
        let state = state();
        let topic = state.topic_store.get_or_create_default().await.unwrap();
        let mut draft = draft(topic.id, "ignored");
        draft.source =
            "export const meta = { name: \"declared\", description: \"From the header.\" };\n\nreturn 1;"
                .into();
        let change = save_workflow_script(&state, draft, "workflow_save", None)
            .await
            .unwrap();
        assert_eq!(change.name, "declared");
        let record = state
            .storage
            .get_workflow_script(change.script_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.description, "From the header.");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_update_uses_the_source_header_description_over_the_request_field() {
        let state = state();
        let topic = state.topic_store.get_or_create_default().await.unwrap();
        let change =
            save_workflow_script(&state, draft(topic.id, "described"), "operator_http", None)
                .await
                .unwrap();

        update_workflow_script(
            &state,
            change.script_id,
            WorkflowScriptEdit {
                description: Some("From the request field.".into()),
                source: Some(
                    "export const meta = { description: \"From the source header.\" };\nreturn 2;"
                        .into(),
                ),
                ..Default::default()
            },
            "operator_http",
            None,
        )
        .await
        .unwrap();

        let record = state
            .storage
            .get_workflow_script(change.script_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.description, "From the source header.");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn rejects_a_name_that_is_not_a_workflow_name() {
        let state = state();
        let topic = state.topic_store.get_or_create_default().await.unwrap();
        let error =
            save_workflow_script(&state, draft(topic.id, "not a name"), "workflow_save", None)
                .await
                .expect_err("a spaced name must be refused");
        assert!(matches!(error, GatewayError::ValidationError(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_save_records_a_configuration_change_naming_the_session() {
        let state = state();
        let topic = state.topic_store.get_or_create_default().await.unwrap();
        let session = session(&state, topic.id).await;
        let mut draft = draft(topic.id, "recorded");
        draft.delivery_grant = Some(serde_json::json!({"targets": ["webhook"]}));
        let change = save_workflow_script(&state, draft, "workflow_save", Some(session))
            .await
            .unwrap();

        let events = state.storage.list_audit_events().await.unwrap();
        let event = events
            .iter()
            .find(|event| {
                event.entity_id == change.script_id.to_string()
                    && event.metadata["action"] == "create_workflow_script"
            })
            .expect("the save's configuration change");
        assert_eq!(event.entity_kind, "workflow_script");
        assert_eq!(event.event_kind, "configuration_change");
        assert_eq!(event.session_id, Some(session));
        assert_eq!(event.metadata["source"], "workflow_save");
        assert_eq!(event.metadata["prior_state"], serde_json::Value::Null);
        assert_eq!(event.metadata["new_state"]["name"], "recorded");
        assert_eq!(event.metadata["new_state"]["head_version"], 1);
        assert_eq!(
            event.metadata["new_state"]["delivery_grant"],
            serde_json::json!({"targets": ["webhook"]})
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn saving_over_an_operator_script_makes_it_the_conversation_s_own() {
        let state = state();
        let topic = state.topic_store.get_or_create_default().await.unwrap();
        let session = session(&state, topic.id).await;
        let operator =
            save_workflow_script(&state, draft(topic.id, "sweep"), "operator_http", None)
                .await
                .unwrap();
        assert_eq!(
            state
                .storage
                .get_workflow_script(operator.script_id)
                .await
                .unwrap()
                .unwrap()
                .authoring_session_id,
            None
        );

        let mut agent = draft(topic.id, "sweep");
        agent.source = "return 2;".into();
        agent.authoring_session_id = Some(session);
        let saved = save_workflow_script(&state, agent, "workflow_save", Some(session))
            .await
            .unwrap();
        assert_eq!(saved.script_id, operator.script_id);
        let record = state
            .storage
            .get_workflow_script(operator.script_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            record.authoring_session_id,
            Some(session),
            "a version written by a conversation makes the script the conversation's"
        );

        // The row a conversation now owns cannot be granted delivery, by the
        // operator surface or by a field write.
        let error = update_workflow_script(
            &state,
            operator.script_id,
            WorkflowScriptEdit {
                delivery_grant: Some(serde_json::json!({"targets": ["webhook"]})),
                ..Default::default()
            },
            "operator_http",
            None,
        )
        .await
        .expect_err("granting delivery to a conversation's script must be refused");
        assert!(matches!(error, GatewayError::ValidationError(_)));
        state
            .storage
            .update_workflow_script(
                operator.script_id,
                None,
                Some(serde_json::json!({"targets": ["webhook"]})),
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            state
                .storage
                .get_workflow_script(operator.script_id)
                .await
                .unwrap()
                .unwrap()
                .effective_delivery_grant(),
            None
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_conversation_cannot_save_over_a_delivering_script() {
        let state = state();
        let topic = state.topic_store.get_or_create_default().await.unwrap();
        let session = session(&state, topic.id).await;
        let mut operator = draft(topic.id, "delivering");
        operator.delivery_grant = Some(serde_json::json!({"targets": ["webhook"]}));
        let operator = save_workflow_script(&state, operator, "operator_http", None)
            .await
            .unwrap();

        let mut agent = draft(topic.id, "delivering");
        agent.source = "return 2;".into();
        agent.authoring_session_id = Some(session);
        let error = save_workflow_script(&state, agent, "workflow_save", Some(session))
            .await
            .expect_err("a conversation must not write over a delivering script");
        assert!(
            matches!(&error, GatewayError::ValidationError(message)
                if message.contains("carries delivery targets")),
            "unexpected error: {error}"
        );

        let record = state
            .storage
            .get_workflow_script(operator.script_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.authoring_session_id, None);
        assert!(
            record.effective_delivery_grant().is_some(),
            "the operator's delivering script is untouched"
        );
        assert_eq!(
            state
                .storage
                .get_workflow_script_head(operator.script_id)
                .await
                .unwrap()
                .unwrap()
                .source,
            SOURCE,
            "the refused save must not have appended a version"
        );
    }
}
