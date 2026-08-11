//! Change cases — the recorded lifecycle every configuration change travels.
//!
//! A change case is a typed semantic operation on a registry resource
//! (never a raw patch): the service validates it, applies it through the
//! resource's own store, derives the diff from before/after snapshots, and
//! writes the version record and audit event in the same flow. Undo re-applies
//! the state immediately before a selected case through the same typed path,
//! recording a new version rather than rewriting history.
//!
//! Under the dogfooding blind-accept posture, applies land immediately —
//! the record, diff, and undo are what survive. Review-at-depth
//! attaches to this same lifecycle when a posture wants it; it is not a
//! separate flow.
//!
//! The apply and its record are separate storage writes, so a crash or
//! storage failure between them can leave an applied change without its
//! case. That window is not silently accepted: a recording failure turns
//! the whole apply into an error naming the applied-but-unrecorded state,
//! so no caller ever reports a recorded change that is not one. Closing
//! the window fully means one transaction spanning the resource store and
//! the version store — a storage-layer seam for when a resource kind
//! needs it, not something to re-derive per kind.
//!
//! Agent definitions are the first registered resource kind. A new kind
//! registers an operation variant set, validation, and apply/snapshot
//! handling here — never a new flow, screen, or version store.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{
    AgentDefinition, NewAgentDefinition, NewConfigurationVersion, NewSchedule, Schedule,
    UpdateSchedule,
};

use crate::state::AppState;

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

/// A typed semantic operation on a registry resource.
///
/// Each variant carries whole intended state, not a patch — the service
/// derives the diff. Risk bands are declared per operation kind so review
/// depth (when a posture uses it) is a rule, not a guess.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ChangeOperation {
    /// Create or replace an agent definition.
    UpsertAgentDefinition { definition: NewAgentDefinition },
    /// Remove an agent definition.
    DeleteAgentDefinition {
        agent_id: String,
        space_id: Option<Uuid>,
    },
    /// Create a schedule from its complete intended state.
    CreateSchedule { schedule: NewSchedule },
    /// Edit an existing schedule, re-arming its lifecycle only when its
    /// timing was explicitly replaced.
    UpdateSchedule { schedule: NewSchedule, rearm: bool },
    /// Pause a schedule without replaying scheduler-owned timestamps.
    PauseSchedule { schedule: NewSchedule },
    /// Resume a schedule with a freshly computed next occurrence.
    ResumeSchedule { schedule: NewSchedule },
    /// Remove a schedule.
    DeleteSchedule {
        schedule_id: Uuid,
        space_id: Option<Uuid>,
    },
}

/// How much scrutiny an operation kind warrants. Declared with the
/// operation, read by review flows; blind-accept records it and proceeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskBand {
    /// Reversible configuration with undo — the fast lane.
    Routine,
    /// Changes that alter what an agent is allowed to do.
    Sensitive,
}

impl RiskBand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Routine => "routine",
            Self::Sensitive => "sensitive",
        }
    }
}

