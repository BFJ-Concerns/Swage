use std::collections::HashMap;
use std::path::PathBuf;
use swage_core::ScopeLocation;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;

use crate::error::ApiError;
use crate::secret::{CreateSecretRequest, RotateSecretRequest};
use crate::state::AppState;

pub use swage_protocol::client::settings::{
    DiscoveredModel, ModelDiscoveryResponse, ModelSettings, ProviderSettings,
    RestartStatusResponse, SaveProviderSettings, SaveSettingsRequest, SettingsResponse,
    ToolRuntimeSettings,
};

fn try_wire_from_config<W: DeserializeOwned, C: Serialize>(value: C) -> Result<W, ApiError> {
    serde_json::from_value(
        serde_json::to_value(value)
            .map_err(|error| ApiError::validation(format!("invalid runtime settings: {error}")))?,
    )
    .map_err(|error| ApiError::validation(format!("invalid runtime settings: {error}")))
}

#[cfg(test)]
fn wire_from_config<W: DeserializeOwned, C: Serialize>(value: C) -> W {
    try_wire_from_config(value).expect("test config and wire settings shapes match")
}

fn config_from_wire<C: DeserializeOwned, W: Serialize>(value: W) -> Result<C, ApiError> {
    serde_json::from_value(
        serde_json::to_value(value)
            .map_err(|error| ApiError::validation(format!("invalid settings: {error}")))?,
    )
    .map_err(|error| ApiError::validation(format!("invalid settings: {error}")))
}

async fn write_config_atomically(path: &std::path::Path, body: &str) -> Result<(), ApiError> {
    if path.parent().is_none() {
        return Err(ApiError::validation(
            "config path has no parent directory".to_string(),
        ));
    }
    if path.file_name().and_then(|name| name.to_str()).is_none() {
        return Err(ApiError::validation(
            "config path has no valid file name".to_string(),
        ));
    }
    let path = path.to_path_buf();
    let body = body.as_bytes().to_vec();
    tokio::task::spawn_blocking(move || swage_client::runtime::write_atomically(&path, &body))
        .await
        .map_err(|error| ApiError::validation(format!("failed to write config: {error}")))?
        .map_err(|error| ApiError::validation(format!("failed to write config: {error}")))
}

fn settings_audit_snapshot(settings: &SettingsResponse) -> Result<serde_json::Value, ApiError> {
    let mut snapshot = serde_json::to_value(settings)
        .map_err(|error| ApiError::validation(format!("failed to snapshot settings: {error}")))?;
    if let Some(api_key) = snapshot.pointer_mut("/tool_runtime/search/api_key")
        && !api_key.is_null()
    {
        *api_key = serde_json::Value::String("[REDACTED]".to_string());
    }
    Ok(snapshot)
}

fn tool_runtime_from_config(
    config: &swage_config::ToolConfig,
) -> Result<ToolRuntimeSettings, ApiError> {
    Ok(ToolRuntimeSettings {
        shell_timeout_secs: config.shell_timeout_secs,
        python_execute: try_wire_from_config(config.python_execute.clone())?,
        max_output_bytes: config.max_output_bytes,
        plugin_directory: config.plugin_directory.clone(),
        search: try_wire_from_config(config.search.clone())?,
    })
}

pub async fn load_settings(state: &AppState) -> Result<SettingsResponse, ApiError> {
    let config = state.active_config();
    let providers = config
        .providers
        .entries
        .iter()
        .map(|(id, provider)| -> Result<_, ApiError> {
            Ok((
                id.clone(),
                ProviderSettings {
                    protocol: try_wire_from_config(provider.protocol.clone())?,
                    worker_staleness_threshold_ms: provider.worker_staleness_threshold_ms,
                    base_url: provider.base_url.clone(),
                    api_key_secret_ref: provider.api_key_secret_ref.clone(),
                    api_key_env: provider.api_key_env.clone(),
                    has_credential: provider.api_key_secret_ref.is_some()
                        || provider.api_key_env.is_some(),
                    command: provider.command.clone(),
                    state_dir: provider.state_dir.clone(),
                },
            ))
        })
        .collect::<Result<_, _>>()?;

    let models = config
        .models
        .entries
        .iter()
        .map(|(id, model)| -> Result<_, ApiError> {
            Ok((
                id.clone(),
                ModelSettings {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                    aliases: model.aliases.clone(),
                    context_window: model.context_window,
                    generation_preset: model.generation_preset.clone(),
                    default_effort: model.default_effort,
                    capabilities: model.capabilities.clone(),
                },
            ))
        })
        .collect::<Result<_, _>>()?;

    Ok(SettingsResponse {
        providers,
        default_provider: config.providers.default.clone(),
        models,
        default_model: config.models.default.clone(),
        generation_presets: config.generation.presets.clone(),
        tool_runtime: tool_runtime_from_config(&config.tools)?,
        tool_permissions: try_wire_from_config(config.tools.permissions.clone())?,
        hitl: try_wire_from_config(config.hitl.clone())?,
        sandbox: config.sandbox.clone(),
        context: config.context.clone(),
        pricing: config.pricing.clone(),
        observability: config.observability.clone(),
        communication: try_wire_from_config(config.communication)?,
        sessions: try_wire_from_config(config.sessions)?,
        restart_required: state.restart_required(),
        can_restart: state.managed_gateway,
    })
}

