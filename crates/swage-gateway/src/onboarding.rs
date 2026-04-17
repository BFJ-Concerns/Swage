use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{
    ChannelLink, HeartbeatStatus, NewChannelLink, NewHeartbeat, NewScheduledJob, NewSession,
    NewThread, ScheduledJobKind, ScheduledJobStatus, model::Project,
};

use crate::{
    channel_gateway::{DeliveryTarget, ExternalChannelIntegration},
    execution_host::{ExecutionHostResponse, RegisterExecutionHostRequest, register_remote_execution_host},
    state::{AppState, PairingCodeRecord},
};

#[derive(Debug, Clone, Deserialize)]
pub struct CreateChannelLinkRequest {
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub project_id: Option<Uuid>,
    pub agent_id: Option<String>,
    pub home_target: Option<DeliveryTarget>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelLinkResponse {
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub project_id: Uuid,
    pub agent_id: String,
    pub session_id: Option<Uuid>,
    pub home_target: Option<serde_json::Value>,
    pub linked_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelLinkListResponse {
    pub links: Vec<ChannelLinkResponse>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateNodePairingCodeRequest {
    pub expires_in_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodePairingCodeResponse {
    pub pairing_code: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RedeemNodePairingRequest {
    pub pairing_code: String,
    pub host_id: String,
    pub label: Option<String>,
    #[serde(default = "default_attached_node_kind")]
    pub kind: String,
    pub base_url: String,
    pub shared_token: String,
    pub workspace_root: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayRecoveryBundle {
    pub version: u32,
    pub projects: Vec<RecoveryProjectRecord>,
    pub sessions: Vec<RecoverySessionRecord>,
    pub channel_links: Vec<RecoveryChannelLinkRecord>,
    pub execution_hosts: Vec<RecoveryExecutionHostRecord>,
    pub scheduled_jobs: Vec<RecoveryScheduledJobRecord>,
    pub heartbeats: Vec<RecoveryHeartbeatRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImportGatewayRecoveryRequest {
    pub bundle: GatewayRecoveryBundle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryProjectRecord {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub workspace_path: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoverySessionRecord {
    pub id: Uuid,
    pub project_id: Uuid,
    pub agent_id: String,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryChannelLinkRecord {
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub project_id: Uuid,
    pub agent_id: String,
    pub session_id: Option<Uuid>,
    pub home_target: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryExecutionHostRecord {
    pub host_id: String,
    pub label: Option<String>,
    pub kind: String,
    pub base_url: String,
    pub shared_token: String,
    pub workspace_root: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryScheduledJobRecord {
    pub session_id: Uuid,
    pub agent_id: String,
    pub message: String,
    pub status: String,
    pub kind: String,
    pub schedule_expression: String,
    pub delivery_routing: serde_json::Value,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryHeartbeatRecord {
    pub session_id: Uuid,
    pub agent_id: String,
    pub message: String,
    pub interval_seconds: i64,
    pub status: String,
    pub last_beat_at: Option<DateTime<Utc>>,
    pub next_beat_at: Option<DateTime<Utc>>,
}

fn default_attached_node_kind() -> String {
    "attached_node".to_string()
}

fn map_channel_link_response(link: ChannelLink) -> ChannelLinkResponse {
    ChannelLinkResponse {
        integration: link.integration,
        account: link.account,
        conversation: link.conversation,
        project_id: link.project_id,
        agent_id: link.agent_id,
        session_id: link.session_id,
        home_target: link.home_target,
        linked_at: link.linked_at,
        updated_at: link.updated_at,
    }
}

fn map_execution_host_response(
    host: crate::execution_host::RegisteredExecutionHost,
) -> ExecutionHostResponse {
    ExecutionHostResponse {
        host_id: host.host_id,
        label: host.label,
        kind: host.kind.to_string(),
        base_url: host.base_url,
        workspace_root: host.workspace_root.to_string_lossy().into_owned(),
        capabilities: host.capabilities,
        registered_at: host.registered_at,
    }
}

pub async fn create_channel_link(
    state: &AppState,
    request: CreateChannelLinkRequest,
) -> Result<ChannelLinkResponse, GatewayError> {
    let integration = ExternalChannelIntegration::parse(request.integration.trim())?;
    if request.account.trim().is_empty() || request.conversation.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "channel account and conversation must not be empty".into(),
        ));
    }

    let project = match request.project_id {
        Some(project_id) => state
            .project_store
            .get(project_id)
            .await?
            .ok_or_else(|| GatewayError::ValidationError(format!("project {project_id} not found")))?,
        None => state.project_store.get_or_create_default().await?,
    };
    let agent_id = request.agent_id.unwrap_or_else(|| "gateway".to_string());
    let metadata = serde_json::json!({
        "channel_origin": {
            "integration": integration.as_metadata_value(),
            "account": request.account,
            "conversation": request.conversation
        }
    });
    let session = state
        .storage
        .create_session(NewSession {
            project_id: project.id,
            agent_id: agent_id.clone(),
            metadata: Some(metadata),
        })
        .await?;
    state
        .storage
        .create(NewThread {
            session_id: session.id,
            agent_id: agent_id.clone(),
        })
        .await?;

    let link = state
        .storage
        .upsert_channel_link(NewChannelLink {
            integration: integration.as_metadata_value().to_string(),
            account: request.account,
            conversation: request.conversation,
            project_id: project.id,
            agent_id,
            session_id: Some(session.id),
            home_target: request
                .home_target
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| {
                    GatewayError::ValidationError(format!(
                        "invalid channel home_target payload: {error}"
                    ))
                })?,
        })
        .await?;

    Ok(map_channel_link_response(link))
}

pub async fn list_channel_links(state: &AppState) -> Result<ChannelLinkListResponse, GatewayError> {
    let mut links = state.storage.list_channel_links().await?;
    links.sort_by(|left, right| {
        left.integration
            .cmp(&right.integration)
            .then(left.account.cmp(&right.account))
            .then(left.conversation.cmp(&right.conversation))
    });
    Ok(ChannelLinkListResponse {
        links: links.into_iter().map(map_channel_link_response).collect(),
    })
}

pub async fn create_node_pairing_code(
    state: &AppState,
    request: CreateNodePairingCodeRequest,
) -> Result<NodePairingCodeResponse, GatewayError> {
    let expires_in_seconds = request.expires_in_seconds.unwrap_or(600);
    if expires_in_seconds <= 0 {
        return Err(GatewayError::ValidationError(
            "pairing expires_in_seconds must be positive".into(),
        ));
    }
    let expires_at = Utc::now() + Duration::seconds(expires_in_seconds);
    let pairing_code = Uuid::new_v4().to_string();
    state.pairing_codes.lock().await.insert(
        pairing_code.clone(),
        PairingCodeRecord { expires_at },
    );
    Ok(NodePairingCodeResponse {
        pairing_code,
        expires_at,
    })
}

pub async fn redeem_node_pairing_code(
    state: &AppState,
    request: RedeemNodePairingRequest,
) -> Result<ExecutionHostResponse, GatewayError> {
    let mut pairing_codes = state.pairing_codes.lock().await;
    let record = pairing_codes.remove(&request.pairing_code).ok_or_else(|| {
        GatewayError::ValidationError("pairing code was not found or has already been used".into())
    })?;
    if record.expires_at < Utc::now() {
        return Err(GatewayError::ValidationError(
            "pairing code has expired".into(),
        ));
    }
    drop(pairing_codes);

    let host = register_remote_execution_host(
        state,
        RegisterExecutionHostRequest {
            host_id: request.host_id,
            label: request.label,
            kind: request.kind,
            base_url: request.base_url,
            shared_token: request.shared_token,
            workspace_root: request.workspace_root,
            capabilities: request.capabilities,
        },
    )
    .await?;
    Ok(map_execution_host_response(host))
}

pub async fn export_gateway_recovery_bundle(
    state: &AppState,
) -> Result<GatewayRecoveryBundle, GatewayError> {
    let projects = state.project_store.list_all().await?;
    let mut sessions = Vec::new();
    for project in &projects {
        sessions.extend(state.storage.list_by_project(project.id).await?);
    }

    let channel_links = state.storage.list_channel_links().await?;
    let execution_hosts = state.storage.list_execution_hosts().await?;
    let scheduled_jobs = state.storage.list_scheduled_jobs().await?;
    let heartbeats = state.storage.list_heartbeats().await?;

    Ok(GatewayRecoveryBundle {
        version: 1,
        projects: projects
            .into_iter()
            .map(|project| RecoveryProjectRecord {
                id: project.id,
                name: project.name,
                slug: project.slug,
                workspace_path: project.workspace_path,
                is_default: project.is_default,
            })
            .collect(),
        sessions: sessions
            .into_iter()
            .map(|session| RecoverySessionRecord {
                id: session.id,
                project_id: session.project_id,
                agent_id: session.agent_id,
                metadata: session.metadata,
            })
            .collect(),
        channel_links: channel_links
            .into_iter()
            .map(|link| RecoveryChannelLinkRecord {
                integration: link.integration,
                account: link.account,
                conversation: link.conversation,
                project_id: link.project_id,
                agent_id: link.agent_id,
                session_id: link.session_id,
                home_target: link.home_target,
            })
            .collect(),
        execution_hosts: execution_hosts
            .into_iter()
            .map(|host| RecoveryExecutionHostRecord {
                host_id: host.host_id,
                label: host.label,
                kind: host.kind,
                base_url: host.base_url,
                shared_token: host.shared_token,
                workspace_root: host.workspace_root,
                capabilities: host.capabilities,
            })
            .collect(),
        scheduled_jobs: scheduled_jobs
            .into_iter()
            .map(|job| RecoveryScheduledJobRecord {
                session_id: job.session_id,
                agent_id: job.agent_id,
                message: job.message,
                status: job.status.to_string(),
                kind: job.kind.to_string(),
                schedule_expression: job.schedule_expression,
                delivery_routing: job.delivery_routing,
                next_run_at: job.next_run_at,
                last_run_at: job.last_run_at,
            })
            .collect(),
        heartbeats: heartbeats
            .into_iter()
            .map(|heartbeat| RecoveryHeartbeatRecord {
                session_id: heartbeat.session_id,
                agent_id: heartbeat.agent_id,
                message: heartbeat.message,
                interval_seconds: heartbeat.interval_seconds,
                status: heartbeat.status.to_string(),
                last_beat_at: heartbeat.last_beat_at,
                next_beat_at: heartbeat.next_beat_at,
            })
            .collect(),
    })
}

pub async fn import_gateway_recovery_bundle(
    state: &AppState,
    bundle: GatewayRecoveryBundle,
) -> Result<(), GatewayError> {
    ensure_fresh_import_target(state).await?;

    let mut project_map = HashMap::new();
    for project in bundle.projects {
        let project_id = project.id;
        let imported = import_project(state, project).await?;
        project_map.insert(project_id, imported.id);
    }

    let mut session_map = HashMap::new();
    for session in bundle.sessions {
        let imported = state
            .storage
            .create_session(NewSession {
                project_id: *project_map.get(&session.project_id).ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "session referenced unknown project {}",
                        session.project_id
                    ))
                })?,
                agent_id: session.agent_id.clone(),
                metadata: session.metadata,
            })
            .await?;
        state
            .storage
            .create(NewThread {
                session_id: imported.id,
                agent_id: session.agent_id,
            })
            .await?;
        session_map.insert(session.id, imported.id);
    }

    for link in bundle.channel_links {
        state
            .storage
            .upsert_channel_link(NewChannelLink {
                integration: link.integration,
                account: link.account,
                conversation: link.conversation,
                project_id: *project_map.get(&link.project_id).ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "channel link referenced unknown project {}",
                        link.project_id
                    ))
                })?,
                agent_id: link.agent_id,
                session_id: link.session_id.and_then(|session_id| session_map.get(&session_id).copied()),
                home_target: link.home_target,
            })
            .await?;
    }

    for host in bundle.execution_hosts {
        register_remote_execution_host(
            state,
            RegisterExecutionHostRequest {
                host_id: host.host_id,
                label: host.label,
                kind: host.kind,
                base_url: host.base_url,
                shared_token: host.shared_token,
                workspace_root: host.workspace_root,
                capabilities: host.capabilities,
            },
        )
        .await?;
    }

    for job in bundle.scheduled_jobs {
        state
            .storage
            .create_scheduled_job(NewScheduledJob {
                session_id: *session_map.get(&job.session_id).ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "scheduled job referenced unknown session {}",
                        job.session_id
                    ))
                })?,
                agent_id: job.agent_id,
                message: job.message,
                status: parse_scheduled_status(&job.status)?,
                kind: parse_scheduled_kind(&job.kind)?,
                schedule_expression: job.schedule_expression,
                delivery_routing: job.delivery_routing,
                next_run_at: job.next_run_at,
                last_run_at: job.last_run_at,
            })
            .await?;
    }

    for heartbeat in bundle.heartbeats {
        state
            .storage
            .create_heartbeat(NewHeartbeat {
                session_id: *session_map.get(&heartbeat.session_id).ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "heartbeat referenced unknown session {}",
                        heartbeat.session_id
                    ))
                })?,
                agent_id: heartbeat.agent_id,
                message: heartbeat.message,
                interval_seconds: heartbeat.interval_seconds,
                status: parse_heartbeat_status(&heartbeat.status)?,
                last_beat_at: heartbeat.last_beat_at,
                next_beat_at: heartbeat.next_beat_at,
            })
            .await?;
    }

    Ok(())
}