impl ChangeOperation {
    fn entity_kind(&self) -> &'static str {
        match self {
            Self::UpsertAgentDefinition { .. } | Self::DeleteAgentDefinition { .. } => {
                "agent_definition"
            }
            Self::CreateSchedule { .. }
            | Self::UpdateSchedule { .. }
            | Self::PauseSchedule { .. }
            | Self::ResumeSchedule { .. }
            | Self::DeleteSchedule { .. } => "schedule",
        }
    }

    /// The version-chain identity. Agent ids are unique only within a scope,
    /// so the scope is part of the identity — a global `assistant` and a
    /// space-scoped `assistant` must never share a version chain.
    fn entity_id(&self) -> String {
        let (id, space_id) = match self {
            Self::UpsertAgentDefinition { definition } => {
                (definition.id.as_str(), definition.space_id)
            }
            Self::DeleteAgentDefinition { agent_id, space_id } => (agent_id.as_str(), *space_id),
            Self::CreateSchedule { schedule }
            | Self::UpdateSchedule { schedule, .. }
            | Self::PauseSchedule { schedule }
            | Self::ResumeSchedule { schedule } => return schedule.id.to_string(),
            Self::DeleteSchedule { schedule_id, .. } => return schedule_id.to_string(),
        };
        scoped_entity_id(id, space_id)
    }

    fn operation_kind(&self) -> &'static str {
        match self {
            Self::UpsertAgentDefinition { .. } => "upsert_agent_definition",
            Self::DeleteAgentDefinition { .. } => "delete_agent_definition",
            Self::CreateSchedule { .. } => "create_schedule",
            Self::UpdateSchedule { .. } => "update_schedule",
            Self::PauseSchedule { .. } => "pause_schedule",
            Self::ResumeSchedule { .. } => "resume_schedule",
            Self::DeleteSchedule { .. } => "delete_schedule",
        }
    }

    async fn resolve_space_id(&self, state: &AppState) -> Result<Option<Uuid>, GatewayError> {
        Ok(match self {
            Self::UpsertAgentDefinition { definition } => definition.space_id,
            Self::DeleteAgentDefinition { space_id, .. } => *space_id,
            Self::CreateSchedule { schedule }
            | Self::UpdateSchedule { schedule, .. }
            | Self::PauseSchedule { schedule }
            | Self::ResumeSchedule { schedule } => {
                resolve_schedule_space_id(state, &schedule.session_policy).await?
            }
            Self::DeleteSchedule { space_id, .. } => *space_id,
        })
    }

    /// Risk bands live with the operation kind so audit and later review can
    /// distinguish standing authority changes from ordinary state changes.
    pub fn risk_band(&self) -> RiskBand {
        match self {
            Self::UpsertAgentDefinition { definition } => {
                if definition.tool_permissions.is_some() {
                    RiskBand::Sensitive
                } else {
                    RiskBand::Routine
                }
            }
            Self::DeleteAgentDefinition { .. } => RiskBand::Routine,
            Self::CreateSchedule { .. } | Self::DeleteSchedule { .. } => RiskBand::Sensitive,
            Self::UpdateSchedule { .. }
            | Self::PauseSchedule { .. }
            | Self::ResumeSchedule { .. } => RiskBand::Routine,
        }
    }

    fn validate(&self) -> Result<(), GatewayError> {
        match self {
            Self::UpsertAgentDefinition { definition } => {
                if definition.id.trim().is_empty() {
                    return Err(GatewayError::ValidationError(
                        "an agent definition needs an id".into(),
                    ));
                }
                if !definition
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                {
                    return Err(GatewayError::ValidationError(
                        "agent ids are lowercase ASCII letters, digits, and hyphens".into(),
                    ));
                }
                if definition.display_name.trim().is_empty() {
                    return Err(GatewayError::ValidationError(
                        "an agent definition needs a display name".into(),
                    ));
                }
                if let Some(overlay) = &definition.tool_permissions {
                    serde_json::from_value::<swage_config::model::ToolPermissionConfig>(
                        overlay.clone(),
                    )
                    .map_err(|error| {
                        GatewayError::ValidationError(format!(
                            "the permission overlay is not a permission configuration: {error}"
                        ))
                    })?;
                }
                if definition
                    .skills
                    .iter()
                    .any(|skill| skill.trim().is_empty())
                {
                    return Err(GatewayError::ValidationError(
                        "declared skill names must not be empty".into(),
                    ));
                }
                let mut unique_skills = std::collections::HashSet::new();
                if definition
                    .skills
                    .iter()
                    .any(|skill| !unique_skills.insert(skill))
                {
                    return Err(GatewayError::ValidationError(
                        "declared skill names must be unique".into(),
                    ));
                }
                if let Some(schema) = &definition.output_schema {
                    jsonschema::validator_for(schema).map_err(|error| {
                        GatewayError::ValidationError(format!(
                            "the output contract is not a valid JSON Schema: {error}"
                        ))
                    })?;
                }
                Ok(())
            }
            Self::DeleteAgentDefinition { agent_id, .. } => {
                if agent_id.trim().is_empty() {
                    return Err(GatewayError::ValidationError(
                        "deleting a definition needs its agent id".into(),
                    ));
                }
                Ok(())
            }
            Self::CreateSchedule { schedule }
            | Self::UpdateSchedule { schedule, .. }
            | Self::PauseSchedule { schedule }
            | Self::ResumeSchedule { schedule } => validate_schedule_shape(schedule),
            Self::DeleteSchedule { .. } => Ok(()),
        }
    }

    async fn validate_for_apply(&self, state: &AppState) -> Result<(), GatewayError> {
        self.validate()?;
        match self {
            Self::CreateSchedule { schedule }
            | Self::UpdateSchedule { schedule, .. }
            | Self::PauseSchedule { schedule }
            | Self::ResumeSchedule { schedule } => {
                crate::schedule::validate_resolved_schedule(state, schedule).await
            }
            _ => Ok(()),
        }
    }
}

