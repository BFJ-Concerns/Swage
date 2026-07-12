use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_core::GatewayError;
use swage_protocol::client::channel::{ChannelLinkCodeResponse, CreateChannelLinkCodeRequest};
use swage_storage::{
    ChannelLink, GateConfig, NewChannelLink, NewSchedule, NewSession, NewThread,
    NewWorkspaceAttachment, ScheduleKind, ScheduleStatus, SessionPolicy,
    model::{Space, SpaceSlug},
};

use crate::{
    channel_gateway::{DeliveryTarget, ExternalChannelIntegration},
    device::{DeviceResponse, RegisterDeviceRequest, register_remote_device},
    state::{AppState, ChannelLinkCodeRecord, PairingCodeRecord},
};

#[derive(Debug, Clone, Deserialize)]
pub struct CreateChannelLinkRequest {
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub space_id: Option<Uuid>,
    pub agent_id: Option<String>,
    pub home_target: Option<DeliveryTarget>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelLinkResponse {
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub space_id: Uuid,
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
pub struct CreateDevicePairingCodeRequest {
    pub expires_in_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DevicePairingCodeResponse {
    pub pairing_code: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RedeemDevicePairingRequest {
    pub pairing_code: String,
    pub device_id: String,
    pub label: Option<String>,
    #[serde(default = "default_attached_device_kind")]
    pub kind: String,
    pub base_url: String,
    pub shared_token_secret_ref: String,
    pub workspace_root: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayRecoveryBundle {
    pub version: u32,
    pub spaces: Vec<RecoverySpaceRecord>,
    #[serde(default)]
    pub workspaces: Vec<RecoveryWorkspaceRecord>,
    pub sessions: Vec<RecoverySessionRecord>,
    pub secrets: Vec<RecoverySecretRecord>,
    pub channel_links: Vec<RecoveryChannelLinkRecord>,
    pub devices: Vec<RecoveryDeviceRecord>,
    pub schedules: Vec<RecoveryScheduleRecord>,
}

#[derive(Debug, Clone)]
pub struct RecoveryExportSnapshot {
    pub summary: RecoveryExportSummary,
    pub bundle: GatewayRecoveryBundle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryExportSummary {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub size_bytes: usize,
    pub spaces: usize,
    pub workspaces: usize,
    pub sessions: usize,
    pub secrets: usize,
    pub channel_links: usize,
    pub devices: usize,
    pub schedules: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryExportListResponse {
    pub exports: Vec<RecoveryExportSummary>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImportGatewayRecoveryRequest {
    pub bundle: GatewayRecoveryBundle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoverySpaceRecord {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryWorkspaceRecord {
    pub id: Uuid,
    pub space_id: Uuid,
    pub label: String,
    pub path: String,
    pub permissions: Vec<String>,
    pub is_trusted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoverySessionRecord {
    pub id: Uuid,
    pub space_id: Uuid,
    pub agent_id: String,
    pub metadata: Option<serde_json::Value>,
    pub active_workspace_attachment_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoverySecretRecord {
    pub id: Uuid,
    pub name: String,
    pub material_kind: String,
    pub owner_kind: String,
    pub backend_kind: String,
    pub scope_kind: String,
    pub scope_id: Option<String>,
    pub external_reference: Option<String>,
    pub metadata: serde_json::Value,
    pub status: String,
    pub managed_value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryChannelLinkRecord {
    pub integration: String,
    pub account: String,
    pub conversation: String,
    pub space_id: Uuid,
    pub agent_id: String,
    pub session_id: Option<Uuid>,
    pub home_target: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryDeviceRecord {
    pub device_id: String,
    pub label: Option<String>,
    pub kind: String,
    pub base_url: String,
    pub shared_token_secret_ref: String,
    pub workspace_root: String,
    #[serde(default)]
    pub workspace_roots: Vec<String>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryScheduleRecord {
    pub message: String,
    pub status: String,
    pub kind: String,
    pub expression: String,
    /// Space/session ids inside the policy refer to the exporting gateway;
    /// import remaps them onto the freshly created rows.
    pub session_policy: SessionPolicy,
    pub delivery_routing: serde_json::Value,
    pub gate: Option<GateConfig>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_run_at: Option<DateTime<Utc>>,
}

fn default_attached_device_kind() -> String {
    "attached_device".to_string()
}

fn map_channel_link_response(link: ChannelLink) -> ChannelLinkResponse {
    ChannelLinkResponse {
        integration: link.integration,
        account: link.account,
        conversation: link.conversation,
        space_id: link.space_id,
        agent_id: link.agent_id,
        session_id: link.session_id,
        home_target: link.home_target,
        linked_at: link.linked_at,
        updated_at: link.updated_at,
    }
}

fn map_device_response(device: crate::device::RegisteredDevice) -> DeviceResponse {
    DeviceResponse {
        device_id: device.device_id,
        label: device.label,
        kind: device.kind.to_string(),
        base_url: device.base_url,
        workspace_root: device.workspace_root.to_string_lossy().into_owned(),
        workspace_roots: device
            .workspace_roots
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        capabilities: device.capabilities,
        registered_at: device.registered_at,
    }
}

pub async fn create_channel_link(
    state: &AppState,
    request: CreateChannelLinkRequest,
) -> Result<ChannelLinkResponse, GatewayError> {
    let integration: ExternalChannelIntegration = request
        .integration
        .trim()
        .parse()
        .map_err(GatewayError::ValidationError)?;
    if request.account.trim().is_empty() || request.conversation.trim().is_empty() {
        return Err(GatewayError::ValidationError(
            "channel account and conversation must not be empty".into(),
        ));
    }

    let space =
        match request.space_id {
            Some(space_id) => state.space_store.get(space_id).await?.ok_or_else(|| {
                GatewayError::ValidationError(format!("space {space_id} not found"))
            })?,
            None => state.space_store.get_or_create_default().await?,
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
            space_id: space.id,
            agent_id: agent_id.clone(),
            metadata: Some(metadata),
            active_workspace_attachment_id: None,
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
            space_id: space.id,
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

pub async fn create_channel_link_code(
    state: &AppState,
    request: CreateChannelLinkCodeRequest,
) -> Result<ChannelLinkCodeResponse, GatewayError> {
    let expires_at = Utc::now() + Duration::minutes(10);
    let link_code = Uuid::new_v4().to_string();
    state.channel_link_codes.lock().await.insert(
        link_code.clone(),
        ChannelLinkCodeRecord {
            integration: request.integration,
            space_id: request.space_id,
            agent_id: request.agent_id,
            home_target: request.home_target,
            expires_at,
        },
    );
    Ok(ChannelLinkCodeResponse {
        link_code,
        expires_at,
    })
}

pub async fn create_device_pairing_code(
    state: &AppState,
    request: CreateDevicePairingCodeRequest,
) -> Result<DevicePairingCodeResponse, GatewayError> {
    let expires_in_seconds = request.expires_in_seconds.unwrap_or(600);
    if expires_in_seconds <= 0 {
        return Err(GatewayError::ValidationError(
            "pairing expires_in_seconds must be positive".into(),
        ));
    }
    let expires_at = Utc::now() + Duration::seconds(expires_in_seconds);
    let pairing_code = Uuid::new_v4().to_string();
    state
        .pairing_codes
        .lock()
        .await
        .insert(pairing_code.clone(), PairingCodeRecord { expires_at });
    Ok(DevicePairingCodeResponse {
        pairing_code,
        expires_at,
    })
}

pub async fn redeem_device_pairing_code(
    state: &AppState,
    request: RedeemDevicePairingRequest,
) -> Result<DeviceResponse, GatewayError> {
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

    let device = register_remote_device(
        state,
        RegisterDeviceRequest {
            device_id: request.device_id,
            label: request.label,
            kind: request.kind,
            base_url: request.base_url,
            shared_token_secret_ref: request.shared_token_secret_ref,
            workspace_root: request.workspace_root,
            workspace_roots: vec![],
            capabilities: request.capabilities,
        },
    )
    .await?;
    Ok(map_device_response(device))
}

pub async fn export_gateway_recovery_bundle(
    state: &AppState,
) -> Result<GatewayRecoveryBundle, GatewayError> {
    let spaces = state.space_store.list_all().await?;
    let mut workspaces = Vec::new();
    let mut sessions = Vec::new();
    for space in &spaces {
        workspaces.extend(
            state
                .storage
                .list_workspace_attachments_by_space(space.id)
                .await?,
        );
        sessions.extend(state.storage.list_by_space(space.id).await?);
    }

    let channel_links = state.storage.list_channel_links().await?;
    let secrets = state.storage.list_secrets().await?;
    let devices = state.storage.list_devices().await?;
    let schedules = state.storage.list_schedules().await?;

    let bundle = GatewayRecoveryBundle {
        version: 1,
        spaces: spaces
            .into_iter()
            .map(|space| RecoverySpaceRecord {
                id: space.id,
                name: space.name,
                slug: space.slug.to_string(),
                is_default: space.is_default,
            })
            .collect(),
        workspaces: workspaces
            .into_iter()
            .map(|workspace| RecoveryWorkspaceRecord {
                id: workspace.id,
                space_id: workspace.space_id,
                label: workspace.label,
                path: workspace.path,
                permissions: workspace.permissions,
                is_trusted: workspace.is_trusted,
            })
            .collect(),
        sessions: sessions
            .into_iter()
            .map(|session| RecoverySessionRecord {
                id: session.id,
                space_id: session.space_id,
                agent_id: session.agent_id,
                metadata: session.metadata,
                active_workspace_attachment_id: session.active_workspace_attachment_id,
            })
            .collect(),
        secrets: {
            let mut records = Vec::new();
            for secret in secrets {
                let managed_value = if secret.backend_kind == "managed" {
                    state.storage.get_managed_secret_value(secret.id).await?
                } else {
                    None
                };
                records.push(RecoverySecretRecord {
                    id: secret.id,
                    name: secret.name,
                    material_kind: secret.material_kind,
                    owner_kind: secret.owner_kind,
                    backend_kind: secret.backend_kind,
                    scope_kind: secret.scope_kind,
                    scope_id: secret.scope_id,
                    external_reference: secret.external_reference,
                    metadata: secret.metadata,
                    status: secret.status,
                    managed_value,
                });
            }
            records
        },
        channel_links: channel_links
            .into_iter()
            .map(|link| RecoveryChannelLinkRecord {
                integration: link.integration,
                account: link.account,
                conversation: link.conversation,
                space_id: link.space_id,
                agent_id: link.agent_id,
                session_id: link.session_id,
                home_target: link.home_target,
            })
            .collect(),
        devices: devices
            .into_iter()
            .map(|device| RecoveryDeviceRecord {
                device_id: device.device_id,
                label: device.label,
                kind: device.kind,
                base_url: device.base_url,
                shared_token_secret_ref: device.shared_token,
                workspace_root: device.workspace_root,
                workspace_roots: device.workspace_roots,
                capabilities: device.capabilities,
            })
            .collect(),
        schedules: schedules
            .into_iter()
            .map(|schedule| RecoveryScheduleRecord {
                message: schedule.message,
                status: schedule.status.to_string(),
                kind: schedule.kind.to_string(),
                expression: schedule.expression,
                session_policy: schedule.session_policy,
                delivery_routing: schedule.delivery_routing,
                gate: schedule.gate,
                next_run_at: schedule.next_run_at,
                last_run_at: schedule.last_run_at,
            })
            .collect(),
    };
    record_recovery_export(state, bundle.clone()).await?;
    Ok(bundle)
}

pub async fn list_gateway_recovery_exports(
    state: &AppState,
) -> Result<RecoveryExportListResponse, GatewayError> {
    let exports = state.recovery_exports.lock().await;
    Ok(RecoveryExportListResponse {
        exports: exports
            .iter()
            .rev()
            .map(|snapshot| snapshot.summary.clone())
            .collect(),
    })
}

pub async fn get_gateway_recovery_export(
    state: &AppState,
    export_id: Uuid,
) -> Result<GatewayRecoveryBundle, GatewayError> {
    let exports = state.recovery_exports.lock().await;
    exports
        .iter()
        .find(|snapshot| snapshot.summary.id == export_id)
        .map(|snapshot| snapshot.bundle.clone())
        .ok_or_else(|| {
            GatewayError::Storage(swage_core::StorageError::NotFound {
                entity: "recovery_export".to_string(),
                id: export_id.to_string(),
            })
        })
}

async fn record_recovery_export(
    state: &AppState,
    bundle: GatewayRecoveryBundle,
) -> Result<(), GatewayError> {
    let size_bytes = serde_json::to_vec(&bundle)
        .map_err(|error| GatewayError::ConfigError(format!("recovery export failed: {error}")))?
        .len();
    let summary = RecoveryExportSummary {
        id: Uuid::new_v4(),
        created_at: Utc::now(),
        size_bytes,
        spaces: bundle.spaces.len(),
        workspaces: bundle.workspaces.len(),
        sessions: bundle.sessions.len(),
        secrets: bundle.secrets.len(),
        channel_links: bundle.channel_links.len(),
        devices: bundle.devices.len(),
        schedules: bundle.schedules.len(),
    };

    let mut exports = state.recovery_exports.lock().await;
    exports.push(RecoveryExportSnapshot { summary, bundle });
    if exports.len() > 20 {
        let stale_count = exports.len() - 20;
        exports.drain(..stale_count);
    }
    Ok(())
}

pub async fn import_gateway_recovery_bundle(
    state: &AppState,
    bundle: GatewayRecoveryBundle,
) -> Result<(), GatewayError> {
    ensure_fresh_import_target(state).await?;

    let mut space_map = HashMap::new();
    for space in bundle.spaces {
        let space_id = space.id;
        let imported = import_space(state, space).await?;
        space_map.insert(space_id, imported.id);
    }

    for workspace in bundle.workspaces {
        state
            .storage
            .create_workspace_attachment(NewWorkspaceAttachment {
                space_id: *space_map.get(&workspace.space_id).ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "workspace referenced unknown space {}",
                        workspace.space_id
                    ))
                })?,
                label: workspace.label,
                path: workspace.path,
                permissions: workspace.permissions,
                is_trusted: workspace.is_trusted,
            })
            .await?;
    }

    let mut session_map = HashMap::new();
    for session in bundle.sessions {
        let imported = state
            .storage
            .create_session(NewSession {
                space_id: *space_map.get(&session.space_id).ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "session referenced unknown space {}",
                        session.space_id
                    ))
                })?,
                agent_id: session.agent_id.clone(),
                metadata: session.metadata,
                active_workspace_attachment_id: None,
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

    for secret in bundle.secrets {
        state
            .storage
            .create_secret(swage_storage::NewSecretRecord {
                id: secret.id,
                name: secret.name,
                material_kind: secret.material_kind,
                owner_kind: secret.owner_kind,
                backend_kind: secret.backend_kind,
                scope_kind: secret.scope_kind,
                scope_id: secret.scope_id,
                external_reference: secret.external_reference,
                metadata: secret.metadata,
                value: secret.managed_value,
            })
            .await?;
        if secret.status == "revoked" {
            state.storage.revoke_secret(secret.id).await?;
        }
    }

    for link in bundle.channel_links {
        state
            .storage
            .upsert_channel_link(NewChannelLink {
                integration: link.integration,
                account: link.account,
                conversation: link.conversation,
                space_id: *space_map.get(&link.space_id).ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "channel link referenced unknown space {}",
                        link.space_id
                    ))
                })?,
                agent_id: link.agent_id,
                session_id: link
                    .session_id
                    .and_then(|session_id| session_map.get(&session_id).copied()),
                home_target: link.home_target,
            })
            .await?;
    }

    for device in bundle.devices {
        register_remote_device(
            state,
            RegisterDeviceRequest {
                device_id: device.device_id,
                label: device.label,
                kind: device.kind,
                base_url: device.base_url,
                shared_token_secret_ref: device.shared_token_secret_ref,
                workspace_root: device.workspace_root,
                workspace_roots: device.workspace_roots,
                capabilities: device.capabilities,
            },
        )
        .await?;
    }

    for schedule in bundle.schedules {
        // The policy's space/session ids belong to the exporting gateway;
        // remap them onto the rows this import just created.
        let session_policy = match schedule.session_policy {
            SessionPolicy::Fresh { space_id, agent_id } => SessionPolicy::Fresh {
                space_id: *space_map.get(&space_id).ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "schedule referenced unknown space {space_id}"
                    ))
                })?,
                agent_id,
            },
            SessionPolicy::Continuation { session_id } => SessionPolicy::Continuation {
                session_id: *session_map.get(&session_id).ok_or_else(|| {
                    GatewayError::ValidationError(format!(
                        "schedule referenced unknown session {session_id}"
                    ))
                })?,
            },
        };
        state
            .storage
            .create_schedule(NewSchedule {
                message: schedule.message,
                status: parse_recovery_value::<ScheduleStatus>(&schedule.status)?,
                kind: parse_recovery_value::<ScheduleKind>(&schedule.kind)?,
                expression: schedule.expression,
                session_policy,
                delivery_routing: schedule.delivery_routing,
                gate: schedule.gate,
                next_run_at: schedule.next_run_at,
                last_run_at: schedule.last_run_at,
            })
            .await?;
    }

    Ok(())
}

async fn ensure_fresh_import_target(state: &AppState) -> Result<(), GatewayError> {
    let spaces = state.space_store.list_all().await?;
    let has_non_default_spaces = spaces.iter().any(|space| !space.is_default);
    let default_space = spaces
        .iter()
        .find(|space| space.is_default)
        .cloned()
        .or_else(|| spaces.first().cloned());
    let has_sessions = if let Some(space) = default_space {
        !state.storage.list_by_space(space.id).await?.is_empty()
    } else {
        false
    };
    if has_non_default_spaces
        || has_sessions
        || !state.storage.list_channel_links().await?.is_empty()
        || !state.storage.list_devices().await?.is_empty()
        || !state.storage.list_schedules().await?.is_empty()
    {
        return Err(GatewayError::ValidationError(
            "gateway recovery import requires a fresh instance".into(),
        ));
    }
    Ok(())
}

async fn import_space(state: &AppState, space: RecoverySpaceRecord) -> Result<Space, GatewayError> {
    let slug = SpaceSlug::new(space.slug).map_err(GatewayError::ValidationError)?;
    if space.is_default {
        let default_space = state.space_store.get_or_create_default().await?;
        state
            .space_store
            .update(default_space.id, Some(space.name), None, Some(slug))
            .await
            .map_err(Into::into)
    } else {
        state
            .space_store
            .create(swage_storage::NewSpace {
                name: space.name,
                slug,
            })
            .await
            .map_err(Into::into)
    }
}

fn parse_recovery_value<T>(value: &str) -> Result<T, GatewayError>
where
    T: std::str::FromStr<Err = String>,
{
    value.parse().map_err(GatewayError::ValidationError)
}
