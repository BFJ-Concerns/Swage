//! The model catalogue as a session can read it: which ids a workflow call or
//! an agent definition may name, of which vendor family, and at which effort
//! levels each is served as asked.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_config::RESERVED_WORKFLOW_AGENT_ID;
use swage_context::working_norms::ModelFamily;
use swage_core::{Effort, EffortCapability, ToolError};
use swage_tool::context::ToolContext;
use uuid::Uuid;

use crate::{state::AppState, tool::Tool};

pub const MODEL_LIST_TOOL: &str = "model_list";

/// The levels a model serves as asked, lowest first: a level outside the
/// list is clamped to another, and an empty list means the model takes no
/// effort setting at all. `off` is in the list where the model can switch
/// reasoning off, and is never what another level clamps to
/// (`swage_core::resolve_effort`).
fn levels_served_as_asked(capability: &EffortCapability) -> Vec<Effort> {
    let (mut levels, can_disable) = match capability {
        EffortCapability::None => return Vec::new(),
        EffortCapability::Levels {
            levels,
            can_disable,
        } => (
            levels
                .iter()
                .copied()
                .filter(|level| *level != Effort::Off)
                .collect::<Vec<_>>(),
            *can_disable,
        ),
        // A budget expresses every level as a distinct token count.
        EffortCapability::Budget { can_disable, .. } => (
            Effort::ALL
                .into_iter()
                .filter(|level| *level != Effort::Off)
                .collect(),
            *can_disable,
        ),
    };
    if can_disable {
        levels.push(Effort::Off);
    }
    levels.sort();
    levels.dedup();
    levels
}

/// The calling topic's catalogue for a workflow worker with no agent
/// override, one entry per model id, sorted by id.
pub(crate) async fn models_for_session(
    state: &AppState,
    session_id: Uuid,
) -> Result<Value, String> {
    let session = state
        .storage
        .get_session(session_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("session {session_id} was not found"))?;
    let topic = state
        .topic_store
        .get(session.topic_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("topic {} was not found", session.topic_id))?;
    let config =
        crate::turn::load_session_config(state, &topic.location(), RESERVED_WORKFLOW_AGENT_ID)
            .map_err(|error| error.to_string())?;
    let mut ids = config.models.entries.keys().cloned().collect::<Vec<_>>();
    ids.sort();
    let mut models = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = &config.models.entries[&id];
        let capability = crate::turn::effort_capability_for_model(&config, &id, None);
        let mut model = json!({
            "id": id,
            "model": entry.model_id,
            "effort_levels": levels_served_as_asked(&capability),
        });
        if !entry.aliases.is_empty() {
            model["aliases"] = json!(entry.aliases);
        }
        if let Some(family) = ModelFamily::from_model_id(&entry.model_id).name() {
            model["family"] = json!(family);
        }
        models.push(model);
    }
    let mut listing = json!({ "models": models });
    if config.models.entries.contains_key(&config.models.default) {
        listing["default"] = json!(config.models.default);
    }
    Ok(listing)
}

pub struct ModelListTool;

#[async_trait]
impl Tool for ModelListTool {
    fn name(&self) -> &str {
        MODEL_LIST_TOOL
    }