pub(crate) async fn resolve_schedule_space_id(
    state: &AppState,
    session_policy: &swage_storage::SessionPolicy,
) -> Result<Option<Uuid>, GatewayError> {
    match session_policy {
        swage_storage::SessionPolicy::Fresh { space_id, .. } => Ok(Some(*space_id)),
        swage_storage::SessionPolicy::Continuation { session_id } => state
            .storage
            .get_session(*session_id)
            .await?
            .map(|session| Some(session.space_id))
            .ok_or(GatewayError::ConversationNotFound { id: *session_id }),
    }
}

fn validate_schedule_shape(schedule: &NewSchedule) -> Result<(), GatewayError> {
    if schedule.message.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "schedule message must not be empty".into(),
        ));
    }
    if schedule.expression.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "schedule expression must not be empty".into(),
        ));
    }
    Ok(())
}

/// One version-chain identity per (resource id, scope). The rendered form is
/// readable in version listings; `agent_id_from_entity` inverts it for
/// undo.
fn scoped_entity_id(id: &str, space_id: Option<Uuid>) -> String {
    match space_id {
        Some(space) => format!("{id}@{space}"),
        None => format!("{id}@global"),
    }
}

fn agent_id_from_entity(entity_id: &str) -> &str {
    entity_id.rsplit_once('@').map_or(entity_id, |(id, _)| id)
}

// ---------------------------------------------------------------------------
// The applied case
// ---------------------------------------------------------------------------

/// The durable outcome of an applied change case.
#[derive(Debug, Clone, Serialize)]
pub struct AppliedCase {
    /// The version record's id — the handle undo takes.
    pub version_id: Uuid,
    pub entity_kind: String,
    pub entity_id: String,
    pub summary: String,
    pub risk_band: RiskBand,
}

/// Apply one typed operation as a recorded change case.
///
/// `source` names the surface the change came from (a tool, the HTTP API) —
/// provenance for the audit trail. `session_id` ties the case to the
/// conversation that authored it, when one did.
pub async fn apply_change(
    state: &AppState,
    operation: ChangeOperation,
    source: &str,
    session_id: Option<Uuid>,
) -> Result<AppliedCase, GatewayError> {
    operation.validate_for_apply(state).await?;

    let entity_kind = operation.entity_kind();
    let entity_id = operation.entity_id();
    let space_id = operation.resolve_space_id(state).await?;

    // Snapshot before, apply through the resource's own store, snapshot
    // after: the diff is derived, never supplied. Snapshots address the
    // resource by its bare id; the scoped entity_id is version-chain
    // identity only.
    let before = snapshot_resource(state, entity_kind, &entity_id, space_id).await?;

    let summary = match &operation {
        ChangeOperation::UpsertAgentDefinition { definition } => {
            let scope = definition
                .space_id
                .map_or_else(|| "globally".to_string(), |id| format!("in space {id}"));
            let action = if before.is_some() {
                "updated"
            } else {
                "created"
            };
            state
                .storage
                .upsert_agent_definition(definition.clone())
                .await?;
            format!("{action} agent definition '{}' {scope}", definition.id)
        }
        ChangeOperation::DeleteAgentDefinition { agent_id, space_id } => {
            let removed = state
                .storage
                .delete_agent_definition(agent_id, *space_id)
                .await?;
            if !removed {
                return Err(GatewayError::ValidationError(format!(
                    "no agent definition '{agent_id}' exists at that scope"
                )));
            }
            format!("deleted agent definition '{agent_id}'")
        }
        ChangeOperation::CreateSchedule { schedule } => {
            if before.is_some() {
                return Err(GatewayError::ValidationError(format!(
                    "schedule {} already exists",
                    schedule.id
                )));
            }
            state.storage.create_schedule(schedule.clone()).await?;
            format!("created schedule '{}'", schedule.message)
        }
        ChangeOperation::UpdateSchedule { schedule, rearm } => {
            apply_schedule_edit(state, schedule, *rearm).await?;
            format!("updated schedule '{}'", schedule.message)
        }
        ChangeOperation::PauseSchedule { schedule } => {
            apply_schedule_pause(state, schedule).await?;
            format!("paused schedule '{}'", schedule.message)
        }
        ChangeOperation::ResumeSchedule { schedule } => {
            apply_schedule_resume(state, schedule).await?;
            format!("resumed schedule '{}'", schedule.message)
        }
        ChangeOperation::DeleteSchedule { schedule_id, .. } => {
            if !state.storage.delete_schedule(*schedule_id).await? {
                return Err(GatewayError::ValidationError(format!(
                    "no schedule with id {schedule_id}"
                )));
            }
            format!("deleted schedule {schedule_id}")
        }
    };

    let after = snapshot_resource(state, entity_kind, &entity_id, space_id).await?;
    record_case(
        state,
        RecordCase {
            entity_kind,
            entity_id: &entity_id,
            operation_kind: operation.operation_kind(),
            risk_band: operation.risk_band(),
            space_id,
            session_id,
            source,
            summary: &summary,
            before,
            after,
            undone_version_id: None,
        },
    )
    .await
    .map_err(|error| {
        // The apply landed but its record did not: the change exists without
        // the case the commission requires. Surface it as the error it is —
        // callers must not report a recorded change — and name the applied
        // state so the operator can reconcile.
        GatewayError::ConfigError(format!(
            "'{summary}' was applied but recording its change case failed ({error}); \
             the configuration is changed without a record — reconcile before further changes"
        ))
    })
}

