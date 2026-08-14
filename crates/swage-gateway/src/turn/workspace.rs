use super::{
    AgentConfig, Arc, GatewayToolAdvertisementFilter, Path, PathBuf, PermissionConfig, Tool,
    ToolRequireApprovalEntry, Uuid,
};

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
