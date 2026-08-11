use super::{
    AgentConfig, AppState, GatewayError, Path, PathBuf, TrustedWorkspacePolicy, Uuid,
    load_trusted_workspace_policy,
};

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

pub(crate) async fn build_agent_config_for_space_workspace(
    state: &AppState,
    agent_id: String,
    config: &swage_config::ResolvedConfig,
    output_schema_override: Option<serde_json::Value>,
    space_id: Uuid,
    session_id: Uuid,
    active_workspace_attachment_id: Option<Uuid>,
) -> AgentConfig {
    let workspace_policy =
        load_trusted_workspace_policy(&state.storage, space_id, active_workspace_attachment_id)
            .await;
    let (workspace_root, allowed_paths) =
        agent_workspace_from_policy(&workspace_policy, space_id, session_id);
    build_agent_config_with_workspace(
        agent_id,
        config,
        output_schema_override,
        &workspace_root,
        allowed_paths,
    )
}

pub(super) fn managed_scratch_workspace(space_id: Uuid, session_id: Uuid) -> PathBuf {
    let base = swage_config::discovery::resolve_root()
        .unwrap_or_else(|_| swage_config::discovery::effective_root_path());
    base.join("scratch")
        .join("spaces")
        .join(space_id.to_string())
        .join("sessions")
        .join(session_id.to_string())
}

pub(super) fn agent_workspace_from_policy(
    policy: &TrustedWorkspacePolicy,
    space_id: Uuid,
    session_id: Uuid,
) -> (PathBuf, Vec<PathBuf>) {
    let workspace_root = policy
        .active_workspace
        .as_ref()
        .map(|grant| grant.path.clone())
        .unwrap_or_else(|| managed_scratch_workspace(space_id, session_id));
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
    space_id: Uuid,
) {
    for grant in grants
        .iter()
        .filter(|grant| grant.kind == swage_storage::model::ApprovalKind::Location)
        .filter(|grant| grant.space_id.is_none() || grant.space_id == Some(space_id))
    {
        let path = PathBuf::from(&grant.name);
        if !allowed_paths.contains(&path) {
            allowed_paths.push(path);
        }
    }
}

pub(crate) fn load_session_config(
    state: &AppState,
    space_slug: &str,
    agent_id: &str,
) -> Result<swage_config::ResolvedConfig, GatewayError> {
    let root = match swage_config::discovery::resolve_root() {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(
                error = %error.kind,
                space_slug,
                agent_id,
                "failed to resolve swage root; using active runtime configuration"
            );
            return Ok(state.active_config());
        }
    };

    let space_dir = root.join("spaces").join(space_slug);
    let space_config_exists = space_dir.join("config.toml").exists();
    let agent_config_exists = space_dir
        .join("agents")
        .join(agent_id)
        .join("config.toml")
        .exists();

    if !space_config_exists && !agent_config_exists {
        return Ok(state.active_config());
    }

    swage_config::load_resolved_config_from(&root, space_slug, agent_id)
        .map_err(|error| GatewayError::ConfigError(error.to_string()))
}

/// Overlay a durable agent definition onto the session's resolved config.
///
/// The definition is the agent's character; the file hierarchy is ambient
/// operator configuration. Its instructions append after the operator's, its
/// model preference applies only where the session has no explicit
/// selection, and its permission overlay intersects with the resolved
/// grants — a definition can narrow what an agent may do, never widen it.
pub(crate) fn apply_agent_definition(
    mut config: swage_config::ResolvedConfig,
    definition: &swage_storage::AgentDefinition,
) -> swage_config::ResolvedConfig {
    if !definition.instructions.trim().is_empty() {
        if config.instructions.is_empty() {
            config.instructions = definition.instructions.clone();
        } else {
            config.instructions = format!("{}\n\n{}", config.instructions, definition.instructions);
        }
    }

    if let Some(model_id) = definition
        .model_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
    {
        if config.models.entries.contains_key(model_id) {
            config.models.default = model_id.to_string();
        } else {
            tracing::warn!(
                agent_id = %definition.id,
                model_id,
                "agent definition prefers an unconfigured model; keeping the resolved default"
            );
        }
    }

    let overlay = definition.tool_permissions.as_ref().and_then(|value| {
        match serde_json::from_value::<swage_config::model::ToolPermissionConfig>(value.clone()) {
            Ok(overlay) => Some(overlay),
            Err(error) => {
                // A malformed overlay must not silently leave the agent's
                // broader grants intact while looking applied. Change-case
                // validation rejects these before storage; a row that fails
                // here anyway is surfaced, and the session falls back to
                // the resolved config's own grants.
                tracing::warn!(
                    agent_id = %definition.id,
                    %error,
                    "agent definition's permission overlay does not parse; ignoring it"
                );
                None
            }
        }
    });
    if let Some(overlay) = overlay {
        intersect_tool_permissions(&mut config, &overlay.allow, overlay.deny);
    }

    config.declared_skills = definition.skills.clone();
    if let Some(schema) = &definition.output_schema {
        config.output.schema = Some(schema.clone());
    }
    if let Some(max_retries) = definition.output_max_retries {
        config.output.max_retries = max_retries;
    }

    config
}