/// Undo a recorded case by restoring the resource's immediately preceding state.
///
/// The predecessor snapshot re-applies through the same typed path as any
/// change. When the selected case has no predecessor, undo restores absence.
/// Undo is itself a recorded case pointing at the case it reverses, so history
/// only ever grows.
pub async fn undo_case(
    state: &AppState,
    case_id: Uuid,
    source: &str,
    session_id: Option<Uuid>,
) -> Result<AppliedCase, GatewayError> {
    let selected_case = state
        .storage
        .get_configuration_version(case_id)
        .await?
        .ok_or_else(|| {
            GatewayError::ValidationError(format!("no recorded configuration case {case_id}"))
        })?;

    if selected_case.entity_kind != "agent_definition" && selected_case.entity_kind != "schedule" {
        return Err(GatewayError::ValidationError(format!(
            "no change-case handler is registered for '{}' resources",
            selected_case.entity_kind
        )));
    }

    let target_snapshot =
        match selected_case.previous_version_id {
            Some(previous_id) => state
                .storage
                .get_configuration_version(previous_id)
                .await?
                .ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "configuration case {case_id} refers to missing predecessor {previous_id}"
                    ))
                })?
                .snapshot,
            None => serde_json::Value::Null,
        };
    let space_id = selected_case.space_id;
    let before = snapshot_resource(
        state,
        &selected_case.entity_kind,
        &selected_case.entity_id,
        space_id,
    )
    .await?;

    // A null target means the resource did not exist before the selected case.
    let summary = match selected_case.entity_kind.as_str() {
        "agent_definition" => {
            let agent_id = agent_id_from_entity(&selected_case.entity_id).to_owned();
            if target_snapshot.is_null() {
                let removed = state
                    .storage
                    .delete_agent_definition(&agent_id, space_id)
                    .await?;
                if removed {
                    format!(
                        "undid configuration case {case_id}: removed agent definition '{agent_id}'"
                    )
                } else {
                    format!(
                        "undid configuration case {case_id}: agent definition '{agent_id}' was already absent; nothing was removed"
                    )
                }
            } else {
                let definition: AgentDefinition =
                    parse_snapshot(&target_snapshot, "agent definition")?;
                state
                    .storage
                    .upsert_agent_definition(NewAgentDefinition {
                        id: definition.id.clone(),
                        space_id: definition.space_id,
                        display_name: definition.display_name,
                        description: definition.description,
                        instructions: definition.instructions,
                        model_id: definition.model_id,
                        tool_permissions: definition.tool_permissions,
                        skills: definition.skills,
                        output_schema: definition.output_schema,
                        output_max_retries: definition.output_max_retries,
                    })
                    .await?;
                format!(
                    "undid configuration case {case_id}: restored agent definition '{agent_id}'"
                )
            }
        }
        "schedule" => {
            let schedule_id = Uuid::parse_str(&selected_case.entity_id).map_err(|error| {
                GatewayError::ValidationError(format!(
                    "recorded schedule identity is invalid: {error}"
                ))
            })?;
            if target_snapshot.is_null() {
                let removed = state.storage.delete_schedule(schedule_id).await?;
                if removed {
                    format!("undid configuration case {case_id}: removed schedule {schedule_id}")
                } else {
                    format!(
                        "undid configuration case {case_id}: schedule {schedule_id} was already absent; nothing was removed"
                    )
                }
            } else {
                let schedule: Schedule = parse_snapshot(&target_snapshot, "schedule")?;
                let intended = new_schedule_from_snapshot(schedule);
                if state.storage.get_schedule(schedule_id).await?.is_some() {
                    apply_complete_schedule(state, &intended).await?;
                } else {
                    state.storage.create_schedule(intended).await?;
                }
                format!("undid configuration case {case_id}: restored schedule {schedule_id}")
            }
        }
        _ => unreachable!("entity kind checked above"),
    };

    let after = snapshot_resource(
        state,
        &selected_case.entity_kind,
        &selected_case.entity_id,
        space_id,
    )
    .await?;
    record_case(
        state,
        RecordCase {
            entity_kind: &selected_case.entity_kind,
            entity_id: &selected_case.entity_id,
            operation_kind: "undo",
            risk_band: RiskBand::Routine,
            space_id,
            session_id,
            source,
            summary: &summary,
            before,
            after,
            undone_version_id: Some(case_id),
        },
    )
    .await
    .map_err(|error| {
        // The undo landed but its record did not. As with an ordinary apply,
        // make the incomplete lifecycle explicit so the operator knows the
        // resource and its audit trail may now disagree.
        GatewayError::ConfigError(format!(
            "'{summary}' was applied but recording its undo case failed ({error}); \
             the configuration may be changed without a complete record — reconcile before further changes"
        ))
    })
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