    fn description(&self) -> &str {
        "List the models this gateway serves to this session: the ids a workflow's `agent()` \
         call can give as `model` or `fallbackModel`, and an agent definition as its model. An \
         id outside the list fails the call that names it. Each entry gives the `id`, any \
         `aliases` that name the same model, `model` (the provider's own name for it), \
         `family` (`claude`, `gpt` or `gemini`; left out when the name matches none of them), \
         and `effort_levels`: the levels the model is served at as asked, lowest first. A \
         call asking for a level not in that list is served at the nearest listed level below \
         it, or at the lowest when none is below it, and `off` never counts as either: it is \
         listed where the model can switch reasoning off, and only a call that asks for `off` \
         is served it. A model whose list is empty takes no effort setting. A workflow call \
         that gives `minEffort` is refused rather than served below that level. `default` is \
         the id a workflow call runs on when neither the call nor its script's `defaults` \
         gives a `model`, an `agent` or a `task`. The listing is read from configuration: it \
         uses no provider credential, calls no provider and changes nothing, so a listed \
         model can still be unavailable when a call is made."
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
        let state = super::require_gateway_state(
            ctx,
            self.name(),
            "model_list requires gateway execution",
        )?;
        models_for_session(state, session_id)
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
    use crate::secret::CreateSecretRequest;
    use crate::tool_executor::GatewayToolContext;
    use swage_storage::{
        AuditStore, NewSession, NewTopic, SessionStore, SqliteStorage, Storage, TopicStore,
    };
    use swage_tool::context::MockToolContext;

    #[tokio::test(flavor = "multi_thread")]
    async fn lists_workflow_models_and_effort_levels_without_accessing_credentials() {
        let root = tempfile::tempdir().expect("configuration root");
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage
            .create(NewTopic {
                name: "Workflow models".into(),
                slug: "workflow-models".into(),
            })
            .await
            .expect("topic");
        let mut config = swage_config::ResolvedConfig::default();
        config.providers.entries.insert(
            "anthropic".into(),
            swage_config::ProviderEndpointConfig {
                protocol: swage_config::Protocol::Anthropic,
                ..Default::default()
            },
        );
        config.models.default = "reviewer".into();
        config.models.entries.insert(
            "reviewer".into(),
            swage_config::ModelEndpointConfig {
                provider_id: "anthropic".into(),
                model_id: "claude-opus-5-5".into(),
                aliases: vec!["opus".into()],
                ..Default::default()
            },
        );
        let mut doer = swage_config::ModelEndpointConfig {
            provider_id: "anthropic".into(),
            model_id: "gpt-6.2-sol".into(),
            ..Default::default()
        };
        doer.capabilities.effort = Some(EffortCapability::Levels {
            levels: vec![Effort::Low, Effort::High],
            can_disable: true,
        });
        config.models.entries.insert("doer".into(), doer);
        config.models.entries.insert(
            "local".into(),
            swage_config::ModelEndpointConfig {
                provider_id: "anthropic".into(),
                model_id: "qwen-local".into(),
                ..Default::default()
            },
        );
        let state = AppState::new_with_config_root(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(swage_provider::anthropic::AnthropicProvider::new(
                "key",
                "claude-opus-5-5",
            )),
            config,
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
            root.path().to_path_buf(),
        );

        // The running provider has no route for a model added by the topic.
        let key_file = root.path().join("provider-key");
        std::fs::write(&key_file, "test-provider-key").expect("write test key");
        let secret = state
            .secret_manager
            .create_secret(CreateSecretRequest {
                name: "Topic provider key".into(),
                material_kind: "provider_key".into(),
                owner_kind: "operator".into(),
                backend_kind: "file".into(),
                scope_kind: "provider".into(),
                scope_id: Some("extra".into()),
                metadata: None,
                value: None,
                external_reference: Some(key_file.to_string_lossy().into_owned()),
            })
            .await
            .expect("register test key");
        let topic_dir = topic.location().data_dir(root.path());
        let agent_dir = topic.location().agent_dir(root.path(), "author");
        std::fs::create_dir_all(&agent_dir).expect("agent directory");
        std::fs::write(
            topic_dir.join("config.toml"),
            format!(
                r#"
[providers.entries.extra]
protocol = "openai"
api_key_secret_ref = "{}"

[models.entries.extra-model]
provider_id = "extra"
model_id = "gpt-5.6-terra"
"#,
                secret.reference
            ),
        )
        .expect("topic configuration");
        std::fs::write(
            agent_dir.join("config.toml"),
            r#"
[models]
default = "author-only"

[models.entries.author-only]
provider_id = "anthropic"
model_id = "claude-opus-5"
"#,
        )
        .expect("agent configuration");
        let author_config = crate::turn::load_session_config(&state, &topic.location(), "author")
            .expect("resolved author configuration");
        assert_eq!(author_config.models.default, "author-only");
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "author".into(),
                origin: swage_storage::SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session");
        let base = MockToolContext::new("gateway", session.id.to_string());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = GatewayToolContext::new(&base, tx, Arc::clone(&storage) as Arc<dyn Storage>)
            .with_app_state(Some(state));

        let listing = ModelListTool
            .execute(json!({}), &ctx)
            .await
            .expect("model listing");

        assert_eq!(listing["default"], "reviewer");
        let models = listing["models"].as_array().expect("models");
        let ids = models
            .iter()
            .map(|model| model["id"].as_str().expect("id"))
            .collect::<Vec<_>>();
        assert_eq!(ids, ["doer", "extra-model", "local", "reviewer"]);
        assert_eq!(models[0]["family"], "gpt");
        assert_eq!(models[0]["effort_levels"], json!(["off", "low", "high"]));
        assert_eq!(models[1]["family"], "gpt");
        assert!(
            models[1]["effort_levels"]
                .as_array()
                .expect("levels")
                .contains(&json!("medium")),
            "the OpenAI adapter declares effort control for the topic model"
        );
        assert!(models[2].get("family").is_none());
        assert_eq!(models[2]["effort_levels"], json!([]));
        assert_eq!(models[3]["family"], "claude");
        assert_eq!(models[3]["aliases"], json!(["opus"]));
        assert!(
            !models[3]["effort_levels"]
                .as_array()
                .expect("levels")
                .is_empty(),
            "the Anthropic adapter declares the Claude model's effort control"
        );
        let events = storage.list_audit_events().await.expect("audit events");
        assert!(
            !events
                .iter()
                .any(|event| { event.entity_kind == "secret" && event.event_kind == "accessed" }),
            "listing models must leave the credential access record empty"
        );
    }
}