/// Intersection, never expansion: the effective allow is what both the
/// resolved config and the overlay permit ("*" on either side defers to the
/// other), and every deny from either side stands. Shared by the
/// agent-definition overlay and the dispatch bound — the one grant-narrowing
/// semantics, applied wherever a narrower context confines a wider one.
fn intersect_tool_permissions(
    config: &mut swage_config::ResolvedConfig,
    overlay_allow: &[String],
    overlay_deny: Vec<String>,
) {
    let config_allows_all = config.tools.permissions.allow.iter().any(|t| t == "*");
    let overlay_allows_all = overlay_allow.iter().any(|t| t == "*");
    config.tools.permissions.allow = match (config_allows_all, overlay_allows_all) {
        (_, true) => config.tools.permissions.allow.clone(),
        (true, false) => overlay_allow.to_vec(),
        (false, false) => config
            .tools
            .permissions
            .allow
            .iter()
            .filter(|tool| overlay_allow.contains(tool))
            .cloned()
            .collect(),
    };
    for denied in overlay_deny {
        if !config.tools.permissions.deny.contains(&denied) {
            config.tools.permissions.deny.push(denied);
        }
    }
}

/// Expand an allow list to concrete tool names using the same config's own
/// bundle definitions. `None` means the list allows everything ("*").
fn expanded_allow(
    permissions: &swage_config::model::ToolPermissionConfig,
) -> Option<std::collections::BTreeSet<String>> {
    if permissions.allow.iter().any(|tool| tool == "*") {
        return None;
    }
    let mut concrete = std::collections::BTreeSet::new();
    for entry in &permissions.allow {
        match permissions.bundles.get(entry) {
            Some(tools) => concrete.extend(tools.iter().cloned()),
            None => {
                concrete.insert(entry.clone());
            }
        }
    }
    Some(concrete)
}