struct RecordCase<'a> {
    entity_kind: &'a str,
    entity_id: &'a str,
    operation_kind: &'a str,
    risk_band: RiskBand,
    space_id: Option<Uuid>,
    session_id: Option<Uuid>,
    source: &'a str,
    summary: &'a str,
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
    undone_version_id: Option<Uuid>,
}

async fn snapshot_agent_definition(
    state: &AppState,
    agent_id: &str,
    space_id: Option<Uuid>,
) -> Result<Option<serde_json::Value>, GatewayError> {
    let definitions = state.storage.list_agent_definitions(space_id).await?;
    Ok(definitions
        .into_iter()
        .find(|definition| definition.id == agent_id)
        .map(|definition| serde_json::to_value(definition).unwrap_or(serde_json::Value::Null)))
}

async fn snapshot_resource(
    state: &AppState,
    entity_kind: &str,
    entity_id: &str,
    space_id: Option<Uuid>,
) -> Result<Option<serde_json::Value>, GatewayError> {
    match entity_kind {
        "agent_definition" => {
            snapshot_agent_definition(state, agent_id_from_entity(entity_id), space_id).await
        }
        "schedule" => {
            let id = Uuid::parse_str(entity_id).map_err(|error| {
                GatewayError::ValidationError(format!("schedule identity is invalid: {error}"))
            })?;
            Ok(state
                .storage
                .get_schedule(id)
                .await?
                .map(|schedule| serde_json::to_value(schedule).unwrap_or(serde_json::Value::Null)))
        }
        other => Err(GatewayError::ValidationError(format!(
            "no change-case handler is registered for '{other}' resources"
        ))),
    }
}

fn parse_snapshot<T: serde::de::DeserializeOwned>(
    snapshot: &serde_json::Value,
    kind: &str,
) -> Result<T, GatewayError> {
    serde_json::from_value(snapshot.clone()).map_err(|error| {
        GatewayError::ValidationError(format!(
            "recorded snapshot no longer parses as a {kind}: {error}"
        ))
    })
}

fn new_schedule_from_snapshot(schedule: Schedule) -> NewSchedule {
    NewSchedule {
        id: schedule.id,
        message: schedule.message,
        status: schedule.status,
        kind: schedule.kind,
        expression: schedule.expression,
        session_policy: schedule.session_policy,
        creator: schedule.creator,
        delivery_routing: schedule.delivery_routing,
        gate: schedule.gate,
        next_run_at: schedule.next_run_at,
        last_run_at: schedule.last_run_at,
    }
}