async fn ensure_fresh_import_target(state: &AppState) -> Result<(), GatewayError> {
    let projects = state.project_store.list_all().await?;
    let has_non_default_projects = projects.iter().any(|project| !project.is_default);
    let default_project = projects
        .iter()
        .find(|project| project.is_default)
        .cloned()
        .or_else(|| projects.first().cloned());
    let has_sessions = if let Some(project) = default_project {
        !state.storage.list_by_project(project.id).await?.is_empty()
    } else {
        false
    };
    if has_non_default_projects
        || has_sessions
        || !state.storage.list_channel_links().await?.is_empty()
        || !state.storage.list_execution_hosts().await?.is_empty()
        || !state.storage.list_scheduled_jobs().await?.is_empty()
        || !state.storage.list_heartbeats().await?.is_empty()
    {
        return Err(GatewayError::ValidationError(
            "gateway recovery import requires a fresh instance".into(),
        ));
    }
    Ok(())
}

async fn import_project(
    state: &AppState,
    project: RecoveryProjectRecord,
) -> Result<Project, GatewayError> {
    if project.is_default {
        let default_project = state.project_store.get_or_create_default().await?;
        state
            .project_store
            .update(
                default_project.id,
                Some(project.name),
                Some(project.slug),
                project.workspace_path,
            )
            .await
            .map_err(Into::into)
    } else {
        state
            .project_store
            .create(swage_storage::NewProject {
                name: project.name,
                slug: project.slug,
                workspace_path: project.workspace_path,
            })
            .await
            .map_err(Into::into)
    }
}

fn parse_scheduled_status(value: &str) -> Result<ScheduledJobStatus, GatewayError> {
    match value {
        "active" => Ok(ScheduledJobStatus::Active),
        "paused" => Ok(ScheduledJobStatus::Paused),
        "completed" => Ok(ScheduledJobStatus::Completed),
        other => Err(GatewayError::ValidationError(format!(
            "unknown scheduled job status: {other}"
        ))),
    }
}

fn parse_scheduled_kind(value: &str) -> Result<ScheduledJobKind, GatewayError> {
    match value {
        "one_shot" => Ok(ScheduledJobKind::OneShot),
        "interval" => Ok(ScheduledJobKind::Interval),
        "cron" => Ok(ScheduledJobKind::Cron),
        other => Err(GatewayError::ValidationError(format!(
            "unknown scheduled job kind: {other}"
        ))),
    }
}

fn parse_heartbeat_status(value: &str) -> Result<HeartbeatStatus, GatewayError> {
    match value {
        "enabled" => Ok(HeartbeatStatus::Enabled),
        "disabled" => Ok(HeartbeatStatus::Disabled),
        other => Err(GatewayError::ValidationError(format!(
            "unknown heartbeat status: {other}"
        ))),
    }
}
