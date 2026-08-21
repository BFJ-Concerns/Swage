use super::{
    AgentConfig, Arc, GatewayToolAdvertisementFilter, Path, PathBuf, PermissionConfig, Tool,
    ToolRequireApprovalEntry, Uuid,
};
use crate::turn::managed_scratch_workspace;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WorkspacePermissionSummary {
    pub(crate) has_read: bool,
    pub(crate) has_write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TrustedWorkspacePath {
    pub(crate) path: PathBuf,
    pub(crate) is_trusted: bool,
    pub(crate) can_read: bool,
    pub(crate) can_write: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TrustedWorkspacePolicy {
    pub(crate) permissions: WorkspacePermissionSummary,
    pub(crate) active_workspace: Option<TrustedWorkspacePath>,
    pub(crate) attachments: Vec<TrustedWorkspacePath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedWorkerDirectory {
    pub(crate) path: PathBuf,
    pub(crate) can_write: bool,
    pub(crate) authority_path: PathBuf,
    pub(crate) authority_context_id: Uuid,
}

/// Inspect the space's workspace attachments and derive which permission
/// grants are in force. Storage failures degrade to "no grants" so that a
/// transient lookup error never silently widens the policy.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn summarise_trusted_workspace_permissions(
    storage: &Arc<dyn swage_storage::store::Storage>,
    space_id: Uuid,
) -> WorkspacePermissionSummary {
    load_trusted_workspace_policy(storage, space_id, None)
        .await
        .permissions
}

pub(crate) async fn load_trusted_workspace_policy(
    storage: &Arc<dyn swage_storage::store::Storage>,
    space_id: Uuid,
    active_workspace_attachment_id: Option<Uuid>,
) -> TrustedWorkspacePolicy {
    let attachments = match storage.list_workspace_attachments_by_space(space_id).await {
        Ok(list) => list,
        Err(error) => {
            tracing::warn!(
                space_id = %space_id,
                %error,
                "failed to load workspace attachments; treating space as having no granted permissions"
            );
            return TrustedWorkspacePolicy::default();
        }
    };

    let mut policy = TrustedWorkspacePolicy::default();
    let default_workspace_attachment_id = active_workspace_attachment_id.or_else(|| {
        attachments
            .iter()
            .find(|attachment| attachment.is_default && !attachment.is_missing)
            .map(|attachment| attachment.id)
    });

    for attachment in attachments.into_iter().filter(|a| !a.is_missing) {
        let mut can_read = false;
        let mut can_write = false;
        for permission in &attachment.permissions {
            match permission.as_str() {
                "read" => {
                    if attachment.is_trusted {
                        policy.permissions.has_read = true;
                    }
                    can_read = true;
                }
                "write" => {
                    if attachment.is_trusted {
                        policy.permissions.has_write = true;
                    }
                    can_write = true;
                }
                _ => {}
            }
        }
        let path = PathBuf::from(if attachment.canonical_path.is_empty() {
            attachment.path
        } else {
            attachment.canonical_path
        });
        let grant = TrustedWorkspacePath {
            path,
            is_trusted: attachment.is_trusted,
            can_read,
            can_write,
        };
        if Some(attachment.id) == default_workspace_attachment_id {
            policy.active_workspace = Some(grant);
            continue;
        }
        if !attachment.is_trusted {
            continue;
        }
        if can_read || can_write {
            policy.attachments.push(grant);
        }
    }
    policy
}

/// Resolve a worker placement without widening the dispatching context's
/// filesystem authority. A placement may select any directory already covered
/// by the context's active workspace, trusted attachment grants, managed
/// scratch root, or approved location grants.
pub(crate) async fn resolve_authorised_worker_directory(
    state: &crate::state::AppState,
    space_id: Uuid,
    context_id: Uuid,
    active_workspace_attachment_id: Option<Uuid>,
    inherited_placement: Option<&ResolvedWorkerDirectory>,
    requested: &str,
) -> Result<ResolvedWorkerDirectory, String> {
    let candidate = tokio::fs::canonicalize(requested)
        .await
        .map_err(|error| format!("cannot resolve worker cwd {requested:?}: {error}"))?;
    if !candidate.is_dir() {
        return Err(format!(
            "worker cwd is not a directory: {}",
            candidate.display()
        ));
    }

    let policy =
        load_trusted_workspace_policy(&state.storage, space_id, active_workspace_attachment_id)
            .await;
    let workspace_root = policy
        .active_workspace
        .as_ref()
        .map(|grant| grant.path.clone())
        .unwrap_or_else(|| managed_scratch_workspace(space_id, context_id));
    let approval_grants = state
        .session_approval_grants
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let within = |root: &Path| {
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        path_within_device_workspace(&candidate, &[canonical_root])
    };

    // A placed worker's exact directory is its complete filesystem authority.
    // Do not let a nested dispatch fall back to the wider space policy.
    if let Some(placement) = inherited_placement {
        if within(&placement.path) {
            return Ok(ResolvedWorkerDirectory {
                path: candidate,
                can_write: placement.can_write,
                authority_path: placement.authority_path.clone(),
                authority_context_id: placement.authority_context_id,
            });
        }
        return Err(format!(
            "worker cwd {} is outside the dispatching context's placed workspace root",
            candidate.display()
        ));
    }

    if let Some(grant) = policy
        .active_workspace
        .iter()
        .chain(policy.attachments.iter())
        .find(|grant| within(&grant.path))
    {
        return Ok(ResolvedWorkerDirectory {
            authority_path: candidate.clone(),
            path: candidate,
            can_write: grant.can_write,
            authority_context_id: context_id,
        });
    }
    if policy.active_workspace.is_none() && within(&workspace_root) {
        return Ok(ResolvedWorkerDirectory {
            authority_path: candidate.clone(),
            path: candidate,
            can_write: true,
            authority_context_id: context_id,
        });
    }

    let matching_approval_grants = approval_grants
        .iter()
        .filter(|grant| grant.kind == swage_storage::model::ApprovalKind::Location)
        .filter(|grant| grant.space_id.is_none() || grant.space_id == Some(space_id))
        .filter(|grant| within(Path::new(&grant.name)))
        .collect::<Vec<_>>();
    if !matching_approval_grants.is_empty() {
        return Ok(ResolvedWorkerDirectory {
            authority_path: candidate.clone(),
            path: candidate,
            can_write: matching_approval_grants
                .iter()
                .any(|grant| grant.access.as_deref() == Some("write")),
            authority_context_id: context_id,
        });
    }

    Err(format!(
        "worker cwd {} is outside the dispatching context's permitted workspace roots and location grants",
        candidate.display()
    ))
}

/// Re-authorise a durable worker placement for this execution. The placement
/// directory remains immutable, while its authority root and context are
/// checked against the current attachment/location grants and its write bit is
/// replaced in the in-memory session metadata used to build this turn.
pub(crate) async fn reauthorise_worker_placement_for_turn(
    state: &crate::state::AppState,
    session: &mut swage_storage::Session,
) -> Result<(), swage_core::GatewayError> {
    if !super::worker_placement_metadata_present(session.metadata.as_ref()) {
        return Ok(());
    }
    let metadata = session.metadata.as_mut().ok_or_else(|| {
        swage_core::GatewayError::ConfigError("worker placement metadata is incomplete".to_string())
    })?;
    let placement_path = metadata
        .get(super::WORKER_PLACEMENT_CWD_METADATA_KEY)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            swage_core::GatewayError::ConfigError(
                "worker placement is missing its immutable cwd".to_string(),
            )
        })?;
    let placement_path = tokio::fs::canonicalize(placement_path)
        .await
        .map_err(|error| {
            swage_core::GatewayError::ConfigError(format!(
                "cannot resolve worker placement cwd: {error}"
            ))
        })?;
    if !placement_path.is_dir() {
        return Err(swage_core::GatewayError::ConfigError(format!(
            "worker placement cwd is not a directory: {}",
            placement_path.display()
        )));
    }
    let authority_path = metadata
        .get(super::WORKER_PLACEMENT_AUTHORITY_PATH_METADATA_KEY)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            swage_core::GatewayError::ConfigError(
                "worker placement is missing its authority path".to_string(),
            )
        })?;
    let authority_context_id = metadata
        .get(super::WORKER_PLACEMENT_AUTHORITY_CONTEXT_METADATA_KEY)
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| {
            swage_core::GatewayError::ConfigError(
                "worker placement is missing its authority context".to_string(),
            )
        })?;
    let authorised = resolve_authorised_worker_directory(
        state,
        session.space_id,
        authority_context_id,
        session.active_workspace_attachment_id,
        None,
        authority_path,
    )
    .await
    .map_err(|error| {
        swage_core::GatewayError::ConfigError(format!(
            "worker placement is no longer authorised: {error}"
        ))
    })?;
    metadata[super::WORKER_PLACEMENT_CWD_METADATA_KEY] =
        serde_json::Value::String(placement_path.to_string_lossy().into_owned());
    metadata[super::WORKER_PLACEMENT_AUTHORITY_PATH_METADATA_KEY] =
        serde_json::Value::String(authorised.path.to_string_lossy().into_owned());
    metadata[super::WORKER_PLACEMENT_CAN_WRITE_METADATA_KEY] =
        serde_json::Value::Bool(authorised.can_write);
    Ok(())
}