async fn apply_complete_schedule(
    state: &AppState,
    schedule: &NewSchedule,
) -> Result<Schedule, GatewayError> {
    state
        .storage
        .update_schedule(
            schedule.id,
            UpdateSchedule {
                message: Some(schedule.message.clone()),
                status: Some(schedule.status),
                kind: Some(schedule.kind),
                expression: Some(schedule.expression.clone()),
                delivery_routing: Some(schedule.delivery_routing.clone()),
                gate: Some(schedule.gate.clone()),
                next_run_at: Some(schedule.next_run_at),
                last_run_at: Some(schedule.last_run_at),
            },
        )
        .await
        .map_err(Into::into)
}

/// Apply configuration without replaying lifecycle fields read before the
/// scheduler's most recent tick. Timing edits deliberately re-arm
/// status/next-run; ordinary edits leave all scheduler-owned state in place,
/// including `last_run_at`.
async fn apply_schedule_edit(
    state: &AppState,
    schedule: &NewSchedule,
    rearm: bool,
) -> Result<Schedule, GatewayError> {
    state
        .storage
        .update_schedule(
            schedule.id,
            UpdateSchedule {
                message: Some(schedule.message.clone()),
                status: rearm.then_some(schedule.status),
                kind: Some(schedule.kind),
                expression: Some(schedule.expression.clone()),
                delivery_routing: Some(schedule.delivery_routing.clone()),
                gate: Some(schedule.gate.clone()),
                next_run_at: rearm.then_some(schedule.next_run_at),
                last_run_at: None,
            },
        )
        .await
        .map_err(Into::into)
}

async fn apply_schedule_pause(
    state: &AppState,
    schedule: &NewSchedule,
) -> Result<Schedule, GatewayError> {
    state
        .storage
        .update_schedule(
            schedule.id,
            UpdateSchedule {
                status: Some(schedule.status),
                ..Default::default()
            },
        )
        .await
        .map_err(Into::into)
}

async fn apply_schedule_resume(
    state: &AppState,
    schedule: &NewSchedule,
) -> Result<Schedule, GatewayError> {
    state
        .storage
        .update_schedule(
            schedule.id,
            UpdateSchedule {
                status: Some(schedule.status),
                next_run_at: Some(schedule.next_run_at),
                ..Default::default()
            },
        )
        .await
        .map_err(Into::into)
}

async fn record_case(state: &AppState, case: RecordCase<'_>) -> Result<AppliedCase, GatewayError> {
    let previous = state
        .storage
        .get_latest_configuration_version(case.entity_kind, case.entity_id)
        .await?;
    let version_number = previous.as_ref().map_or(1, |v| v.version_number + 1);

    let version_id = Uuid::new_v4();
    let snapshot = case.after.clone().unwrap_or(serde_json::Value::Null);
    let diff = derive_diff(case.before.as_ref(), case.after.as_ref());

    state
        .storage
        .create_configuration_version(NewConfigurationVersion {
            id: version_id,
            space_id: case.space_id,
            session_id: case.session_id,
            entity_kind: case.entity_kind.to_owned(),
            entity_id: case.entity_id.to_owned(),
            version_number,
            operation_kind: case.operation_kind.to_owned(),
            source: case.source.to_owned(),
            summary: case.summary.to_owned(),
            snapshot,
            diff,
            previous_version_id: previous.map(|v| v.id),
            undone_version_id: case.undone_version_id,
        })
        .await?;

    // The audit event is part of the apply, not a courtesy: a change that
    // cannot be recorded is a change that did not happen cleanly, surfaced.
    state
        .storage
        .create_audit_event(swage_storage::NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: case.session_id,
            entity_kind: case.entity_kind.to_owned(),
            entity_id: case.entity_id.to_owned(),
            event_kind: "configuration_change".to_owned(),
            summary: case.summary.to_owned(),
            metadata: serde_json::json!({
                "operation": case.operation_kind,
                "version_id": version_id,
                "risk_band": case.risk_band,
                "source": case.source,
            }),
        })
        .await?;

    Ok(AppliedCase {
        version_id,
        entity_kind: case.entity_kind.to_owned(),
        entity_id: case.entity_id.to_owned(),
        summary: case.summary.to_owned(),
        risk_band: case.risk_band,
    })
}

