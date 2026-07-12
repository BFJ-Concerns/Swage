use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_config::{HierarchyLevel, discovery::resolve_root};
use swage_core::GatewayError;

use crate::capability_bundle::{
    CapabilityToolDefinition, CapabilityWriteRequest, discover_capability_bundles,
    read_capability_bundle, validate_capability_name, validate_capability_parts,
    write_capability_bundle,
};
use crate::state::AppState;

const WORKSPACE_EXTENSION_ROOT: &str = ".swage/extensions";

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSpaceExtensionRequest {
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    #[serde(default)]
    pub requested_tools: Vec<String>,
    #[serde(default)]
    pub provided_ui_modules: Vec<String>,
    #[serde(default)]
    pub provider_integrations: Vec<String>,
    #[serde(default)]
    pub tool: Option<CapabilityToolDefinition>,
    pub instructions: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSpaceExtensionRequest {
    pub display_name: Option<String>,
    pub description: String,
    #[serde(default)]
    pub requested_tools: Vec<String>,
    #[serde(default)]
    pub provided_ui_modules: Vec<String>,
    #[serde(default)]
    pub provider_integrations: Vec<String>,
    #[serde(default)]
    pub tool: Option<CapabilityToolDefinition>,
    pub instructions: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceExtensionResponse {
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    pub requested_tools: Vec<String>,
    pub instructions: String,
    pub provided_ui_modules: Vec<String>,
    pub provider_integrations: Vec<String>,
    pub tool: Option<CapabilityToolDefinition>,
    pub status: String,
    pub source_kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpaceExtensionListResponse {
    pub extensions: Vec<SpaceExtensionResponse>,
}

pub async fn list_space_extensions(
    state: &AppState,
    space_id: Uuid,
) -> Result<SpaceExtensionListResponse, GatewayError> {
    let space = state
        .space_store
        .get(space_id)
        .await?
        .ok_or(GatewayError::SpaceNotFound { id: space_id })?;

    let managed_root = managed_extension_root(&space.slug)?;
    let workspace_roots = workspace_extension_roots(state, space_id).await?;

    let mut extensions = Vec::new();
    extensions.extend(scan_extension_root_path(
        &managed_root,
        "active",
        "managed",
    )?);
    for workspace_root in workspace_roots {
        extensions.extend(scan_extension_root_path(
            &workspace_root,
            "approval_required",
            "workspace_candidate",
        )?);
    }
    extensions.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(SpaceExtensionListResponse { extensions })
}

pub async fn create_space_extension(
    state: &AppState,
    space_id: Uuid,
    request: CreateSpaceExtensionRequest,
) -> Result<SpaceExtensionResponse, GatewayError> {
    let space = state
        .space_store
        .get(space_id)
        .await?
        .ok_or(GatewayError::SpaceNotFound { id: space_id })?;
    let name = validate_extension_name(&request.name)?;
    validate_extension_body(
        &request.description,
        &request.requested_tools,
        &request.instructions,
        &request.provided_ui_modules,
        &request.provider_integrations,
        request.tool.as_ref(),
    )?;

    let extension_dir = managed_extension_root(&space.slug)?.join(&name);
    write_capability_bundle(
        &extension_dir,
        &name,
        &CapabilityWriteRequest {
            display_name: request.display_name,
            description: request.description,
            requested_tools: request.requested_tools,
            instructions: request.instructions,
            provided_ui_modules: request.provided_ui_modules,
            provider_integrations: request.provider_integrations,
            tool: request.tool,
        },
    )?;

    read_extension_response(&extension_dir, &name, "active".into(), "managed".into())
}

pub async fn update_space_extension(
    state: &AppState,
    space_id: Uuid,
    extension_name: &str,
    request: UpdateSpaceExtensionRequest,
) -> Result<SpaceExtensionResponse, GatewayError> {
    let space = state
        .space_store
        .get(space_id)
        .await?
        .ok_or(GatewayError::SpaceNotFound { id: space_id })?;
    let name = validate_extension_name(extension_name)?;
    validate_extension_body(
        &request.description,
        &request.requested_tools,
        &request.instructions,
        &request.provided_ui_modules,
        &request.provider_integrations,
        request.tool.as_ref(),
    )?;

    let extension_dir = managed_extension_root(&space.slug)?.join(&name);
    write_capability_bundle(
        &extension_dir,
        &name,
        &CapabilityWriteRequest {
            display_name: request.display_name,
            description: request.description,
            requested_tools: request.requested_tools,
            instructions: request.instructions,
            provided_ui_modules: request.provided_ui_modules,
            provider_integrations: request.provider_integrations,
            tool: request.tool,
        },
    )?;

    read_extension_response(&extension_dir, &name, "active".into(), "managed".into())
}

pub async fn approve_workspace_extension(
    state: &AppState,
    space_id: Uuid,
    extension_name: &str,
) -> Result<SpaceExtensionResponse, GatewayError> {
    let name = validate_extension_name(extension_name)?;
    let workspace_roots = workspace_extension_roots(state, space_id).await?;
    let matching_sources = workspace_roots
        .into_iter()
        .map(|root| root.join(&name))
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    let source_dir = match matching_sources.as_slice() {
        [source] => source,
        [] => {
            return Err(GatewayError::ValidationError(format!(
                "workspace extension '{name}' is not available on any attached workspace"
            )));
        }
        _ => {
            return Err(GatewayError::ValidationError(format!(
                "workspace extension '{name}' exists in multiple attached workspaces; detach or rename one before approving"
            )));
        }
    };

    let space = state
        .space_store
        .get(space_id)
        .await?
        .ok_or(GatewayError::SpaceNotFound { id: space_id })?;
    ensure_skill_dir_exists(source_dir, &name).await?;
    let target_dir = managed_extension_root(&space.slug)?.join(&name);
    copy_directory_recursive(source_dir, &target_dir).await?;

    read_extension_response(&target_dir, &name, "active".into(), "managed".into())
}

fn managed_extension_root(space_slug: &str) -> Result<PathBuf, GatewayError> {
    Ok(resolve_root()
        .map_err(|problem| GatewayError::ValidationError(problem.to_string()))?
        .join("spaces")
        .join(space_slug)
        .join("skills"))
}

async fn workspace_extension_roots(
    state: &AppState,
    space_id: Uuid,
) -> Result<Vec<PathBuf>, GatewayError> {
    let workspaces = state
        .storage
        .list_workspace_attachments_by_space(space_id)
        .await?;
    Ok(workspaces
        .into_iter()
        .map(|workspace| PathBuf::from(workspace.path).join(WORKSPACE_EXTENSION_ROOT))
        .collect())
}

fn scan_extension_root_path(
    root: &Path,
    status: &str,
    source_kind: &str,
) -> Result<Vec<SpaceExtensionResponse>, GatewayError> {
    let (bundles, warnings) = discover_capability_bundles(root, HierarchyLevel::Space);
    for warning in warnings {
        tracing::warn!(path = %warning.path.display(), reason = %warning.reason, "skipping invalid space capability bundle");
    }
    Ok(bundles
        .into_iter()
        .map(|bundle| SpaceExtensionResponse {
            name: bundle.name,
            display_name: bundle.display_name,
            description: bundle.description,
            requested_tools: bundle.requested_tools,
            instructions: bundle.instructions,
            provided_ui_modules: bundle.provided_ui_modules,
            provider_integrations: bundle.provider_integrations,
            tool: bundle.tool,
            status: status.to_string(),
            source_kind: source_kind.to_string(),
        })
        .collect())
}

fn read_extension_response(
    extension_dir: &Path,
    extension_name: &str,
    status: String,
    source_kind: String,
) -> Result<SpaceExtensionResponse, GatewayError> {
    let capability = read_capability_bundle(extension_dir, extension_name, HierarchyLevel::Space)?;
    Ok(SpaceExtensionResponse {
        name: capability.name,
        display_name: capability.display_name,
        description: capability.description,
        requested_tools: capability.requested_tools,
        instructions: capability.instructions,
        provided_ui_modules: capability.provided_ui_modules,
        provider_integrations: capability.provider_integrations,
        tool: capability.tool,
        status,
        source_kind,
    })
}

async fn ensure_skill_dir_exists(
    extension_dir: &Path,
    extension_name: &str,
) -> Result<(), GatewayError> {
    let metadata = tokio::fs::metadata(extension_dir).await.map_err(|error| {
        GatewayError::ValidationError(format!(
            "workspace extension '{extension_name}' is not available for approval: {error}"
        ))
    })?;
    if !metadata.is_dir() {
        return Err(GatewayError::ValidationError(format!(
            "workspace extension '{extension_name}' is not a directory"
        )));
    }
    let _ = read_capability_bundle(extension_dir, extension_name, HierarchyLevel::Space)?;
    Ok(())
}

pub(crate) fn validate_extension_name(name: &str) -> Result<String, GatewayError> {
    validate_capability_name(name).map_err(|error| {
        GatewayError::ValidationError(
            error
                .to_string()
                .replace("capability", "extension")
                .replace("Capability", "Extension"),
        )
    })
}

pub(crate) fn validate_extension_body(
    description: &str,
    requested_tools: &[String],
    instructions: &str,
    provided_ui_modules: &[String],
    provider_integrations: &[String],
    tool: Option<&CapabilityToolDefinition>,
) -> Result<(), GatewayError> {
    validate_capability_parts(
        description,
        requested_tools,
        instructions,
        provided_ui_modules,
        provider_integrations,
        tool,
    )
    .map_err(|error| {
        GatewayError::ValidationError(
            error
                .to_string()
                .replace("capability", "extension")
                .replace("Capability", "Extension"),
        )
    })
}

async fn copy_directory_recursive(source: &Path, target: &Path) -> Result<(), GatewayError> {
    let source = source.to_path_buf();
    let target = target.to_path_buf();
    tokio::task::spawn_blocking(move || copy_directory_recursive_blocking(&source, &target))
        .await
        .map_err(|error| {
            GatewayError::ValidationError(format!("failed to join copy task: {error}"))
        })?
}

fn copy_directory_recursive_blocking(source: &Path, target: &Path) -> Result<(), GatewayError> {
    std::fs::create_dir_all(target).map_err(|error| {
        GatewayError::ValidationError(format!(
            "failed to create extension target {}: {error}",
            target.display()
        ))
    })?;

    for entry in std::fs::read_dir(source).map_err(|error| {
        GatewayError::ValidationError(format!(
            "failed to read extension source {}: {error}",
            source.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            GatewayError::ValidationError(format!(
                "failed to read extension source entry in {}: {error}",
                source.display()
            ))
        })?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type().map_err(|error| {
            GatewayError::ValidationError(format!(
                "failed to inspect extension source entry {}: {error}",
                source_path.display()
            ))
        })?;
        if file_type.is_dir() {
            copy_directory_recursive_blocking(&source_path, &target_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    GatewayError::ValidationError(format!(
                        "failed to prepare extension target {}: {error}",
                        parent.display()
                    ))
                })?;
            }
            std::fs::copy(&source_path, &target_path).map_err(|error| {
                GatewayError::ValidationError(format!(
                    "failed to copy extension file {} to {}: {error}",
                    source_path.display(),
                    target_path.display()
                ))
            })?;
        }
    }

    Ok(())
}