pub async fn save_settings(
    state: &AppState,
    request: SaveSettingsRequest,
) -> Result<SettingsResponse, ApiError> {
    validate_settings_request(&request)?;
    let prior_settings = settings_audit_snapshot(&load_settings(state).await?)?;
    let SaveSettingsRequest {
        default_provider,
        providers: requested_providers,
        default_model,
        models: requested_models,
        generation_presets,
        tool_runtime,
        tool_permissions,
        hitl,
        sandbox,
        context,
        pricing,
        observability,
        communication,
        sessions,
    } = request;

    let tool_permissions: Option<swage_config::ToolPermissionConfig> =
        config_from_wire(tool_permissions)?;
    let hitl: Option<swage_config::HitlConfig> = config_from_wire(hitl)?;
    let communication: Option<swage_config::CommunicationConfig> = config_from_wire(communication)?;
    let sessions: Option<swage_config::SessionsConfig> = config_from_wire(sessions)?;

    let mut providers = HashMap::new();
    for (provider_id, provider) in requested_providers {
        let secret_ref = persist_provider_key(state, &provider_id, &provider).await?;
        providers.insert(
            provider_id,
            swage_config::ProviderEndpointConfig {
                worker_staleness_threshold_ms: provider.worker_staleness_threshold_ms,
                protocol: config_from_wire(provider.protocol)?,
                api_key_secret_ref: secret_ref.or(provider.api_key_secret_ref),
                api_key_env: provider.api_key_env,
                base_url: provider.base_url,
                command: provider.command,
                state_dir: provider.state_dir,
            },
        );
    }

    let models = requested_models
        .iter()
        .map(|(id, model)| -> Result<_, ApiError> {
            Ok((
                id.clone(),
                swage_config::ModelEndpointConfig {
                    provider_id: model.provider_id.clone(),
                    model_id: model.model_id.clone(),
                    aliases: model.aliases.clone(),
                    context_window: model.context_window,
                    generation_preset: model.generation_preset.clone(),
                    default_effort: model.default_effort,
                    capabilities: model.capabilities.clone(),
                },
            ))
        })
        .collect::<Result<_, _>>()?;

    let path = global_config_path(&state.config_root);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            ApiError::validation(format!("failed to create config directory: {error}"))
        })?;
    }
    let mut partial = read_partial_config(&path).await?;
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
    apply_runtime_settings_to_partial(
        &mut partial,
        tool_runtime.clone(),
        tool_permissions.clone(),
        hitl.clone(),
        sandbox.clone(),
        context.clone(),
        pricing.clone(),
        observability.clone(),
        communication.clone(),
        sessions.clone(),
    )?;
    let body = toml::to_string_pretty(&partial)
        .map_err(|error| ApiError::validation(format!("failed to serialise settings: {error}")))?;
    write_config_atomically(&path, &body).await?;

    let saved_providers = partial
        .providers
        .as_ref()
        .and_then(|providers| providers.entries.as_ref())
        .expect("settings save always writes provider entries");
    let mut runtime_config = state.active_config();
    runtime_config.providers.default = default_provider;
    runtime_config.providers.entries = saved_providers.clone();
    runtime_config.models.default = default_model;
    runtime_config.models.entries = requested_models
        .into_iter()
        .map(|(id, model)| -> Result<_, ApiError> {
            Ok((
                id,
                swage_config::ModelEndpointConfig {
                    provider_id: model.provider_id,
                    model_id: model.model_id,
                    aliases: model.aliases,
                    context_window: model.context_window,
                    generation_preset: model.generation_preset,
                    default_effort: model.default_effort,
                    capabilities: model.capabilities,
                },
            ))
        })
        .collect::<Result<_, _>>()?;
    runtime_config.generation.presets = generation_presets;
    if let Some(tool_runtime) = tool_runtime {
        apply_tool_runtime_settings(&mut runtime_config.tools, tool_runtime)?;
    }
    runtime_config.tools.permissions =
        tool_permissions.unwrap_or_else(|| runtime_config.tools.permissions.clone());
    runtime_config.hitl = hitl.unwrap_or_else(|| runtime_config.hitl.clone());
    runtime_config.sandbox = sandbox.unwrap_or_else(|| runtime_config.sandbox.clone());
    runtime_config.context = context.unwrap_or_else(|| runtime_config.context.clone());
    runtime_config.pricing = pricing.unwrap_or_else(|| runtime_config.pricing.clone());
    runtime_config.observability =
        observability.unwrap_or_else(|| runtime_config.observability.clone());
    runtime_config.communication =
        communication.unwrap_or_else(|| runtime_config.communication.clone());
    runtime_config.sessions = sessions.unwrap_or_else(|| runtime_config.sessions.clone());

    let provider = crate::startup::create_provider(&runtime_config, &state.secret_manager).await;
    let tool_dispatcher = crate::startup::build_tool_dispatcher(
        &runtime_config,
        state.storage.clone(),
        &state.config_root,
    );
    state.replace_runtime(runtime_config, provider, tool_dispatcher);
    let response = load_settings(state).await?;
    let new_settings = settings_audit_snapshot(&response)?;
    crate::config_change::record_config_change(
        state,
        crate::config_change::ConfigChange {
            entity_kind: "settings",
            entity_id: "global",
            action: "save_settings",
            source: "operator_http",
            session_id: None,
            summary: "saved settings",
            prior_state: Some(prior_settings),
            new_state: Some(new_settings),
        },
    )
    .await
    .map_err(ApiError::from)?;

    Ok(response)
}

pub(crate) async fn persist_permanent_tool_approval_for_topic(
    config_root: &std::path::Path,
    config: &swage_config::ResolvedConfig,
    topic: &swage_storage::model::Topic,
    tool_name: &str,
) -> Result<(), ApiError> {
    let path = scope_config_path(config_root, &topic.location());
    persist_permanent_tool_approval_to_path(config, tool_name, path).await
}

pub(crate) async fn persist_permanent_tool_approval_globally(
    state: &AppState,
    tool_name: &str,
) -> Result<(), ApiError> {
    let path = global_config_path(&state.config_root);
    persist_permanent_tool_approval_to_path(&state.active_config(), tool_name, path).await
}

async fn persist_permanent_tool_approval_to_path(
    config: &swage_config::ResolvedConfig,
    tool_name: &str,
    path: PathBuf,
) -> Result<(), ApiError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            ApiError::validation(format!("failed to create config directory: {error}"))
        })?;
    }

    let mut partial = read_partial_config(&path).await?;
    let tools = partial.tools.get_or_insert_with(Default::default);
    tools.require_approval = Some(require_approval_entries_without_tool(config, tool_name));

    let body = toml::to_string_pretty(&partial)
        .map_err(|error| ApiError::validation(format!("failed to serialise settings: {error}")))?;
    write_config_atomically(&path, &body).await?;

    Ok(())
}

fn scope_config_path(config_root: &std::path::Path, scope: &ScopeLocation) -> PathBuf {
    scope.data_dir(config_root).join("config.toml")
}

fn require_approval_entries_without_tool(
    config: &swage_config::ResolvedConfig,
    tool_name: &str,
) -> Vec<swage_config::model::RequireApprovalEntry> {
    config
        .tools
        .permissions
        .require_approval
        .iter()
        .flat_map(|entry| match entry {
            swage_config::model::RequireApprovalEntry::Simple(name)
                if config
                    .tools
                    .permissions
                    .bundles
                    .get(name)
                    .is_some_and(|bundle_tools| {
                        bundle_tools.iter().any(|tool| tool == tool_name)
                    }) =>
            {
                config.tools.permissions.bundles[name]
                    .iter()
                    .filter(|bundle_tool| bundle_tool.as_str() != tool_name)
                    .map(|bundle_tool| {
                        swage_config::model::RequireApprovalEntry::Simple(bundle_tool.clone())
                    })
                    .collect::<Vec<_>>()
            }
            swage_config::model::RequireApprovalEntry::Simple(name) if name == tool_name => vec![],
            swage_config::model::RequireApprovalEntry::Simple(name) => {
                vec![swage_config::model::RequireApprovalEntry::Simple(
                    name.clone(),
                )]
            }
            swage_config::model::RequireApprovalEntry::WithOptions {
                tool,
                timeout,
                timeout_policy,
            } if config
                .tools
                .permissions
                .bundles
                .get(tool)
                .is_some_and(|bundle_tools| bundle_tools.iter().any(|tool| tool == tool_name)) =>
            {
                config.tools.permissions.bundles[tool]
                    .iter()
                    .filter(|bundle_tool| bundle_tool.as_str() != tool_name)
                    .map(
                        |bundle_tool| swage_config::model::RequireApprovalEntry::WithOptions {
                            tool: bundle_tool.clone(),
                            timeout: *timeout,
                            timeout_policy: timeout_policy.clone(),
                        },
                    )
                    .collect::<Vec<_>>()
            }
            swage_config::model::RequireApprovalEntry::WithOptions { tool, .. }
                if tool == tool_name =>
            {
                vec![]
            }
            swage_config::model::RequireApprovalEntry::WithOptions {
                tool,
                timeout,
                timeout_policy,
            } => vec![swage_config::model::RequireApprovalEntry::WithOptions {
                tool: tool.clone(),
                timeout: *timeout,
                timeout_policy: timeout_policy.clone(),
            }],
        })
        .collect()
}

