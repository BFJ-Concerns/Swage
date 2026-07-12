use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use serde_json::Value as JsonValue;
use swage_core::{GatewayError, StorageError};
use swage_storage::{
    model::{NewTeam, Team, TeamAgentDefinition, TeamTaskDefinition},
    store::{BenchStore, Storage},
};

// ---------------------------------------------------------------------------
/// Request body for creating a team within a bench.
#[derive(Debug, Deserialize)]
pub struct CreateTeamRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub entry_agent_id: String,
    #[serde(default)]
    pub agents: Vec<TeamAgentPayload>,
    #[serde(default)]
    pub tasks: Vec<TeamTaskPayload>,
}

/// Request body for partially updating a team.
#[derive(Debug, Deserialize)]
pub struct UpdateTeamRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub entry_agent_id: Option<String>,
    pub agents: Option<Vec<TeamAgentPayload>>,
    pub tasks: Option<Vec<TeamTaskPayload>>,
    #[serde(default)]
    pub members: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TeamAgentPayload {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TeamTaskPayload {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub output_schema: Option<JsonValue>,
    #[serde(default)]
    pub max_output_retries: Option<u32>,
    #[serde(default)]
    pub tool_allow: Option<Vec<String>>,
    #[serde(default)]
    pub tool_deny: Option<Vec<String>>,
}

/// API response for a single team.
#[derive(Debug, Serialize, Deserialize)]
pub struct TeamResponse {
    pub id: Uuid,
    pub bench_id: Uuid,
    pub name: String,
    pub description: String,
    pub entry_agent_id: String,
    pub agents: Vec<TeamAgentPayload>,
    pub tasks: Vec<TeamTaskPayload>,
    pub members: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// API response for the list-teams endpoint.
#[derive(Debug, Serialize)]
pub struct TeamListResponse {
    pub teams: Vec<TeamResponse>,
}

/// Mediates between HTTP team handlers and storage.
#[derive(Clone)]
pub struct TeamService {
    pub(crate) storage: Arc<dyn Storage>,
    pub(crate) bench_store: Arc<dyn BenchStore>,
}

impl TeamService {
    pub fn new(storage: Arc<dyn Storage>, bench_store: Arc<dyn BenchStore>) -> Self {
        Self {
            storage,
            bench_store,
        }
    }

    pub async fn create_team(
        &self,
        bench_id: Uuid,
        req: CreateTeamRequest,
    ) -> Result<TeamResponse, GatewayError> {
        self.ensure_bench_exists(bench_id).await?;
        validate_team_name(&req.name)?;
        validate_agent_id(&req.entry_agent_id, "entry_agent_id")?;

        if self
            .storage
            .get_team_by_name(bench_id, &req.name)
            .await?
            .is_some()
        {
            return Err(GatewayError::Storage(StorageError::ConstraintViolation(
                format!(
                    "a team named '{}' already exists in bench {bench_id}",
                    req.name
                ),
            )));
        }

        let agents = normalise_agents(req.agents, &req.entry_agent_id)?;
        let tasks = normalise_tasks(req.tasks)?;
        let team = self
            .storage
            .create_team(NewTeam {
                bench_id,
                name: req.name,
                description: req.description,
                entry_agent_id: req.entry_agent_id,
                agents,
                tasks,
            })
            .await?;

        Ok(team_to_response(team))
    }

    pub async fn list_teams(&self, bench_id: Uuid) -> Result<TeamListResponse, GatewayError> {
        self.ensure_bench_exists(bench_id).await?;
        let teams = self.storage.list_teams_by_bench(bench_id).await?;
        Ok(TeamListResponse {
            teams: teams.into_iter().map(team_to_response).collect(),
        })
    }

    pub async fn get_team(&self, id: Uuid) -> Result<TeamResponse, GatewayError> {
        self.storage
            .get_team(id)
            .await?
            .map(team_to_response)
            .ok_or_else(|| team_not_found(id))
    }

    pub async fn update_team(
        &self,
        id: Uuid,
        req: UpdateTeamRequest,
    ) -> Result<TeamResponse, GatewayError> {
        let UpdateTeamRequest {
            name,
            description,
            entry_agent_id,
            agents,
            tasks,
            members,
        } = req;

        let existing = self
            .storage
            .get_team(id)
            .await?
            .ok_or_else(|| team_not_found(id))?;

        if let Some(ref name) = name {
            validate_team_name(name)?;
            if let Some(conflict) = self
                .storage
                .get_team_by_name(existing.bench_id, name)
                .await?
                && conflict.id != id
            {
                return Err(GatewayError::Storage(StorageError::ConstraintViolation(
                    format!(
                        "a team named '{}' already exists in bench {}",
                        name, existing.bench_id
                    ),
                )));
            }
        }

        if let Some(ref entry_agent_id) = entry_agent_id {
            validate_agent_id(entry_agent_id, "entry_agent_id")?;
        }

        let resolved_entry_agent_id = entry_agent_id
            .clone()
            .unwrap_or_else(|| existing.entry_agent_id.clone());
        let agents = match (agents, members) {
            (Some(_agents), Some(_)) => {
                return Err(GatewayError::ValidationError(
                    "provide agents or members when updating a team, not both".into(),
                ));
            }
            (Some(agents), None) => Some(normalise_agents(agents, &resolved_entry_agent_id)?),
            (None, Some(member_ids)) => Some(expand_members_into_agents(
                member_ids,
                &resolved_entry_agent_id,
                &existing.agents,
            )?),
            (None, None) if resolved_entry_agent_id != existing.entry_agent_id => Some(
                ensure_entry_agent_present(existing.agents.clone(), &resolved_entry_agent_id)?,
            ),
            (None, None) => None,
        };
        let tasks = tasks.map(normalise_tasks).transpose()?;

        let team = self
            .storage
            .update_team(id, name, description, entry_agent_id, agents, tasks)
            .await?;

        Ok(team_to_response(team))
    }

    pub async fn delete_team(&self, id: Uuid) -> Result<(), GatewayError> {
        if !self.storage.delete_team(id).await? {
            return Err(team_not_found(id));
        }

        Ok(())
    }

    async fn ensure_bench_exists(&self, bench_id: Uuid) -> Result<(), GatewayError> {
        self.bench_store
            .get(bench_id)
            .await?
            .ok_or(GatewayError::BenchNotFound { id: bench_id })?;
        Ok(())
    }
}

fn validate_team_name(name: &str) -> Result<(), GatewayError> {
    if name.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "name must not be empty".into(),
        ));
    }
    Ok(())
}

