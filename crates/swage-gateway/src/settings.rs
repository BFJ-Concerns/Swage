use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::ApiError;
use crate::secret::{CreateSecretRequest, RotateSecretRequest};
use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct SettingsResponse {
    pub providers: HashMap<String, ProviderSettings>,
    pub default_provider: String,
    pub models: HashMap<String, ModelSettings>,
    pub default_model: String,
    pub generation_presets: HashMap<String, swage_config::GenerationPresetConfig>,
    pub restart_required: bool,
    pub can_restart: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub protocol: swage_config::Protocol,
    pub base_url: Option<String>,
    pub api_key_secret_ref: Option<String>,
    pub api_key_env: Option<String>,
    pub has_credential: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSettings {
    pub provider_id: String,
    pub model_id: String,
    pub aliases: Vec<String>,
    pub context_window: Option<u32>,
    pub generation_preset: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SaveSettingsRequest {
    pub default_provider: String,
    pub providers: HashMap<String, SaveProviderSettings>,
    pub default_model: String,
    pub models: HashMap<String, ModelSettings>,
    #[serde(default)]
    pub generation_presets: HashMap<String, swage_config::GenerationPresetConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SaveProviderSettings {
    pub protocol: swage_config::Protocol,
    pub base_url: Option<String>,
    pub api_key_secret_ref: Option<String>,
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredModel {
    pub id: String,
    pub name: String,
    pub context_window: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelDiscoveryResponse {
    pub provider_id: String,
    pub models: Vec<DiscoveredModel>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestartStatusResponse {
    pub restart_required: bool,
    pub can_restart: bool,
    pub guidance: String,
}

pub async fn load_settings(state: &AppState) -> SettingsResponse {
    let providers = state
        .config
        .providers
        .entries
        .iter()
        .map(|(id, provider)| {
            (
                id.clone(),
                ProviderSettings {
                    protocol: provider.protocol.clone(),
                    base_url: provider.base_url.clone(),
                    api_key_secret_ref: provider.api_key_secret_ref.clone(),
                    api_key_env: provider.api_key_env.clone(),
                    has_credential: provider.api_key_secret_ref.is_some()
                        || provider.api_key_env.is_some(),
                },
            )
        })
        .collect();

    let models = state
        .config
        .models
        .entries
        .iter()
        .map(|(id, model)| {
            (
                id.clone(),
                ModelSettings {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                    aliases: model.aliases.clone(),
                    context_window: model.context_window,
                    generation_preset: model.generation_preset.clone(),
                },
            )
        })
        .collect();

    SettingsResponse {
        providers,
        default_provider: state.config.providers.default.clone(),
        models,
        default_model: state.config.models.default.clone(),
        generation_presets: state.config.generation.presets.clone(),
        restart_required: state.restart_required(),
        can_restart: state.managed_gateway,
    }
}

pub async fn save_settings(
    state: &AppState,
    request: SaveSettingsRequest,
) -> Result<SettingsResponse, ApiError> {
    validate_settings_request(&request)?;

    let mut providers = HashMap::new();
    for (provider_id, provider) in request.providers {
        let secret_ref = persist_provider_key(state, &provider_id, &provider).await?;
        providers.insert(
            provider_id,
            swage_config::ProviderEndpointConfig {
                protocol: provider.protocol,
                api_key_secret_ref: secret_ref.or(provider.api_key_secret_ref),
                api_key_env: provider.api_key_env,
                base_url: provider.base_url,
            },
        );
    }

    let models = request
        .models
        .into_iter()
        .map(|(id, model)| {
            (
                id,
                swage_config::ModelEndpointConfig {
                    provider_id: model.provider_id,
                    model_id: model.model_id,
                    aliases: model.aliases,
                    context_window: model.context_window,
                    generation_preset: model.generation_preset,
                },
            )
        })
        .collect();

    let path = global_config_path()?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            ApiError::validation(format!("failed to create config directory: {error}"))
        })?;
    }
    let mut partial = read_global_partial_config(&path).await?;
    partial.providers = Some(swage_config::merge::PartialProviderCatalogConfig {
        default: Some(request.default_provider),
        entries: Some(providers),
    });
    partial.models = Some(swage_config::merge::PartialModelCatalogConfig {
        default: Some(request.default_model),
        default_model_id: None,
        entries: Some(models),
    });
    partial.generation = Some(swage_config::merge::PartialGenerationConfig {
        presets: Some(request.generation_presets),
    });
    let body = toml::to_string_pretty(&partial)
        .map_err(|error| ApiError::validation(format!("failed to serialise settings: {error}")))?;
    tokio::fs::write(&path, body)
        .await
        .map_err(|error| ApiError::validation(format!("failed to write config: {error}")))?;

    state.mark_restart_required();
    Ok(load_settings(state).await)
}

pub async fn discover_models(
    state: &AppState,
    provider_id: String,
    query: Option<String>,
) -> Result<ModelDiscoveryResponse, ApiError> {
    let Some(provider) = state.config.providers.entries.get(&provider_id) else {
        return Err(ApiError::not_found_str("provider", &provider_id));
    };

    let mut models = match provider.protocol {
        swage_config::Protocol::OpenRouter => discover_openrouter_models().await?,
        swage_config::Protocol::OpenAi => {
            known_models(&["gpt-4.1", "gpt-4.1-mini", "gpt-4o", "gpt-4o-mini"])
        }
        swage_config::Protocol::Anthropic => known_models(&[
            "claude-sonnet-4-20250514",
            "claude-opus-4-20250514",
            "claude-3-5-haiku-20241022",
        ]),
        swage_config::Protocol::Google => known_models(&["gemini-2.5-pro", "gemini-2.5-flash"]),
    };

    if let Some(query) = query.filter(|q| !q.trim().is_empty()) {
        let needle = query.to_lowercase();
        models.retain(|model| {
            model.id.to_lowercase().contains(&needle) || model.name.to_lowercase().contains(&needle)
        });
    }

    Ok(ModelDiscoveryResponse {
        provider_id,
        models,
    })
}

pub fn restart_status(state: &AppState) -> RestartStatusResponse {
    RestartStatusResponse {
        restart_required: state.restart_required(),
        can_restart: state.managed_gateway,
        guidance: if state.managed_gateway {
            "Settings are saved. Restart the managed gateway to apply them.".to_string()
        } else {
            "Settings are saved. Restart the gateway process manually to apply them.".to_string()
        },
    }
}

pub async fn restart_gateway(state: &AppState) -> Result<RestartStatusResponse, ApiError> {
    if !state.managed_gateway {
        return Err(ApiError::validation(
            "this gateway is not managed by the desktop runtime".to_string(),
        ));
    }
    state.shutdown_signal.cancel();
    Ok(restart_status(state))
}

fn validate_settings_request(request: &SaveSettingsRequest) -> Result<(), ApiError> {
    if !request.default_provider.is_empty()
        && !request.providers.contains_key(&request.default_provider)
    {
        return Err(ApiError::validation(
            "default provider does not exist".to_string(),
        ));
    }
    if !(request.default_model.is_empty() && request.models.is_empty())
        && !request.models.contains_key(&request.default_model)
    {
        return Err(ApiError::validation(
            "default model does not exist".to_string(),
        ));
    }
    for (id, preset) in &request.generation_presets {
        if id.trim().is_empty() || preset.is_empty() {
            return Err(ApiError::validation(
                "generation presets must be named and non-empty".to_string(),
            ));
        }
    }
    Ok(())
}

async fn persist_provider_key(
    state: &AppState,
    provider_id: &str,
    provider: &SaveProviderSettings,
) -> Result<Option<String>, ApiError> {
    let Some(api_key) = provider
        .api_key
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };

    if let Some(reference) = provider.api_key_secret_ref.as_deref() {
        let id = reference
            .strip_prefix("secret:")
            .and_then(|raw| uuid::Uuid::parse_str(raw).ok())
            .ok_or_else(|| ApiError::validation("invalid provider secret reference".to_string()))?;
        let secret = state
            .secret_manager
            .rotate_secret(
                id,
                RotateSecretRequest {
                    value: Some(api_key.to_string()),
                    external_reference: None,
                    metadata: None,
                },
            )
            .await?;
        return Ok(Some(secret.reference));
    }

    let secret = state
        .secret_manager
        .create_secret(CreateSecretRequest {
            name: format!("provider:{provider_id}:api-key"),
            material_kind: "provider_key".to_string(),
            owner_kind: "provider".to_string(),
            backend_kind: "managed".to_string(),
            scope_kind: "global".to_string(),
            scope_id: None,
            metadata: Some(json!({ "provider_id": provider_id })),
            value: Some(api_key.to_string()),
            external_reference: None,
        })
        .await?;
    Ok(Some(secret.reference))
}

fn global_config_path() -> Result<PathBuf, ApiError> {
    let root = swage_config::discovery::resolve_root()
        .map_err(|problem| ApiError::validation(problem.to_string()))?;
    Ok(root.join("config.toml"))
}

async fn read_global_partial_config(
    path: &std::path::Path,
) -> Result<swage_config::PartialConfig, ApiError> {
    if !path.exists() {
        return Ok(swage_config::PartialConfig::default());
    }
    let body = tokio::fs::read_to_string(path)
        .await
        .map_err(|error| ApiError::validation(format!("failed to read config: {error}")))?;
    toml::from_str(&body)
        .map_err(|error| ApiError::validation(format!("failed to parse config: {error}")))
}

fn known_models(ids: &[&str]) -> Vec<DiscoveredModel> {
    ids.iter()
        .map(|id| DiscoveredModel {
            id: (*id).to_string(),
            name: (*id).to_string(),
            context_window: None,
        })
        .collect()
}

async fn discover_openrouter_models() -> Result<Vec<DiscoveredModel>, ApiError> {
    let body: serde_json::Value = reqwest::Client::new()
        .get("https://openrouter.ai/api/v1/models")
        .send()
        .await
        .map_err(|error| ApiError::validation(format!("model discovery failed: {error}")))?
        .json()
        .await
        .map_err(|error| {
            ApiError::validation(format!("model discovery response failed: {error}"))
        })?;

    let models = body
        .get("data")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item.get("id")?.as_str()?.to_string();
            let name = item
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or(&id)
                .to_string();
            let context_window = item
                .get("context_length")
                .and_then(|value| value.as_u64())
                .and_then(|value| u32::try_from(value).ok());
            Some(DiscoveredModel {
                id,
                name,
                context_window,
            })
        })
        .collect();
    Ok(models)
}