pub async fn discover_models(
    state: &AppState,
    provider_id: String,
    query: Option<String>,
    draft_provider: Option<SaveProviderSettings>,
) -> Result<ModelDiscoveryResponse, ApiError> {
    let (provider, draft_api_key) = match draft_provider {
        Some(provider) => {
            let key = provider
                .api_key
                .clone()
                .filter(|value| !value.trim().is_empty());
            (save_provider_into_endpoint_config(provider)?, key)
        }
        None => (provider_for_discovery(state, &provider_id).await?, None),
    };

    // Last-resort catalogues current for this fixes batch as of 2026-08-21.
    let mut models = match provider.protocol {
        swage_config::Protocol::OpenRouter => discover_openrouter_models().await?,
        swage_config::Protocol::OpenAi | swage_config::Protocol::OpenAiResponses => {
            if let Some(base_url) = provider.base_url.as_deref() {
                let api_key = match draft_api_key {
                    Some(key) => Some(key),
                    None => discovery_credential(state, &provider).await,
                };
                discover_openai_compatible_models(base_url, api_key.as_deref()).await?
            } else {
                known_models(&["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"])
            }
        }
        swage_config::Protocol::Anthropic => known_models(&[
            "claude-fable-5",
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-haiku-4-5",
        ]),
        swage_config::Protocol::Google => known_models(&[
            "gemini-3.6-flash",
            "gemini-3.5-flash",
            "gemini-3.5-flash-lite",
            "gemini-3.1-pro-preview",
        ]),
        swage_config::Protocol::Codex => {
            let response = state
                .codex_supervisor
                .client()
                .await
                .map_err(|error| ApiError::internal(error.to_string()))?
                .request("model/list", json!({}))
                .await
                .map_err(|error| ApiError::internal(error.to_string()))?;
            response
                .get("data")
                .or_else(|| response.get("models"))
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|model| {
                    let id = model.get("id")?.as_str()?.to_string();
                    let name = model
                        .get("displayName")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(&id)
                        .to_string();
                    let context_window = model
                        .get("contextWindow")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|value| u32::try_from(value).ok());
                    Some(DiscoveredModel {
                        id,
                        name,
                        context_window,
                    })
                })
                .collect()
        }
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

fn save_provider_into_endpoint_config(
    provider: SaveProviderSettings,
) -> Result<swage_config::ProviderEndpointConfig, ApiError> {
    Ok(swage_config::ProviderEndpointConfig {
        worker_staleness_threshold_ms: provider.worker_staleness_threshold_ms,
        protocol: config_from_wire(provider.protocol)?,
        base_url: provider.base_url,
        api_key_secret_ref: provider.api_key_secret_ref,
        api_key_env: provider.api_key_env,
        command: provider.command,
        state_dir: provider.state_dir,
    })
}

#[cfg(test)]
fn settings_response_from_saved_config(
    state: &AppState,
    default_provider: String,
    providers: &HashMap<String, swage_config::ProviderEndpointConfig>,
    default_model: String,
    models: HashMap<String, ModelSettings>,
    generation_presets: HashMap<String, swage_config::GenerationPresetConfig>,
    tool_permissions: swage_config::ToolPermissionConfig,
    hitl: swage_config::HitlConfig,
    sandbox: swage_config::SandboxConfig,
    context: swage_config::ContextConfig,
) -> SettingsResponse {
    let active_config = state.active_config();
    let providers = providers
        .iter()
        .map(|(id, provider)| {
            (
                id.clone(),
                ProviderSettings {
                    protocol: wire_from_config(provider.protocol.clone()),
                    worker_staleness_threshold_ms: provider.worker_staleness_threshold_ms,
                    base_url: provider.base_url.clone(),
                    api_key_secret_ref: provider.api_key_secret_ref.clone(),
                    api_key_env: provider.api_key_env.clone(),
                    has_credential: provider.api_key_secret_ref.is_some()
                        || provider.api_key_env.is_some(),
                    command: provider.command.clone(),
                    state_dir: provider.state_dir.clone(),
                },
            )
        })
        .collect();

    SettingsResponse {
        providers,
        default_provider,
        models,
        default_model,
        generation_presets: wire_from_config(generation_presets),
        tool_runtime: tool_runtime_from_config(&active_config.tools)
            .expect("test runtime settings convert"),
        tool_permissions: wire_from_config(tool_permissions),
        hitl: wire_from_config(hitl),
        sandbox: wire_from_config(sandbox),
        context: wire_from_config(context),
        pricing: wire_from_config(active_config.pricing),
        observability: wire_from_config(active_config.observability),
        communication: wire_from_config(active_config.communication),
        sessions: wire_from_config(active_config.sessions),
        restart_required: true,
        can_restart: state.managed_gateway,
    }
}

