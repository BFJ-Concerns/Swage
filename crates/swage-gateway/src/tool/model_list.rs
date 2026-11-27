//! The model catalogue as a session can read it: which ids a workflow call or
//! an agent definition may name, of which vendor family, and at which effort
//! levels each is served as asked.

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_context::working_norms::ModelFamily;
use swage_core::{Effort, EffortCapability, ToolError};
use swage_tool::context::ToolContext;
use uuid::Uuid;

use crate::{state::AppState, tool::Tool};

pub const MODEL_LIST_TOOL: &str = "model_list";

/// The levels a model serves as asked, lowest first: a level outside the
/// list is clamped to another, and an empty list means the model takes no
/// effort setting at all.
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

/// The catalogue the calling session's topic and agent resolve, one entry
/// per model id, sorted by id.
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
    let config = crate::turn::load_session_config(state, &topic.location(), &session.agent_id)
        .map_err(|error| error.to_string())?;
    let mut ids = config.models.entries.keys().cloned().collect::<Vec<_>>();
    ids.sort();
    let mut models = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = &config.models.entries[&id];
        let mut served = config.clone();
        served.models.default = id.clone();
        let capability = crate::turn::served_effort_capability(state, &served).await;
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
         call can give as `model` or `fallbackModel`, and an agent definition as its model. \
         Each entry gives the `id`, any `aliases` that name the same model, `model` (the \
         provider's own name for it), `family` (`claude`, `gpt` or `gemini`; left out when the \
         name matches none of them), and `effort_levels`: the levels the model is served at as \
         asked. A call asking for a level not in that list is clamped to a lower one, and a \
         model whose list is empty takes no effort setting. `default` is the id a call that \
         names no model runs on. Listing changes nothing."
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
    use crate::tool_executor::GatewayToolContext;
    use swage_storage::{NewSession, SessionStore, SqliteStorage, Storage, TopicStore};
    use swage_tool::context::MockToolContext;

    /// A session sees every configured id with its family and the levels it
    /// is served at as asked, read from the catalogue's declaration where
    /// there is one and from the adapter that would serve it otherwise.
    #[tokio::test(flavor = "multi_thread")]
    async fn lists_each_catalogue_model_with_its_family_and_effort_levels() {
        let storage = Arc::new(SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
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
        let state = AppState::for_test(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(swage_provider::anthropic::AnthropicProvider::new(
                "key",
                "claude-opus-5-5",
            )),
            config,
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "default".into(),
                origin: swage_storage::SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .expect("session");
        let base = MockToolContext::new("gateway", session.id.to_string());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = GatewayToolContext::new(&base, tx, storage as Arc<dyn Storage>)
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
        assert_eq!(ids, ["doer", "local", "reviewer"]);
        assert_eq!(models[0]["family"], "gpt");
        assert_eq!(models[0]["effort_levels"], json!(["off", "low", "high"]));
        assert!(models[1].get("family").is_none());
        assert_eq!(models[1]["effort_levels"], json!([]));
        assert_eq!(models[2]["family"], "claude");
        assert_eq!(models[2]["aliases"], json!(["opus"]));
        assert!(
            !models[2]["effort_levels"]
                .as_array()
                .expect("levels")
                .is_empty(),
            "the Anthropic adapter declares the Claude model's effort control"
        );
    }
}