pub(super) fn build_permission_config(
    config: &swage_config::ResolvedConfig,
    startup_config: &swage_config::ResolvedConfig,
    space_id: Option<Uuid>,
    session_approval_grants: &std::collections::HashSet<crate::state::ApprovalGrantKey>,
) -> PermissionConfig {
    // Top-level escape hatch: when permissions are disabled at the gateway,
    // hand the dispatcher a fully open config. The framework remains intact
    // so flipping the flag back restores normal enforcement.
    if config.dangerously_skip_permissions || startup_config.dangerously_skip_permissions {
        return PermissionConfig {
            allow: vec!["*".into()],
            deny: Vec::new(),
            bundles: std::collections::HashMap::new(),
            require_approval: Vec::new(),
        };
    }

    let mut deny = config.tools.permissions.deny.clone();
    for entry in &startup_config.tools.permissions.deny {
        if !deny.contains(entry) {
            deny.push(entry.clone());
        }
    }

    PermissionConfig {
        allow: config.tools.permissions.allow.clone(),
        deny,
        bundles: config.tools.permissions.bundles.clone(),
        require_approval: expand_require_approval_entries(config)
            .into_iter()
            .filter(|entry| {
                !session_approval_grants.contains(&crate::state::ApprovalGrantKey {
                    space_id: None,
                    kind: swage_storage::model::ApprovalKind::Tool,
                    name: entry.tool_name.clone(),
                    access: None,
                }) && !space_id.is_some_and(|space_id| {
                    session_approval_grants.contains(&crate::state::ApprovalGrantKey {
                        space_id: Some(space_id),
                        kind: swage_storage::model::ApprovalKind::Tool,
                        name: entry.tool_name.clone(),
                        access: None,
                    })
                })
            })
            .collect(),
    }
}

