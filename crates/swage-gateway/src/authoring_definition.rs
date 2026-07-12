use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_config::{HierarchyLevel, discovery::resolve_root, model::ContextConfig};
use swage_context::{assembly::assemble, tokeniser::TiktokenTokeniser};
use swage_core::{ChatMessage, GatewayError, Role};
use swage_topology::TopologyGraph;

use crate::{state::AppState, turn::build_assembly_elements};

const TOPOLOGY_FLOW_ROOT: &str = "topology-flows";
const CONTEXT_DEFINITION_ROOT: &str = "context-definitions";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyFlowDefinitionRecord {
    pub name: String,
    pub description: String,
    pub graph: TopologyGraph,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTopologyFlowDefinitionRequest {
    pub name: String,
    pub description: String,
    pub graph: TopologyGraph,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTopologyFlowDefinitionRequest {
    pub description: String,
    pub graph: TopologyGraph,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopologyFlowDefinitionListResponse {
    pub flows: Vec<TopologyFlowDefinitionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextDefinitionRecord {
    pub name: String,
    pub description: String,
    pub config: ContextConfig,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateContextDefinitionRequest {
    pub name: String,
    pub description: String,
    pub config: ContextConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateContextDefinitionRequest {
    pub description: String,
    pub config: ContextConfig,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextDefinitionListResponse {
    pub definitions: Vec<ContextDefinitionRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PreviewContextDefinitionRequest {
    pub instructions: String,
    #[serde(default)]
    pub history: Vec<PreviewContextMessageInput>,
    #[serde(default)]
    pub memory_content: String,
    pub space_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PreviewContextMessageInput {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PreviewContextDefinitionResponse {
    pub messages: Vec<ChatMessage>,
    pub report: swage_context::report::AssemblyReport,
}

pub async fn list_topology_flows(
    state: &AppState,
    space_id: Uuid,
) -> Result<TopologyFlowDefinitionListResponse, GatewayError> {
    let space = require_space(state, space_id).await?;
    let mut flows: Vec<TopologyFlowDefinitionRecord> =
        read_definition_directory(&topology_flow_root(&space.slug)?)?;
    flows.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(TopologyFlowDefinitionListResponse { flows })
}

pub async fn get_topology_flow(
    state: &AppState,
    space_id: Uuid,
    flow_name: &str,
) -> Result<TopologyFlowDefinitionRecord, GatewayError> {
    let space = require_space(state, space_id).await?;
    read_definition_record(&topology_flow_root(&space.slug)?, flow_name)
}

pub async fn create_topology_flow(
    state: &AppState,
    space_id: Uuid,
    request: CreateTopologyFlowDefinitionRequest,
) -> Result<TopologyFlowDefinitionRecord, GatewayError> {
    let space = require_space(state, space_id).await?;
    let name = validate_definition_name(&request.name)?;
    let now = Utc::now();
    write_definition_record(
        &topology_flow_root(&space.slug)?,
        &name,
        &TopologyFlowDefinitionRecord {
            name: name.clone(),
            description: validate_description(&request.description)?,
            graph: request.graph,
            created_at: now,
            updated_at: now,
        },
    )
}

pub async fn update_topology_flow(
    state: &AppState,
    space_id: Uuid,
    flow_name: &str,
    request: UpdateTopologyFlowDefinitionRequest,
) -> Result<TopologyFlowDefinitionRecord, GatewayError> {
    let space = require_space(state, space_id).await?;
    let root = topology_flow_root(&space.slug)?;
    let existing = read_definition_record::<TopologyFlowDefinitionRecord>(&root, flow_name)?;
    write_definition_record(
        &root,
        flow_name,
        &TopologyFlowDefinitionRecord {
            name: existing.name,
            description: validate_description(&request.description)?,
            graph: request.graph,
            created_at: existing.created_at,
            updated_at: Utc::now(),
        },
    )
}

pub async fn list_context_definitions(
    state: &AppState,
    space_id: Uuid,
) -> Result<ContextDefinitionListResponse, GatewayError> {
    let space = require_space(state, space_id).await?;
    let mut definitions: Vec<ContextDefinitionRecord> =
        read_definition_directory(&context_definition_root(&space.slug)?)?;
    definitions.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(ContextDefinitionListResponse { definitions })
}

pub async fn get_context_definition(
    state: &AppState,
    space_id: Uuid,
    definition_name: &str,
) -> Result<ContextDefinitionRecord, GatewayError> {
    let space = require_space(state, space_id).await?;
    read_definition_record(&context_definition_root(&space.slug)?, definition_name)
}

pub async fn create_context_definition(
    state: &AppState,
    space_id: Uuid,
    request: CreateContextDefinitionRequest,
) -> Result<ContextDefinitionRecord, GatewayError> {
    let space = require_space(state, space_id).await?;
    let name = validate_definition_name(&request.name)?;
    validate_context_config(&request.config)?;
    let now = Utc::now();
    write_definition_record(
        &context_definition_root(&space.slug)?,
        &name,
        &ContextDefinitionRecord {
            name: name.clone(),
            description: validate_description(&request.description)?,
            config: request.config,
            created_at: now,
            updated_at: now,
        },
    )
}

pub async fn update_context_definition(
    state: &AppState,
    space_id: Uuid,
    definition_name: &str,
    request: UpdateContextDefinitionRequest,
) -> Result<ContextDefinitionRecord, GatewayError> {
    let space = require_space(state, space_id).await?;
    validate_context_config(&request.config)?;
    let root = context_definition_root(&space.slug)?;
    let existing = read_definition_record::<ContextDefinitionRecord>(&root, definition_name)?;
    write_definition_record(
        &root,
        definition_name,
        &ContextDefinitionRecord {
            name: existing.name,
            description: validate_description(&request.description)?,
            config: request.config,
            created_at: existing.created_at,
            updated_at: Utc::now(),
        },
    )
}

pub async fn preview_context_definition(
    state: &AppState,
    space_id: Uuid,
    definition_name: &str,
    request: PreviewContextDefinitionRequest,
) -> Result<PreviewContextDefinitionResponse, GatewayError> {
    let space = require_space(state, space_id).await?;
    let config = read_context_definition_config_for_slug(&space.slug, definition_name)?;
    let assembly_search_paths = build_assembly_search_paths(&space.slug);
    let search_level_refs: Vec<(HierarchyLevel, &Path)> = assembly_search_paths
        .iter()
        .map(|(level, path)| (level.clone(), path.as_path()))
        .collect();
    let elements = build_assembly_elements(
        &request.instructions,
        vec![],
        request
            .history
            .into_iter()
            .map(|message| ChatMessage::new(message.role, message.content))
            .collect(),
        request.memory_content,
        request.space_name,
    );
    let tokeniser = TiktokenTokeniser::new(state.config.default_model_id());
    let assembly_runner =
        swage_python::ConfiguredAssemblyRunner::new(state.config.tools.python_execute.clone());
    let output = assemble(
        &elements,
        &config,
        &tokeniser,
        &search_level_refs,
        state.provider.as_ref(),
        Some(&assembly_runner),
    )
    .await;
    Ok(PreviewContextDefinitionResponse {
        messages: output.messages,
        report: output.report,
    })
}

pub async fn load_topology_flow_graph(
    state: &AppState,
    space_id: Uuid,
    flow_name: &str,
) -> Result<TopologyGraph, GatewayError> {
    Ok(get_topology_flow(state, space_id, flow_name).await?.graph)
}

pub fn read_context_definition_config_for_slug(
    space_slug: &str,
    definition_name: &str,
) -> Result<ContextConfig, GatewayError> {
    Ok(read_definition_record::<ContextDefinitionRecord>(
        &context_definition_root(space_slug)?,
        definition_name,
    )?
    .config)
}

fn build_assembly_search_paths(space_slug: &str) -> Vec<(HierarchyLevel, PathBuf)> {
    let Ok(root) = resolve_root() else {
        return Vec::new();
    };
    vec![
        (HierarchyLevel::Global, root.clone()),
        (HierarchyLevel::Space, root.join("spaces").join(space_slug)),
    ]
}

async fn require_space(
    state: &AppState,
    space_id: Uuid,
) -> Result<swage_storage::Space, GatewayError> {
    state
        .space_store
        .get(space_id)
        .await?
        .ok_or(GatewayError::SpaceNotFound { id: space_id })
}

fn topology_flow_root(space_slug: &str) -> Result<PathBuf, GatewayError> {
    Ok(resolve_root()
        .map_err(|problem| GatewayError::ValidationError(problem.to_string()))?
        .join("spaces")
        .join(space_slug)
        .join(TOPOLOGY_FLOW_ROOT))
}

fn context_definition_root(space_slug: &str) -> Result<PathBuf, GatewayError> {
    Ok(resolve_root()
        .map_err(|problem| GatewayError::ValidationError(problem.to_string()))?
        .join("spaces")
        .join(space_slug)
        .join(CONTEXT_DEFINITION_ROOT))
}

fn validate_definition_name(name: &str) -> Result<String, GatewayError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(GatewayError::ValidationError(
            "definition name must not be empty".into(),
        ));
    }
    if !trimmed
        .chars()
        .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'))
    {
        return Err(GatewayError::ValidationError(
            "definition name must match [a-z0-9-]+".into(),
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_description(description: &str) -> Result<String, GatewayError> {
    let trimmed = description.trim();
    if trimmed.is_empty() {
        return Err(GatewayError::ValidationError(
            "definition description must not be empty".into(),
        ));
    }
    Ok(trimmed.to_string())
}

fn validate_context_config(config: &ContextConfig) -> Result<(), GatewayError> {
    if config.assembly.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "context definition assembly must not be empty".into(),
        ));
    }
    if config.timeout == 0 {
        return Err(GatewayError::ValidationError(
            "context definition timeout must be greater than zero".into(),
        ));
    }
    Ok(())
}

fn read_definition_directory<T>(root: &Path) -> Result<Vec<T>, GatewayError>
where
    T: for<'de> Deserialize<'de>,
{
    let mut definitions = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(definitions),
        Err(error) => {
            return Err(GatewayError::ValidationError(format!(
                "failed to read definition directory {}: {error}",
                root.display()
            )));
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).map_err(|error| {
            GatewayError::ValidationError(format!(
                "failed to read definition record {}: {error}",
                path.display()
            ))
        })?;
        definitions.push(serde_json::from_str(&raw).map_err(|error| {
            GatewayError::ValidationError(format!(
                "failed to parse definition record {}: {error}",
                path.display()
            ))
        })?);
    }
    Ok(definitions)
}

fn read_definition_record<T>(root: &Path, name: &str) -> Result<T, GatewayError>
where
    T: for<'de> Deserialize<'de>,
{
    let path = root.join(format!("{}.json", validate_definition_name(name)?));
    let raw = std::fs::read_to_string(&path).map_err(|error| {
        GatewayError::ValidationError(format!(
            "failed to read definition {}: {error}",
            path.display()
        ))
    })?;
    serde_json::from_str(&raw).map_err(|error| {
        GatewayError::ValidationError(format!(
            "failed to parse definition {}: {error}",
            path.display()
        ))
    })
}

fn write_definition_record<T>(root: &Path, name: &str, value: &T) -> Result<T, GatewayError>
where
    T: Serialize + for<'de> Deserialize<'de> + Clone,
{
    std::fs::create_dir_all(root).map_err(|error| {
        GatewayError::ValidationError(format!(
            "failed to create definition directory {}: {error}",
            root.display()
        ))
    })?;
    let path = root.join(format!("{}.json", validate_definition_name(name)?));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(value)
            .map_err(|error| GatewayError::ValidationError(error.to_string()))?,
    )
    .map_err(|error| {
        GatewayError::ValidationError(format!(
            "failed to write definition {}: {error}",
            path.display()
        ))
    })?;
    Ok(value.clone())
}