/// Field-level diff of two snapshots: for each top-level field that changed,
/// `{ "field": { "from": …, "to": … } }`. Creation and deletion diff against
/// null. Derived here so no caller ever supplies its own account of what
/// changed.
fn derive_diff(
    before: Option<&serde_json::Value>,
    after: Option<&serde_json::Value>,
) -> serde_json::Value {
    use serde_json::{Map, Value};

    let empty = Map::new();
    let before_map = before.and_then(Value::as_object).unwrap_or(&empty);
    let after_map = after.and_then(Value::as_object).unwrap_or(&empty);

    let mut diff = Map::new();
    let keys: std::collections::BTreeSet<&String> =
        before_map.keys().chain(after_map.keys()).collect();
    for key in keys {
        // Timestamps churn on every write and carry no intent.
        if key == "created_at" || key == "updated_at" {
            continue;
        }
        let from = before_map.get(key).cloned().unwrap_or(Value::Null);
        let to = after_map.get(key).cloned().unwrap_or(Value::Null);
        if from != to {
            diff.insert(key.clone(), serde_json::json!({ "from": from, "to": to }));
        }
    }
    Value::Object(diff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_validate_before_touching_anything() {
        let bad_id = ChangeOperation::UpsertAgentDefinition {
            definition: NewAgentDefinition {
                id: "Not Valid".into(),
                space_id: None,
                display_name: "x".into(),
                description: String::new(),
                instructions: String::new(),
                model_id: None,
                tool_permissions: None,
                skills: vec![],
                output_schema: None,
                output_max_retries: None,
            },
        };
        assert!(bad_id.validate().is_err());

        let bad_overlay = ChangeOperation::UpsertAgentDefinition {
            definition: NewAgentDefinition {
                id: "ok".into(),
                space_id: None,
                display_name: "x".into(),
                description: String::new(),
                instructions: String::new(),
                model_id: None,
                tool_permissions: Some(serde_json::json!("not a permission config")),
                skills: vec![],
                output_schema: None,
                output_max_retries: None,
            },
        };
        assert!(bad_overlay.validate().is_err());
    }

    #[test]
    fn risk_band_is_declared_by_operation_content() {
        let routine = ChangeOperation::UpsertAgentDefinition {
            definition: NewAgentDefinition {
                id: "plain".into(),
                space_id: None,
                display_name: "Plain".into(),
                description: String::new(),
                instructions: "be helpful".into(),
                model_id: None,
                tool_permissions: None,
                skills: vec![],
                output_schema: None,
                output_max_retries: None,
            },
        };
        assert_eq!(routine.risk_band(), RiskBand::Routine);

        let sensitive = ChangeOperation::UpsertAgentDefinition {
            definition: NewAgentDefinition {
                id: "scoped".into(),
                space_id: None,
                display_name: "Scoped".into(),
                description: String::new(),
                instructions: String::new(),
                model_id: None,
                tool_permissions: Some(serde_json::json!({"allow": [], "deny": []})),
                skills: vec![],
                output_schema: None,
                output_max_retries: None,
            },
        };
        assert_eq!(
            sensitive.risk_band(),
            RiskBand::Sensitive,
            "permission-bearing changes carry the higher band"
        );
    }

    #[test]
    fn diff_is_field_level_and_ignores_timestamps() {
        let before = serde_json::json!({
            "id": "researcher",
            "instructions": "old",
            "updated_at": "2026-01-01T00:00:00Z"
        });
        let after = serde_json::json!({
            "id": "researcher",
            "instructions": "new",
            "updated_at": "2026-02-02T00:00:00Z"
        });

        let diff = derive_diff(Some(&before), Some(&after));
        assert_eq!(diff["instructions"]["from"], "old");
        assert_eq!(diff["instructions"]["to"], "new");
        assert!(diff.get("id").is_none(), "unchanged fields stay out");
        assert!(
            diff.get("updated_at").is_none(),
            "timestamps carry no intent"
        );
    }

    #[test]
    fn creation_diffs_against_null() {
        let after = serde_json::json!({ "id": "fresh", "instructions": "hi" });
        let diff = derive_diff(None, Some(&after));
        assert_eq!(diff["id"]["from"], serde_json::Value::Null);
        assert_eq!(diff["id"]["to"], "fresh");
    }
}
