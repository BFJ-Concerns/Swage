use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::fs;
use uuid::Uuid;

use swage_config::{
    HierarchyLevel,
    discovery::resolve_root,
    skill::{SkillDefinition, parse_skill_file},
};
use swage_core::GatewayError;

use crate::state::AppState;

const WORKSPACE_EXTENSION_ROOT: &str = ".swage/extensions";

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectExtensionRequest {
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    #[serde(default)]
    pub requested_tools: Vec<String>,
    pub instructions: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProjectExtensionRequest {
    pub display_name: Option<String>,
    pub description: String,
    #[serde(default)]
    pub requested_tools: Vec<String>,
    pub instructions: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectExtensionResponse {
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    pub requested_tools: Vec<String>,
    pub instructions: String,
    pub status: String,
    pub source_kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectExtensionListResponse {
    pub extensions: Vec<ProjectExtensionResponse>,
}

pub async fn list_project_extensions(
    state: &AppState,
    project_id: Uuid,
) -> Result<ProjectExtensionListResponse, GatewayError> {
    let project = state
        .project_store
        .get(project_id)
        .await?
        .ok_or(GatewayError::ProjectNotFound { id: project_id })?;

    let managed_root = managed_extension_root(&project.slug)?;
    let workspace_root = workspace_extension_root(project.workspace_path.as_deref());

    let mut extensions = Vec::new();
    extensions.extend(scan_extension_root_path(&managed_root, "active", "managed").await?);
    extensions.extend(
        scan_extension_root(&workspace_root, "approval_required", "workspace_candidate").await?,
    );
    extensions.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(ProjectExtensionListResponse { extensions })
}

pub async fn create_project_extension(
    state: &AppState,
    project_id: Uuid,
    request: CreateProjectExtensionRequest,
) -> Result<ProjectExtensionResponse, GatewayError> {
    let project = state
        .project_store
        .get(project_id)
        .await?
        .ok_or(GatewayError::ProjectNotFound { id: project_id })?;
    let name = validate_extension_name(&request.name)?;
    validate_extension_body(
        &request.description,
        &request.requested_tools,
        &request.instructions,
    )?;

    let extension_dir = managed_extension_root(&project.slug)?.join(&name);
    write_extension_dir(
        &extension_dir,
        &name,
        request.display_name.as_deref(),
        &request.description,
        &request.requested_tools,
        &request.instructions,
    )
    .await?;

    read_extension_response(&extension_dir, &name, "active".into(), "managed".into()).await
}

pub async fn update_project_extension(
    state: &AppState,
    project_id: Uuid,
    extension_name: &str,
    request: UpdateProjectExtensionRequest,
) -> Result<ProjectExtensionResponse, GatewayError> {
    let project = state
        .project_store
        .get(project_id)
        .await?
        .ok_or(GatewayError::ProjectNotFound { id: project_id })?;
    let name = validate_extension_name(extension_name)?;
    validate_extension_body(
        &request.description,
        &request.requested_tools,
        &request.instructions,
    )?;

    let extension_dir = managed_extension_root(&project.slug)?.join(&name);
    write_extension_dir(
        &extension_dir,
        &name,
        request.display_name.as_deref(),
        &request.description,
        &request.requested_tools,
        &request.instructions,
    )
    .await?;

    read_extension_response(&extension_dir, &name, "active".into(), "managed".into()).await
}

pub async fn approve_workspace_extension(
    state: &AppState,
    project_id: Uuid,
    extension_name: &str,
) -> Result<ProjectExtensionResponse, GatewayError> {
    let project = state
        .project_store
        .get(project_id)
        .await?
        .ok_or(GatewayError::ProjectNotFound { id: project_id })?;
    let name = validate_extension_name(extension_name)?;
    let workspace_root = workspace_extension_root(project.workspace_path.as_deref()).ok_or(
        GatewayError::ValidationError(
            "project has no workspace path for workspace extension approval".into(),
        ),
    )?;

    let source_dir = workspace_root.join(&name);
    ensure_skill_dir_exists(&source_dir, &name).await?;
    let target_dir = managed_extension_root(&project.slug)?.join(&name);
    copy_directory_recursive(&source_dir, &target_dir).await?;

    read_extension_response(&target_dir, &name, "active".into(), "managed".into()).await
}

fn managed_extension_root(project_slug: &str) -> Result<PathBuf, GatewayError> {
    Ok(resolve_root()
        .map_err(|problem| GatewayError::ValidationError(problem.to_string()))?
        .join("projects")
        .join(project_slug)
        .join("skills"))
}

fn workspace_extension_root(workspace_path: Option<&str>) -> Option<PathBuf> {
    workspace_path.map(|path| PathBuf::from(path).join(WORKSPACE_EXTENSION_ROOT))
}

async fn scan_extension_root(
    root: &Option<PathBuf>,
    status: &str,
    source_kind: &str,
) -> Result<Vec<ProjectExtensionResponse>, GatewayError> {
    let Some(root) = root else {
        return Ok(Vec::new());
    };
    scan_extension_root_path(root, status, source_kind).await
}

async fn scan_extension_root_path(
    root: &Path,
    status: &str,
    source_kind: &str,
) -> Result<Vec<ProjectExtensionResponse>, GatewayError> {
    let mut extensions = Vec::new();
    let mut entries = match fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(extensions),
        Err(error) => {
            return Err(GatewayError::ValidationError(format!(
                "failed to read extension directory {}: {error}",
                root.display()
            )));
        }
    };
    while let Some(entry) = entries.next_entry().await.map_err(|error| {
        GatewayError::ValidationError(format!(
            "failed to read extension directory entry in {}: {error}",
            root.display()
        ))
    })? {
        let path = entry.path();
        if !entry
            .file_type()
            .await
            .map_err(|error| {
                GatewayError::ValidationError(format!(
                    "failed to read extension entry type for {}: {error}",
                    path.display()
                ))
            })?
            .is_dir()
        {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if let Ok(response) =
            read_extension_response(&path, name, status.to_string(), source_kind.to_string()).await
        {
            extensions.push(response);
        }
    }

    Ok(extensions)
}

async fn read_extension_response(
    extension_dir: &Path,
    extension_name: &str,
    status: String,
    source_kind: String,
) -> Result<ProjectExtensionResponse, GatewayError> {
    let skill = parse_skill_definition(extension_dir, extension_name).await?;
    Ok(ProjectExtensionResponse {
        name: skill.name,
        display_name: skill.display_name,
        description: skill.description,
        requested_tools: skill.tools,
        instructions: skill.instructions,
        status,
        source_kind,
    })
}

async fn parse_skill_definition(
    extension_dir: &Path,
    extension_name: &str,
) -> Result<SkillDefinition, GatewayError> {
    let skill_path = extension_dir.join("SKILL.md");
    let parse_result = parse_skill_file(&skill_path, extension_name, HierarchyLevel::Project)
        .map_err(|warning| GatewayError::ValidationError(warning.reason))?;
    Ok(parse_result)
}

async fn ensure_skill_dir_exists(
    extension_dir: &Path,
    extension_name: &str,
) -> Result<(), GatewayError> {
    let skill_path = extension_dir.join("SKILL.md");
    let metadata = fs::metadata(&skill_path).await.map_err(|error| {
        GatewayError::ValidationError(format!(
            "workspace extension '{extension_name}' is not available for approval: {error}"
        ))
    })?;
    if !metadata.is_file() {
        return Err(GatewayError::ValidationError(format!(
            "workspace extension '{extension_name}' is missing SKILL.md"
        )));
    }
    Ok(())
}

async fn write_extension_dir(
    extension_dir: &Path,
    extension_name: &str,
    display_name: Option<&str>,
    description: &str,
    requested_tools: &[String],
    instructions: &str,
) -> Result<(), GatewayError> {
    fs::create_dir_all(extension_dir).await.map_err(|error| {
        GatewayError::ValidationError(format!(
            "failed to create extension directory {}: {error}",
            extension_dir.display()
        ))
    })?;
    let skill_path = extension_dir.join("SKILL.md");
    fs::write(
        &skill_path,
        render_skill_markdown(display_name, description, requested_tools, instructions),
    )
    .await
    .map_err(|error| {
        GatewayError::ValidationError(format!(
            "failed to write extension skill {}: {error}",
            skill_path.display()
        ))
    })?;

    let _ = parse_skill_definition(extension_dir, extension_name).await?;
    Ok(())
}

fn render_skill_markdown(
    display_name: Option<&str>,
    description: &str,
    requested_tools: &[String],
    instructions: &str,
) -> String {
    let mut rendered = String::from("---\n");
    if let Some(display_name) = display_name.filter(|value| !value.trim().is_empty()) {
        rendered.push_str("name: ");
        rendered.push_str(display_name.trim());
        rendered.push('\n');
    }
    rendered.push_str("description: ");
    rendered.push_str(description.trim());
    rendered.push('\n');
    if !requested_tools.is_empty() {
        rendered.push_str("tools:\n");
        for tool in requested_tools {
            rendered.push_str("  - ");
            rendered.push_str(tool);
            rendered.push('\n');
        }
    }
    rendered.push_str("---\n\n");
    rendered.push_str(instructions.trim());
    rendered.push('\n');
    rendered
}

pub(crate) fn validate_extension_name(name: &str) -> Result<String, GatewayError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(GatewayError::ValidationError(
            "extension name must not be empty".into(),
        ));
    }
    if !trimmed
        .chars()
        .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'))
    {
        return Err(GatewayError::ValidationError(
            "extension name must match [a-z0-9-]+".into(),
        ));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn validate_extension_body(
    description: &str,
    requested_tools: &[String],
    instructions: &str,
) -> Result<(), GatewayError> {
    if description.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "extension description must not be empty".into(),
        ));
    }
    if instructions.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "extension instructions must not be empty".into(),
        ));
    }
    if requested_tools.iter().any(|tool| tool.trim().is_empty()) {
        return Err(GatewayError::ValidationError(
            "requested_tools must not contain empty entries".into(),
        ));
    }
    Ok(())
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