async fn provider_for_discovery(
    state: &AppState,
    provider_id: &str,
) -> Result<swage_config::ProviderEndpointConfig, ApiError> {
    let config = state.active_config();
    if let Some(provider) = config.providers.entries.get(provider_id) {
        return Ok(provider.clone());
    }

    {
        let partial = read_partial_config(&global_config_path(&state.config_root)).await?;
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
        guidance: if !state.restart_required() {
            "Settings are saved and active for new turns.".to_string()
        } else if state.managed_gateway {
            "Settings are saved, but the running gateway is still using the provider/model runtime from startup. Restart the managed gateway to apply them.".to_string()
        } else {
            "Settings are saved, but the running gateway is still using the provider/model runtime from startup. Restart the gateway process manually to apply them.".to_string()
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
    if request.default_model.is_empty() {
        if !request.models.is_empty() {
            return Err(ApiError::validation(
                "default model does not exist".to_string(),
            ));
        }
    } else if !request.models.contains_key(&request.default_model) {
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
    if let Some(tool_permissions) = &request.tool_permissions {
        validate_tool_permissions(tool_permissions)?;
    }
    if let Some(tool_runtime) = &request.tool_runtime {
        validate_tool_runtime_settings(tool_runtime)?;
    }
    if let Some(hitl) = &request.hitl
        && hitl.timeout == 0
    {
        return Err(ApiError::validation(
            "hitl timeout must be positive".to_string(),
        ));
    }
    if let Some(context) = &request.context {
        if context.assembly.trim().is_empty() {
            return Err(ApiError::validation(
                "context assembly must not be empty".to_string(),
            ));
        }
        if !(1..=60).contains(&context.timeout) {
            return Err(ApiError::validation(
                "context timeout must be between 1 and 60".to_string(),
            ));
        }
        if matches!(context.budget, Some(0)) {
            return Err(ApiError::validation(
                "context budget must be positive when set".to_string(),
            ));
        }
    }
    if let Some(communication) = &request.communication
        && (communication.message_ttl_secs == 0 || communication.expiry_interval_secs == 0)
    {
        return Err(ApiError::validation(
            "communication TTL and expiry interval must be positive".to_string(),
        ));
    }
    if let Some(sessions) = &request.sessions
        && sessions.archive_after_days == 0
    {
        return Err(ApiError::validation(
            "sessions archive delay must be at least one day".to_string(),
        ));
    }
    Ok(())
}

fn apply_runtime_settings_to_partial(
    partial: &mut swage_config::PartialConfig,
    tool_runtime: Option<ToolRuntimeSettings>,
    tool_permissions: Option<swage_config::ToolPermissionConfig>,
    hitl: Option<swage_config::HitlConfig>,
    sandbox: Option<swage_config::SandboxConfig>,
    context: Option<swage_config::ContextConfig>,
    pricing: Option<swage_config::PricingConfig>,
    observability: Option<swage_config::ObservabilityConfig>,
    communication: Option<swage_config::CommunicationConfig>,
    sessions: Option<swage_config::SessionsConfig>,
) -> Result<(), ApiError> {
    if let Some(sessions) = sessions {
        partial.sessions = Some(swage_config::merge::PartialSessionsConfig {
            archive_after_days: Some(sessions.archive_after_days),
        });
    }
    if let Some(tool_runtime) = tool_runtime {
        let tools = partial.tools.get_or_insert_with(Default::default);
        tools.shell_timeout_secs = Some(tool_runtime.shell_timeout_secs);
        tools.python_execute = Some(swage_config::merge::PartialPythonExecuteToolConfig {
            enabled: Some(tool_runtime.python_execute.enabled),
            timeout_secs: Some(tool_runtime.python_execute.timeout_secs),
            startup_timeout_secs: Some(tool_runtime.python_execute.startup_timeout_secs),
            idle_ttl_secs: Some(tool_runtime.python_execute.idle_ttl_secs),
            warm_target: Some(tool_runtime.python_execute.warm_target),
            max_workers: Some(tool_runtime.python_execute.max_workers),
            runtime_policy: Some(config_from_wire(
                tool_runtime.python_execute.runtime_policy,
            )?),
            runtime_path: Some(tool_runtime.python_execute.runtime_path),
            require_sandbox: Some(tool_runtime.python_execute.require_sandbox),
        });
        tools.max_output_bytes = Some(tool_runtime.max_output_bytes);
        tools.plugin_directory = Some(tool_runtime.plugin_directory);
        tools.search = Some(config_from_wire(tool_runtime.search)?);
    }
    if let Some(tool_permissions) = tool_permissions {
        let tools = partial.tools.get_or_insert_with(Default::default);
        tools.allow = Some(tool_permissions.allow);
        tools.deny = Some(tool_permissions.deny);
        tools.bundles = Some(tool_permissions.bundles);
        tools.require_approval = Some(tool_permissions.require_approval);
    }
    if let Some(hitl) = hitl {
        partial.hitl = Some(swage_config::merge::PartialHitlConfig {
            timeout: Some(hitl.timeout),
            timeout_policy: Some(hitl.timeout_policy),
        });
    }
    if let Some(sandbox) = sandbox {
        partial.sandbox = Some(swage_config::merge::PartialSandboxConfig {
            enabled: Some(sandbox.enabled),
            allow_degraded_execution: Some(sandbox.allow_degraded_execution),
        });
    }
    if let Some(context) = context {
        partial.context = Some(swage_config::merge::PartialContextConfig {
            assembly: Some(context.assembly),
            alternatives: Some(context.alternatives),
            timeout: Some(context.timeout),
            budget: Some(context.budget),
        });
    }
    if let Some(pricing) = pricing {
        partial.pricing = Some(swage_config::merge::PartialPricingConfig {
            models: Some(pricing.models),
        });
    }
    if let Some(observability) = observability {
        partial.observability = Some(swage_config::merge::PartialObservabilityConfig {
            force_on: Some(observability.force_on),
        });
    }
    if let Some(communication) = communication {
        partial.communication = Some(swage_config::merge::PartialCommunicationConfig {
            context_transfer: Some(swage_config::merge::PartialContextTransferConfig {
                mode: Some(communication.context_transfer.mode),
                allowed_fields: Some(communication.context_transfer.allowed_fields),
                summary_budget: communication.context_transfer.summary_budget,
                targets: if communication.context_transfer.targets.is_empty() {
                    None
                } else {
                    Some(
                        communication
                            .context_transfer
                            .targets
                            .into_iter()
                            .map(|(id, target)| {
                                (
                                    id,
                                    swage_config::merge::PartialContextTransferTargetConfig {
                                        mode: Some(target.mode),
                                        allowed_fields: Some(target.allowed_fields),
                                        summary_budget: target.summary_budget,
                                    },
                                )
                            })
                            .collect(),
                    )
                },
            }),
            message_ttl_secs: Some(communication.message_ttl_secs),
            expiry_interval_secs: Some(communication.expiry_interval_secs),
        });
    }
    Ok(())
}

fn apply_tool_runtime_settings(
    config: &mut swage_config::ToolConfig,
    runtime: ToolRuntimeSettings,
) -> Result<(), ApiError> {
    config.shell_timeout_secs = runtime.shell_timeout_secs;
    config.python_execute = config_from_wire(runtime.python_execute)?;
    config.max_output_bytes = runtime.max_output_bytes;
    config.plugin_directory = runtime.plugin_directory;
    config.search = config_from_wire(runtime.search)?;
    Ok(())
}

fn validate_tool_runtime_settings(runtime: &ToolRuntimeSettings) -> Result<(), ApiError> {
    if runtime.shell_timeout_secs == 0 {
        return Err(ApiError::validation(
            "tool shell timeout must be positive".to_string(),
        ));
    }
    if runtime.max_output_bytes == 0 {
        return Err(ApiError::validation(
            "tool max output bytes must be positive".to_string(),
        ));
    }
    if let Some(directory) = runtime.plugin_directory.as_deref()
        && directory.trim().is_empty()
    {
        return Err(ApiError::validation(
            "tool plugin directory must not be empty".to_string(),
        ));
    }
    if let Some(search) = &runtime.search
        && (search.provider_type.trim().is_empty() || search.base_url.trim().is_empty())
    {
        return Err(ApiError::validation(
            "tool search provider type and base URL must not be empty".to_string(),
        ));
    }

    let python = &runtime.python_execute;
    if python.timeout_secs == 0
        || python.startup_timeout_secs == 0
        || python.idle_ttl_secs == 0
        || python.warm_target == 0
        || python.max_workers == 0
    {
        return Err(ApiError::validation(
            "python_execute timeouts, warm target, and max workers must be positive".to_string(),
        ));
    }
    if let Some(runtime_path) = python.runtime_path.as_deref() {
        if runtime_path.trim().is_empty() {
            return Err(ApiError::validation(
                "python_execute runtime path must not be empty".to_string(),
            ));
        }
        if !PathBuf::from(runtime_path).exists() {
            return Err(ApiError::validation(format!(
                "python_execute runtime path does not exist: {runtime_path}"
            )));
        }
    }
    Ok(())
}

fn validate_tool_permissions(
    permissions: &swage_protocol::client::settings::ToolPermissionConfig,
) -> Result<(), ApiError> {
    validate_named_list("tool allow rules", &permissions.allow)?;
    validate_named_list("tool deny rules", &permissions.deny)?;
    for (bundle, tools) in &permissions.bundles {
        if bundle.trim().is_empty() {
            return Err(ApiError::validation(
                "tool bundle names must not be empty".to_string(),
            ));
        }
        validate_named_list("tool bundle entries", tools)?;
    }
    for entry in &permissions.require_approval {
        if entry.tool_name().trim().is_empty() {
            return Err(ApiError::validation(
                "approval-gated tool names must not be empty".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_named_list(label: &str, values: &[String]) -> Result<(), ApiError> {
    if values.iter().any(|value| value.trim().is_empty()) {
        return Err(ApiError::validation(format!(
            "{label} must not contain empty entries"
        )));
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

fn global_config_path(config_root: &std::path::Path) -> PathBuf {
    config_root.join("config.toml")
}

async fn read_partial_config(
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

/// Resolve the credential a discovery request should present, mirroring the
/// runtime order: governed secret reference first, then the legacy
/// environment-variable fallback. Discovery proceeds unauthenticated when
/// neither resolves — the endpoint's own response reports whether it minds.
async fn discovery_credential(
    state: &AppState,
    provider: &swage_config::ProviderEndpointConfig,
) -> Option<String> {
    if let Some(reference) = provider.api_key_secret_ref.as_deref() {
        match state.secret_manager.resolve(reference).await {
            Ok(resolved) => return Some(resolved.value),
            Err(error) => {
                tracing::warn!("model discovery could not resolve {reference}: {error}");
            }
        }
    }
    if let Some(api_key_env) = provider.api_key_env.as_deref()
        && let Ok(value) = std::env::var(api_key_env)
    {
        return Some(value);
    }
    None
}

async fn discover_openai_compatible_models(
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<DiscoveredModel>, ApiError> {
    let client = reqwest::Client::new();
    let mut last_error = None;

    for url in openai_compatible_model_urls(base_url) {
        let mut request = client.get(&url);
        if let Some(api_key) = api_key {
            request = request.bearer_auth(api_key);
        }
        let response = match request.send().await {
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
    use crate::test_support::SwageHomeGuard;

    #[tokio::test(flavor = "multi_thread")]
    async fn atomic_config_write_never_exposes_partial_content() {
        let directory = tempfile::TempDir::new().expect("temporary config directory");
        let path = directory.path().join("config.toml");
        let old = format!("value = \"{}\"\n", "a".repeat(512 * 1024));
        let new = format!("value = \"{}\"\n", "b".repeat(512 * 1024));
        std::fs::write(&path, &old).expect("seed old config");

        let writer_path = path.clone();
        let writer_old = old.clone();
        let writer_new = new.clone();
        let writer = tokio::spawn(async move {
            for iteration in 0..32 {
                let body = if iteration % 2 == 0 {
                    &writer_new
                } else {
                    &writer_old
                };
                write_config_atomically(&writer_path, body)
                    .await
                    .expect("atomic config write");
                tokio::task::yield_now().await;
            }
        });

        while !writer.is_finished() {
            let observed = tokio::fs::read_to_string(&path)
                .await
                .expect("read config during replacement");
            assert!(
                observed == old || observed == new,
                "reader observed neither complete config ({} bytes)",
                observed.len()
            );
            tokio::task::yield_now().await;
        }
        writer.await.expect("writer task");

        let entries = std::fs::read_dir(directory.path())
            .expect("list config directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read config entries");
        assert_eq!(entries.len(), 1, "temporary config files should be removed");
        assert_eq!(entries[0].path(), path);
    }

    use serde_json::json;

    use super::*;

    fn assert_bidirectional_settings_bridge<C, W>(value: serde_json::Value)
    where
        C: DeserializeOwned + Serialize,
        W: DeserializeOwned + Serialize,
    {
        let config: C = serde_json::from_value(value.clone()).expect("fully populated config");
        let wire: W = try_wire_from_config(config).expect("config converts to wire");
        assert_eq!(serde_json::to_value(&wire).unwrap(), value);
        let config: C = config_from_wire(wire).expect("wire converts to config");
        assert_eq!(serde_json::to_value(config).unwrap(), value);
    }

    #[test]
    fn duplicated_settings_shapes_round_trip_bidirectionally() {
        use swage_protocol::client::settings as wire;

        assert_bidirectional_settings_bridge::<
            swage_config::PythonExecuteToolConfig,
            wire::PythonExecuteToolConfig,
        >(
            json!({"enabled": true, "timeout_secs": 31, "startup_timeout_secs": 11, "idle_ttl_secs": 61, "warm_target": 2, "max_workers": 3, "runtime_policy": "bundled_then_system", "runtime_path": "/opt/python", "require_sandbox": true}),
        );
        assert_bidirectional_settings_bridge::<
            swage_config::SearchProviderConfig,
            wire::SearchProviderConfig,
        >(
            json!({"provider_type": "searxng", "base_url": "https://search.invalid", "api_key": "secret-ref"}),
        );
        assert_bidirectional_settings_bridge::<
            swage_config::ToolPermissionConfig,
            wire::ToolPermissionConfig,
        >(
            json!({"allow": ["file_read"], "deny": ["shell"], "bundles": {"inspect": ["file_read"]}, "require_approval": [{"tool": "python_execute", "timeout": 120, "timeout_policy": "interrupt"}]}),
        );
        assert_bidirectional_settings_bridge::<swage_config::HitlConfig, wire::HitlConfig>(
            json!({"timeout": 600, "timeout_policy": "interrupt"}),
        );
        assert_bidirectional_settings_bridge::<
            swage_config::CommunicationConfig,
            wire::CommunicationConfig,
        >(
            json!({"context_transfer": {"mode": "explicit", "allowed_fields": ["system", "user", "assistant"], "summary_budget": 512, "targets": {"reviewer": {"mode": "summary", "allowed_fields": ["user"], "summary_budget": 256}}}, "message_ttl_secs": 7200, "expiry_interval_secs": 30}),
        );
    }

    #[test]
    fn settings_wire_shapes_reject_unknown_nested_fields() {
        use swage_protocol::client::settings as wire;

        let python_execute = json!({
            "enabled": true,
            "timeout_secs": 30,
            "startup_timeout_secs": 10,
            "idle_ttl_secs": 60,
            "warm_target": 1,
            "max_workers": 2,
            "runtime_policy": "bundled_then_system",
            "runtime_path": null,
            "require_sandbox": true,
            "timeout_second": 30
        });
        assert!(serde_json::from_value::<wire::PythonExecuteToolConfig>(python_execute).is_err());

        let search = json!({
            "provider_type": "searxng",
            "base_url": "https://search.invalid",
            "api_key": null,
            "endpoint": "https://ignored.invalid"
        });
        assert!(serde_json::from_value::<wire::SearchProviderConfig>(search).is_err());

        let hitl = json!({
            "timeout": 300,
            "timeout_policy": "reject",
            "auto_approve": true
        });
        assert!(serde_json::from_value::<wire::HitlConfig>(hitl).is_err());

        let approval = json!({
            "allow": [],
            "deny": [],
            "bundles": {},
            "require_approval": [{
                "tool": "shell",
                "timeout": 60,
                "timeout_polciy": "interrupt"
            }]
        });
        assert!(serde_json::from_value::<wire::ToolPermissionConfig>(approval).is_err());

        let context_transfer = json!({
            "mode": "explicit",
            "allowed_fields": ["user"],
            "summary_budget": 512,
            "targets": {},
            "typo": true
        });
        assert!(serde_json::from_value::<wire::ContextTransferConfig>(context_transfer).is_err());

        let target = json!({
            "mode": "summary",
            "allowed_fields": ["user"],
            "summary_budget": 256,
            "typo": true
        });
        assert!(serde_json::from_value::<wire::ContextTransferTargetConfig>(target).is_err());
    }

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
        let storage = std::sync::Arc::new(crate::turn::tests::MockStorage::new());
        let state = AppState::new(
            storage.clone() as std::sync::Arc<dyn swage_storage::store::Storage>,
            storage as std::sync::Arc<dyn swage_storage::store::TopicStore>,
            std::sync::Arc::new(crate::turn::tests::MockProvider::new(vec![]))
                as std::sync::Arc<dyn swage_provider::LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let mut providers = HashMap::new();
        providers.insert(
            "llama-cpp".to_string(),
            swage_config::ProviderEndpointConfig {
                worker_staleness_threshold_ms: None,
                protocol: swage_config::Protocol::OpenAi,
                base_url: Some("http://127.0.0.1:8080/v1".to_string()),
                api_key_secret_ref: Some("secret:abc".to_string()),
                api_key_env: None,
                command: None,
                state_dir: None,
            },
        );

        let response = settings_response_from_saved_config(
            &state,
            "llama-cpp".to_string(),
            &providers,
            String::new(),
            HashMap::new(),
            HashMap::new(),
            wire_from_config(swage_config::ToolPermissionConfig::default()),
            wire_from_config(swage_config::HitlConfig::default()),
            wire_from_config(swage_config::SandboxConfig::default()),
            wire_from_config(swage_config::ContextConfig::default()),
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

    #[tokio::test(flavor = "multi_thread")]
    async fn save_settings_hot_applies_model_runtime_without_restart() {
        let swage_home = SwageHomeGuard::new();

        let storage = std::sync::Arc::new(crate::turn::tests::MockStorage::new());
        let mut initial_config = swage_config::ResolvedConfig::default();
        initial_config.models.default = "local:fast".to_string();
        initial_config.models.entries.insert(
            "local:fast".to_string(),
            swage_config::ModelEndpointConfig {
                provider_id: "local".to_string(),
                model_id: "fast-wire".to_string(),
                aliases: vec![],
                context_window: Some(4096),
                generation_preset: Some("quick".to_string()),
                default_effort: Some(swage_protocol::client::settings::Effort::Low),
                capabilities: swage_config::ModelCapabilitiesConfig {
                    reasoning_replay: true,
                    effort: None,
                },
            },
        );
        let state = AppState::new(
            storage.clone() as std::sync::Arc<dyn swage_storage::store::Storage>,
            storage as std::sync::Arc<dyn swage_storage::store::TopicStore>,
            std::sync::Arc::new(crate::turn::tests::MockProvider::new(vec![]))
                as std::sync::Arc<dyn swage_provider::LlmProvider>,
            initial_config,
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );

        let request = SaveSettingsRequest {
            default_provider: "local".to_string(),
            providers: [(
                "local".to_string(),
                SaveProviderSettings {
                    protocol: swage_protocol::client::settings::ProviderProtocol::OpenAi,
                    worker_staleness_threshold_ms: None,
                    base_url: Some("http://127.0.0.1:65535/v1".to_string()),
                    api_key_secret_ref: None,
                    api_key_env: None,
                    api_key: None,
                    command: None,
                    state_dir: None,
                },
            )]
            .into_iter()
            .collect(),
            default_model: "local:fast".to_string(),
            models: [(
                "local:fast".to_string(),
                ModelSettings {
                    provider_id: "local".to_string(),
                    model_id: "fast-wire".to_string(),
                    aliases: vec![],
                    context_window: Some(4096),
                    generation_preset: Some("quick".to_string()),
                    default_effort: Some(swage_protocol::client::settings::Effort::High),
                    capabilities: swage_protocol::client::settings::ModelCapabilitiesConfig {
                        reasoning_replay: true,
                        effort: None,
                    },
                },
            )]
            .into_iter()
            .collect(),
            generation_presets: [(
                "quick".to_string(),
                swage_protocol::client::settings::GenerationPresetConfig {
                    max_tokens: Some(123),
                    temperature: Some(0.2),
                    stop_sequences: vec![],
                    metadata: None,
                },
            )]
            .into_iter()
            .collect(),
            tool_runtime: None,
            tool_permissions: Some(swage_protocol::client::settings::ToolPermissionConfig {
                allow: vec!["file_read".to_string()],
                deny: vec!["shell".to_string()],
                bundles: HashMap::new(),
                require_approval: vec![],
            }),
            hitl: Some(wire_from_config(swage_config::HitlConfig::default())),
            sandbox: Some(swage_protocol::client::settings::SandboxConfig {
                enabled: false,
                allow_degraded_execution: true,
            }),
            context: Some(swage_protocol::client::settings::ContextConfig {
                assembly: "default".to_string(),
                alternatives: Vec::new(),
                timeout: 7,
                budget: Some(2048),
            }),
            pricing: None,
            observability: None,
            communication: None,
            sessions: None,
        };

        let response = save_settings(&state, request)
            .await
            .expect("settings should save and hot-apply");

        assert!(!response.restart_required);
        assert_eq!(response.default_model, "local:fast");
        assert_eq!(state.active_config().default_model_id(), "fast-wire");
        assert!(
            state.active_config().models.entries["local:fast"]
                .capabilities
                .reasoning_replay
        );
        assert_eq!(state.active_config().context.timeout, 7);
        assert_eq!(state.active_config().tools.permissions.deny, vec!["shell"]);
        assert!(!state.restart_required());
        let events = state.storage.list_audit_events().await.unwrap();
        let changes: Vec<_> = events
            .iter()
            .filter(|event| event.event_kind == "configuration_change")
            .collect();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].entity_kind, "settings");
        assert_eq!(changes[0].metadata["action"], "save_settings");
        assert_eq!(
            changes[0].metadata["prior_state"]["models"]["local:fast"]["default_effort"],
            "low"
        );
        assert_eq!(
            changes[0].metadata["new_state"]["models"]["local:fast"]["default_effort"],
            "high"
        );
        assert_ne!(
            changes[0].metadata["prior_state"]["models"]["local:fast"]["default_effort"],
            changes[0].metadata["new_state"]["models"]["local:fast"]["default_effort"]
        );
        assert_eq!(changes[0].metadata["new_state"]["context"]["timeout"], 7);
        assert_eq!(
            changes[0].metadata["new_state"]["tool_permissions"]["deny"][0],
            "shell"
        );
        assert!(changes[0].metadata["prior_state"]["providers"].is_object());

        let reloaded = swage_config::load_resolved_config_from(
            swage_home.path(),
            &ScopeLocation::General,
            "default",
        )
        .expect("saved settings should reload from durable configuration");
        assert_eq!(
            reloaded.models.entries["local:fast"].default_effort,
            Some(swage_protocol::client::settings::Effort::High)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn saving_settings_keeps_each_route_staleness_threshold() {
        let swage_home = SwageHomeGuard::new();

        let storage = std::sync::Arc::new(crate::turn::tests::MockStorage::new());
        let mut initial_config = swage_config::ResolvedConfig::default();
        initial_config.providers.default = "patient".to_string();
        for (id, threshold) in [("patient", Some(600_000u64)), ("unset", None)] {
            initial_config.providers.entries.insert(
                id.to_string(),
                swage_config::ProviderEndpointConfig {
                    worker_staleness_threshold_ms: threshold,
                    protocol: swage_config::Protocol::OpenAi,
                    base_url: Some("http://127.0.0.1:65535/v1".to_string()),
                    api_key_secret_ref: None,
                    api_key_env: None,
                    command: None,
                    state_dir: None,
                },
            );
        }
        let state = AppState::new(
            storage.clone() as std::sync::Arc<dyn swage_storage::store::Storage>,
            storage as std::sync::Arc<dyn swage_storage::store::TopicStore>,
            std::sync::Arc::new(crate::turn::tests::MockProvider::new(vec![]))
                as std::sync::Arc<dyn swage_provider::LlmProvider>,
            initial_config,
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );

        let loaded = load_settings(&state).await.expect("load settings");
        assert_eq!(
            loaded.providers["patient"].worker_staleness_threshold_ms,
            Some(600_000),
            "the configured route threshold reaches the settings surface"
        );
        assert_eq!(
            loaded.providers["unset"].worker_staleness_threshold_ms,
            None
        );

        let providers = loaded
            .providers
            .iter()
            .map(|(id, provider)| {
                (
                    id.clone(),
                    SaveProviderSettings {
                        protocol: provider.protocol.clone(),
                        worker_staleness_threshold_ms: provider.worker_staleness_threshold_ms,
                        base_url: provider.base_url.clone(),
                        api_key_secret_ref: provider.api_key_secret_ref.clone(),
                        api_key_env: provider.api_key_env.clone(),
                        api_key: None,
                        command: provider.command.clone(),
                        state_dir: provider.state_dir.clone(),
                    },
                )
            })
            .collect();
        let request = SaveSettingsRequest {
            default_provider: "patient".to_string(),
            providers,
            default_model: String::new(),
            models: HashMap::new(),
            generation_presets: HashMap::new(),
            tool_runtime: None,
            tool_permissions: None,
            hitl: None,
            sandbox: None,
            context: None,
            pricing: None,
            observability: None,
            communication: None,
            sessions: None,
        };
        save_settings(&state, request)
            .await
            .expect("an unrelated settings save succeeds");

        assert_eq!(
            state.active_config().providers.entries["patient"].worker_staleness_threshold_ms,
            Some(600_000),
            "a settings save must not erase a configured route threshold"
        );
        let reloaded = swage_config::load_resolved_config_from(
            swage_home.path(),
            &ScopeLocation::General,
            "default",
        )
        .expect("saved settings reload from durable configuration");
        assert_eq!(
            reloaded.providers.entries["patient"].worker_staleness_threshold_ms,
            Some(600_000)
        );
        assert_eq!(
            reloaded.providers.entries["unset"].worker_staleness_threshold_ms,
            None
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn save_settings_hot_rebuilds_tool_runtime_without_restart() {
        let swage_home = SwageHomeGuard::new();

        let plugin_dir = swage_home.path().join("plugins").join("echo-plugin");
        std::fs::create_dir_all(&plugin_dir).expect("plugin directory");
        std::fs::write(
            plugin_dir.join("capability.json"),
            serde_json::json!({
                "description": "Echo plugin",
                "tool": {
                    "name": "echo_plugin",
                    "description": "Echo plugin",
                    "parameters": {"type": "object", "properties": {"text": {"type": "string"}}},
                    "executable": "run.py"
                }
            })
            .to_string(),
        )
        .expect("plugin manifest");
        std::fs::write(
            plugin_dir.join("run.py"),
            "#!/usr/bin/env python3\nprint('{}')\n",
        )
        .expect("plugin executable");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(plugin_dir.join("run.py"))
                .expect("plugin metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(plugin_dir.join("run.py"), permissions)
                .expect("plugin executable permissions");
        }

        let storage = std::sync::Arc::new(crate::turn::tests::MockStorage::new());
        let mut config = swage_config::ResolvedConfig::default();
        config.tools.search = Some(swage_config::SearchProviderConfig {
            provider_type: "searxng".to_string(),
            base_url: "http://127.0.0.1:8080".to_string(),
            api_key: Some("old-search-key".into()),
        });
        let state = AppState::new(
            storage.clone() as std::sync::Arc<dyn swage_storage::store::Storage>,
            storage as std::sync::Arc<dyn swage_storage::store::TopicStore>,
            std::sync::Arc::new(crate::turn::tests::MockProvider::new(vec![]))
                as std::sync::Arc<dyn swage_provider::LlmProvider>,
            config,
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        assert!(
            state
                .active_tool_dispatcher()
                .registry()
                .get("echo_plugin")
                .is_none()
        );

        let mut python_execute: swage_protocol::client::settings::PythonExecuteToolConfig =
            wire_from_config(swage_config::PythonExecuteToolConfig::default());
        python_execute.enabled = false;
        let plugin_root = swage_home.path().join("plugins").display().to_string();
        let request = SaveSettingsRequest {
            default_provider: "local".to_string(),
            providers: [(
                "local".to_string(),
                SaveProviderSettings {
                    protocol: swage_protocol::client::settings::ProviderProtocol::OpenAi,
                    worker_staleness_threshold_ms: None,
                    base_url: Some("http://127.0.0.1:65535/v1".to_string()),
                    api_key_secret_ref: None,
                    api_key_env: None,
                    api_key: None,
                    command: None,
                    state_dir: None,
                },
            )]
            .into_iter()
            .collect(),
            default_model: "local:fast".to_string(),
            models: [(
                "local:fast".to_string(),
                ModelSettings {
                    provider_id: "local".to_string(),
                    model_id: "fast-wire".to_string(),
                    aliases: vec![],
                    context_window: None,
                    generation_preset: None,
                    default_effort: None,
                    capabilities:
                        swage_protocol::client::settings::ModelCapabilitiesConfig::default(),
                },
            )]
            .into_iter()
            .collect(),
            generation_presets: HashMap::new(),
            tool_runtime: Some(ToolRuntimeSettings {
                shell_timeout_secs: 2,
                python_execute,
                max_output_bytes: 12_345,
                plugin_directory: Some(plugin_root.clone()),
                search: Some(swage_protocol::client::settings::SearchProviderConfig {
                    provider_type: "searxng".to_string(),
                    base_url: "http://127.0.0.1:8080".to_string(),
                    api_key: Some("new-search-key".to_string()),
                }),
            }),
            tool_permissions: None,
            hitl: None,
            sandbox: None,
            context: None,
            pricing: None,
            observability: None,
            communication: None,
            sessions: None,
        };

        let response = save_settings(&state, request)
            .await
            .expect("settings should save and hot-rebuild tools");

        assert!(!response.restart_required);
        assert_eq!(response.tool_runtime.shell_timeout_secs, 2);
        assert_eq!(state.active_config().tools.shell_timeout_secs, 2);
        assert_eq!(state.active_config().tools.max_output_bytes, 12_345);
        assert_eq!(
            state.active_config().tools.plugin_directory.as_deref(),
            Some(plugin_root.as_str())
        );
        assert!(
            state
                .active_tool_dispatcher()
                .registry()
                .get("echo_plugin")
                .is_some(),
            "hot-rebuilt dispatcher should include tools from the saved plugin directory"
        );
        assert!(!state.restart_required());
        let events = state.storage.list_audit_events().await.unwrap();
        let change = events
            .iter()
            .find(|event| event.event_kind == "configuration_change")
            .expect("settings configuration change");
        assert_eq!(
            change.metadata["prior_state"]["tool_runtime"]["search"]["api_key"],
            "[REDACTED]"
        );
        assert_eq!(
            change.metadata["new_state"]["tool_runtime"]["search"]["api_key"],
            "[REDACTED]"
        );
        let audit_json = serde_json::to_string(&change.metadata).unwrap();
        assert!(!audit_json.contains("old-search-key"));
        assert!(!audit_json.contains("new-search-key"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn save_settings_hot_applies_observability_pricing_and_communication() {
        let _swage_home = SwageHomeGuard::new();

        let storage = std::sync::Arc::new(crate::turn::tests::MockStorage::new());
        let state = AppState::new(
            storage.clone() as std::sync::Arc<dyn swage_storage::store::Storage>,
            storage as std::sync::Arc<dyn swage_storage::store::TopicStore>,
            std::sync::Arc::new(crate::turn::tests::MockProvider::new(vec![]))
                as std::sync::Arc<dyn swage_provider::LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        state.start_agent_message_expiry();

        let pricing = swage_config::PricingConfig {
            models: [(
                "fast-wire".to_string(),
                swage_config::ModelPricing {
                    input_price_per_mtok: 10,
                    output_price_per_mtok: 25,
                },
            )]
            .into_iter()
            .collect(),
        };
        let observability = swage_config::ObservabilityConfig { force_on: true };
        let communication = swage_config::CommunicationConfig {
            context_transfer: swage_config::ContextTransferConfig {
                mode: swage_config::ContextTransferMode::Explicit,
                allowed_fields: vec![swage_config::ContextTransferField::User],
                summary_budget: None,
                targets: HashMap::new(),
            },
            message_ttl_secs: 42,
            expiry_interval_secs: 3,
        };

        let request = SaveSettingsRequest {
            default_provider: "local".to_string(),
            providers: [(
                "local".to_string(),
                SaveProviderSettings {
                    protocol: swage_protocol::client::settings::ProviderProtocol::OpenAi,
                    worker_staleness_threshold_ms: None,
                    base_url: Some("http://127.0.0.1:65535/v1".to_string()),
                    api_key_secret_ref: None,
                    api_key_env: None,
                    api_key: None,
                    command: None,
                    state_dir: None,
                },
            )]
            .into_iter()
            .collect(),
            default_model: "local:fast".to_string(),
            models: [(
                "local:fast".to_string(),
                ModelSettings {
                    provider_id: "local".to_string(),
                    model_id: "fast-wire".to_string(),
                    aliases: vec![],
                    context_window: None,
                    generation_preset: None,
                    default_effort: None,
                    capabilities:
                        swage_protocol::client::settings::ModelCapabilitiesConfig::default(),
                },
            )]
            .into_iter()
            .collect(),
            generation_presets: HashMap::new(),
            tool_runtime: None,
            tool_permissions: None,
            hitl: None,
            sandbox: None,
            context: None,
            pricing: Some(wire_from_config(pricing)),
            observability: Some(wire_from_config(observability)),
            communication: Some(wire_from_config(communication.clone())),
            sessions: None,
        };

        let response = save_settings(&state, request)
            .await
            .expect("settings should hot-apply observability and communication");

        assert!(response.observability.force_on);
        assert_eq!(
            state
                .active_config()
                .pricing
                .compute_cost("fast-wire", 1_000_000, 2_000_000),
            Some(60)
        );
        assert_eq!(state.active_config().communication, communication);
        assert_eq!(
            state.active_agent_message_expiry_communication(),
            Some(communication)
        );
        assert!(!state.restart_required());
        state.stop_agent_message_expiry();
    }

    #[test]
    fn validate_settings_rejects_invalid_runtime_controls() {
        let mut request = SaveSettingsRequest {
            default_provider: String::new(),
            providers: HashMap::new(),
            default_model: String::new(),
            models: HashMap::new(),
            generation_presets: HashMap::new(),
            tool_runtime: None,
            tool_permissions: Some(swage_protocol::client::settings::ToolPermissionConfig {
                allow: vec!["*".into()],
                deny: vec!["".into()],
                bundles: HashMap::new(),
                require_approval: vec![],
            }),
            hitl: Some(wire_from_config(swage_config::HitlConfig::default())),
            sandbox: Some(wire_from_config(swage_config::SandboxConfig::default())),
            context: Some(wire_from_config(swage_config::ContextConfig::default())),
            pricing: None,
            observability: None,
            communication: None,
            sessions: None,
        };

        let error = validate_settings_request(&request).expect_err("empty deny rule is invalid");
        assert!(error.message().contains("tool deny rules"));

        request.tool_permissions = Some(wire_from_config(
            swage_config::ToolPermissionConfig::default(),
        ));
        request.hitl = Some(swage_protocol::client::settings::HitlConfig {
            timeout: 0,
            timeout_policy: swage_protocol::client::settings::ApprovalTimeoutPolicy::Reject,
        });
        let error = validate_settings_request(&request).expect_err("zero HITL timeout is invalid");
        assert!(error.message().contains("hitl timeout"));

        request.hitl = Some(wire_from_config(swage_config::HitlConfig::default()));
        request.context = Some(swage_protocol::client::settings::ContextConfig {
            assembly: "default".into(),
            alternatives: Vec::new(),
            timeout: 61,
            budget: None,
        });
        let error = validate_settings_request(&request)
            .expect_err("context timeout above range is invalid");
        assert!(error.message().contains("context timeout"));

        request.context = Some(wire_from_config(swage_config::ContextConfig::default()));
        request.tool_runtime = Some(ToolRuntimeSettings {
            shell_timeout_secs: 0,
            python_execute: wire_from_config(swage_config::PythonExecuteToolConfig::default()),
            max_output_bytes: 100_000,
            plugin_directory: None,
            search: None,
        });
        let error = validate_settings_request(&request).expect_err("zero shell timeout is invalid");
        assert!(error.message().contains("shell timeout"));

        let mut python_execute: swage_protocol::client::settings::PythonExecuteToolConfig =
            wire_from_config(swage_config::PythonExecuteToolConfig::default());
        python_execute.runtime_path = Some("/definitely/missing/python".to_string());
        request.tool_runtime = Some(ToolRuntimeSettings {
            shell_timeout_secs: 30,
            python_execute,
            max_output_bytes: 100_000,
            plugin_directory: None,
            search: None,
        });
        let error =
            validate_settings_request(&request).expect_err("missing python runtime is invalid");
        assert!(error.message().contains("runtime path does not exist"));

        request.tool_runtime = Some(ToolRuntimeSettings {
            shell_timeout_secs: 30,
            python_execute: wire_from_config(swage_config::PythonExecuteToolConfig::default()),
            max_output_bytes: 100_000,
            plugin_directory: Some("/definitely/missing/plugins".to_string()),
            search: None,
        });
        validate_settings_request(&request)
            .expect("missing plugin directory should not block unrelated settings saves");
    }

    #[test]
    fn runtime_settings_are_written_to_partial_config() {
        let mut partial = swage_config::PartialConfig::default();
        let mut bundles = HashMap::new();
        bundles.insert("dangerous".to_string(), vec!["shell".to_string()]);
        let permissions = swage_config::ToolPermissionConfig {
            allow: vec!["file_read".into()],
            deny: vec!["shell".into()],
            bundles,
            require_approval: vec![swage_config::RequireApprovalEntry::WithOptions {
                tool: "python_execute".into(),
                timeout: Some(120),
                timeout_policy: Some(swage_core::ApprovalTimeoutPolicy::Interrupt),
            }],
        };

        apply_runtime_settings_to_partial(
            &mut partial,
            None,
            Some(permissions),
            Some(swage_config::HitlConfig {
                timeout: 600,
                timeout_policy: swage_core::ApprovalTimeoutPolicy::Interrupt,
            }),
            Some(swage_config::SandboxConfig {
                enabled: false,
                allow_degraded_execution: true,
            }),
            Some(swage_config::ContextConfig {
                assembly: "compact".into(),
                alternatives: Vec::new(),
                timeout: 10,
                budget: Some(8192),
            }),
            None,
            None,
            None,
            None,
        )
        .expect("runtime settings convert");

        let body = toml::to_string(&partial).expect("partial config should serialise");
        assert!(body.contains("[tools]"));
        assert!(body.contains("allow = [\"file_read\"]"));
        assert!(body.contains("deny = [\"shell\"]"));
        assert!(body.contains("[hitl]"));
        assert!(body.contains("timeout = 600"));
        assert!(body.contains("timeout_policy = \"interrupt\""));
        assert!(body.contains("[sandbox]"));
        assert!(body.contains("enabled = false"));
        assert!(body.contains("[context]"));
        assert!(body.contains("assembly = \"compact\""));
        assert!(body.contains("budget = 8192"));
    }
}