fn validate_agent_id(agent_id: &str, field_name: &str) -> Result<(), GatewayError> {
    if agent_id.trim().is_empty() {
        return Err(GatewayError::ValidationError(format!(
            "{field_name} must not be empty"
        )));
    }
    Ok(())
}

fn normalise_agents(
    agents: Vec<TeamAgentPayload>,
    entry_agent_id: &str,
) -> Result<Vec<TeamAgentDefinition>, GatewayError> {
    let mut normalised = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let entry_agent_id = entry_agent_id.trim();
    if entry_agent_id.is_empty() {
        return Err(GatewayError::ValidationError(
            "entry_agent_id must not be empty".into(),
        ));
    }

    for agent in agents {
        validate_agent_id(&agent.id, "agents[].id")?;
        let trimmed_id = agent.id.trim().to_string();
        if !seen.insert(trimmed_id.clone()) {
            return Err(GatewayError::ValidationError(format!(
                "agents must not contain duplicate IDs: {trimmed_id}"
            )));
        }
        normalised.push(TeamAgentDefinition {
            id: trimmed_id,
            display_name: agent.display_name.trim().to_string(),
            description: agent.description.trim().to_string(),
            instructions: agent.instructions.trim().to_string(),
            model: agent.model.as_ref().map(|model| model.trim().to_string()),
        });
    }

    if !normalised.iter().any(|agent| agent.id == entry_agent_id) {
        return Err(GatewayError::ValidationError(format!(
            "entry_agent_id '{entry_agent_id}' must match one of the defined team agents"
        )));
    }

    Ok(normalised)
}

