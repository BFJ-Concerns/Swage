use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use swage_context::report::AssemblyReport;
use swage_core::{GatewayError, StorageError};
use swage_storage::{NewSharedReferenceEvent, SharedReferenceKind, Team};

use crate::{
    authoring_definition::{
        ContextDefinitionRecord, TopologyFlowDefinitionRecord, UpdateContextDefinitionRequest,
        UpdateTopologyFlowDefinitionRequest,
    },
    builder_runtime::BuilderConfigurationService,
    service::{TeamAgentPayload, UpdateTeamRequest},
    state::AppState,
};

#[derive(Debug, Clone, Deserialize)]
pub struct SharedReferenceListQuery {
    pub bench_id: Uuid,
    #[serde(default = "default_snapshot_limit")]
    pub snapshot_limit: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SharedReferenceListResponse {
    pub objects: Vec<SharedReferenceSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SharedReferenceSummary {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SharedReferenceDetailResponse {
    pub object: SharedReferenceObject,
    pub interactions: Vec<SharedReferenceInteractionResponse>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SharedReferenceObject {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct SharedReferenceInteractionResponse {
    pub id: Uuid,
    pub event_kind: String,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateSharedReferenceEventRequest {
    pub event_kind: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTeamTopologyReferenceRequest {
    pub entry_agent_id: String,
    #[serde(default)]
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateOrchestrationFlowReferenceRequest {
    pub description: String,
    pub graph: swage_topology::TopologyGraph,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateContextDefinitionReferenceRequest {
    pub description: String,
    pub config: swage_config::ContextConfig,
}

pub async fn list_shared_objects(
    state: &AppState,
    query: SharedReferenceListQuery,
) -> Result<SharedReferenceListResponse, GatewayError> {
    state
        .bench_store
        .get(query.bench_id)
        .await?
        .ok_or(GatewayError::BenchNotFound { id: query.bench_id })?;

    let teams = state.storage.list_teams_by_bench(query.bench_id).await?;
    let snapshots = state
        .storage
        .list_snapshots_by_bench(query.bench_id, query.snapshot_limit)
        .await?;
    let topology_flows = crate::authoring_definition::list_topology_flows(state, query.bench_id)
        .await?
        .flows;
    let context_definitions =
        crate::authoring_definition::list_context_definitions(state, query.bench_id)
            .await?
            .definitions;

    let mut objects: Vec<SharedReferenceSummary> =
        teams.into_iter().map(team_topology_summary).collect();
    objects.extend(snapshots.into_iter().filter_map(context_snapshot_summary));
    objects.extend(
        topology_flows
            .into_iter()
            .map(|flow| orchestration_flow_summary(query.bench_id, flow)),
    );
    objects.extend(
        context_definitions
            .into_iter()
            .map(|definition| context_definition_summary(query.bench_id, definition)),
    );
    objects.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(SharedReferenceListResponse { objects })
}

pub async fn get_shared_object(
    state: &AppState,
    kind: SharedReferenceKind,
    id: &str,
) -> Result<SharedReferenceDetailResponse, GatewayError> {
    let object = load_object(state, kind, id).await?;
    let interactions = state
        .storage
        .list_shared_reference_events(kind, id)
        .await?
        .into_iter()
        .map(|event| SharedReferenceInteractionResponse {
            id: event.id,
            event_kind: event.event_kind,
            payload: event.payload,
            created_at: event.created_at.to_rfc3339(),
        })
        .collect();

    Ok(SharedReferenceDetailResponse {
        object,
        interactions,
    })
}

pub async fn create_shared_object_event(
    state: &AppState,
    kind: SharedReferenceKind,
    id: &str,
    request: CreateSharedReferenceEventRequest,
) -> Result<SharedReferenceInteractionResponse, GatewayError> {
    if request.event_kind.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "event_kind must not be empty".into(),
        ));
    }
    let _ = load_object(state, kind, id).await?;
    let created = state
        .storage
        .create_shared_reference_event(NewSharedReferenceEvent {
            id: Uuid::new_v4(),
            object_kind: kind,
            object_id: id.to_string(),
            event_kind: request.event_kind,
            payload: request.payload,
        })
        .await?;
    Ok(SharedReferenceInteractionResponse {
        id: created.id,
        event_kind: created.event_kind,
        payload: created.payload,
        created_at: created.created_at.to_rfc3339(),
    })
}

pub async fn update_team_topology_reference(
    state: &AppState,
    team_id: Uuid,
    request: UpdateTeamTopologyReferenceRequest,
) -> Result<SharedReferenceDetailResponse, GatewayError> {
    let existing = state.storage.get_team(team_id).await?.ok_or_else(|| {
        GatewayError::Storage(StorageError::NotFound {
            entity: "team".into(),
            id: team_id.to_string(),
        })
    })?;
    let agents = expand_topology_members(&existing, &request.entry_agent_id, &request.members);
    let service = BuilderConfigurationService::new(state.clone());
    service
        .update_team(
            team_id,
            UpdateTeamRequest {
                name: None,
                description: None,
                entry_agent_id: Some(request.entry_agent_id.clone()),
                agents: Some(agents),
                tasks: None,
                members: None,
            },
            None,
            "shared_reference",
        )
        .await?;

    let detail = get_shared_object(
        state,
        SharedReferenceKind::TeamTopology,
        &team_id.to_string(),
    )
    .await?;

    state
        .storage
        .create_shared_reference_event(NewSharedReferenceEvent {
            id: Uuid::new_v4(),
            object_kind: SharedReferenceKind::TeamTopology,
            object_id: team_id.to_string(),
            event_kind: "updated".into(),
            payload: serde_json::json!({
                "entry_agent_id": request.entry_agent_id,
                "members": detail.object.payload["members"].clone(),
            }),
        })
        .await?;

    get_shared_object(
        state,
        SharedReferenceKind::TeamTopology,
        &team_id.to_string(),
    )
    .await
}

pub async fn update_orchestration_flow_reference(
    state: &AppState,
    id: &str,
    request: UpdateOrchestrationFlowReferenceRequest,
) -> Result<SharedReferenceDetailResponse, GatewayError> {
    let (bench_id, flow_name) = parse_bench_scoped_object_id(id, "orchestration flow")?;
    let updated = crate::authoring_definition::update_topology_flow(
        state,
        bench_id,
        &flow_name,
        UpdateTopologyFlowDefinitionRequest {
            description: request.description,
            graph: request.graph,
        },
    )
    .await?;
    record_structural_shared_reference_event(
        state,
        SharedReferenceKind::OrchestrationFlow,
        id,
        serde_json::json!({
            "bench_id": bench_id,
            "name": updated.name,
            "description": updated.description,
            "graph": updated.graph,
        }),
    )
    .await?;
    get_shared_object(state, SharedReferenceKind::OrchestrationFlow, id).await
}

pub async fn update_context_definition_reference(
    state: &AppState,
    id: &str,
    request: UpdateContextDefinitionReferenceRequest,
) -> Result<SharedReferenceDetailResponse, GatewayError> {
    let (bench_id, definition_name) = parse_bench_scoped_object_id(id, "context definition")?;
    let updated = crate::authoring_definition::update_context_definition(
        state,
        bench_id,
        &definition_name,
        UpdateContextDefinitionRequest {
            description: request.description,
            config: request.config,
        },
    )
    .await?;
    record_structural_shared_reference_event(
        state,
        SharedReferenceKind::ContextDefinition,
        id,
        serde_json::json!({
            "bench_id": bench_id,
            "name": updated.name,
            "description": updated.description,
            "config": updated.config,
        }),
    )
    .await?;
    get_shared_object(state, SharedReferenceKind::ContextDefinition, id).await
}

fn expand_topology_members(
    team: &Team,
    entry_agent_id: &str,
    members: &[String],
) -> Vec<TeamAgentPayload> {
    let mut ordered_ids = Vec::with_capacity(members.len() + 1);
    ordered_ids.push(entry_agent_id.to_string());
    for member in members {
        if member != entry_agent_id && !ordered_ids.iter().any(|existing| existing == member) {
            ordered_ids.push(member.clone());
        }
    }

    ordered_ids
        .into_iter()
        .map(|agent_id| {
            let existing = team.agents.iter().find(|agent| agent.id == agent_id);
            TeamAgentPayload {
                id: agent_id.clone(),
                display_name: existing
                    .map(|agent| agent.display_name.clone())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| agent_id.clone()),
                description: existing
                    .map(|agent| agent.description.clone())
                    .unwrap_or_default(),
                instructions: existing
                    .map(|agent| agent.instructions.clone())
                    .unwrap_or_default(),
                model: existing.and_then(|agent| agent.model.clone()),
            }
        })
        .collect()
}

async fn load_object(
    state: &AppState,
    kind: SharedReferenceKind,
    id: &str,
) -> Result<SharedReferenceObject, GatewayError> {
    match kind {
        SharedReferenceKind::TeamTopology => {
            let team_id = id.parse().map_err(|_| {
                GatewayError::ValidationError("team topology id must be a UUID".into())
            })?;
            let team = state.storage.get_team(team_id).await?.ok_or_else(|| {
                GatewayError::Storage(StorageError::NotFound {
                    entity: "team".into(),
                    id: id.to_string(),
                })
            })?;
            Ok(team_topology_object(team))
        }
        SharedReferenceKind::ContextSnapshot => {
            let turn_id = id.parse().map_err(|_| {
                GatewayError::ValidationError("context snapshot id must be a UUID".into())
            })?;
            let report_json = state.storage.get_snapshot(turn_id).await?.ok_or_else(|| {
                GatewayError::Storage(StorageError::NotFound {
                    entity: "context_snapshot".into(),
                    id: id.to_string(),
                })
            })?;
            let report: AssemblyReport = serde_json::from_str(&report_json)
                .map_err(|error| GatewayError::ValidationError(error.to_string()))?;
            Ok(context_snapshot_object(turn_id, report))
        }
        SharedReferenceKind::OrchestrationFlow => {
            let (bench_id, flow_name) = parse_bench_scoped_object_id(id, "orchestration flow")?;
            let flow =
                crate::authoring_definition::get_topology_flow(state, bench_id, &flow_name).await?;
            Ok(orchestration_flow_object(bench_id, flow))
        }
        SharedReferenceKind::ContextDefinition => {
            let (bench_id, definition_name) =
                parse_bench_scoped_object_id(id, "context definition")?;
            let definition = crate::authoring_definition::get_context_definition(
                state,
                bench_id,
                &definition_name,
            )
            .await?;
            Ok(context_definition_object(bench_id, definition))
        }
    }
}

fn team_topology_summary(team: Team) -> SharedReferenceSummary {
    SharedReferenceSummary {
        kind: SharedReferenceKind::TeamTopology.to_string(),
        id: team.id.to_string(),
        title: team.name.clone(),
        subtitle: format!(
            "{} member(s) · lead {}",
            team.members.len(),
            team.entry_agent_id
        ),
        updated_at: team.updated_at.to_rfc3339(),
    }
}

fn context_snapshot_summary(
    snapshot: swage_storage::ContextSnapshotRecord,
) -> Option<SharedReferenceSummary> {
    let report: AssemblyReport = serde_json::from_str(&snapshot.report_json).ok()?;
    Some(SharedReferenceSummary {
        kind: SharedReferenceKind::ContextSnapshot.to_string(),
        id: snapshot.turn_id.to_string(),
        title: format!("Context {}", &snapshot.turn_id.to_string()[..8]),
        subtitle: format!(
            "{} · {} element(s) · {} token(s)",
            report.assembly_name,
            report.elements.len(),
            report.total_tokens
        ),
        updated_at: snapshot.created_at.to_rfc3339(),
    })
}

fn orchestration_flow_summary(
    bench_id: Uuid,
    flow: TopologyFlowDefinitionRecord,
) -> SharedReferenceSummary {
    SharedReferenceSummary {
        kind: SharedReferenceKind::OrchestrationFlow.to_string(),
        id: bench_scoped_object_id(bench_id, &flow.name),
        title: flow.name.clone(),
        subtitle: format!(
            "{} node(s) · {} edge(s)",
            flow.graph.nodes.len(),
            flow.graph.edges.len()
        ),
        updated_at: flow.updated_at.to_rfc3339(),
    }
}

fn context_definition_summary(
    bench_id: Uuid,
    definition: ContextDefinitionRecord,
) -> SharedReferenceSummary {
    SharedReferenceSummary {
        kind: SharedReferenceKind::ContextDefinition.to_string(),
        id: bench_scoped_object_id(bench_id, &definition.name),
        title: definition.name.clone(),
        subtitle: format!(
            "{} · timeout {}s",
            definition.config.assembly, definition.config.timeout
        ),
        updated_at: definition.updated_at.to_rfc3339(),
    }
}

fn team_topology_object(team: Team) -> SharedReferenceObject {
    let nodes: Vec<Value> = team
        .members
        .iter()
        .enumerate()
        .map(|(index, agent_id)| {
            serde_json::json!({
                "agent_id": agent_id,
                "label": agent_id,
                "column": index,
                "is_entry": agent_id == &team.entry_agent_id,
            })
        })
        .collect();
    let edges: Vec<Value> = team
        .members
        .iter()
        .filter(|agent_id| *agent_id != &team.entry_agent_id)
        .map(|agent_id| {
            serde_json::json!({
                "from": team.entry_agent_id,
                "to": agent_id,
                "kind": "delegates_to",
            })
        })
        .collect();

    SharedReferenceObject {
        kind: SharedReferenceKind::TeamTopology.to_string(),
        id: team.id.to_string(),
        title: team.name,
        subtitle: team.description,
        payload: serde_json::json!({
            "team_id": team.id,
            "bench_id": team.bench_id,
            "entry_agent_id": team.entry_agent_id,
            "members": team.members,
            "nodes": nodes,
            "edges": edges,
        }),
    }
}

fn context_snapshot_object(turn_id: Uuid, report: AssemblyReport) -> SharedReferenceObject {
    let assembly_name = report.assembly_name.clone();
    let total_tokens = report.total_tokens;
    let budget = report.budget;
    let warnings = report.warnings.clone();
    let elements: Vec<Value> = report
        .elements
        .iter()
        .enumerate()
        .map(|(index, element)| {
            serde_json::json!({
                "element_id": format!("{}:{index}", element.name),
                "name": element.name,
                "source": element.source,
                "token_count": element.token_count,
                "priority": element.priority,
                "preview": element.preview,
                "child_count": element
                    .named_assembly
                    .as_ref()
                    .map(|assembly| assembly.child_elements.len())
                    .unwrap_or(0),
            })
        })
        .collect();

    SharedReferenceObject {
        kind: SharedReferenceKind::ContextSnapshot.to_string(),
        id: turn_id.to_string(),
        title: format!("Context {}", &turn_id.to_string()[..8]),
        subtitle: format!("{assembly_name} · {total_tokens} token(s)"),
        payload: serde_json::json!({
            "turn_id": turn_id,
            "assembly_name": assembly_name,
            "total_tokens": total_tokens,
            "budget": budget,
            "warnings": warnings,
            "elements": elements,
        }),
    }
}

fn orchestration_flow_object(
    bench_id: Uuid,
    flow: TopologyFlowDefinitionRecord,
) -> SharedReferenceObject {
    let flow_name = flow.name.clone();
    let description = flow.description.clone();
    SharedReferenceObject {
        kind: SharedReferenceKind::OrchestrationFlow.to_string(),
        id: bench_scoped_object_id(bench_id, &flow_name),
        title: flow_name.clone(),
        subtitle: description.clone(),
        payload: serde_json::json!({
            "bench_id": bench_id,
            "name": flow_name,
            "description": description,
            "entry_node_id": flow.graph.entry_node_id,
            "budget": flow.graph.budget,
            "nodes": flow.graph.nodes,
            "edges": flow.graph.edges,
            "created_at": flow.created_at,
            "updated_at": flow.updated_at,
        }),
    }
}

fn context_definition_object(
    bench_id: Uuid,
    definition: ContextDefinitionRecord,
) -> SharedReferenceObject {
    let definition_name = definition.name.clone();
    let description = definition.description.clone();
    SharedReferenceObject {
        kind: SharedReferenceKind::ContextDefinition.to_string(),
        id: bench_scoped_object_id(bench_id, &definition_name),
        title: definition_name.clone(),
        subtitle: description.clone(),
        payload: serde_json::json!({
            "bench_id": bench_id,
            "name": definition_name,
            "description": description,
            "config": definition.config,
            "created_at": definition.created_at,
            "updated_at": definition.updated_at,
        }),
    }
}

fn bench_scoped_object_id(bench_id: Uuid, name: &str) -> String {
    format!("{bench_id}:{name}")
}

fn parse_bench_scoped_object_id(id: &str, label: &str) -> Result<(Uuid, String), GatewayError> {
    let (bench_id, name) = id.split_once(':').ok_or_else(|| {
        GatewayError::ValidationError(format!("{label} id must have the form <bench-id>:<name>"))
    })?;
    let bench_id = bench_id.parse().map_err(|_| {
        GatewayError::ValidationError(format!("{label} id must start with a valid bench UUID"))
    })?;
    if name.trim().is_empty() {
        return Err(GatewayError::ValidationError(format!(
            "{label} id must include a non-empty name"
        )));
    }
    Ok((bench_id, name.to_string()))
}

async fn record_structural_shared_reference_event(
    state: &AppState,
    kind: SharedReferenceKind,
    id: &str,
    payload: Value,
) -> Result<(), GatewayError> {
    state
        .storage
        .create_shared_reference_event(NewSharedReferenceEvent {
            id: Uuid::new_v4(),
            object_kind: kind,
            object_id: id.to_string(),
            event_kind: "updated".into(),
            payload,
        })
        .await?;
    Ok(())
}

fn default_snapshot_limit() -> usize {
    8
}