/// Confine a worker's resolved config to its dispatching context's grants.
///
/// An ephemeral worker dispatched by a workflow run derives its grants from
/// the dispatching context as a subset — intersection, never expansion (the
/// configuration-capability Decision's rule). The bound context arrives
/// already resolved (file config plus its own definition overlay), and only
/// tool permissions are confined: the worker's instructions, model, and
/// output contract are its own business.
///
/// The intersection runs on EXPANDED allow sets, unlike the definition
/// overlay's (which shares one bundle namespace with the config it overlays):
/// a bundle name means whatever the config defining it says, so the same
/// label on both sides is no licence — intersecting raw names would let a
/// worker's wider bundle ride through under the dispatcher's narrower one.
/// Concrete tool names are the only safe meeting point. The dispatcher's
/// approval demands also stand on the worker: a tool the dispatching context
/// could only use with approval must not run approval-free in a worker it
/// dispatched.
pub(crate) fn apply_dispatch_bound(
    mut config: swage_config::ResolvedConfig,
    bound: &swage_config::ResolvedConfig,
) -> swage_config::ResolvedConfig {
    let worker_allow = expanded_allow(&config.tools.permissions);
    let bound_allow = expanded_allow(&bound.tools.permissions);
    config.tools.permissions.allow = match (worker_allow, bound_allow) {
        // The bound allows everything: the worker's own grants stand, in
        // their original form (its bundles keep expanding at check time).
        (_, None) => config.tools.permissions.allow.clone(),
        // An allow-all worker narrows to the dispatcher's concrete set.
        (None, Some(bound_set)) => bound_set.into_iter().collect(),
        (Some(worker_set), Some(bound_set)) => {
            worker_set.intersection(&bound_set).cloned().collect()
        }
    };
    for denied in bound.tools.permissions.deny.clone() {
        if !config.tools.permissions.deny.contains(&denied) {
            config.tools.permissions.deny.push(denied);
        }
    }
    let worker_approval_names: std::collections::BTreeSet<String> = config
        .tools
        .permissions
        .require_approval
        .iter()
        .flat_map(|entry| {
            let name = entry.tool_name().to_string();
            match config.tools.permissions.bundles.get(&name) {
                Some(tools) => tools.clone(),
                None => vec![name],
            }
        })
        .collect();
    let mut carried = std::collections::BTreeSet::new();
    for entry in &bound.tools.permissions.require_approval {
        let name = entry.tool_name().to_string();
        // Bound entries expand through the BOUND's own bundles, for the
        // same aliasing reason as the allow intersection. They carry as
        // plain demands — the worker's own approval defaults time them.
        let names = bound
            .tools
            .permissions
            .bundles
            .get(&name)
            .cloned()
            .unwrap_or_else(|| vec![name]);
        for concrete in names {
            if !worker_approval_names.contains(&concrete) {
                carried.insert(concrete);
            }
        }
    }
    config.tools.permissions.require_approval.extend(
        carried
            .into_iter()
            .map(swage_config::model::RequireApprovalEntry::Simple),
    );
    config
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

#[cfg(test)]
mod tests {
    use super::*;

    fn definition_with(
        instructions: &str,
        model_id: Option<&str>,
        tool_permissions: Option<serde_json::Value>,
    ) -> swage_storage::AgentDefinition {
        swage_storage::AgentDefinition {
            id: "researcher".into(),
            space_id: None,
            display_name: "Researcher".into(),
            description: String::new(),
            instructions: instructions.into(),
            model_id: model_id.map(str::to_owned),
            tool_permissions,
            skills: vec![],
            output_schema: None,
            output_max_retries: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn definition_instructions_append_after_operator_instructions() {
        let config = swage_config::ResolvedConfig {
            instructions: "Operator ambient rules.".into(),
            ..Default::default()
        };

        let applied = apply_agent_definition(
            config,
            &definition_with("Gather and cite evidence.", None, None),
        );

        let operator = applied
            .instructions
            .find("Operator ambient rules.")
            .unwrap();
        let agent = applied
            .instructions
            .find("Gather and cite evidence.")
            .unwrap();
        assert!(
            operator < agent,
            "the definition extends the operator's instructions, never precedes them"
        );
    }

    #[test]
    fn definition_fills_empty_instructions() {
        let config = swage_config::ResolvedConfig::default();
        let applied =
            apply_agent_definition(config, &definition_with("Agent character.", None, None));
        assert_eq!(applied.instructions, "Agent character.");
    }

    #[test]
    fn unconfigured_model_preference_is_ignored() {
        let config = swage_config::ResolvedConfig::default();
        let default_before = config.models.default.clone();

        let applied =
            apply_agent_definition(config, &definition_with("", Some("no-such-model"), None));
        assert_eq!(
            applied.models.default, default_before,
            "a preference for an absent model must not break the session"
        );
    }

    #[test]
    fn permission_overlay_intersects_and_never_expands() {
        let mut config = swage_config::ResolvedConfig::default();
        config.tools.permissions.allow = vec!["shell".into(), "knowledge_search".into()];

        let applied = apply_agent_definition(
            config,
            &definition_with(
                "",
                None,
                Some(serde_json::json!({
                    // The overlay asks for one held tool and one the config
                    // never granted: only the held one survives. The shape is
                    // a full ToolPermissionConfig — bundles included — as
                    // change-case validation requires.
                    "allow": ["knowledge_search", "schedule"],
                    "deny": ["browser_navigate"],
                    "bundles": {}
                })),
            ),
        );

        assert_eq!(
            applied.tools.permissions.allow,
            vec!["knowledge_search".to_string()],
            "intersection keeps only what both sides grant"
        );
        assert!(
            applied
                .tools
                .permissions
                .deny
                .contains(&"browser_navigate".to_string()),
            "overlay denies are added"
        );
    }

    #[test]
    fn wildcard_config_narrows_to_the_overlay_allow() {
        let config = swage_config::ResolvedConfig::default();
        assert!(config.tools.permissions.allow.iter().any(|t| t == "*"));

        let applied = apply_agent_definition(
            config,
            &definition_with(
                "",
                None,
                Some(serde_json::json!({
                    "allow": ["knowledge_search"],
                    "deny": [],
                    "bundles": {}
                })),
            ),
        );

        assert_eq!(
            applied.tools.permissions.allow,
            vec!["knowledge_search".to_string()],
            "an allow-all config defers to the definition's narrower grant"
        );
    }

    #[test]
    fn dispatch_bound_intersects_allows_and_unions_denies() {
        let mut worker = swage_config::ResolvedConfig::default();
        worker.tools.permissions.allow =
            vec!["shell".into(), "knowledge_search".into(), "schedule".into()];
        worker.tools.permissions.deny = vec!["browser_navigate".into()];

        let mut bound = swage_config::ResolvedConfig::default();
        bound.tools.permissions.allow = vec!["knowledge_search".into(), "web_fetch".into()];
        bound.tools.permissions.deny = vec!["python_execute".into()];

        let confined = apply_dispatch_bound(worker, &bound);

        assert_eq!(
            confined.tools.permissions.allow,
            vec!["knowledge_search".to_string()],
            "the worker keeps only what its dispatcher also holds"
        );
        assert!(
            confined
                .tools
                .permissions
                .deny
                .iter()
                .any(|tool| tool == "python_execute"),
            "the dispatcher's denies stand on the worker"
        );
        assert!(
            confined
                .tools
                .permissions
                .deny
                .iter()
                .any(|tool| tool == "browser_navigate"),
            "the worker's own denies survive"
        );
    }

    #[test]
    fn a_shared_bundle_name_is_no_licence_when_definitions_differ() {
        // The dispatcher and the worker both allow a bundle called "safe",
        // but define it differently. Intersecting the raw name would hand
        // the worker its own wider definition under the dispatcher's
        // narrower one; the expanded intersection must not.
        let mut worker = swage_config::ResolvedConfig::default();
        worker.tools.permissions.allow = vec!["safe".into()];
        worker.tools.permissions.bundles =
            std::collections::HashMap::from([("safe".to_string(), vec!["shell".to_string()])]);

        let mut bound = swage_config::ResolvedConfig::default();
        bound.tools.permissions.allow = vec!["safe".into()];
        bound.tools.permissions.bundles = std::collections::HashMap::from([(
            "safe".to_string(),
            vec!["file_read".to_string()],
        )]);

        let confined = apply_dispatch_bound(worker, &bound);
        assert!(
            confined.tools.permissions.allow.is_empty(),
            "shell ∩ file_read is empty; the shared label must grant nothing: {:?}",
            confined.tools.permissions.allow
        );
    }

    #[test]
    fn the_dispatchers_approval_demands_stand_on_the_worker() {
        let mut worker = swage_config::ResolvedConfig::default();
        worker.tools.permissions.allow = vec!["shell".into(), "file_read".into()];
        worker.tools.permissions.require_approval = vec![];

        let mut bound = swage_config::ResolvedConfig::default();
        bound.tools.permissions.allow = vec!["shell".into(), "file_read".into()];
        bound.tools.permissions.require_approval =
            vec![swage_config::model::RequireApprovalEntry::Simple(
                "shell".into(),
            )];

        let confined = apply_dispatch_bound(worker, &bound);
        let demanded: Vec<&str> = confined
            .tools
            .permissions
            .require_approval
            .iter()
            .map(|entry| entry.tool_name())
            .collect();
        assert!(
            demanded.contains(&"shell"),
            "a tool the dispatcher gates behind approval must not run approval-free in its worker"
        );

        // Already-demanded tools are not duplicated.
        let mut worker = swage_config::ResolvedConfig::default();
        worker.tools.permissions.require_approval =
            vec![swage_config::model::RequireApprovalEntry::Simple(
                "shell".into(),
            )];
        let confined = apply_dispatch_bound(worker, &bound);
        assert_eq!(
            confined
                .tools
                .permissions
                .require_approval
                .iter()
                .filter(|entry| entry.tool_name() == "shell")
                .count(),
            1
        );
    }

    #[test]
    fn a_wildcard_dispatcher_leaves_the_workers_own_grants() {
        let mut worker = swage_config::ResolvedConfig::default();
        worker.tools.permissions.allow = vec!["shell".into()];

        // The default config allows "*": a dispatcher with everything
        // confines nothing, and a worker never gains from the intersection.
        let bound = swage_config::ResolvedConfig::default();
        let confined = apply_dispatch_bound(worker, &bound);
        assert_eq!(confined.tools.permissions.allow, vec!["shell".to_string()]);
    }

    #[test]
    fn a_wildcard_worker_narrows_to_the_dispatchers_grants() {
        let worker = swage_config::ResolvedConfig::default();
        assert!(worker.tools.permissions.allow.iter().any(|t| t == "*"));

        let mut bound = swage_config::ResolvedConfig::default();
        bound.tools.permissions.allow = vec!["knowledge_search".into()];

        let confined = apply_dispatch_bound(worker, &bound);
        assert_eq!(
            confined.tools.permissions.allow,
            vec!["knowledge_search".to_string()],
            "an allow-all worker runs no wider than its dispatcher"
        );
    }

    #[test]
    fn definition_applies_skills_and_output_contract() {
        let mut definition = definition_with("", None, None);
        definition.skills = vec!["source-checking".into()];
        definition.output_schema = Some(serde_json::json!({"type": "object"}));
        definition.output_max_retries = Some(2);

        let applied = apply_agent_definition(swage_config::ResolvedConfig::default(), &definition);

        assert_eq!(applied.declared_skills, ["source-checking"]);
        assert_eq!(applied.output.schema.unwrap()["type"], "object");
        assert_eq!(applied.output.max_retries, 2);
    }
}
