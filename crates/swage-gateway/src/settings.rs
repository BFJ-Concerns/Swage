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
    // TODO Add global tool permission controls to settings
    // Extend the settings API, persistence, validation, and UI so operators can edit global allow, deny, bundle, and approval-gated tool rules from settings.
    // ---
    // TODO Add global approval defaults to settings
    // Surface global HITL timeout and timeout-policy defaults in settings, including save validation and honest restart-required messaging.
    // ---
    // TODO Add global sandbox policy controls to settings
    // Expose the global sandbox enabled policy in settings with persistence through the global config file.
    // ---
    // TODO Add global context assembly controls to settings
    // Let operators edit global context assembly, timeout, and budget defaults from settings.
    // ---
    // TODO Add global workspace default controls to settings
    // Surface the global workspace path default in settings while keeping bench workspace attachments and grants conceptually separate.
    // ---
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
    let SaveSettingsRequest {
        default_provider,
        providers: requested_providers,
        default_model,
        models: requested_models,
        generation_presets,
    } = request;

    let mut providers = HashMap::new();
    for (provider_id, provider) in requested_providers {
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

    let models = requested_models
        .iter()
        .map(|(id, model)| {
            (
                id.clone(),
                swage_config::ModelEndpointConfig {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                    aliases: model.aliases.clone(),
                    context_window: model.context_window,
                    generation_preset: model.generation_preset.clone(),
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
        default: Some(default_provider.clone()),
        entries: Some(providers),
    });
    partial.models = Some(swage_config::merge::PartialModelCatalogConfig {
        default: Some(default_model.clone()),
        default_model_id: None,
        entries: Some(models),
    });
    partial.generation = if generation_presets.is_empty() {
        None
    } else {
        Some(swage_config::merge::PartialGenerationConfig {
            presets: Some(generation_presets.clone()),
        })
    };
    let body = toml::to_string_pretty(&partial)
        .map_err(|error| ApiError::validation(format!("failed to serialise settings: {error}")))?;
    tokio::fs::write(&path, body)
        .await
        .map_err(|error| ApiError::validation(format!("failed to write config: {error}")))?;

    state.mark_restart_required();
    let saved_providers = partial
        .providers
        .as_ref()
        .and_then(|providers| providers.entries.as_ref())
        .expect("settings save always writes provider entries");
    Ok(settings_response_from_saved_config(
        state,
        default_provider,
        saved_providers,
        default_model,
        requested_models,
        generation_presets,
    ))
}

pub async fn discover_models(
    state: &AppState,
    provider_id: String,
    query: Option<String>,
    draft_provider: Option<SaveProviderSettings>,
) -> Result<ModelDiscoveryResponse, ApiError> {
    let provider = match draft_provider {
        Some(provider) => provider.into_endpoint_config(),
        None => provider_for_discovery(state, &provider_id).await?,
    };

    let mut models = match provider.protocol {
        swage_config::Protocol::OpenRouter => discover_openrouter_models().await?,
        swage_config::Protocol::OpenAi => {
            if let Some(base_url) = provider.base_url.as_deref() {
                discover_openai_compatible_models(base_url).await?
            } else {
                known_models(&["gpt-4.1", "gpt-4.1-mini", "gpt-4o", "gpt-4o-mini"])
            }
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

impl SaveProviderSettings {
    fn into_endpoint_config(self) -> swage_config::ProviderEndpointConfig {
        swage_config::ProviderEndpointConfig {
            protocol: self.protocol,
            base_url: self.base_url,
            api_key_secret_ref: self.api_key_secret_ref,
            api_key_env: self.api_key_env,
        }
    }
}

fn settings_response_from_saved_config(
    state: &AppState,
    default_provider: String,
    providers: &HashMap<String, swage_config::ProviderEndpointConfig>,
    default_model: String,
    models: HashMap<String, ModelSettings>,
    generation_presets: HashMap<String, swage_config::GenerationPresetConfig>,
) -> SettingsResponse {
    let providers = providers
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

    SettingsResponse {
        providers,
        default_provider,
        models,
        default_model,
        generation_presets,
        restart_required: true,
        can_restart: state.managed_gateway,
    }
}

async fn provider_for_discovery(
    state: &AppState,
    provider_id: &str,
) -> Result<swage_config::ProviderEndpointConfig, ApiError> {
    if let Some(provider) = state.config.providers.entries.get(provider_id) {
        return Ok(provider.clone());
    }

    if let Ok(path) = global_config_path() {
        let partial = read_global_partial_config(&path).await?;
        if let Some(providers) = partial.providers
            && let Some(entries) = providers.entries
            && let Some(provider) = entries.get(provider_id)
        {
            return Ok(provider.clone());
        }
    }

    Err(ApiError::not_found_str("provider", provider_id))
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

async fn discover_openai_compatible_models(
    base_url: &str,
) -> Result<Vec<DiscoveredModel>, ApiError> {
    let client = reqwest::Client::new();
    let mut last_error = None;

    for url in openai_compatible_model_urls(base_url) {
        let response = match client.get(&url).send().await {
            Ok(response) => response,
            Err(error) => {
                last_error = Some(format!("{url}: {error}"));
                continue;
            }
        };

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            last_error = Some(format!("{url}: HTTP 404"));
            continue;
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::validation(format!(
                "model discovery failed: {url} returned HTTP {status}: {body}"
            )));
        }

        let body: serde_json::Value = response.json().await.map_err(|error| {
            ApiError::validation(format!("model discovery response failed: {error}"))
        })?;
        return parse_openai_compatible_models(&body);
    }

    Err(ApiError::validation(format!(
        "model discovery failed: {}",
        last_error.unwrap_or_else(|| "no model endpoint candidates were available".to_string())
    )))
}

fn openai_compatible_model_urls(base_url: &str) -> Vec<String> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Vec::new();
    }

    let mut urls = vec![format!("{base}/models")];
    if !base.ends_with("/v1") {
        urls.push(format!("{base}/v1/models"));
    }
    urls
}

fn parse_openai_compatible_models(
    body: &serde_json::Value,
) -> Result<Vec<DiscoveredModel>, ApiError> {
    let models = body
        .get("data")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item.get("id")?.as_str()?.to_string();
            Some(DiscoveredModel {
                name: item
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or(&id)
                    .to_string(),
                id,
                context_window: item
                    .get("context_length")
                    .or_else(|| item.get("context_window"))
                    .and_then(|value| value.as_u64())
                    .and_then(|value| u32::try_from(value).ok()),
            })
        })
        .collect();
    Ok(models)
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::*;

    #[test]
    fn openai_compatible_model_urls_accept_v1_and_plain_roots() {
        assert_eq!(
            openai_compatible_model_urls("http://127.0.0.1:8080/v1"),
            vec!["http://127.0.0.1:8080/v1/models"]
        );
        assert_eq!(
            openai_compatible_model_urls("http://127.0.0.1:8080"),
            vec![
                "http://127.0.0.1:8080/models",
                "http://127.0.0.1:8080/v1/models"
            ]
        );
    }

    #[test]
    fn parse_openai_compatible_models_reads_openai_shape() {
        let models = parse_openai_compatible_models(&json!({
            "data": [
                { "id": "llama-3.2", "context_length": 8192 },
                { "id": "tiny", "name": "Tiny Test", "context_window": 1024 }
            ]
        }))
        .expect("models should parse");

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "llama-3.2");
        assert_eq!(models[0].name, "llama-3.2");
        assert_eq!(models[0].context_window, Some(8192));
        assert_eq!(models[1].id, "tiny");
        assert_eq!(models[1].name, "Tiny Test");
        assert_eq!(models[1].context_window, Some(1024));
    }

    #[test]
    fn settings_response_from_saved_config_echoes_secret_refs_without_raw_keys() {
        let state = AppState {
            storage: std::sync::Arc::new(crate::turn::tests::MockStorage::new())
                as std::sync::Arc<dyn swage_storage::store::Storage>,
            bench_store: std::sync::Arc::new(crate::turn::tests::MockStorage::new())
                as std::sync::Arc<dyn swage_storage::store::BenchStore>,
            provider: std::sync::Arc::new(crate::turn::tests::MockProvider::new(vec![]))
                as std::sync::Arc<dyn swage_provider::LlmProvider>,
            voice_provider: std::sync::Arc::new(swage_provider::UnsupportedVoiceProvider::new(
                "voice unavailable in test",
            )),
            config: swage_config::ResolvedConfig::default(),
            active_turns: std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            active_connections: std::sync::Arc::new(tokio::sync::Mutex::new(HashSet::new())),
            tool_dispatcher: std::sync::Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            activity_tracker: std::sync::Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: tokio::sync::broadcast::channel(16).0,
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            hook_registry: None,
            channel_routes: std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            presence: std::sync::Arc::new(tokio::sync::Mutex::new(
                crate::presence::PresenceRuntime::default(),
            )),
            execution_hosts: std::sync::Arc::new(tokio::sync::Mutex::new(
                crate::execution_host::ExecutionHostRuntime::default(),
            )),
            pairing_codes: std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            secret_manager: std::sync::Arc::new(crate::secret::SecretManager::new(
                std::sync::Arc::new(crate::turn::tests::MockStorage::new())
                    as std::sync::Arc<dyn swage_storage::store::Storage>,
            )),
            workflow_capabilities: crate::capabilities::WorkflowCapabilities::default(),
            restart_required: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            managed_gateway: false,
        };
        let mut providers = HashMap::new();
        providers.insert(
            "llama-cpp".to_string(),
            swage_config::ProviderEndpointConfig {
                protocol: swage_config::Protocol::OpenAi,
                base_url: Some("http://127.0.0.1:8080/v1".to_string()),
                api_key_secret_ref: Some("secret:abc".to_string()),
                api_key_env: None,
            },
        );

        let response = settings_response_from_saved_config(
            &state,
            "llama-cpp".to_string(),
            &providers,
            String::new(),
            HashMap::new(),
            HashMap::new(),
        );

        let provider = response.providers.get("llama-cpp").expect("provider");
        assert!(provider.has_credential);
        assert_eq!(provider.api_key_secret_ref.as_deref(), Some("secret:abc"));
        assert!(
            serde_json::to_value(provider)
                .expect("provider json")
                .get("api_key")
                .is_none()
        );
    }
}