fn ensure_entry_agent_present(
    agents: Vec<TeamAgentDefinition>,
    entry_agent_id: &str,
) -> Result<Vec<TeamAgentDefinition>, GatewayError> {
    if agents.iter().any(|agent| agent.id == entry_agent_id) {
        Ok(agents)
    } else {
        Err(GatewayError::ValidationError(format!(
            "entry_agent_id '{entry_agent_id}' must match one of the defined team agents"
        )))
    }
}

fn expand_members_into_agents(
    members: Vec<String>,
    entry_agent_id: &str,
    existing_agents: &[TeamAgentDefinition],
) -> Result<Vec<TeamAgentDefinition>, GatewayError> {
    let mut normalised = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let entry_agent_id = entry_agent_id.trim();
    if entry_agent_id.is_empty() {
        return Err(GatewayError::ValidationError(
            "entry_agent_id must not be empty".into(),
        ));
    }

    if let Some(existing) = existing_agents
        .iter()
        .find(|agent| agent.id == entry_agent_id)
    {
        normalised.push(existing.clone());
        seen.insert(existing.id.clone());
    } else {
        normalised.push(TeamAgentDefinition {
            id: entry_agent_id.to_string(),
            display_name: entry_agent_id.to_string(),
            description: String::new(),
            instructions: String::new(),
            model: None,
        });
        seen.insert(entry_agent_id.to_string());
    }

    for member in members {
        let member = member.trim();
        if member.is_empty() {
            return Err(GatewayError::ValidationError(
                "members must not contain empty agent IDs".into(),
            ));
        }
        if seen.insert(member.to_string()) {
            let existing = existing_agents.iter().find(|agent| agent.id == member);
            normalised.push(existing.cloned().unwrap_or_else(|| TeamAgentDefinition {
                id: member.to_string(),
                display_name: member.to_string(),
                description: String::new(),
                instructions: String::new(),
                model: None,
            }));
        }
    }

    Ok(normalised)
}

fn normalise_tasks(tasks: Vec<TeamTaskPayload>) -> Result<Vec<TeamTaskDefinition>, GatewayError> {
    let mut normalised = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for task in tasks {
        let name = task.name.trim();
        if name.is_empty() {
            return Err(GatewayError::ValidationError(
                "tasks must not contain empty names".into(),
            ));
        }
        if !seen.insert(name.to_string()) {
            return Err(GatewayError::ValidationError(format!(
                "tasks must not contain duplicate names: {name}"
            )));
        }
        normalised.push(TeamTaskDefinition {
            name: name.to_string(),
            description: task.description.trim().to_string(),
            instructions: task.instructions.trim().to_string(),
            model: task.model.as_ref().map(|model| model.trim().to_string()),
            output_schema: task.output_schema,
            max_output_retries: task.max_output_retries,
            tool_allow: task.tool_allow,
            tool_deny: task.tool_deny,
        });
    }

    Ok(normalised)
}

fn team_not_found(id: Uuid) -> GatewayError {
    GatewayError::Storage(StorageError::NotFound {
        entity: "team".into(),
        id: id.to_string(),
    })
}

fn team_to_response(team: Team) -> TeamResponse {
    TeamResponse {
        id: team.id,
        bench_id: team.bench_id,
        name: team.name,
        description: team.description,
        entry_agent_id: team.entry_agent_id,
        agents: team
            .agents
            .into_iter()
            .map(|agent| TeamAgentPayload {
                id: agent.id,
                display_name: agent.display_name,
                description: agent.description,
                instructions: agent.instructions,
                model: agent.model,
            })
            .collect(),
        tasks: team
            .tasks
            .into_iter()
            .map(|task| TeamTaskPayload {
                name: task.name,
                description: task.description,
                instructions: task.instructions,
                model: task.model,
                output_schema: task.output_schema,
                max_output_retries: task.max_output_retries,
                tool_allow: task.tool_allow,
                tool_deny: task.tool_deny,
            })
            .collect(),
        members: team.members,
        created_at: team.created_at,
        updated_at: team.updated_at,
    }
}

// ---------------------------------------------------------------------------
// Topology run service
// ---------------------------------------------------------------------------
