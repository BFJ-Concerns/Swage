use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use swage_context::report::AssemblyReport;
use swage_core::{GatewayError, StorageError};
use swage_storage::{NewSharedReferenceEvent, SharedReferenceKind, Team};

use crate::{
    service::{TeamService, UpdateTeamRequest},
    state::AppState,
};

#[derive(Debug, Clone, Deserialize)]
pub struct SharedReferenceListQuery {
    pub project_id: Uuid,
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

pub async fn list_shared_objects(
    state: &AppState,
    query: SharedReferenceListQuery,
) -> Result<SharedReferenceListResponse, GatewayError> {
    state
        .project_store
        .get(query.project_id)
        .await?
        .ok_or(GatewayError::ProjectNotFound {
            id: query.project_id,
        })?;

    let teams = state
        .storage
        .list_teams_by_project(query.project_id)
        .await?;
    let snapshots = state
        .storage
        .list_snapshots_by_project(query.project_id, query.snapshot_limit)
        .await?;

    let mut objects: Vec<SharedReferenceSummary> =
        teams.into_iter().map(team_topology_summary).collect();
    objects.extend(snapshots.into_iter().filter_map(context_snapshot_summary));
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
    let service = TeamService::new(state.storage.clone(), state.project_store.clone());
    service
        .update_team(
            team_id,
            UpdateTeamRequest {
                name: None,
                description: None,
                entry_agent_id: Some(request.entry_agent_id.clone()),
                members: Some(request.members),
            },
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
            "project_id": team.project_id,
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

fn default_snapshot_limit() -> usize {
    8
}