pub(super) fn build_gateway_advertisement_filter(
    device: Option<&crate::device::RegisteredDevice>,
    agent_config: &AgentConfig,
) -> Option<Arc<GatewayToolAdvertisementFilter>> {
    let host = device?;
    if host.kind == crate::device::DeviceKind::Local {
        return None;
    }

    let capabilities = host.capabilities.clone();
    let has_shared_token = host.shared_token_secret_ref.is_some();
    let context_in_host_workspace =
        agent_config_within_device_workspaces(agent_config, &host.workspace_roots);

    Some(Arc::new(move |tool: &dyn Tool| {
        has_shared_token
            && context_in_host_workspace
            && capabilities
                .iter()
                .any(|capability| capability == tool.name())
    }))
}

pub(super) fn agent_config_within_device_workspaces(
    agent_config: &AgentConfig,
    workspace_roots: &[PathBuf],
) -> bool {
    let canonical_roots = workspace_roots
        .iter()
        .map(|root| root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
        .collect::<Vec<_>>();

    path_within_device_workspace(&agent_config.working_directory, &canonical_roots)
        && agent_config
            .allowed_paths
            .iter()
            .all(|path| path_within_device_workspace(path, &canonical_roots))
}

pub(super) fn path_within_device_workspace(path: &Path, workspace_roots: &[PathBuf]) -> bool {
    let candidate = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    workspace_roots
        .iter()
        .any(|root| candidate.starts_with(root))
}

pub(super) fn expand_require_approval_entries(
    config: &swage_config::ResolvedConfig,
) -> Vec<ToolRequireApprovalEntry> {
    config
        .tools
        .permissions
        .require_approval
        .iter()
        .flat_map(|entry| {
            let approval_entry = match entry {
                swage_config::model::RequireApprovalEntry::Simple(tool_name) => {
                    ToolRequireApprovalEntry {
                        tool_name: tool_name.clone(),
                        timeout_secs: Some(config.hitl.timeout),
                        timeout_policy: Some(config.hitl.timeout_policy.clone()),
                    }
                }
                swage_config::model::RequireApprovalEntry::WithOptions {
                    tool,
                    timeout,
                    timeout_policy,
                } => ToolRequireApprovalEntry {
                    tool_name: tool.clone(),
                    timeout_secs: Some(timeout.unwrap_or(config.hitl.timeout)),
                    timeout_policy: Some(
                        timeout_policy
                            .clone()
                            .unwrap_or_else(|| config.hitl.timeout_policy.clone()),
                    ),
                },
            };

            match config
                .tools
                .permissions
                .bundles
                .get(&approval_entry.tool_name)
            {
                Some(bundle_tools) => bundle_tools
                    .iter()
                    .map(|tool_name| ToolRequireApprovalEntry {
                        tool_name: tool_name.clone(),
                        timeout_secs: approval_entry.timeout_secs,
                        timeout_policy: approval_entry.timeout_policy.clone(),
                    })
                    .collect::<Vec<_>>(),
                None => vec![approval_entry],
            }
        })
        .collect()
}
