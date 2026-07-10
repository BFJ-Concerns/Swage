use super::*;

pub(crate) fn build_agent_config_with_workspace(
    agent_id: String,
    config: &swage_config::ResolvedConfig,
    output_schema_override: Option<serde_json::Value>,
    workspace_root: &Path,
    allowed_paths: Vec<PathBuf>,
) -> AgentConfig {
    let mut agent_config = AgentConfig::new(agent_id).with_workspace(workspace_root, allowed_paths);
    agent_config.skip_permissions = config.dangerously_skip_permissions;
    if !config.default_model_id().is_empty() {
        agent_config.model_id = Some(config.default_model_id().to_string());
    }
    if let Some(schema) = output_schema_override.or_else(|| config.output.schema.clone()) {
        agent_config = agent_config.with_output_schema(schema, config.output.max_retries);
    }
    if let Some(model) = config.models.default_model() {
        agent_config.replay_reasoning = model.capabilities.reasoning_replay;
        if let Some(preset_id) = model.generation_preset.as_deref()
            && let Some(preset) = config.generation.presets.get(preset_id)
        {
            agent_config.max_tokens = preset.max_tokens;
            agent_config.temperature = preset.temperature;
            agent_config.stop_sequences = if preset.stop_sequences.is_empty() {
                None
            } else {
                Some(preset.stop_sequences.clone())
            };
            agent_config.generation_metadata = preset.metadata.clone();
        }
    }
    agent_config
}

pub(crate) async fn build_agent_config_for_bench_workspace(
    state: &AppState,
    agent_id: String,
    config: &swage_config::ResolvedConfig,
    output_schema_override: Option<serde_json::Value>,
    bench_id: Uuid,
    session_id: Uuid,
    active_workspace_attachment_id: Option<Uuid>,
) -> AgentConfig {
    let workspace_policy =
        load_trusted_workspace_policy(&state.storage, bench_id, active_workspace_attachment_id)
            .await;
    let (workspace_root, allowed_paths) =
        agent_workspace_from_policy(&workspace_policy, bench_id, session_id);
    build_agent_config_with_workspace(
        agent_id,
        config,
        output_schema_override,
        &workspace_root,
        allowed_paths,
    )
}

pub(super) fn managed_scratch_workspace(bench_id: Uuid, session_id: Uuid) -> PathBuf {
    let base = swage_config::discovery::resolve_root()
        .unwrap_or_else(|_| swage_config::discovery::effective_root_path());
    base.join("scratch")
        .join("benches")
        .join(bench_id.to_string())
        .join("sessions")
        .join(session_id.to_string())
}

pub(super) fn agent_workspace_from_policy(
    policy: &TrustedWorkspacePolicy,
    bench_id: Uuid,
    session_id: Uuid,
) -> (PathBuf, Vec<PathBuf>) {
    let workspace_root = policy
        .active_workspace
        .as_ref()
        .map(|grant| grant.path.clone())
        .unwrap_or_else(|| managed_scratch_workspace(bench_id, session_id));
    if let Err(error) = std::fs::create_dir_all(&workspace_root) {
        tracing::warn!(
            path = %workspace_root.display(),
            %error,
            "failed to create managed turn workspace"
        );
    }

    let mut allowed_paths = vec![workspace_root.clone()];
    for grant in &policy.attachments {
        if grant.can_read || grant.can_write {
            allowed_paths.push(grant.path.clone());
        }
    }
    (workspace_root, allowed_paths)
}

pub(super) fn append_approved_location_paths(
    allowed_paths: &mut Vec<PathBuf>,
    grants: &std::collections::HashSet<crate::state::ApprovalGrantKey>,
    bench_id: Uuid,
) {
    for grant in grants
        .iter()
        .filter(|grant| grant.kind == swage_storage::model::ApprovalKind::Location)
        .filter(|grant| grant.bench_id.is_none() || grant.bench_id == Some(bench_id))
    {
        let path = PathBuf::from(&grant.name);
        if !allowed_paths.contains(&path) {
            allowed_paths.push(path);
        }
    }
}

pub(crate) fn load_session_config(
    state: &AppState,
    bench_slug: &str,
    agent_id: &str,
) -> Result<swage_config::ResolvedConfig, GatewayError> {
    let Ok(root) = swage_config::discovery::resolve_root() else {
        return Ok(state.active_config());
    };

    let bench_dir = root.join("benches").join(bench_slug);
    let bench_config_exists = bench_dir.join("config.toml").exists();
    let agent_config_exists = bench_dir
        .join("agents")
        .join(agent_id)
        .join("config.toml")
        .exists();

    if !bench_config_exists && !agent_config_exists {
        return Ok(state.active_config());
    }

    swage_config::load_resolved_config_from(&root, bench_slug, agent_id)
        .map_err(|error| GatewayError::ConfigError(error.to_string()))
}

pub(super) fn effective_session_config(
    mut config: swage_config::ResolvedConfig,
    selected_model_id: Option<&str>,
) -> swage_config::ResolvedConfig {
    let Some(selected_model_id) = selected_model_id.filter(|model_id| !model_id.trim().is_empty())
    else {
        return config;
    };

    if config.models.entries.contains_key(selected_model_id) {
        config.models.default = selected_model_id.to_string();
    } else {
        tracing::warn!(
            selected_model_id,
            default_model_id = %config.models.default,
            "Conversation selected an unknown model; falling back to the configured default"
        );
    }
    config
}

pub(super) fn ensure_selected_model_available(
    state: &AppState,
    config: &swage_config::ResolvedConfig,
    selected_model_id: Option<&str>,
    session_id: Uuid,
) -> Result<(), GatewayError> {
    let Some(selected_model_id) = selected_model_id.filter(|model_id| !model_id.trim().is_empty())
    else {
        return Ok(());
    };

    if config.models.entries.contains_key(selected_model_id) {
        return Ok(());
    }

    if state.restart_required() {
        return Err(GatewayError::ConfigError(format!(
            "Conversation {session_id} selected model '{selected_model_id}', but the running gateway has not loaded that model. Restart the gateway to apply the saved provider/model settings, or choose a model that existed before the last settings save."
        )));
    }

    Ok(())
}
