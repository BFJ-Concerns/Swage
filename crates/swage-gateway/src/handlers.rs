use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{ConnectInfo, Extension, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_agent::approval::ApprovalDecisionValue;
use swage_core::GatewayError;
use swage_protocol::{
    client::{
        channel as channel_wire, schedule as schedule_wire,
        skill::{SkillItem, SkillListResponse},
    },
    hosting::{
        AgentDefinitionListResponse, AgentDefinitionMutationResponse, AgentDefinitionResponse,
        AgentDefinitionScopeQuery, CreateAgentDefinitionRequest,
        EffectiveAgentDefinitionListResponse, EffectiveAgentDefinitionResponse,
        EffectiveAgentDefinitionSourceScope, OperatorHistoryResponse, UpdateAgentDefinitionRequest,
    },
};

use crate::{
    attachment::{
        AttachmentListResponse, BackgroundJobResponse, CreateAttachmentRequest,
        QueuedAttachmentResponse,
    },
    audit::{
        AgentMemoryListQuery, AgentMemoryListResponse, OperatorSearchQuery, OperatorSearchResponse,
        OperatorUsageQuery, OperatorUsageResponse,
    },
    capabilities::WorkflowCapabilitiesResponse,
    channel_gateway::{
        ChannelInboundResponse, ChannelStatusListResponse, DeliveryLogQuery, DeliveryLogResponse,
        DevDmInboundRequest, DevThreadInboundRequest,
    },
    device::{DeviceListResponse, DeviceResponse, RegisterDeviceRequest},
    error::{ApiError, Json, Path},
    material::{
        CreateMaterialRequest, MaterialListResponse, MaterialResponse, PromoteMaterialRequest,
    },
    onboarding::{
        ChannelLinkListResponse, ChannelLinkResponse, CreateChannelLinkRequest,
        CreateDevicePairingCodeRequest, DevicePairingCodeResponse, ImportGatewayRecoveryRequest,
        RecoveryExportListResponse, RedeemDevicePairingRequest,
    },
    operator::OperatorOverviewResponse,
    presence::PresenceSnapshotResponse,
    secret::{
        CreateSecretRequest, GenerateSecretRequest, RotateSecretRequest, SecretListResponse,
        SecretResponse, SecretValidationResponse,
    },
    service::{
        ConversationBranchListResponse, ConversationDetail, ConversationResponse,
        ConversationService, ConversationSummary, ConversationThreadDetail,
        CreateConversationBranchRequest, CreateTopicRequest, CreateWorkspaceAttachmentRequest,
        TopicListResponse, TopicResponse, TopicService, UpdateTopicRequest,
        UpdateWorkspaceTrustRequest, WorkspaceAttachmentListResponse, WorkspaceAttachmentResponse,
        WorkspaceAttachmentService,
    },
    settings::{SaveProviderSettings, SaveSettingsRequest, SettingsResponse},
    state::AppState,
    supervision::{RunDetailResponse, RunListQuery, RunListResponse},
    voice::{
        SynthesiseSpeechRequest, SynthesiseSpeechResponse, TranscribeAudioRequest,
        TranscribeAudioResponse, VoiceCapabilitiesResponse,
    },
};

// ---------------------------------------------------------------------------
// Health check
// ---------------------------------------------------------------------------

/// `GET /api/health` — lightweight startup and readiness check.
///
/// Returns `{"status": "ok"}` with no database queries or provider calls.
/// Used by process managers (systemd, wrapper scripts) to verify the gateway
/// is up and accepting connections before routing traffic to it.
pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

/// `GET /api/capabilities` — inspect gateway workflow capability gating.
pub async fn workflow_capabilities(
    State(state): State<AppState>,
) -> Json<WorkflowCapabilitiesResponse> {
    Json(crate::capabilities::workflow_capabilities(&state).await)
}

/// `GET /api/voice/capabilities` — inspect voice support for the current runtime.
pub async fn voice_capabilities(State(state): State<AppState>) -> Json<VoiceCapabilitiesResponse> {
    Json(crate::voice::voice_capabilities(&state).await)
}

pub async fn get_settings(
    State(state): State<AppState>,
) -> Result<Json<SettingsResponse>, ApiError> {
    Ok(Json(crate::settings::load_settings(&state).await?))
}

pub async fn save_settings(
    State(state): State<AppState>,
    Json(request): Json<SaveSettingsRequest>,
) -> Result<Json<SettingsResponse>, ApiError> {
    Ok(Json(crate::settings::save_settings(&state, request).await?))
}

pub async fn discover_provider_models(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<crate::settings::ModelDiscoveryResponse>, ApiError> {
    Ok(Json(
        crate::settings::discover_models(&state, provider_id, query.get("q").cloned(), None)
            .await?,
    ))
}

pub async fn discover_draft_provider_models(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    Json(provider): Json<SaveProviderSettings>,
) -> Result<Json<crate::settings::ModelDiscoveryResponse>, ApiError> {
    Ok(Json(
        crate::settings::discover_models(
            &state,
            provider_id,
            query.get("q").cloned(),
            Some(provider),
        )
        .await?,
    ))
}

fn ensure_codex_provider(state: &AppState, provider_id: &str) -> Result<(), ApiError> {
    let config = state.active_config();
    let provider = config
        .providers
        .entries
        .get(provider_id)
        .ok_or_else(|| ApiError::not_found_str("provider", provider_id))?;
    if provider.protocol != swage_config::Protocol::Codex {
        return Err(ApiError::from(GatewayError::SignInUnsupported(
            provider_id.to_string(),
        )));
    }
    Ok(())
}

pub async fn provider_sign_in_state(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
) -> Result<Json<swage_protocol::client::provider_signin::ProviderSignInState>, ApiError> {
    ensure_codex_provider(&state, &provider_id)?;
    Ok(Json(crate::codex::sign_in_state(&state).await.map_err(
        |error| ApiError::from(GatewayError::ProviderRuntimeUnavailable(error)),
    )?))
}

pub async fn start_provider_sign_in(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
    Json(request): Json<swage_protocol::client::provider_signin::StartProviderSignInRequest>,
) -> Result<Json<swage_protocol::client::provider_signin::StartProviderSignInResponse>, ApiError> {
    ensure_codex_provider(&state, &provider_id)?;
    Ok(Json(
        crate::codex::start_sign_in(&state, request)
            .await
            .map_err(|error| ApiError::from(GatewayError::SignInFailed(error)))?,
    ))
}

pub async fn provider_sign_out(
    State(state): State<AppState>,
    Path(provider_id): Path<String>,
) -> Result<Json<swage_protocol::client::provider_signin::ProviderSignInState>, ApiError> {
    ensure_codex_provider(&state, &provider_id)?;
    Ok(Json(crate::codex::sign_out(&state).await.map_err(
        |error| ApiError::from(GatewayError::SignInFailed(error)),
    )?))
}

pub async fn restart_status(
    State(state): State<AppState>,
) -> Json<crate::settings::RestartStatusResponse> {
    Json(crate::settings::restart_status(&state))
}

pub async fn restart_gateway(
    State(state): State<AppState>,
) -> Result<Json<crate::settings::RestartStatusResponse>, ApiError> {
    Ok(Json(crate::settings::restart_gateway(&state).await?))
}

/// `POST /api/voice/transcriptions` — turn uploaded audio bytes into text.
pub async fn transcribe_audio(
    State(state): State<AppState>,
    Json(request): Json<TranscribeAudioRequest>,
) -> Result<Json<TranscribeAudioResponse>, ApiError> {
    Ok(Json(crate::voice::transcribe_audio(&state, request).await?))
}

/// `POST /api/voice/speech` — synthesise speech audio from assistant text.
pub async fn synthesise_speech(
    State(state): State<AppState>,
    Json(request): Json<SynthesiseSpeechRequest>,
) -> Result<Json<SynthesiseSpeechResponse>, ApiError> {
    Ok(Json(
        crate::voice::synthesise_speech(&state, request).await?,
    ))
}

// ---------------------------------------------------------------------------
// External channel gateway
// ---------------------------------------------------------------------------

/// `POST /api/channels/dev-dm/inbound` — accept a direct-message style inbound event.
pub async fn channel_dev_dm_inbound(
    State(state): State<AppState>,
    Json(req): Json<DevDmInboundRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let response: ChannelInboundResponse =
        crate::channel_gateway::handle_dev_dm_inbound(&state, req).await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// `POST /api/channels/dev-thread/inbound` — accept a thread/topic style inbound event.
pub async fn channel_dev_thread_inbound(
    State(state): State<AppState>,
    Json(req): Json<DevThreadInboundRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let response: ChannelInboundResponse =
        crate::channel_gateway::handle_dev_thread_inbound(&state, req).await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

/// `GET /api/deliveries/log` — inspect resolved deliveries for a session.
pub async fn delivery_log(
    State(state): State<AppState>,
    Query(query): Query<DeliveryLogQuery>,
) -> Json<DeliveryLogResponse> {
    Json(crate::channel_gateway::read_delivery_log(&state, query).await)
}

/// `GET /api/channels` — inspect channel routing and delivery health.
pub async fn list_channel_statuses(
    State(state): State<AppState>,
) -> Json<ChannelStatusListResponse> {
    Json(crate::channel_gateway::list_channel_statuses(&state).await)
}

/// `GET /api/operator/overview` — inspect the canonical operator control surface.
pub async fn operator_overview(
    State(state): State<AppState>,
) -> Result<Json<OperatorOverviewResponse>, ApiError> {
    Ok(Json(crate::operator::operator_overview(&state).await?))
}

/// `GET /api/operator/history` — reconstruct configuration and experiment history.
pub async fn operator_history(
    State(state): State<AppState>,
) -> Result<Json<OperatorHistoryResponse>, ApiError> {
    Ok(Json(crate::audit::operator_history(&state).await?))
}

fn agent_definition_response(
    definition: swage_storage::AgentDefinition,
) -> AgentDefinitionResponse {
    AgentDefinitionResponse {
        id: definition.id,
        topic_id: definition.topic_id,
        display_name: definition.display_name,
        description: definition.description,
        instructions: definition.instructions,
        model_id: definition.model_id,
        tool_permissions: definition.tool_permissions,
        skills: definition.skills,
        output_schema: definition.output_schema,
        output_max_retries: definition.output_max_retries,
        created_at: definition.created_at,
        updated_at: definition.updated_at,
    }
}

pub async fn list_agent_definitions(
    State(state): State<AppState>,
    Query(query): Query<AgentDefinitionScopeQuery>,
) -> Result<Json<AgentDefinitionListResponse>, ApiError> {
    let definitions = state
        .storage
        .list_agent_definitions(query.topic_id)
        .await
        .map_err(GatewayError::from)?
        .into_iter()
        .map(agent_definition_response)
        .collect();
    Ok(Json(AgentDefinitionListResponse { definitions }))
}

pub async fn list_effective_agent_definitions(
    State(state): State<AppState>,
    Path(topic_id): Path<Uuid>,
) -> Result<Json<EffectiveAgentDefinitionListResponse>, ApiError> {
    effective_agent_definitions_for_scope(&state, topic_id).await
}

/// `GET /api/agent-definitions` — the agent definitions a session started
/// without a topic can run under: the global definitions and the built-in
/// default, resolved against the general scope.
pub async fn list_general_agent_definitions(
    State(state): State<AppState>,
) -> Result<Json<EffectiveAgentDefinitionListResponse>, ApiError> {
    let general = state
        .topic_store
        .general_scope()
        .await
        .map_err(GatewayError::from)?;
    effective_agent_definitions_for_scope(&state, general.id).await
}

async fn effective_agent_definitions_for_scope(
    state: &AppState,
    scope_id: Uuid,
) -> Result<Json<EffectiveAgentDefinitionListResponse>, ApiError> {
    let definitions = crate::service::effective_agent_definitions(state.storage.as_ref(), scope_id)
        .await?
        .into_iter()
        .map(effective_agent_definition_response)
        .collect();
    Ok(Json(EffectiveAgentDefinitionListResponse { definitions }))
}

fn effective_agent_definition_response(
    effective: crate::service::EffectiveAgentDefinition,
) -> EffectiveAgentDefinitionResponse {
    let definition = effective.definition;
    let source_scope = if effective.is_builtin {
        EffectiveAgentDefinitionSourceScope::BuiltIn
    } else if definition.topic_id.is_some() {
        EffectiveAgentDefinitionSourceScope::Topic
    } else {
        EffectiveAgentDefinitionSourceScope::Global
    };
    EffectiveAgentDefinitionResponse {
        id: definition.id,
        display_name: definition.display_name,
        description: definition.description,
        instructions: definition.instructions,
        model_id: definition.model_id,
        tool_permissions: definition.tool_permissions,
        skills: definition.skills,
        output_schema: definition.output_schema,
        output_max_retries: definition.output_max_retries,
        is_builtin: effective.is_builtin,
        source_scope,
    }
}

fn new_agent_definition(
    id: String,
    request: UpdateAgentDefinitionRequest,
) -> swage_storage::NewAgentDefinition {
    swage_storage::NewAgentDefinition {
        id,
        topic_id: request.topic_id,
        display_name: request.display_name,
        description: request.description,
        instructions: request.instructions,
        model_id: request.model_id,
        tool_permissions: request.tool_permissions,
        skills: request.skills,
        output_schema: request.output_schema,
        output_max_retries: request.output_max_retries,
    }
}

fn mutation_response(
    change: crate::service::AgentDefinitionChange,
) -> AgentDefinitionMutationResponse {
    AgentDefinitionMutationResponse {
        agent_id: change.agent_id,
        topic_id: change.topic_id,
        summary: change.summary,
    }
}

pub async fn create_agent_definition(
    State(state): State<AppState>,
    Json(request): Json<CreateAgentDefinitionRequest>,
) -> Result<Json<AgentDefinitionMutationResponse>, ApiError> {
    let definition = new_agent_definition(
        request.id,
        UpdateAgentDefinitionRequest {
            topic_id: request.topic_id,
            display_name: request.display_name,
            description: request.description,
            instructions: request.instructions,
            model_id: request.model_id,
            tool_permissions: request.tool_permissions,
            skills: request.skills,
            output_schema: request.output_schema,
            output_max_retries: request.output_max_retries,
        },
    );
    let change =
        crate::service::upsert_agent_definition(&state, definition, "operator_http", None).await?;
    Ok(Json(mutation_response(change)))
}

pub async fn update_agent_definition(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Json(request): Json<UpdateAgentDefinitionRequest>,
) -> Result<Json<AgentDefinitionMutationResponse>, ApiError> {
    let change = crate::service::upsert_agent_definition(
        &state,
        new_agent_definition(agent_id, request),
        "operator_http",
        None,
    )
    .await?;
    Ok(Json(mutation_response(change)))
}

pub async fn delete_agent_definition(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Query(query): Query<AgentDefinitionScopeQuery>,
) -> Result<Json<AgentDefinitionMutationResponse>, ApiError> {
    let change = crate::service::delete_agent_definition(
        &state,
        &agent_id,
        query.topic_id,
        "operator_http",
        None,
    )
    .await?;
    Ok(Json(mutation_response(change)))
}

/// `GET /api/operator/search` — search durable operator-visible history.
pub async fn operator_search(
    State(state): State<AppState>,
    Query(query): Query<OperatorSearchQuery>,
) -> Result<Json<OperatorSearchResponse>, ApiError> {
    Ok(Json(crate::audit::operator_search(&state, query).await?))
}

/// `GET /api/operator/memories` — inspect durable agent memories with provenance.
pub async fn list_operator_memories(
    State(state): State<AppState>,
    Query(query): Query<AgentMemoryListQuery>,
) -> Result<Json<AgentMemoryListResponse>, ApiError> {
    Ok(Json(
        crate::audit::list_agent_memories(&state, query).await?,
    ))
}

/// `GET /api/operator/memories/:id` — inspect one durable agent-memory record.
pub async fn get_operator_memory(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<swage_storage::AgentMemoryRecord>, ApiError> {
    let memory = crate::audit::get_agent_memory(&state, id)
        .await?
        .ok_or_else(|| ApiError::not_found("agent memory", id))?;
    Ok(Json(memory))
}

/// `GET /api/operator/usage` — inspect run and aggregate token/cost usage.
pub async fn operator_usage(
    State(state): State<AppState>,
    Query(query): Query<OperatorUsageQuery>,
) -> Result<Json<OperatorUsageResponse>, ApiError> {
    Ok(Json(crate::audit::operator_usage(&state, query).await?))
}

/// `GET /api/secrets` — list governed secrets without revealing raw values.
pub async fn list_secrets(
    State(state): State<AppState>,
) -> Result<Json<SecretListResponse>, ApiError> {
    Ok(Json(state.secret_manager.list_secrets().await?))
}

/// `POST /api/secrets` — create a governed secret.
pub async fn create_secret(
    State(state): State<AppState>,
    Json(req): Json<CreateSecretRequest>,
) -> Result<(StatusCode, Json<SecretResponse>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(state.secret_manager.create_secret(req).await?),
    ))
}

/// `POST /api/secrets/generate` — generate a managed secret material.
pub async fn generate_secret(
    State(state): State<AppState>,
    Json(req): Json<GenerateSecretRequest>,
) -> Result<(StatusCode, Json<SecretResponse>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(state.secret_manager.generate_secret(req).await?),
    ))
}

/// `GET /api/secrets/:id` — inspect one governed secret without revealing its value.
pub async fn get_secret(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretResponse>, ApiError> {
    Ok(Json(state.secret_manager.get_secret(id).await?))
}

/// `POST /api/secrets/:id/rotate` — rotate a governed secret.
pub async fn rotate_secret(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<RotateSecretRequest>,
) -> Result<Json<SecretResponse>, ApiError> {
    Ok(Json(state.secret_manager.rotate_secret(id, req).await?))
}

/// `POST /api/secrets/:id/revoke` — revoke a governed secret.
pub async fn revoke_secret(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretResponse>, ApiError> {
    Ok(Json(state.secret_manager.revoke_secret(id).await?))
}

/// `POST /api/secrets/:id/restore` — restore a previously revoked secret.
pub async fn restore_secret(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretResponse>, ApiError> {
    Ok(Json(state.secret_manager.restore_secret(id).await?))
}

/// `POST /api/secrets/:id/validate` — resolve and validate a governed secret.
pub async fn validate_secret(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretValidationResponse>, ApiError> {
    Ok(Json(state.secret_manager.validate_secret(id).await?))
}

/// `GET /api/operator/runs` — inspect live and historical run supervision.
pub async fn list_operator_runs(
    State(state): State<AppState>,
    Query(query): Query<RunListQuery>,
) -> Result<Json<RunListResponse>, ApiError> {
    Ok(Json(crate::supervision::list_runs(&state, query).await?))
}

/// `GET /api/operator/runs/:kind/:id` — inspect one supervised run in detail.
pub async fn get_operator_run(
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, Uuid)>,
) -> Result<Json<RunDetailResponse>, ApiError> {
    Ok(Json(
        crate::supervision::get_run_detail(&state, &kind, id).await?,
    ))
}

/// `POST /api/attachments` — enqueue durable attachment ingestion.
pub async fn create_attachment(
    State(state): State<AppState>,
    Json(req): Json<CreateAttachmentRequest>,
) -> Result<(StatusCode, Json<QueuedAttachmentResponse>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(crate::attachment::create_attachment(&state, req).await?),
    ))
}

/// `GET /api/sessions/:id/attachments` — list session-scoped attachments.
pub async fn list_session_attachments(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AttachmentListResponse>, ApiError> {
    Ok(Json(
        crate::attachment::list_session_attachments(&state, id).await?,
    ))
}

/// `GET /api/topics/:id/attachments` — list topic-scoped attachments.
pub async fn list_topic_attachments(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AttachmentListResponse>, ApiError> {
    Ok(Json(
        crate::attachment::list_topic_attachments(&state, id).await?,
    ))
}

/// `GET /api/background-jobs/:id` — inspect one background job.
pub async fn get_background_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<BackgroundJobResponse>, ApiError> {
    Ok(Json(
        crate::attachment::get_background_job(&state, id).await?,
    ))
}

/// `POST /api/background-jobs/:id/retry` — re-queue a failed background job.
pub async fn retry_background_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<BackgroundJobResponse>, ApiError> {
    Ok(Json(
        crate::attachment::retry_background_job(&state, id).await?,
    ))
}

/// `POST /api/materials` — create a managed material.
pub async fn create_material(
    State(state): State<AppState>,
    Json(req): Json<CreateMaterialRequest>,
) -> Result<(StatusCode, Json<MaterialResponse>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(crate::material::create_material(&state, req).await?),
    ))
}

/// `GET /api/materials/:id` — inspect one managed material.
pub async fn get_material(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<MaterialResponse>, ApiError> {
    Ok(Json(crate::material::get_material(&state, id).await?))
}

/// `GET /api/sessions/:id/materials` — list session-scoped managed materials.
pub async fn list_session_materials(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<MaterialListResponse>, ApiError> {
    Ok(Json(
        crate::material::list_session_materials(&state, id).await?,
    ))
}

/// `GET /api/topics/:id/materials` — list topic-scoped managed materials.
pub async fn list_topic_materials(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<MaterialListResponse>, ApiError> {
    Ok(Json(
        crate::material::list_topic_materials(&state, id).await?,
    ))
}

/// `POST /api/materials/:id/archive` — archive a managed material.
pub async fn archive_material(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<MaterialResponse>, ApiError> {
    Ok(Json(crate::material::archive_material(&state, id).await?))
}

/// `POST /api/materials/:id/restore` — restore an archived managed material.
pub async fn restore_material(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<MaterialResponse>, ApiError> {
    Ok(Json(crate::material::restore_material(&state, id).await?))
}

/// `POST /api/materials/:id/promote` — promote a material to a broader scope.
pub async fn promote_material(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<PromoteMaterialRequest>,
) -> Result<Json<MaterialResponse>, ApiError> {
    Ok(Json(
        crate::material::promote_material(&state, id, req).await?,
    ))
}

/// `GET /api/onboarding/channels` — list durable external-channel links.
pub async fn list_channel_links(
    State(state): State<AppState>,
) -> Result<Json<ChannelLinkListResponse>, ApiError> {
    Ok(Json(crate::onboarding::list_channel_links(&state).await?))
}

/// `POST /api/onboarding/channels` — link an external channel origin to a session.
pub async fn create_channel_link(
    State(state): State<AppState>,
    Json(req): Json<CreateChannelLinkRequest>,
) -> Result<(StatusCode, Json<ChannelLinkResponse>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(crate::onboarding::create_channel_link(&state, req).await?),
    ))
}

/// `POST /api/onboarding/channels/link-codes` — issue a one-time channel link code.
pub async fn create_channel_link_code(
    State(state): State<AppState>,
    Json(req): Json<channel_wire::CreateChannelLinkCodeRequest>,
) -> Result<(StatusCode, Json<channel_wire::ChannelLinkCodeResponse>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(crate::onboarding::create_channel_link_code(&state, req).await?),
    ))
}

/// `POST /api/onboarding/devices/pairing-codes` — issue a one-time device pairing code.
pub async fn create_device_pairing_code(
    State(state): State<AppState>,
    Json(req): Json<CreateDevicePairingCodeRequest>,
) -> Result<(StatusCode, Json<DevicePairingCodeResponse>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(crate::onboarding::create_device_pairing_code(&state, req).await?),
    ))
}

/// `POST /api/onboarding/devices/redeem` — redeem a pairing code and trust a remote device.
pub async fn redeem_device_pairing_code(
    State(state): State<AppState>,
    Json(req): Json<RedeemDevicePairingRequest>,
) -> Result<(StatusCode, Json<DeviceResponse>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(crate::onboarding::redeem_device_pairing_code(&state, req).await?),
    ))
}

/// `GET /api/recovery/export` — export durable operator-managed recovery state.
pub async fn export_gateway_recovery(
    State(state): State<AppState>,
) -> Result<Json<crate::onboarding::GatewayRecoveryBundle>, ApiError> {
    Ok(Json(
        crate::onboarding::export_gateway_recovery_bundle(&state).await?,
    ))
}

/// `GET /api/recovery/exports` — list recovery exports produced by this process.
pub async fn list_gateway_recovery_exports(
    State(state): State<AppState>,
) -> Result<Json<RecoveryExportListResponse>, ApiError> {
    Ok(Json(
        crate::onboarding::list_gateway_recovery_exports(&state).await?,
    ))
}

/// `GET /api/recovery/exports/{id}` — download a previously produced recovery export.
pub async fn get_gateway_recovery_export(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::onboarding::GatewayRecoveryBundle>, ApiError> {
    Ok(Json(
        crate::onboarding::get_gateway_recovery_export(&state, id).await?,
    ))
}

/// `POST /api/recovery/import` — import a recovery bundle into a fresh instance.
pub async fn import_gateway_recovery(
    State(state): State<AppState>,
    Json(req): Json<ImportGatewayRecoveryRequest>,
) -> Result<StatusCode, ApiError> {
    crate::onboarding::import_gateway_recovery_bundle(&state, req.bundle).await?;
    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/schedules` — create a durable schedule.
pub async fn create_schedule(
    State(state): State<AppState>,
    Json(req): Json<schedule_wire::CreateScheduleRequest>,
) -> Result<(StatusCode, Json<schedule_wire::Schedule>), ApiError> {
    // Creation over the HTTP surface is a person's action.
    let response =
        crate::schedule::create_schedule(&state, req, schedule_wire::ScheduleCreator::User).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// `GET /api/schedules` — list all configured schedules.
pub async fn list_schedules(
    State(state): State<AppState>,
) -> Result<Json<schedule_wire::ScheduleListResponse>, ApiError> {
    Ok(Json(crate::schedule::list_schedules(&state).await?))
}

/// `PUT /api/schedules/:id` — edit an existing schedule.
pub async fn update_schedule(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<schedule_wire::UpdateScheduleRequest>,
) -> Result<Json<schedule_wire::Schedule>, ApiError> {
    Ok(Json(
        crate::schedule::update_schedule(&state, id, req).await?,
    ))
}

/// `POST /api/schedules/:id/pause` — pause a schedule.
pub async fn pause_schedule(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<schedule_wire::Schedule>, ApiError> {
    Ok(Json(crate::schedule::pause_schedule(&state, id).await?))
}

/// `POST /api/schedules/:id/resume` — resume a paused schedule.
pub async fn resume_schedule(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<schedule_wire::Schedule>, ApiError> {
    Ok(Json(crate::schedule::resume_schedule(&state, id).await?))
}

/// `POST /api/schedules/:id/trigger` — fire a schedule immediately.
///
/// The response carries the recorded run in its terminal state; a failed
/// or skipped run is reported through the record, not as an HTTP error.
pub async fn trigger_schedule(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<schedule_wire::TriggerScheduleResponse>), ApiError> {
    Ok((
        StatusCode::ACCEPTED,
        Json(crate::schedule::trigger_schedule(&state, id).await?),
    ))
}

/// `GET /api/schedules/:id/runs` — list recorded runs for a schedule.
pub async fn list_schedule_runs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<schedule_wire::ScheduleRunListResponse>, ApiError> {
    Ok(Json(crate::schedule::list_schedule_runs(&state, id).await?))
}

/// `DELETE /api/schedules/:id` — delete a schedule and its recorded runs.
pub async fn delete_schedule(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    if crate::schedule::delete_schedule(&state, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("schedule", id))
    }
}

/// `GET /api/presence` — inspect gateway, client, and device presence.
pub async fn presence_snapshot(State(state): State<AppState>) -> Json<PresenceSnapshotResponse> {
    Json(crate::presence::snapshot_presence(&state).await)
}

/// `DELETE /api/presence/devices/:id` — close a device's connection from
/// the gateway's side and mark it disconnected.
pub async fn disconnect_device(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if crate::device::disconnect_device(&state, &id).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found_str("device", &id))
    }
}

/// `GET /api/devices` — list local and attached devices.
pub async fn list_devices(State(state): State<AppState>) -> Json<DeviceListResponse> {
    Json(crate::device::list_devices(&state).await)
}

/// `POST /api/devices/register` — attach a remote device.
pub async fn register_device(
    State(state): State<AppState>,
    Json(request): Json<RegisterDeviceRequest>,
) -> Result<(StatusCode, Json<DeviceResponse>), ApiError> {
    if request.device_id.trim().is_empty() {
        return Err(ApiError::validation(
            "device id must not be empty".to_string(),
        ));
    }
    if request.shared_token_secret_ref.trim().is_empty() {
        return Err(ApiError::validation(
            "device shared_token_secret_ref must not be empty".to_string(),
        ));
    }

    let device = crate::device::register_remote_device(&state, request).await?;
    Ok((
        StatusCode::CREATED,
        Json(crate::device::device_response(device)),
    ))
}

// ---------------------------------------------------------------------------
// Wrapper type for list endpoint
// ---------------------------------------------------------------------------

/// Top-level response body for the list-conversations endpoint.
#[derive(Debug, Serialize)]
pub struct ConversationListResponse {
    pub conversations: Vec<ConversationSummary>,
}

pub use swage_protocol::client::conversation::CreateConversationRequest;
use swage_protocol::client::conversation::RecentSessionsResponse;

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// `POST /api/conversations` — create a new conversation.
///
/// Returns 201 Created with the new conversation's metadata.
pub async fn create_conversation(
    State(state): State<AppState>,
    payload: Option<Json<CreateConversationRequest>>,
) -> Result<impl IntoResponse, ApiError> {
    let config = state.active_config();
    let svc = ConversationService::new(state.storage, state.topic_store)
        .with_config(config, state.config_root);
    let request = payload.map(|Json(request)| request).unwrap_or_default();
    let resp: ConversationResponse = svc.create_conversation_from_request(request).await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/conversations` — list all conversations.
///
/// Returns 200 with `{ conversations: [...] }`.
pub async fn list_conversations(
    State(state): State<AppState>,
) -> Result<Json<ConversationListResponse>, ApiError> {
    let config = state.active_config();
    let svc = ConversationService::new(state.storage, state.topic_store)
        .with_config(config, state.config_root);
    let conversations = svc.list_conversations().await?;
    Ok(Json(ConversationListResponse { conversations }))
}

/// Query parameters for the recent-sessions list.
#[derive(Debug, Default, Deserialize)]
pub struct RecentSessionsQuery {
    /// Narrow to one topic's sessions; absent lists every scope.
    pub topic_id: Option<Uuid>,
    /// List the archived sessions instead of the live ones.
    #[serde(default)]
    pub archived: bool,
}

/// `GET /api/sessions` — the recent-sessions list across every scope.
///
/// Returns 200 with `{ entries: [...] }`, newest activity first, automated
/// sessions grouped beneath their source.
pub async fn list_recent_sessions(
    State(state): State<AppState>,
    Query(query): Query<RecentSessionsQuery>,
) -> Result<Json<RecentSessionsResponse>, ApiError> {
    let response = crate::recent_sessions::list_recent_sessions(
        &state,
        swage_storage::SessionListing {
            topic_id: query.topic_id,
            archived: query.archived,
        },
    )
    .await?;
    Ok(Json(response))
}

/// `GET /api/conversations/:id` — get a conversation with full message history.
///
/// Returns 200 with the conversation detail, or 404 if not found.
pub async fn get_conversation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ConversationDetail>, ApiError> {
    let svc = ConversationService::new(state.storage, state.topic_store);
    let detail = svc.get_conversation(id).await?;
    Ok(Json(detail))
}

/// `GET /api/topics/:topic_id/sessions` — list sessions for a topic.
///
/// Returns 200 with `{ conversations: [...] }`, or 404 if the topic is not found.
pub async fn list_topic_sessions(
    State(state): State<AppState>,
    Path(topic_id): Path<Uuid>,
) -> Result<Json<ConversationListResponse>, ApiError> {
    let svc = ConversationService::new(state.storage, state.topic_store);
    let conversations = svc.list_sessions_by_topic(topic_id).await?;
    Ok(Json(ConversationListResponse { conversations }))
}

/// `POST /api/topics/:topic_id/sessions` — create a session in a topic.
///
/// Returns 201 Created with the new session's metadata, or 404 if the topic is not found.
pub async fn create_topic_session(
    State(state): State<AppState>,
    Path(topic_id): Path<Uuid>,
    payload: Option<Json<CreateConversationRequest>>,
) -> Result<impl IntoResponse, ApiError> {
    let config = state.active_config();
    let svc = ConversationService::new(state.storage, state.topic_store)
        .with_config(config, state.config_root);
    let request = payload.map(|Json(request)| request).unwrap_or_default();
    let resp = svc
        .create_session_in_topic_from_request(topic_id, request)
        .await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `DELETE /api/conversations/:id` — delete a conversation.
///
/// Returns 204 No Content on success, or 404 if not found.
pub async fn delete_conversation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let svc = ConversationService::new(state.storage, state.topic_store);
    svc.delete_conversation(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Topic handlers
// ---------------------------------------------------------------------------

/// `POST /api/topics` — create a new topic.
///
/// Returns 201 Created with the topic's metadata, or 409/422 on error.
pub async fn create_topic(
    State(state): State<AppState>,
    Json(req): Json<CreateTopicRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = TopicService::new(state.topic_store, state.config_root);
    let resp = svc.create_topic(req).await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/topics` — list all topics.
///
/// Returns 200 with `{ topics: [...] }`.
pub async fn list_topics(
    State(state): State<AppState>,
) -> Result<Json<TopicListResponse>, ApiError> {
    let svc = TopicService::new(state.topic_store, state.config_root);
    let resp = svc.list_topics().await?;
    Ok(Json(resp))
}

/// `GET /api/topics/:id` — get a topic by ID.
///
/// Returns 200 with the topic, or 404 if not found.
pub async fn get_topic(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TopicResponse>, ApiError> {
    let svc = TopicService::new(state.topic_store, state.config_root);
    let resp = svc.get_topic(id).await?;
    Ok(Json(resp))
}

/// `PUT /api/topics/:id` — partially update a topic.
///
/// Returns 200 with the updated topic, 404 if not found, or 422/409 on error.
pub async fn update_topic(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateTopicRequest>,
) -> Result<Json<TopicResponse>, ApiError> {
    let svc = TopicService::new(state.topic_store, state.config_root);
    let resp = svc.update_topic(id, req).await?;
    Ok(Json(resp))
}

/// `DELETE /api/topics/:id` — delete a topic.
///
/// Returns 204 No Content on success, 404 if not found, or 409 if refusing
/// to delete the general scope.
pub async fn delete_topic(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let svc = TopicService::new(state.topic_store, state.config_root);
    svc.delete_topic(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/topics/:id/workspaces` — list workspaces attached to a topic.
pub async fn list_topic_workspaces(
    State(state): State<AppState>,
    Path(topic_id): Path<Uuid>,
) -> Result<Json<WorkspaceAttachmentListResponse>, ApiError> {
    let svc = WorkspaceAttachmentService::from_state(state);
    Ok(Json(svc.list_workspaces(topic_id).await?))
}

/// `POST /api/topics/:id/workspaces` — attach an external workspace.
pub async fn attach_topic_workspace(
    State(state): State<AppState>,
    Path(topic_id): Path<Uuid>,
    Json(req): Json<CreateWorkspaceAttachmentRequest>,
) -> Result<(StatusCode, Json<WorkspaceAttachmentResponse>), ApiError> {
    let svc = WorkspaceAttachmentService::from_state(state);
    Ok((
        StatusCode::CREATED,
        Json(svc.attach_workspace(topic_id, req).await?),
    ))
}

/// `PUT /api/topics/:topic_id/workspaces/:workspace_id/trust` — set workspace trust.
pub async fn update_topic_workspace_trust(
    State(state): State<AppState>,
    Path((topic_id, workspace_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateWorkspaceTrustRequest>,
) -> Result<Json<WorkspaceAttachmentResponse>, ApiError> {
    let svc = WorkspaceAttachmentService::from_state(state);
    Ok(Json(svc.set_trust(topic_id, workspace_id, req).await?))
}

/// `DELETE /api/topics/:topic_id/workspaces/:workspace_id` — detach a workspace.
pub async fn detach_topic_workspace(
    State(state): State<AppState>,
    Path((topic_id, workspace_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let svc = WorkspaceAttachmentService::from_state(state);
    svc.detach_workspace(topic_id, workspace_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/conversations/:id/branches` — create a branch thread.
pub async fn create_conversation_branch(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<CreateConversationBranchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = ConversationService::new(state.storage, state.topic_store);
    let resp = svc.create_branch(session_id, req).await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/conversations/:id/branches` — list branches in a conversation.
pub async fn list_conversation_branches(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<ConversationBranchListResponse>, ApiError> {
    let svc = ConversationService::new(state.storage, state.topic_store);
    let resp = svc.list_branches(session_id).await?;
    Ok(Json(resp))
}

/// `POST /api/threads/:id/abandon` — set a branch aside; it takes no further
/// turns and keeps its history and lineage.
pub async fn abandon_thread(
    State(state): State<AppState>,
    Path(thread_id): Path<Uuid>,
) -> Result<Json<ConversationThreadDetail>, ApiError> {
    let svc = ConversationService::new(state.storage, state.topic_store);
    let resp = svc.abandon_thread(thread_id).await?;
    Ok(Json(resp))
}

/// `GET /api/threads/:id` — fetch a single thread with messages and lineage.
pub async fn get_thread_detail(
    State(state): State<AppState>,
    Path(thread_id): Path<Uuid>,
) -> Result<Json<ConversationThreadDetail>, ApiError> {
    let svc = ConversationService::new(state.storage, state.topic_store);
    let resp = svc.get_thread_detail(thread_id).await?;
    Ok(Json(resp))
}

// ---------------------------------------------------------------------------
// Skills listing
// ---------------------------------------------------------------------------

/// Query parameters for `GET /api/skills`.
#[derive(Debug, Deserialize)]
pub struct SkillsQueryParams {
    /// The topic whose hierarchy to resolve skills against; absent resolves
    /// the general scope, which has no topic layer.
    pub topic_id: Option<Uuid>,
    /// Agent ID to include agent-level skills. When provided, the agent's
    /// directory is searched for a `skills/` folder.
    pub agent_id: Option<String>,
}

/// `GET /api/skills` — list skills available for a topic/agent combination.
///
/// Resolves skills from the Swage config hierarchy (global → topic → agent),
/// applying override rules when the same skill name appears at multiple levels.
/// Returns 200 with `{ skills: [...] }`, sorted agent-first then alphabetically.
///
/// Returns 404 if `topic_id` does not correspond to a known topic.
/// Returns 400 if `topic_id` is not a valid UUID (axum `Query` extractor
/// enforces this automatically). Without a `topic_id`, lists the general
/// scope's skills: the global layer and the agent's own.
///
/// When the Swage config root directory does not exist (e.g. on a fresh
/// install or in a test environment without `SWAGE_HOME`), returns an empty
/// skills list rather than an error.
pub async fn list_skills(
    State(state): State<AppState>,
    Query(params): Query<SkillsQueryParams>,
) -> Result<Json<SkillListResponse>, ApiError> {
    use swage_config::HierarchyLevel;

    // Verify the topic exists — return 404 for unknown IDs. No topic means
    // the general scope.
    let topic = match params.topic_id {
        Some(topic_id) => state
            .topic_store
            .get(topic_id)
            .await
            .map_err(|e| ApiError::from(GatewayError::Storage(e)))?
            .ok_or(ApiError::from(GatewayError::TopicNotFound { id: topic_id }))?,
        None => state
            .topic_store
            .general_scope()
            .await
            .map_err(|e| ApiError::from(GatewayError::Storage(e)))?,
    };

    // Skill discovery is best-effort beneath the root retained by this gateway.
    let root = state.config_root.clone();

    let skills = crate::skill::discover_available_for_scope(
        &root,
        &topic.location(),
        params.agent_id.as_deref(),
    );

    let skill_items: Vec<SkillItem> = skills
        .into_iter()
        .map(|s| {
            let always_active = s.level == HierarchyLevel::Agent;
            let level = match s.level {
                HierarchyLevel::Agent => "agent",
                HierarchyLevel::Topic => "topic",
                HierarchyLevel::Global => "global",
            };
            SkillItem {
                id: s.shipped.then(|| {
                    let shipped = crate::shipped_skills::find_by_name(&s.name)
                        .expect("shipped definition has a catalogue entry");
                    crate::shipped_skills::id(topic.id, shipped.name)
                }),
                name: s.name,
                display_name: s.display_name,
                description: s.description,
                level: level.to_string(),
                tools: s.tools,
                always_active,
                shipped: s.shipped,
            }
        })
        .collect();

    Ok(Json(SkillListResponse {
        skills: skill_items,
    }))
}

/// `GET /api/turns/:id/context` — fetch the context assembly report for a completed turn.
///
/// Returns 200 with `{ turn_id, report }` if a snapshot is found. The `report`
/// field has the same shape as the `ContextAssembled` WebSocket event's report
/// field (a serialised `AssemblyReport`).
///
/// Returns 404 when no snapshot exists for the given turn — either the turn ID
/// is unknown or the turn completed before context assembly was enabled.
pub async fn get_turn_context(
    State(state): State<AppState>,
    Path(turn_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let snapshot = state
        .storage
        .get_snapshot(turn_id)
        .await
        .map_err(|e| ApiError::from(GatewayError::Storage(e)))?;

    match snapshot {
        None => Err(ApiError::not_found("context snapshot for turn", turn_id)),
        Some(report_json) => {
            let report: swage_protocol::client::context::AssemblyReport =
                serde_json::from_str(&report_json)
                    .map_err(|e| ApiError::from(GatewayError::ConfigError(e.to_string())))?;
            Ok(
                Json(swage_protocol::client::context::TurnContextResponse { turn_id, report })
                    .into_response(),
            )
        }
    }
}

/// `POST /api/shutdown` — initiate a graceful server shutdown.
///
/// # Localhost restriction
///
/// Only requests from `127.0.0.1` or `::1` are accepted. All other source
/// addresses receive `403 Forbidden`. When `ConnectInfo` is absent (e.g. in
/// unit tests that use `Router::oneshot` without a real TCP connection) the
/// restriction is bypassed — this is intentional test-environment behaviour.
///
/// # Behaviour
///
/// 1. Broadcasts `ServerEvent::Shutdown` to all connected WebSocket clients
///    so they can close gracefully before the connection is dropped.
/// 2. Cancels `AppState::shutdown_signal`, which the server's graceful-shutdown
///    future awaits alongside SIGTERM.
/// 3. Returns `200 OK` with `{ status: "shutting_down", message }`.
pub async fn shutdown(
    State(state): State<AppState>,
    connect_info: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> impl IntoResponse {
    use crate::websocket::ServerEvent;

    // Enforce localhost-only access when a real TCP connection provides the
    // peer address. Without ConnectInfo (unit tests / same-process calls),
    // the restriction is not enforced.
    if let Some(Extension(ConnectInfo(addr))) = connect_info
        && !addr.ip().is_loopback()
    {
        return ApiError::forbidden("shutdown is only accessible from localhost").into_response();
    }

    let reason = "admin shutdown request".to_string();

    // Notify all connected WebSocket clients. Errors here are non-fatal:
    // if no clients are subscribed the send returns Err(SendError), which we
    // discard — the server is shutting down anyway.
    let _ = state.broadcast.send(ServerEvent::Shutdown {
        reason: reason.clone(),
    });

    // Signal the graceful-shutdown future so axum drains in-flight requests
    // before the process exits.
    state.shutdown_signal.cancel();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "shutting_down",
            "message": format!("Gateway is shutting down: {reason}"),
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Programmatic interface — REST endpoints for async turn submission,
// approval decisions, agent messaging, and turn listing.
// ---------------------------------------------------------------------------

pub use swage_protocol::client::turn::{
    MessageResponse, SendMessageRequest, SubmitTurnRequest, SubmitTurnResponse, TurnPollResponse,
};

/// `POST /api/turns` — submit a turn for async processing.
///
/// Returns 202 Accepted with the turn_id. The client polls GET /api/turns/:id
/// for the result. The turn is dispatched to the agent runtime in the background —
/// no WebSocket connection required.
pub async fn submit_turn(
    State(state): State<AppState>,
    Json(req): Json<SubmitTurnRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use swage_storage::model::NewTurn;

    let session = state
        .storage
        .get_session(req.conversation_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("conversation", req.conversation_id))?;

    let thread = crate::thread_selection::resolve_thread_for_session(
        state.storage.as_ref(),
        req.conversation_id,
        req.thread_id,
    )
    .await
    .map_err(ApiError::from)?;

    crate::turn::resolve_turn_model_and_attachments(
        &state,
        state.active_config(),
        req.model.as_deref(),
        session.selected_model_id.as_deref(),
        session.id,
        &req.attachments,
    )
    .map_err(ApiError::from)?;
    if state
        .active_turns
        .lock()
        .await
        .contains_key(&req.conversation_id)
    {
        return Err(GatewayError::TurnInProgress {
            session_id: req.conversation_id,
        }
        .into());
    }

    let existing_turns = state
        .storage
        .list_turns_by_thread(thread.id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    // REST submit creates a durable pending placeholder before the background
    // worker has a chance to register `active_turns`, so every non-terminal
    // persisted state must count as in-flight here to close that race.
    if existing_turns.iter().any(|turn| !turn.state.is_terminal()) {
        return Err(GatewayError::TurnInProgress {
            session_id: req.conversation_id,
        }
        .into());
    }

    // Create a placeholder turn in Pending state — visible to the poller
    // immediately. The agent will create its own internal turn record during
    // processing; the background turn loop reuses this pending turn so the REST
    // poller and turn list see one stable lifecycle record.
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: session.agent_id.clone(),
            state: swage_storage::TurnState::Pending,
            user_input: Some(req.message.clone()),
            parent_turn_id: None,
        })
        .await
        .map_err(|error| match error {
            swage_core::StorageError::ConstraintViolation(_) => GatewayError::TurnInProgress {
                session_id: req.conversation_id,
            }
            .into(),
            other => ApiError::internal(other.to_string()),
        })?;

    let turn_id = turn.id;
    let thread_id = thread.id;

    // Spawn background turn processing — the REST client polls for result.
    let bg_state = state.clone();
    let input = crate::turn::TurnInput::with_attachments(req.message, req.attachments);
    let output_schema = req.output_schema;
    let compaction_focus = req.compaction_focus;
    let model = req.model;
    let session_id = req.conversation_id;
    tokio::spawn(async move {
        use crate::subscription::SubscriptionState;
        use std::sync::Arc;
        use tokio::sync::{Mutex, mpsc};

        // Create a dummy WebSocket sender — events are discarded but the
        // turn processing infrastructure requires it.
        let (ws_tx, mut ws_rx) = mpsc::unbounded_channel();
        let subscription = Arc::new(Mutex::new(SubscriptionState::new(false)));

        // Drain events in background so the channel never fills.
        tokio::spawn(async move { while ws_rx.recv().await.is_some() {} });

        let result = crate::turn::process_turn_with_existing_turn_and_focus_and_model(
            &bg_state,
            session_id,
            Some(thread_id),
            input,
            ws_tx,
            subscription,
            output_schema,
            None,
            None,
            None,
            Some(turn_id),
            crate::turn::TurnAttendance::Unattended,
            compaction_focus,
            model,
        )
        .await;

        match result {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(turn_id = %turn_id, error = %e, "async turn processing failed");
                let _ = bg_state
                    .storage
                    .update_turn_state(turn_id, swage_storage::TurnState::Failed)
                    .await;
            }
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(SubmitTurnResponse {
            turn_id,
            status: "pending".into(),
        }),
    ))
}

/// `GET /api/turns/:id` — poll a turn for status and result.
///
/// Returns the turn's current state. When completed, includes the assistant
/// response, structured output, and token counts.
pub async fn get_turn(
    State(state): State<AppState>,
    Path(turn_id): Path<Uuid>,
) -> Result<Json<TurnPollResponse>, ApiError> {
    let turn = state
        .storage
        .get_turn(turn_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("turn", turn_id))?;

    Ok(Json(TurnPollResponse {
        turn_id: turn.id,
        state: turn.state.to_string(),
        assistant_response: turn.assistant_response,
        structured_output: turn.structured_output,
        input_tokens: turn.input_tokens,
        output_tokens: turn.output_tokens,
        delegated_input_tokens: turn.delegated_input_tokens,
        delegated_output_tokens: turn.delegated_output_tokens,
        parent_turn_id: turn.parent_turn_id,
    }))
}

/// `POST /api/messages` — send a typed message to an agent.
///
/// Creates a durable message (Delegate, Result, Cancel, Shutdown) or an
/// ephemeral Notify. Returns the message ID and initial status.
pub async fn send_agent_message(
    State(state): State<AppState>,
    Json(req): Json<SendMessageRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use swage_agent::MessageRouter;
    use swage_core::MessageKind;
    use swage_storage::model::NewAgentMessage;

    let kind: MessageKind = req
        .kind
        .parse::<MessageKind>()
        .map_err(|e| ApiError::validation(e.to_string()))?;
    let topic_id = req.topic_id;

    if kind == MessageKind::Delegate && topic_id.is_none() {
        return Err(ApiError::validation(
            "delegate messages require topic_id so execution uses the correct topic configuration"
                .to_string(),
        ));
    }

    // Durable messages are persisted first; delegate messages then start
    // background execution so the caller can poll the message status.
    let router = MessageRouter::new(state.storage.clone(), None);
    let sender_agent_id = req.sender_agent_id;
    let recipient_agent_id = req.recipient_agent_id;
    let payload = req.payload;
    let sender_turn_id = req.sender_turn_id;

    let msg = router
        .send_message(NewAgentMessage {
            sender_agent_id,
            recipient_agent_id,
            kind,
            correlation_id: req.correlation_id,
            payload,
        })
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    if msg.kind == MessageKind::Delegate {
        let execution_topic_id = topic_id.expect("delegate topic_id was validated before storage");
        let state_for_task = state.clone();
        let msg_for_task = msg.clone();
        tokio::spawn(async move {
            let message_id = msg_for_task.id;
            if let Err(error) = Box::pin(execute_external_delegate_message(
                state_for_task.clone(),
                msg_for_task,
                execution_topic_id,
                sender_turn_id,
            ))
            .await
            {
                tracing::warn!(error = ?error, "external delegate execution failed");
                let router = MessageRouter::new(state_for_task.storage.clone(), None);
                if let Err(fail_error) = router.fail_message(message_id, error.message()).await {
                    tracing::warn!(
                        message_id = %message_id,
                        error = ?fail_error,
                        "failed to mark external delegate message failed"
                    );
                }
            }
        });
    }

    Ok((
        StatusCode::CREATED,
        Json(MessageResponse {
            id: msg.id,
            status: msg.status.to_string(),
        }),
    ))
}

async fn execute_external_delegate_message(
    state: AppState,
    delegate_msg: swage_storage::model::AgentMessage,
    topic_id: Uuid,
    sender_turn_id: Option<Uuid>,
) -> Result<(), ApiError> {
    use std::sync::Arc;

    use swage_agent::{DelegationExecutor, MessageRouter};

    let topic = state
        .topic_store
        .get(topic_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("topic", topic_id))?;

    let target_resolved = crate::turn::load_session_config(
        &state,
        &topic.location(),
        &delegate_msg.recipient_agent_id,
    )
    .map_err(|error| ApiError::internal(error.to_string()))?;
    let mut bound_agent_ids = match sender_turn_id {
        Some(turn_id) => crate::turn::dispatch_bound_agent_ids_for_turn(&state, turn_id)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?,
        None => Vec::new(),
    };
    bound_agent_ids.push(delegate_msg.sender_agent_id.clone());
    let bound_task_definition_ids = match sender_turn_id {
        Some(turn_id) => crate::turn::dispatch_bound_task_definition_ids_for_turn(&state, turn_id)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?,
        None => Vec::new(),
    };
    let effective_target_resolved = crate::turn::apply_dispatch_bounds(
        &state,
        &topic.location(),
        topic_id,
        target_resolved.clone(),
        &bound_agent_ids,
        &format!("delegated agent {:?}", delegate_msg.recipient_agent_id),
    )
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?;
    let effective_target_resolved = crate::turn::apply_dispatch_task_bounds(
        &state,
        effective_target_resolved,
        &bound_task_definition_ids,
        &format!("delegated agent {:?}", delegate_msg.recipient_agent_id),
    )
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?;
    let mut session_metadata = crate::turn::dispatch_bound_metadata(&bound_agent_ids)
        .expect("a delegated session always has at least its sender bound");
    crate::turn::add_dispatch_bound_task_definition_ids(
        &mut session_metadata,
        &bound_task_definition_ids,
    );
    let active_workspace_attachment_id =
        active_workspace_attachment_for_sender_turn(&state, sender_turn_id).await?;
    let mut target_agent_config = crate::turn::build_agent_config_for_topic_workspace(
        &state,
        delegate_msg.recipient_agent_id.clone(),
        &target_resolved,
        None,
        &topic,
        sender_turn_id.unwrap_or_else(Uuid::new_v4),
        active_workspace_attachment_id,
    )
    .await;
    target_agent_config.approval_prompts_enabled = false;
    let target_context_transfer_config = target_resolved.communication.context_transfer.clone();
    let composition = crate::turn::DispatchContextComposition::from_payload(&delegate_msg.payload)
        .map_err(|error| ApiError::validation(error.to_string()))?;
    // Delegated activity must not reach the parent WebSocket. Shared assembly
    // still emits ContextAssembled into this silent channel; the stored child-
    // turn snapshot is the delegated assembly's audit record.
    let (silent_ws_tx, silent_ws_rx) = tokio::sync::mpsc::unbounded_channel();
    drop(silent_ws_rx);
    let dispatcher_build = crate::turn::build_gateway_dispatcher_for_unattended_execution(
        &state,
        crate::turn::GatewayDispatcherRequest {
            config: &effective_target_resolved,
            agent_config: &target_agent_config,
            session_metadata: None,
            topic_id: Some(topic_id),
            active_workspace_attachment_id: None,
            ws_tx: silent_ws_tx.clone(),
            session_id: None,
            turn_id: sender_turn_id,
        },
        "external delegated turn",
    )
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?;

    let router = Arc::new(MessageRouter::new(state.storage.clone(), None));

    let executor = DelegationExecutor {
        storage: state.storage.clone(),
        provider: state.active_provider(),
        hook_registry: state.hook_registry.clone(),
        leak_detector: None,
        approval_registrar: Some(crate::turn::build_approval_registrar(
            state.approval_channels.clone(),
        )),
        delegation_handler: Some(Arc::new(crate::turn::GatewayDelegationHandler::new(
            state.clone(),
        ))),
        router: router.clone(),
        active_delegations: swage_agent::active_delegations(),
        context_assembler: crate::turn::delegated_context_assembler(
            state.clone(),
            topic.clone(),
            target_resolved.clone(),
            delegate_msg.recipient_agent_id.clone(),
            target_agent_config.working_directory.display().to_string(),
            silent_ws_tx,
            crate::turn::effective_tool_permissions_record(&effective_target_resolved),
            composition,
        ),
        snapshot_recorder: Some(crate::turn::delegation_snapshot_recorder_for_state(
            state.clone(),
        )),
    };

    let result = executor
        .execute_delegation(
            &delegate_msg,
            sender_turn_id,
            None,
            topic_id,
            target_agent_config,
            target_context_transfer_config,
            Some(dispatcher_build.dispatcher),
            session_metadata,
            tokio_util::sync::CancellationToken::new(),
            None,
            None,
            None,
        )
        .await;

    crate::turn::shutdown_sandbox_guard(
        &dispatcher_build.sandbox_guard,
        "failed to shut down external delegated sandbox",
    )
    .await;

    let result_msg = result.map_err(|error| ApiError::internal(error.to_string()))?;
    deliver_external_delegate_completion(
        state.storage.clone(),
        &router,
        delegate_msg.id,
        &result_msg,
    )
    .await?;

    Ok(())
}

async fn active_workspace_attachment_for_sender_turn(
    state: &AppState,
    sender_turn_id: Option<Uuid>,
) -> Result<Option<Uuid>, ApiError> {
    let Some(turn_id) = sender_turn_id else {
        return Ok(None);
    };
    let Some(turn) = state
        .storage
        .get_turn(turn_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    let Some(thread) = swage_storage::store::ThreadStore::get(&*state.storage, turn.thread_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    let Some(session) = state
        .storage
        .get_session(thread.session_id)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    Ok(session.active_workspace_attachment_id)
}

async fn deliver_external_delegate_completion(
    storage: Arc<dyn swage_storage::store::Storage>,
    router: &swage_agent::MessageRouter,
    delegate_message_id: Uuid,
    result_msg: &swage_storage::model::AgentMessage,
) -> Result<(), ApiError> {
    // Deliver the result before completing the delegate. If result delivery
    // fails, the spawned caller still has a pending delegate it can fail with a
    // machine-readable status.
    router
        .deliver_message(result_msg)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let current_delegate = storage
        .get_agent_message(delegate_message_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("message", delegate_message_id))?;

    if current_delegate.status == swage_core::MessageStatus::Pending {
        router
            .deliver_message(&current_delegate)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?;
    }

    Ok(())
}

/// `GET /api/messages/:id` — query message delivery status.
pub async fn get_message_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<MessageResponse>, ApiError> {
    let msg = state
        .storage
        .get_agent_message(id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("message", id))?;

    Ok(Json(MessageResponse {
        id: msg.id,
        status: msg.status.to_string(),
    }))
}

pub use swage_protocol::hosting::{ApprovalListQuery, ApprovalListResponse, ApprovalSummary};

/// `GET /api/approvals` — list pending approval requests.
///
/// Optionally filtered by agent_id. Returns all pending approvals visible
/// to the caller.
pub async fn list_approvals(
    State(state): State<AppState>,
    Query(query): Query<ApprovalListQuery>,
) -> Result<Json<ApprovalListResponse>, ApiError> {
    let approvals = if let Some(ref agent_id) = query.agent_id {
        state.storage.list_pending_by_agent(agent_id).await
    } else {
        state.storage.list_all_pending().await
    }
    .map_err(|e| ApiError::internal(e.to_string()))?;

    let summaries = approvals
        .into_iter()
        .map(|a| ApprovalSummary {
            id: a.id,
            turn_id: a.turn_id,
            agent_id: a.agent_id,
            kind: a.kind.to_string(),
            tool_name: a.tool_name,
            prompt: a.prompt,
            timeout_at: Some(a.timeout_at.to_rfc3339()),
        })
        .collect();

    Ok(Json(ApprovalListResponse {
        approvals: summaries,
    }))
}

/// The two valid approval outcomes.
///
/// Used for both REST and WebSocket decision paths. Parsed from the request
/// string early so invalid values surface as 422 before any side-effects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ApprovalOutcome {
    Approved,
    Rejected,
}

impl ApprovalOutcome {
    /// Parse a decision string, returning `None` for unrecognised values.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

pub use swage_protocol::hosting::ApprovalDecisionRequest;

/// `POST /api/approvals/:id` — decide on a pending approval.
///
/// Routes through the same approval channel mechanism as WebSocket decisions.
/// Returns 200 on success, 404 if the approval doesn't exist, 409 if already decided.
pub async fn decide_approval(
    State(state): State<AppState>,
    Path(approval_id): Path<Uuid>,
    Json(req): Json<ApprovalDecisionRequest>,
) -> Result<impl IntoResponse, ApiError> {
    use swage_agent::ApprovalDecision;

    // Validate the decision string early — before any side effects.
    let outcome = match req.decision {
        ApprovalDecisionValue::Approved => ApprovalOutcome::Approved,
        ApprovalDecisionValue::Rejected => ApprovalOutcome::Rejected,
    };

    let approval = state
        .storage
        .get_approval(approval_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("approval", approval_id))?;
    if approval.state != swage_storage::model::ApprovalState::Pending {
        return Err(ApiError::conflict(
            "approval request has already been resolved".to_string(),
        ));
    }

    // Try to fire the oneshot channel for this approval.
    let sender = state
        .approval_channels
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&approval_id);

    // Map the validated outcome to storage state once, used by all paths.
    let new_state = match outcome {
        ApprovalOutcome::Approved => swage_storage::model::ApprovalState::Approved,
        ApprovalOutcome::Rejected => swage_storage::model::ApprovalState::Rejected,
    };

    match sender {
        Some(tx) => {
            let decision = ApprovalDecision {
                decision: req.decision,
                scope: req.scope,
                reason: req.reason.clone(),
            };

            if tx.send(decision).is_ok() {
                crate::approval_scope::apply_approval_scope(
                    &state,
                    &approval,
                    req.decision,
                    req.scope,
                )
                .await?;
                // Agent received the decision via the live channel.
                Ok((
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "approval_id": approval_id,
                        "decision": outcome.as_str(),
                    })),
                ))
            } else {
                // Agent timed out or cancelled just before we sent —
                // persist the decision to storage so it isn't lost.
                match state
                    .storage
                    .update_approval_state(approval_id, new_state, req.reason.as_deref())
                    .await
                {
                    Ok(true) => {
                        crate::approval_scope::apply_approval_scope(
                            &state,
                            &approval,
                            req.decision,
                            req.scope,
                        )
                        .await?;
                        Ok((
                            StatusCode::OK,
                            Json(serde_json::json!({
                                "approval_id": approval_id,
                                "decision": outcome.as_str(),
                                "note": "agent timed out; decision persisted to storage",
                            })),
                        ))
                    }
                    Ok(false) => Err(ApiError::conflict(format!(
                        "approval {approval_id} already resolved"
                    ))),
                    Err(e) => Err(ApiError::internal(e.to_string())),
                }
            }
        }
        None => {
            // No in-memory channel — either already resolved, or the agent
            // timed out / restarted. Persist the decision to storage directly
            // so recovered approvals are resolvable via the REST API.
            let existing = state
                .storage
                .get_approval(approval_id)
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?;

            match existing {
                None => Err(ApiError::not_found("approval", approval_id)),
                Some(a) if a.state != swage_storage::model::ApprovalState::Pending => Err(
                    ApiError::conflict(format!("approval {} already {}", approval_id, a.state)),
                ),
                Some(a) => {
                    // Pending in storage with no live channel — persist the
                    // decision so the approval record reflects the outcome.
                    // Because there is no live channel, the original waiting
                    // agent no longer exists; interrupt the stranded turn so
                    // the conversation is not left permanently in-progress.
                    match state
                        .storage
                        .update_approval_state(approval_id, new_state, req.reason.as_deref())
                        .await
                    {
                        Ok(true) => {
                            crate::approval_scope::apply_approval_scope(
                                &state,
                                &a,
                                req.decision,
                                req.scope,
                            )
                            .await?;
                            let note = match state
                                .storage
                                .update_turn_state(
                                    a.turn_id,
                                    swage_storage::TurnState::Interrupted,
                                )
                                .await
                            {
                                Ok(()) => {
                                    "no live channel; decision persisted and stranded turn interrupted"
                                        .to_string()
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        approval_id = %approval_id,
                                        turn_id = %a.turn_id,
                                        %error,
                                        "persisted recovered approval decision but failed to interrupt stranded turn"
                                    );
                                    "no live channel; decision persisted, but interrupting the stranded turn failed"
                                        .to_string()
                                }
                            };

                            Ok((
                                StatusCode::OK,
                                Json(serde_json::json!({
                                    "approval_id": approval_id,
                                    "decision": outcome.as_str(),
                                    "note": note,
                                })),
                            ))
                        }
                        Ok(false) => Err(ApiError::conflict(format!(
                            "approval {approval_id} already resolved"
                        ))),
                        Err(e) => Err(ApiError::internal(e.to_string())),
                    }
                }
            }
        }
    }
}

/// Turn listing response for a conversation.
#[derive(Debug, Serialize)]
pub struct TurnListResponse {
    pub turns: Vec<TurnPollResponse>,
}

/// Query parameters for turn listing.
#[derive(Debug, Deserialize)]
pub struct TurnListQuery {
    pub thread_id: Option<Uuid>,
    pub state: Option<String>,
    pub agent_id: Option<String>,
}

/// `GET /api/conversations/:id/turns` — list turns in a conversation.
///
/// Returns turns for the default thread, optionally filtered by state and agent_id.
pub async fn list_turns(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Query(query): Query<TurnListQuery>,
) -> Result<Json<TurnListResponse>, ApiError> {
    // Verify session exists.
    state
        .storage
        .get_session(session_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("conversation", session_id))?;

    let thread = crate::thread_selection::resolve_thread_for_session(
        state.storage.as_ref(),
        session_id,
        query.thread_id,
    )
    .await
    .map_err(ApiError::from)?;

    // List all turns in the thread.
    let mut turns = state
        .storage
        .list_turns_by_thread(thread.id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Apply optional filters.
    if let Some(ref state_filter) = query.state {
        turns.retain(|t| t.state.to_string() == *state_filter);
    }
    if let Some(ref agent_filter) = query.agent_id {
        turns.retain(|t| t.agent_id == *agent_filter);
    }

    let responses = turns
        .into_iter()
        .map(|t| TurnPollResponse {
            turn_id: t.id,
            state: t.state.to_string(),
            assistant_response: t.assistant_response,
            structured_output: t.structured_output,
            input_tokens: t.input_tokens,
            output_tokens: t.output_tokens,
            delegated_input_tokens: t.delegated_input_tokens,
            delegated_output_tokens: t.delegated_output_tokens,
            parent_turn_id: t.parent_turn_id,
        })
        .collect();

    Ok(Json(TurnListResponse { turns: responses }))
}

// ---------------------------------------------------------------------------
// Integration tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // Test HTTP servers are scoped to individual tests; abort is the cleanup
    // path after assertions have observed the gateway behaviour.

    use std::{net::SocketAddr, sync::Arc};

    use axum::{Router, body::to_bytes, extract::ConnectInfo, http::Request};
    use tokio::sync::broadcast;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use uuid::Uuid;

    use swage_provider::LlmProvider;
    use swage_storage::store::{MessageStore, SessionStore, Storage, TopicStore, TurnStore};

    use super::*;
    use crate::{
        server::build_router,
        state::AppState,
        test_support::SwageHomeGuard,
        turn::tests::{CapturingMultiRoundProvider, MockProvider, MockStorage},
        websocket::ServerEvent,
    };

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    /// Build an axum `Router` backed by an empty `MockStorage`.
    ///
    /// Returns the router and the storage handle so tests can inspect state.
    fn make_router() -> (Router, Arc<MockStorage>) {
        let storage = Arc::new(MockStorage::new());
        let provider = Arc::new(MockProvider::new(vec![]));
        make_router_with_provider(provider as Arc<dyn LlmProvider>, storage)
    }

    fn make_router_with_provider(
        provider: Arc<dyn LlmProvider>,
        storage: Arc<MockStorage>,
    ) -> (Router, Arc<MockStorage>) {
        make_router_with_provider_and_config(
            provider,
            storage,
            swage_config::ResolvedConfig::default(),
        )
    }

    fn make_router_with_provider_and_config(
        provider: Arc<dyn LlmProvider>,
        storage: Arc<MockStorage>,
        config: swage_config::ResolvedConfig,
    ) -> (Router, Arc<MockStorage>) {
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            provider,
            config,
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        (build_router(state), storage)
    }

    async fn spawn_live_server(
        provider: Arc<dyn LlmProvider>,
        storage: Arc<MockStorage>,
    ) -> (String, tokio::task::JoinHandle<()>, Arc<MockStorage>) {
        let (router, storage) = make_router_with_provider(provider, storage);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve test router");
        });
        (format!("http://{address}"), server, storage)
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn effective_agent_response_maps_each_source_scope() {
        fn effective(
            topic_id: Option<Uuid>,
            is_builtin: bool,
        ) -> crate::service::EffectiveAgentDefinition {
            crate::service::EffectiveAgentDefinition {
                definition: swage_storage::NewAgentDefinition {
                    id: "agent".into(),
                    topic_id,
                    display_name: "Agent".into(),
                    description: String::new(),
                    instructions: String::new(),
                    model_id: None,
                    tool_permissions: None,
                    skills: Vec::new(),
                    output_schema: None,
                    output_max_retries: None,
                },
                is_builtin,
            }
        }

        assert_eq!(
            effective_agent_definition_response(effective(None, true)).source_scope,
            EffectiveAgentDefinitionSourceScope::BuiltIn
        );
        assert_eq!(
            effective_agent_definition_response(effective(None, false)).source_scope,
            EffectiveAgentDefinitionSourceScope::Global
        );
        assert_eq!(
            effective_agent_definition_response(effective(Some(Uuid::new_v4()), false))
                .source_scope,
            EffectiveAgentDefinitionSourceScope::Topic
        );
    }

    async fn wait_for_terminal_message_status(
        client: &swage_client::http::GatewayClient,
        message_id: Uuid,
    ) -> swage_client::http::MessageResponse {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < deadline {
            let current = client
                .get_message_status(message_id)
                .await
                .expect("query message status");
            if matches!(current.status.as_str(), "delivered" | "failed") {
                return current;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("delegate message should leave pending before the deadline");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gateway_client_async_rest_helpers_cover_live_gateway_routes() {
        use swage_client::http::GatewayClient;
        use swage_core::{FinishReason, StreamChunk, TokenUsage};
        use swage_storage::model::{ApprovalKind, NewApprovalRequest, NewTurn, TurnState};
        use swage_storage::store::{ApprovalStore, SessionStore, ThreadStore, TurnStore};

        let provider = Arc::new(MockProvider::new(vec![
            StreamChunk::Delta {
                content: "Hello from the async route".to_string(),
            },
            StreamChunk::Usage(TokenUsage {
                input_tokens: 12,
                output_tokens: 5,
            }),
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ]));
        let storage = Arc::new(MockStorage::new());
        let (base_url, server, storage) =
            spawn_live_server(provider as Arc<dyn LlmProvider>, storage).await;

        let client = GatewayClient::new(&base_url);
        let http = reqwest::Client::new();
        let settings = http
            .get(format!("{base_url}/api/settings"))
            .send()
            .await
            .expect("fetch settings through live gateway")
            .error_for_status()
            .expect("settings response succeeds")
            .json::<swage_protocol::client::settings::SettingsResponse>()
            .await
            .expect("settings response matches protocol");
        assert!(!settings.can_restart);

        let discovery = http
            .post(format!(
                "{base_url}/api/settings/providers/anthropic/models"
            ))
            .json(&swage_protocol::client::settings::SaveProviderSettings {
                protocol: swage_protocol::client::settings::ProviderProtocol::Anthropic,
                worker_staleness_threshold_ms: None,
                base_url: None,
                api_key_secret_ref: None,
                api_key_env: None,
                api_key: None,
                command: None,
                state_dir: None,
            })
            .send()
            .await
            .expect("discover models through live gateway")
            .error_for_status()
            .expect("model discovery succeeds")
            .json::<swage_protocol::client::settings::ModelDiscoveryResponse>()
            .await
            .expect("model discovery response matches protocol");
        assert_eq!(discovery.provider_id, "anthropic");
        assert!(!discovery.models.is_empty());

        let restart = http
            .get(format!("{base_url}/api/settings/restart-required"))
            .send()
            .await
            .expect("fetch restart status through live gateway")
            .error_for_status()
            .expect("restart status succeeds")
            .json::<swage_protocol::client::settings::RestartStatusResponse>()
            .await
            .expect("restart status response matches protocol");
        assert!(!restart.restart_required);
        let conversation = client
            .create_conversation()
            .await
            .expect("create conversation through live gateway");

        let turn_id = client
            .submit_turn(conversation.id, None, "hello", None)
            .await
            .expect("submit async turn");

        let poll = {
            // Generous deadline: under a full-workspace test run the async
            // turn competes with every other suite for CPU, and a one-second
            // budget starves reliably.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let mut completed = None;
            while std::time::Instant::now() < deadline {
                let current = client.poll_turn(turn_id).await.expect("poll turn");
                let state = current.state.as_str();
                if matches!(state, "completed" | "failed" | "interrupted") {
                    completed = Some(current);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            completed.expect("turn should reach a terminal state")
        };
        assert_eq!(poll.turn_id, turn_id);
        assert_eq!(poll.state, "completed");
        assert_eq!(
            poll.assistant_response.as_deref(),
            Some("Hello from the async route")
        );

        let context = http
            .get(format!("{base_url}/api/turns/{turn_id}/context"))
            .send()
            .await
            .expect("fetch turn context through live gateway")
            .error_for_status()
            .expect("turn context succeeds")
            .json::<swage_protocol::client::context::TurnContextResponse>()
            .await
            .expect("turn context response matches protocol");
        assert_eq!(context.turn_id, turn_id);
        assert!(!context.report.elements.is_empty());

        let turns = client
            .list_turns(conversation.id, None, None)
            .await
            .expect("list turns through live gateway");
        let turn_rows = turns["turns"].as_array().expect("turn list array");
        assert_eq!(
            turn_rows.len(),
            1,
            "async REST path should surface one turn record"
        );
        assert_eq!(turn_rows[0]["turn_id"], turn_id.to_string());

        let default_topic = storage.general_scope().await.expect("get default topic");
        let sent_message = client
            .send_agent_message_in_topic(
                default_topic.id,
                "agent-a",
                "agent-b",
                "delegate",
                None,
                serde_json::json!({ "task": "summarise" }),
            )
            .await
            .expect("send agent message through live gateway");
        let message_id = sent_message.id;
        let message_status = wait_for_terminal_message_status(&client, message_id).await;
        assert!(
            matches!(message_status.status.as_str(), "delivered" | "failed"),
            "delegate execution should transition the message out of pending"
        );

        let session = storage
            .get_session(conversation.id)
            .await
            .expect("get session")
            .expect("session exists");
        let thread = storage
            .list_by_session(session.id)
            .await
            .expect("list session threads")
            .into_iter()
            .next()
            .expect("default thread exists");
        let approval_turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "gateway".to_string(),
                state: TurnState::AwaitingApproval,
                user_input: None,
                parent_turn_id: None,
            })
            .await
            .expect("create approval turn");
        let approval = storage
            .create_approval(NewApprovalRequest {
                turn_id: approval_turn.id,
                agent_id: "gateway".to_string(),
                kind: ApprovalKind::Tool,
                tool_name: Some("shell".to_string()),
                tool_arguments: Some(serde_json::json!({ "command": "ls" })),
                hook_name: None,
                prompt: "Allow shell?".to_string(),
                context: None,
                timeout_at: chrono::Utc::now() + chrono::Duration::minutes(5),
                timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
            })
            .await
            .expect("create approval");

        let approvals = client
            .list_approvals(Some("gateway"))
            .await
            .expect("list approvals through live gateway");
        let approval_rows = approvals["approvals"]
            .as_array()
            .expect("approval list array");
        assert!(
            approval_rows
                .iter()
                .any(|row| row["id"] == approval.id.to_string()),
            "approval list should include the seeded approval"
        );

        let decision = client
            .decide_approval(
                approval.id,
                swage_protocol::client::approval::ApprovalDecisionValue::Approved,
                Some("looks safe"),
            )
            .await
            .expect("decide approval through live gateway");
        assert_eq!(decision["approval_id"], approval.id.to_string());
        assert_eq!(decision["decision"], "approved");

        let stored_approval = storage
            .get_approval(approval.id)
            .await
            .expect("get approval")
            .expect("approval exists");
        assert_eq!(
            stored_approval.state,
            swage_storage::model::ApprovalState::Approved
        );
        let recovered_turn = storage
            .get_turn(approval_turn.id)
            .await
            .expect("get approval turn")
            .expect("approval turn exists");
        assert_eq!(recovered_turn.state, TurnState::Interrupted);

        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn async_turn_focus_reaches_compaction_prompt_and_snapshot() {
        use std::pin::Pin;
        use std::sync::Mutex as StdMutex;

        use futures::{Stream, stream};
        use swage_client::http::GatewayClient;
        use swage_core::{
            CompletionRequest, CompletionResponse, FinishReason, ProviderError, Role, StreamChunk,
            ToolCompletionRequest, ToolCompletionResponse,
        };
        use swage_storage::model::AppendMessage;
        use swage_storage::store::{MessageStore, ThreadStore};

        struct CompactionCapturingProvider {
            compaction_requests: StdMutex<Vec<CompletionRequest>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for CompactionCapturingProvider {
            fn model_id(&self) -> &str {
                "compaction-capturing"
            }

            async fn complete(
                &self,
                request: CompletionRequest,
            ) -> Result<CompletionResponse, ProviderError> {
                self.compaction_requests
                    .lock()
                    .expect("compaction request lock")
                    .push(request);
                Ok(CompletionResponse {
                    content: "Compacted history".to_string(),
                    input_tokens: 10,
                    output_tokens: 2,
                    finish_reason: FinishReason::Stop,
                    cost: None,
                })
            }

            async fn complete_with_tools(
                &self,
                _request: ToolCompletionRequest,
            ) -> Result<ToolCompletionResponse, ProviderError> {
                unreachable!("focused turn uses streaming completion")
            }

            fn stream(
                &self,
                _request: CompletionRequest,
            ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
                Box::pin(stream::iter(vec![
                    StreamChunk::Delta {
                        content: "Focused reply".to_string(),
                    },
                    StreamChunk::Done {
                        finish_reason: Some(FinishReason::Stop),
                    },
                ]))
            }
        }

        let provider = Arc::new(CompactionCapturingProvider {
            compaction_requests: StdMutex::new(Vec::new()),
        });
        let storage = Arc::new(MockStorage::new());
        let (base_url, server, storage) =
            spawn_live_server(Arc::clone(&provider) as Arc<dyn LlmProvider>, storage).await;
        let client = GatewayClient::new(&base_url);
        let conversation = client
            .create_conversation()
            .await
            .expect("create conversation through live gateway");
        let thread = storage
            .list_by_session(conversation.id)
            .await
            .expect("list conversation threads")
            .into_iter()
            .next()
            .expect("default thread exists");

        for index in 0..101 {
            storage
                .append_message(AppendMessage {
                    thread_id: thread.id,
                    attachments: Vec::new(),
                    turn_id: None,
                    role: if index % 2 == 0 {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    content: format!("history message {index}"),
                    agent_id: "gateway".to_string(),
                })
                .await
                .expect("seed history message");
        }

        let focus = "Preserve dates and named decisions";
        let turn_id = client
            .submit_turn_with_compaction_focus(
                conversation.id,
                Some(thread.id),
                "continue",
                None,
                Some(focus.to_string()),
            )
            .await
            .expect("submit focused async turn");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let turn = client.poll_turn(turn_id).await.expect("poll focused turn");
            if matches!(turn.state.as_str(), "completed" | "failed" | "interrupted") {
                assert_eq!(turn.state, "completed");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "focused turn should finish before the deadline"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let requests = provider
            .compaction_requests
            .lock()
            .expect("compaction request lock");
        assert_eq!(requests.len(), 1, "history should trigger one compaction");
        assert!(
            requests[0]
                .messages
                .iter()
                .any(|message| message.text_content().contains(focus)),
            "the focus reaches the summarising request"
        );
        drop(requests);

        let context = reqwest::get(format!("{base_url}/api/turns/{turn_id}/context"))
            .await
            .expect("fetch focused turn context")
            .error_for_status()
            .expect("focused context response succeeds")
            .json::<swage_protocol::client::context::TurnContextResponse>()
            .await
            .expect("focused context matches protocol");
        assert_eq!(context.report.compaction_applied.len(), 1);
        assert_eq!(
            context.report.compaction_applied[0].focus.as_deref(),
            Some(focus)
        );

        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn external_delegate_stream_error_fails_original_message() {
        use swage_client::http::GatewayClient;
        use swage_core::StreamChunk;
        use swage_storage::store::AgentMessageStore;

        let provider = Arc::new(MockProvider::new(vec![StreamChunk::Error {
            message: "delegate stream failed".into(),
        }]));
        let storage = Arc::new(MockStorage::new());
        let (base_url, server, storage) =
            spawn_live_server(provider as Arc<dyn LlmProvider>, storage).await;

        let client = GatewayClient::new(&base_url);
        let default_topic = storage.general_scope().await.expect("get default topic");
        let sent_message = client
            .send_agent_message_in_topic(
                default_topic.id,
                "agent-a",
                "agent-b",
                "delegate",
                None,
                serde_json::json!({ "task": "fail during delegated work" }),
            )
            .await
            .expect("send delegate message through live gateway");
        let message_id = sent_message.id;

        let message_status = wait_for_terminal_message_status(&client, message_id).await;

        assert_eq!(message_status.status, "failed");
        let stored = storage
            .get_agent_message(message_id)
            .await
            .expect("message lookup")
            .expect("message exists");
        assert!(
            stored
                .failure_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("delegate stream failed")),
            "failure reason should include provider stream failure: {:?}",
            stored.failure_reason
        );

        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn malformed_external_delegate_composition_is_rejected_before_dispatcher_provisioning() {
        use swage_agent::MessageRouter;
        use swage_core::MessageKind;
        use swage_storage::Storage;

        let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("storage"));
        let topic = storage.general_scope().await.expect("topic");
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn swage_storage::TopicStore>,
            Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        // The injected failure is consumed at build_gateway_dispatcher entry.
        // Non-consumption therefore proves the builder was never entered.
        state.fail_next_dispatcher_builds(1);
        let router = MessageRouter::new(state.storage.clone(), None);
        let malformed = router
            .send_message(swage_storage::NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-b".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({"task": "inspect", "assembly": ["invalid"]}),
            })
            .await
            .expect("malformed delegate message");

        let error = Box::pin(execute_external_delegate_message(
            state.clone(),
            malformed,
            topic.id,
            None,
        ))
        .await
        .expect_err("malformed composition must be rejected");
        assert!(matches!(
            error.cause(),
            Some(GatewayError::ValidationError(_))
        ));

        let valid = router
            .send_message(swage_storage::NewAgentMessage {
                sender_agent_id: "agent-a".into(),
                recipient_agent_id: "agent-b".into(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({"task": "inspect", "assembly": "focused"}),
            })
            .await
            .expect("valid delegate message");
        let error = Box::pin(execute_external_delegate_message(
            state, valid, topic.id, None,
        ))
        .await
        .expect_err("unconsumed dispatcher failure proves malformed input provisioned nothing");
        assert!(
            error
                .message()
                .contains("injected dispatcher build failure")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn external_delegate_post_executes_granted_approval_gated_tool_without_parking() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use swage_core::{FinishReason, StreamChunk};
        use swage_storage::store::{AgentMessageStore, ApprovalStore};

        let _swage_home = SwageHomeGuard::new();

        struct CountingEchoTool(Arc<AtomicUsize>);

        #[async_trait::async_trait]
        impl crate::tool::Tool for CountingEchoTool {
            fn name(&self) -> &str {
                "echo"
            }

            fn description(&self) -> &str {
                "Echo the input back"
            }

            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                })
            }

            async fn execute(
                &self,
                arguments: serde_json::Value,
                _ctx: &dyn swage_tool::context::ToolContext,
            ) -> Result<serde_json::Value, swage_core::ToolError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({ "output": arguments["text"] }))
            }
        }

        let (provider, _) = CapturingMultiRoundProvider::new(vec![
            vec![
                StreamChunk::ToolUse {
                    call_id: "external-delegate-echo".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({ "text": "delegated" }),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            vec![
                StreamChunk::Delta {
                    content: "External delegated tool completed".into(),
                },
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::Stop),
                },
            ],
        ]);
        let storage = Arc::new(MockStorage::new());
        let topic = storage.general_scope().await.expect("create default topic");
        let executions = Arc::new(AtomicUsize::new(0));
        let mut config = swage_config::ResolvedConfig::default();
        config.tools.permissions.require_approval =
            vec![swage_config::RequireApprovalEntry::Simple("echo".into())];
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(provider) as Arc<dyn LlmProvider>,
            config,
            crate::tool::ToolDispatcher::allow_all(vec![Arc::new(CountingEchoTool(Arc::clone(
                &executions,
            )))
                as Arc<dyn crate::tool::Tool>]),
            None,
        );
        let router = build_router(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/messages")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({
                            "sender_agent_id": "agent-a",
                            "recipient_agent_id": "agent-b",
                            "kind": "delegate",
                            "topic_id": topic.id,
                            "payload": { "task": "Use echo" }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        let message_id = body["id"]
            .as_str()
            .expect("message id string")
            .parse::<Uuid>()
            .expect("message id uuid");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let message = storage
                .get_agent_message(message_id)
                .await
                .expect("message lookup")
                .expect("message exists");
            if matches!(
                message.status,
                swage_core::MessageStatus::Delivered | swage_core::MessageStatus::Failed
            ) {
                assert_eq!(message.status, swage_core::MessageStatus::Delivered);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "external delegate should complete without waiting for approval"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert!(
            storage.list_approvals().await.unwrap().is_empty(),
            "an external unattended delegate must not create approval rows"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn external_delegate_message_uses_shared_context_assembly() {
        use swage_agent::MessageRouter;
        use swage_core::{FinishReason, MessageKind, StreamChunk};
        use swage_storage::model::NewAgentMessage;
        use swage_storage::store::{AuditStore, ContextSnapshotStore, ThreadStore, TurnStore};

        let (provider, captured_requests) = CapturingMultiRoundProvider::new(vec![vec![
            StreamChunk::Delta {
                content: "External delegate answer".into(),
            },
            StreamChunk::Done {
                finish_reason: Some(FinishReason::Stop),
            },
        ]]);
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let topic = storage.general_scope().await.expect("get default topic");
        let swage_home = SwageHomeGuard::new();
        let target_skill_dir = swage_home
            .path()
            .join("general/agents/agent-b/skills/external-specialist");
        std::fs::create_dir_all(&target_skill_dir).expect("create target skill directory");
        std::fs::write(
            target_skill_dir.join("SKILL.md"),
            "---\ndescription: External specialist behaviour\n---\nUse the external specialist skill.\n",
        )
        .expect("write target skill");
        let topic_element = swage_home.path().join("elements/current-work.md");
        std::fs::create_dir_all(topic_element.parent().unwrap())
            .expect("create topic element directory");
        std::fs::write(&topic_element, "Shared child grounding; task is {{task}}.")
            .expect("write topic context element");
        let config = swage_config::ResolvedConfig {
            instructions: "Use the external delegated specialist instructions.".to_string(),
            ..swage_config::ResolvedConfig::default()
        };
        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(provider) as Arc<dyn LlmProvider>,
            config,
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        let router = MessageRouter::new(state.storage.clone(), None);
        let delegate_msg = router
            .send_message(NewAgentMessage {
                sender_agent_id: "agent-a".to_string(),
                recipient_agent_id: "agent-b".to_string(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({ "task": "summarise externally" }),
            })
            .await
            .expect("create external delegate message");

        Box::pin(execute_external_delegate_message(
            state,
            delegate_msg,
            topic.id,
            None,
        ))
        .await
        .expect("external delegation should complete");

        let child_sessions = storage
            .list_sessions_by_agent_id("agent-b")
            .await
            .expect("list child sessions");
        assert_eq!(child_sessions.len(), 1, "expected one child session");
        let child_threads = storage
            .list_by_session(child_sessions[0].id)
            .await
            .expect("list child threads");
        assert_eq!(child_threads.len(), 1, "expected one child thread");
        let child_turns = storage
            .list_turns_by_thread(child_threads[0].id)
            .await
            .expect("list child turns");
        assert_eq!(child_turns.len(), 1, "expected one child turn");
        let child_snapshot = storage
            .get_snapshot(child_turns[0].id)
            .await
            .expect("read child context snapshot")
            .expect("external delegated assembly report should be persisted");
        let child_snapshot: serde_json::Value = serde_json::from_str(&child_snapshot)
            .expect("external delegated context snapshot should be JSON");
        assert!(
            child_snapshot["elements"].is_array(),
            "external delegated assembly report should contain its element decisions"
        );
        assert!(
            child_snapshot["elements"]
                .as_array()
                .unwrap()
                .iter()
                .any(|element| element["name"] == "current-work"),
            "external delegated assembly report should identify the topic context element"
        );
        let child_audit = storage
            .list_audit_events_by_session(child_sessions[0].id)
            .await
            .expect("list child audit events");
        assert!(
            child_audit.iter().any(|event| {
                event.entity_id == crate::experimentation::LOCATION_CONTEXT_ASSEMBLY
                    && event.event_kind == "variant_selected"
                    && event.metadata["turn_id"] == serde_json::json!(child_turns[0].id)
            }),
            "external delegated assembly selection should be recorded against the child turn"
        );

        let captured_requests = captured_requests.lock().unwrap();
        assert_eq!(captured_requests.len(), 1);
        let request_text = captured_requests[0]
            .messages
            .iter()
            .map(|message| message.text_content())
            .collect::<Vec<_>>()
            .join("\n\n");
        assert!(
            request_text.contains("Use the external delegated specialist instructions."),
            "external delegated provider request should include target agent instructions: {request_text}"
        );
        assert!(
            request_text.contains("You are an agent running on Swage"),
            "external delegated provider request should include platform identity: {request_text}"
        );
        assert!(
            request_text.contains("Topic: none — this session was started without a topic")
                && request_text.contains("Working directory:")
                && request_text.contains("Session started:"),
            "external delegated provider request should include the child session's place: {request_text}"
        );
        assert!(
            request_text.contains("## Skills available in this session")
                && request_text.contains("external-specialist — External specialist behaviour"),
            "external delegated provider request should include the target skill catalogue: {request_text}"
        );
        assert!(
            request_text.contains("Shared child grounding; task is ⟦unset:task⟧."),
            "external delegated provider request should include the rendered topic context element: {request_text}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn external_delegate_message_confines_child_to_sender_grants() {
        use swage_agent::MessageRouter;
        use swage_core::{FinishReason, MessageKind, StreamChunk};
        use swage_storage::{
            model::NewAgentMessage,
            store::{ThreadStore, ToolCallStore, TurnStore},
        };

        use crate::test_support::NamedTool;

        let (provider, captured_requests) = CapturingMultiRoundProvider::new(vec![
            vec![
                StreamChunk::ToolUse {
                    call_id: "excluded-target-tool".into(),
                    name: "target_only".into(),
                    arguments: serde_json::json!({}),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ],
            vec![
                StreamChunk::Delta {
                    content: "External delegate remained confined".into(),
                },
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::Stop),
                },
            ],
        ]);
        let storage =
            Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
        let topic = storage.general_scope().await.expect("get default topic");
        let swage_home = SwageHomeGuard::new();
        for (agent_id, allowed_tools) in [
            ("agent-a", "[\"shared\"]"),
            ("agent-b", "[\"shared\", \"target_only\"]"),
        ] {
            let agent_dir = swage_home.path().join("general/agents").join(agent_id);
            std::fs::create_dir_all(&agent_dir).expect("create agent config directory");
            std::fs::write(
                agent_dir.join("config.toml"),
                format!(
                    "[tools]\nallow = {allowed_tools}\ndeny = []\nbundles = {{}}\nrequire_approval = []\n"
                ),
            )
            .expect("write agent permissions");
        }

        let state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            Arc::new(provider) as Arc<dyn LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![
                Arc::new(NamedTool::new("shared")) as Arc<dyn crate::tool::Tool>,
                Arc::new(NamedTool::new("target_only")) as Arc<dyn crate::tool::Tool>,
            ]),
            None,
        );
        let router = MessageRouter::new(state.storage.clone(), None);
        let delegate_msg = router
            .send_message(NewAgentMessage {
                sender_agent_id: "agent-a".to_string(),
                recipient_agent_id: "agent-b".to_string(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({ "task": "exercise external confinement" }),
            })
            .await
            .expect("create external delegate message");

        Box::pin(execute_external_delegate_message(
            state,
            delegate_msg,
            topic.id,
            None,
        ))
        .await
        .expect("external delegation should complete");

        let child_sessions = storage
            .list_sessions_by_agent_id("agent-b")
            .await
            .expect("list child sessions");
        assert_eq!(child_sessions.len(), 1, "expected one child session");
        assert_eq!(
            child_sessions[0]
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get(crate::turn::DISPATCH_BOUND_METADATA_KEY))
                .and_then(serde_json::Value::as_array),
            Some(&vec![serde_json::Value::String("agent-a".into())]),
            "external delegated child should persist its dispatching agent bound"
        );

        let captured_requests = captured_requests.lock().unwrap();
        assert_eq!(captured_requests.len(), 2);
        for request in captured_requests.iter() {
            assert!(
                request
                    .tool_definitions
                    .iter()
                    .any(|definition| definition["name"] == "shared"),
                "the sender/target intersection should retain their shared tool: {:?}",
                request.tool_definitions
            );
            assert!(
                request
                    .tool_definitions
                    .iter()
                    .all(|definition| definition["name"] != "target_only"),
                "the sender/target intersection should exclude the target-only tool: {:?}",
                request.tool_definitions
            );
        }
        drop(captured_requests);

        let child_threads = storage
            .list_by_session(child_sessions[0].id)
            .await
            .expect("list child threads");
        let child_turns = storage
            .list_turns_by_thread(child_threads[0].id)
            .await
            .expect("list child turns");
        let tool_calls = storage
            .list_tool_calls_by_turn(child_turns[0].id)
            .await
            .expect("list child tool calls");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].tool, "target_only");
        assert_eq!(tool_calls[0].status.as_deref(), Some("error"));
        assert!(
            tool_calls[0]
                .result
                .as_deref()
                .is_some_and(|result| result.contains("not permitted")),
            "the excluded tool call should leave a denied audit record: {tool_calls:?}"
        );
    }

    #[tokio::test]
    async fn post_delegate_message_requires_topic_id_before_persisting() {
        use swage_storage::store::AgentMessageStore;

        let (router, storage) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/messages")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({
                            "sender_agent_id": "agent-a",
                            "recipient_agent_id": "agent-b",
                            "kind": "delegate",
                            "payload": { "task": "summarise" }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = body_json(resp).await;
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("require topic_id")),
            "validation error should explain the missing topic context: {body:?}"
        );
        assert!(
            storage
                .list_pending_for_agent("agent-b")
                .await
                .expect("list pending messages")
                .is_empty(),
            "ambiguous delegate requests should fail before persistence"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn external_delegate_result_delivery_failure_leaves_delegate_pending() {
        use swage_agent::MessageRouter;
        use swage_core::{MessageKind, MessageStatus};
        use swage_hook::{Hook, HookEvent, HookOutcome, HookRegistration};
        use swage_storage::model::NewAgentMessage;
        use swage_storage::store::AgentMessageStore;

        struct BlockResultDeliveryHook;

        #[async_trait::async_trait]
        impl Hook for BlockResultDeliveryHook {
            fn name(&self) -> &str {
                "block-result-delivery"
            }

            async fn execute(&self, _event: &HookEvent) -> HookOutcome {
                HookOutcome::Block("result delivery failed".to_string())
            }
        }

        fn is_result_delivery(event: &HookEvent) -> bool {
            matches!(
                event,
                HookEvent::MessageDeliver { kind, .. } if kind == &MessageKind::Result.to_string()
            )
        }

        let storage = Arc::new(MockStorage::new());
        let storage_for_router = Arc::clone(&storage) as Arc<dyn Storage>;
        let normal_router = MessageRouter::new(storage_for_router.clone(), None);
        let delegate_msg = normal_router
            .send_message(NewAgentMessage {
                sender_agent_id: "agent-a".to_string(),
                recipient_agent_id: "agent-b".to_string(),
                kind: MessageKind::Delegate,
                correlation_id: None,
                payload: serde_json::json!({ "task": "summarise" }),
            })
            .await
            .expect("create delegate message");
        let result_msg = normal_router
            .send_message(NewAgentMessage {
                sender_agent_id: "agent-b".to_string(),
                recipient_agent_id: "agent-a".to_string(),
                kind: MessageKind::Result,
                correlation_id: Some(delegate_msg.id),
                payload: serde_json::json!({ "result": "done" }),
            })
            .await
            .expect("create result message");

        let mut registry = swage_hook::HookRegistry::new();
        let mut registration = HookRegistration::new(Arc::new(BlockResultDeliveryHook));
        registration.event_filter = Some(is_result_delivery);
        registry.register(registration);
        let blocking_router =
            MessageRouter::new(storage_for_router.clone(), Some(Arc::new(registry)));

        let err = deliver_external_delegate_completion(
            storage_for_router,
            &blocking_router,
            delegate_msg.id,
            &result_msg,
        )
        .await
        .expect_err("result delivery failure should surface");
        assert!(
            err.message().contains("result delivery failed"),
            "error should explain the result delivery failure: {:?}",
            err
        );

        let delegate = storage
            .get_agent_message(delegate_msg.id)
            .await
            .expect("get delegate")
            .expect("delegate exists");
        assert_eq!(delegate.status, MessageStatus::Pending);
        assert_eq!(delegate.failure_reason, None);

        let result = storage
            .get_agent_message(result_msg.id)
            .await
            .expect("get result")
            .expect("result exists");
        assert_eq!(result.status, MessageStatus::Failed);
    }

    // -----------------------------------------------------------------------
    // POST /api/conversations
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_conversations_returns_201_with_json() {
        let (router, _storage) = make_router();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert!(body["id"].is_string(), "response should have a string id");
        // A conversation created without a topic runs in the general scope
        // and carries no topic on the wire.
        assert!(body.get("topic_id").is_none(), "{body}");
        assert!(body["created_at"].is_string());
        assert!(body["last_activity"].is_string());
    }

    #[tokio::test]
    async fn post_conversations_rejects_unknown_request_fields() {
        let (router, _storage) = make_router();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"topic_id":"not-a-target"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        // An extractor rejection answers in the wire error contract's shape,
        // exactly as a handler's own refusal would.
        let body = body_json(resp).await;
        assert_eq!(body["code"], "validation_error");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("topic_id")),
            "rejection should name the unknown field: {body}"
        );
    }

    #[tokio::test]
    async fn post_conversations_accepts_documented_request_fields() {
        let (router, _storage) = make_router();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"selected_model_id":"test-model","agent_id":"gateway"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert_eq!(body["selected_model_id"], "test-model");
    }

    #[tokio::test]
    async fn post_conversations_rejects_unknown_effort_before_writes() {
        let (router, storage) = make_router();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"effort":"extreme"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let db = storage.db.lock().await;
        assert!(db.sessions.is_empty());
        assert!(db.threads.is_empty());
    }

    #[tokio::test]
    async fn post_turns_returns_409_when_thread_has_pending_placeholder_turn() {
        use swage_storage::model::{NewTurn, TurnState};
        use swage_storage::store::{SessionStore, ThreadStore, TurnStore};

        let (router, storage) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let create_body = body_json(create_resp).await;
        let conversation_id = create_body["id"]
            .as_str()
            .expect("conversation id")
            .parse::<Uuid>()
            .expect("conversation uuid");

        let session = storage
            .get_session(conversation_id)
            .await
            .expect("get session")
            .expect("session exists");
        let thread = storage
            .list_by_session(session.id)
            .await
            .expect("list threads")
            .into_iter()
            .next()
            .expect("root thread exists");

        storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: session.agent_id.clone(),
                state: TurnState::Pending,
                user_input: Some("first message".to_string()),
                parent_turn_id: None,
            })
            .await
            .expect("seed pending placeholder turn");

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/turns")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({
                            "conversation_id": conversation_id,
                            "message": "second message",
                            "thread_id": null,
                            "output_schema": null
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = body_json(resp).await;
        assert_eq!(body["code"], "turn_in_progress");
    }

    #[tokio::test]
    async fn post_turns_rejects_unknown_model_before_creating_a_pending_turn() {
        use swage_storage::store::{SessionStore, ThreadStore, TurnStore};

        let storage = Arc::new(MockStorage::new());
        let mut config = swage_config::ResolvedConfig::default();
        config.models.default = "first".to_string();
        config.models.entries.insert(
            "first".to_string(),
            swage_config::ModelEndpointConfig::default(),
        );
        config.models.entries.insert(
            "second".to_string(),
            swage_config::ModelEndpointConfig::default(),
        );
        let (router, storage) = make_router_with_provider_and_config(
            Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>,
            storage,
            config,
        );

        let create = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .expect("create conversation response");
        let create_body = body_json(create).await;
        let conversation_id = create_body["id"]
            .as_str()
            .expect("conversation id")
            .parse::<Uuid>()
            .expect("valid conversation id");

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/turns")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({
                            "conversation_id": conversation_id,
                            "message": "hello",
                            "model": "missing"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .expect("unknown-model response");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = body_json(response).await;
        let message = body["message"].as_str().expect("validation message");
        assert!(message.contains("missing"));
        assert!(message.contains("first"));
        assert!(message.contains("second"));

        let session = storage
            .get_session(conversation_id)
            .await
            .expect("get conversation")
            .expect("conversation exists");
        let thread = storage
            .list_by_session(session.id)
            .await
            .expect("list threads")
            .into_iter()
            .next()
            .expect("root thread");
        assert!(
            storage
                .list_turns_by_thread(thread.id)
                .await
                .expect("list turns")
                .is_empty(),
            "an invalid requested model must not create a pending turn"
        );
    }

    #[tokio::test]
    async fn post_turns_admits_an_image_for_the_selected_openai_provider() {
        let storage = Arc::new(MockStorage::new());
        let mut config = swage_config::ResolvedConfig::default();
        config.providers.entries.insert(
            "google".into(),
            swage_config::ProviderEndpointConfig {
                protocol: swage_config::Protocol::Google,
                ..Default::default()
            },
        );
        config.providers.entries.insert(
            "openai".into(),
            swage_config::ProviderEndpointConfig {
                protocol: swage_config::Protocol::OpenAi,
                ..Default::default()
            },
        );
        config.models.default = "gemini".into();
        config.models.entries.insert(
            "gemini".into(),
            swage_config::ModelEndpointConfig {
                provider_id: "google".into(),
                model_id: "gemini-wire".into(),
                ..Default::default()
            },
        );
        config.models.entries.insert(
            "gpt".into(),
            swage_config::ModelEndpointConfig {
                provider_id: "openai".into(),
                model_id: "gpt-wire".into(),
                ..Default::default()
            },
        );
        let (router, _storage) = make_router_with_provider_and_config(
            Arc::new(MockProvider::new(vec![
                swage_core::StreamChunk::Delta {
                    content: "a cat".into(),
                },
                swage_core::StreamChunk::Done {
                    finish_reason: None,
                },
            ])) as Arc<dyn LlmProvider>,
            storage,
            config,
        );

        let create = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .expect("create conversation response");
        let conversation_id = body_json(create).await["id"]
            .as_str()
            .expect("conversation id")
            .to_string();

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/turns")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({
                            "conversation_id": conversation_id,
                            "message": "what is this?",
                            "model": "gpt",
                            "attachments": [{"kind": "image_url", "url": "https://example.test/cat.png"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .expect("turn response");

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = body_json(response).await;
        let turn_id: Uuid = body["turn_id"]
            .as_str()
            .expect("turn id")
            .parse()
            .expect("turn UUID");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let response = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method("GET")
                            .uri(format!("/api/turns/{turn_id}"))
                            .body(axum::body::Body::empty())
                            .unwrap(),
                    )
                    .await
                    .expect("turn poll response");
                assert_eq!(response.status(), StatusCode::OK);
                let turn = body_json(response).await;
                if turn["state"] == "completed" {
                    break;
                }
                assert_ne!(turn["state"], "failed", "{turn}");
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("selected-provider turn completes");
    }

    #[tokio::test]
    async fn post_turns_refuses_a_selected_codex_image_before_persistence() {
        let storage = Arc::new(MockStorage::new());
        let mut config = swage_config::ResolvedConfig::default();
        config.providers.entries.insert(
            "openai".into(),
            swage_config::ProviderEndpointConfig {
                protocol: swage_config::Protocol::OpenAi,
                ..Default::default()
            },
        );
        config.providers.entries.insert(
            "codex".into(),
            swage_config::ProviderEndpointConfig {
                protocol: swage_config::Protocol::Codex,
                ..Default::default()
            },
        );
        config.models.default = "gpt".into();
        config.models.entries.insert(
            "gpt".into(),
            swage_config::ModelEndpointConfig {
                provider_id: "openai".into(),
                model_id: "gpt-wire".into(),
                ..Default::default()
            },
        );
        config.models.entries.insert(
            "codex".into(),
            swage_config::ModelEndpointConfig {
                provider_id: "codex".into(),
                model_id: "codex-wire".into(),
                ..Default::default()
            },
        );
        let (router, storage) = make_router_with_provider_and_config(
            Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>,
            storage,
            config,
        );

        let create = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .expect("create conversation response");
        let conversation_id = body_json(create).await["id"]
            .as_str()
            .expect("conversation id")
            .to_string();

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/turns")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({
                            "conversation_id": conversation_id,
                            "message": "what is this?",
                            "model": "codex",
                            "attachments": [{"kind": "image_url", "url": "https://example.test/cat.png"}]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .expect("refused response");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = body_json(response).await;
        assert_eq!(body["code"], "validation_error");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("image")),
            "{body}"
        );
        let session = storage
            .get_session(
                conversation_id
                    .parse()
                    .expect("conversation id remains a UUID"),
            )
            .await
            .expect("read conversation")
            .expect("conversation exists");
        let thread =
            swage_storage::store::ThreadStore::list_by_session(storage.as_ref(), session.id)
                .await
                .expect("list threads")
                .into_iter()
                .next()
                .expect("root thread");
        assert!(
            storage
                .list_turns_by_thread(thread.id)
                .await
                .expect("list turns")
                .is_empty(),
            "a refused attachment must not create a turn record"
        );
    }

    // -----------------------------------------------------------------------
    // GET /api/conversations
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_conversations_returns_200_with_array() {
        let (router, _) = make_router();

        // Create two conversations first.
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let list = body["conversations"].as_array().unwrap();
        assert_eq!(list.len(), 2);
    }

    // -----------------------------------------------------------------------
    // GET /api/conversations/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_conversation_by_id_returns_200_with_messages() {
        let (router, _) = make_router();

        // Create a conversation.
        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let create_body = body_json(create_resp).await;
        let id = create_body["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/conversations/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["id"].as_str().unwrap(), id);
        assert!(body["messages"].is_array());
    }

    #[tokio::test]
    async fn get_non_existent_conversation_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/conversations/{missing}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    // -----------------------------------------------------------------------
    // DELETE /api/conversations/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_conversation_returns_204() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let create_body = body_json(create_resp).await;
        let id = create_body["id"].as_str().unwrap();

        let del_resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/conversations/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);
    }

    // -----------------------------------------------------------------------
    // POST /api/topics
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_spaces_returns_201_with_json() {
        let (router, _) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/topics")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"My Topic"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert!(body["id"].is_string(), "response should have a string id");
        assert!(body["slug"].is_string(), "response should have a slug");
        assert_eq!(body["name"].as_str().unwrap(), "My Topic");
    }

    #[tokio::test]
    async fn post_spaces_empty_name_returns_422() {
        let (router, _) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/topics")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_spaces_duplicate_name_returns_409() {
        let (router, _) = make_router();

        // First creation succeeds.
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/topics")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Dup"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Second with same name must conflict.
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/topics")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Dup"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // -----------------------------------------------------------------------
    // GET /api/topics
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_spaces_returns_200_with_list() {
        let (router, _) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/topics")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // At least the seeded default topic.
        assert!(body["topics"].is_array());
    }

    // -----------------------------------------------------------------------
    // GET /api/topics/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_topic_by_id_returns_200() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/topics")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Fetched"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let id = created["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/topics/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["id"].as_str().unwrap(), id);
    }

    #[tokio::test]
    async fn get_topic_nonexistent_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/topics/{missing}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // PUT /api/topics/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn put_topic_returns_200_with_updated_fields() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/topics")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Original"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let id = created["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/topics/{id}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Renamed"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["name"].as_str().unwrap(), "Renamed");
    }

    #[tokio::test]
    async fn put_topic_nonexistent_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/topics/{missing}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"X"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // DELETE /api/topics/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_topic_returns_204() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/topics")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Deletable"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let id = created["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/topics/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_general_scope_returns_409() {
        use crate::service::TopicService;

        let (router, storage) = make_router();
        // The general scope is not a topic the API can delete.
        let svc = TopicService::new(
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            std::path::PathBuf::from(".swage"),
        );
        let general = svc.topic_store.general_scope().await.unwrap();
        let id = general.id;

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/topics/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // -----------------------------------------------------------------------
    // GET /api/topics/:topic_id/sessions
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_topic_sessions_returns_200_with_array() {
        let (router, storage) = make_router();

        // Create a named topic directly in storage.
        let topic = storage
            .create(swage_storage::model::NewTopic {
                name: "my-topic".into(),
                slug: "my-topic".into(),
            })
            .await
            .unwrap();

        // Add two sessions to that topic.
        storage
            .create_session(swage_storage::model::NewSession {
                topic_id: topic.id,
                agent_id: "gateway".to_string(),
                origin: swage_storage::SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        storage
            .create_session(swage_storage::model::NewSession {
                topic_id: topic.id,
                agent_id: "gateway".to_string(),
                origin: swage_storage::SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/topics/{}/sessions", topic.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // Gateway wraps sessions in a "conversations" key (frontend-compatible).
        let list = body["conversations"].as_array().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn get_topic_sessions_scopes_to_topic() {
        let (router, storage) = make_router();

        let topic_a = storage
            .create(swage_storage::model::NewTopic {
                name: "topic-a".into(),
                slug: "topic-a".into(),
            })
            .await
            .unwrap();
        let topic_b = storage
            .create(swage_storage::model::NewTopic {
                name: "topic-b".into(),
                slug: "topic-b".into(),
            })
            .await
            .unwrap();

        // One session in A, two in B.
        storage
            .create_session(swage_storage::model::NewSession {
                topic_id: topic_a.id,
                agent_id: "gateway".to_string(),
                origin: swage_storage::SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        storage
            .create_session(swage_storage::model::NewSession {
                topic_id: topic_b.id,
                agent_id: "gateway".to_string(),
                origin: swage_storage::SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        storage
            .create_session(swage_storage::model::NewSession {
                topic_id: topic_b.id,
                agent_id: "gateway".to_string(),
                origin: swage_storage::SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/topics/{}/sessions", topic_a.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = body_json(resp).await;
        let list = body["conversations"].as_array().unwrap();
        // Only sessions belonging to topic A.
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn get_topic_sessions_returns_404_for_unknown_topic() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/topics/{missing}/sessions"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    // -----------------------------------------------------------------------
    // POST /api/topics/:topic_id/sessions
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_topic_sessions_returns_201_with_json() {
        let (router, storage) = make_router();

        let topic = storage
            .create(swage_storage::model::NewTopic {
                name: "my-topic".into(),
                slug: "my-topic".into(),
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/topics/{}/sessions", topic.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert!(body["id"].is_string());
        // Session should be associated with the specified topic.
        assert_eq!(body["topic_id"].as_str().unwrap(), topic.id.to_string());
    }

    #[tokio::test]
    async fn post_topic_sessions_rejects_unknown_request_fields() {
        let (router, storage) = make_router();
        let topic = storage
            .create(swage_storage::model::NewTopic {
                name: "strict-request-topic".into(),
                slug: "strict-request-topic".into(),
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/topics/{}/sessions", topic.id))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"topic_id":"not-a-target"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        // An extractor rejection answers in the wire error contract's shape,
        // exactly as a handler's own refusal would.
        let body = body_json(resp).await;
        assert_eq!(body["code"], "validation_error");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("topic_id")),
            "rejection should name the unknown field: {body}"
        );
    }

    #[tokio::test]
    async fn post_topic_sessions_accepts_documented_request_fields() {
        let (router, storage) = make_router();
        let topic = storage
            .create(swage_storage::model::NewTopic {
                name: "documented-fields-topic".into(),
                slug: "documented-fields-topic".into(),
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/topics/{}/sessions", topic.id))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"selected_model_id":"test-model","agent_id":"gateway"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert_eq!(body["topic_id"], topic.id.to_string());
        assert_eq!(body["selected_model_id"], "test-model");
    }

    #[tokio::test]
    async fn post_topic_sessions_returns_404_for_unknown_topic() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/topics/{missing}/sessions"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    // -----------------------------------------------------------------------
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_conversation_branches_creates_independent_thread_copy() {
        let (router, storage) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/conversations")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let create_body = body_json(create_resp).await;
        let session_id: Uuid = create_body["id"].as_str().unwrap().parse().unwrap();

        let threads = swage_storage::ThreadStore::list_by_session(storage.as_ref(), session_id)
            .await
            .unwrap();
        let parent_thread_id = threads[0].id;
        let first = storage
            .create_message(swage_storage::model::NewMessage {
                thread_id: parent_thread_id,
                attachments: Vec::new(),
                turn_id: None,
                role: swage_core::Role::User,
                content: "hello".into(),
                position: 0,
                agent_id: "gateway".into(),
            })
            .await
            .unwrap();
        storage
            .create_message(swage_storage::model::NewMessage {
                thread_id: parent_thread_id,
                attachments: Vec::new(),
                turn_id: None,
                role: swage_core::Role::Assistant,
                content: "draft".into(),
                position: 1,
                agent_id: "gateway".into(),
            })
            .await
            .unwrap();

        let branch_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/conversations/{session_id}/branches"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({
                            "thread_id": parent_thread_id,
                            "from_message_id": first.id,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(branch_resp.status(), StatusCode::CREATED);
        let branch_body = body_json(branch_resp).await;
        let child_thread_id: Uuid = branch_body["child_thread_id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();

        storage
            .create_message(swage_storage::model::NewMessage {
                thread_id: parent_thread_id,
                attachments: Vec::new(),
                turn_id: None,
                role: swage_core::Role::User,
                content: "parent continues".into(),
                position: 2,
                agent_id: "gateway".into(),
            })
            .await
            .unwrap();

        let thread_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/threads/{child_thread_id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(thread_resp.status(), StatusCode::OK);
        let thread_body = body_json(thread_resp).await;
        let branch_messages = thread_body["messages"].as_array().unwrap();
        assert_eq!(branch_messages.len(), 1);
        assert_eq!(branch_messages[0]["content"], "hello");

        let branches_resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/conversations/{session_id}/branches"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(branches_resp.status(), StatusCode::OK);
        let branches_body = body_json(branches_resp).await;
        let branches = branches_body["branches"].as_array().unwrap();
        assert_eq!(branches.len(), 1);
        assert_eq!(branches[0]["child_thread_id"], child_thread_id.to_string());
        assert_eq!(branches[0]["state"], "idle");
        assert_eq!(branches[0]["branched_from_preview"], "hello");
    }

    #[tokio::test]
    async fn delete_non_existent_conversation_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/conversations/{missing}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    // -----------------------------------------------------------------------
    // GET /api/operator/history
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn operator_history_reconstructs_only_configuration_and_experiment_records_newest_first()
    {
        use swage_storage::model::{NewAuditEvent, NewSession, NewTurn, TurnState};
        use swage_storage::store::{AuditStore, ContextSnapshotStore, TurnStore};

        let (router, storage) = make_router();
        let topic = storage.general_scope().await.unwrap();
        let session = storage
            .create_session(NewSession {
                topic_id: topic.id,
                agent_id: "operator".into(),
                origin: swage_storage::SessionOrigin::Direct,
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        let thread = swage_storage::ThreadStore::create(
            storage.as_ref(),
            swage_storage::model::NewThread {
                session_id: session.id,
                agent_id: "operator".into(),
            },
        )
        .await
        .unwrap();
        let turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "operator".into(),
                state: TurnState::Completed,
                user_input: Some("inspect history".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();

        storage
            .create_audit_event(NewAuditEvent {
                id: "config-1".into(),
                session_id: Some(session.id),
                entity_kind: "provider".into(),
                entity_id: "openai".into(),
                event_kind: "configuration_change".into(),
                summary: "updated provider".into(),
                metadata: serde_json::json!({
                    "action": "updated",
                    "source": "settings_ui",
                    "prior_state": {"model": "old", "api_key": "[REDACTED]"},
                    "new_state": {"model": "new", "api_key": "[REDACTED]"}
                }),
            })
            .await
            .unwrap();
        storage
            .create_audit_event(NewAuditEvent {
                id: "config-2".into(),
                session_id: None,
                entity_kind: "schedule".into(),
                entity_id: "daily".into(),
                event_kind: "configuration_change".into(),
                summary: "created schedule".into(),
                metadata: serde_json::json!({
                    "action": "created",
                    "source": "operator_http",
                    "prior_state": null,
                    "new_state": {"message": "Daily report", "status": "active"}
                }),
            })
            .await
            .unwrap();
        storage
            .create_audit_event(NewAuditEvent {
                id: "ignored".into(),
                session_id: Some(session.id),
                entity_kind: "turn".into(),
                entity_id: turn.id.to_string(),
                event_kind: "turn_completed".into(),
                summary: "completed".into(),
                metadata: serde_json::json!({"secret": "must not leak"}),
            })
            .await
            .unwrap();
        storage
            .create_audit_event(NewAuditEvent {
                id: "variant-1".into(),
                session_id: Some(session.id),
                entity_kind: "experiment_location".into(),
                entity_id: "context_assembly".into(),
                event_kind: "variant_selected".into(),
                summary: "selected variant".into(),
                metadata: serde_json::json!({
                    "variant": "compact",
                    "reason": "drawn",
                    "turn_id": turn.id
                }),
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        storage
            .create_audit_event(NewAuditEvent {
                id: "variant-2".into(),
                session_id: Some(session.id),
                entity_kind: "experiment_location".into(),
                entity_id: "task_prompt".into(),
                event_kind: "variant_selected".into(),
                summary: "selected task prompt variant".into(),
                metadata: serde_json::json!({
                    "variant": "focused",
                    "reason": "drawn",
                    "turn_id": turn.id
                }),
            })
            .await
            .unwrap();
        let report = swage_protocol::client::context::AssemblyReport {
            assembly_name: "balanced".into(),
            total_tokens: 321,
            ..Default::default()
        };
        storage
            .store_snapshot(turn.id, &serde_json::to_string(&report).unwrap())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        let later_turn = storage
            .create_turn(NewTurn {
                thread_id: thread.id,
                agent_id: "operator".into(),
                state: TurnState::Completed,
                user_input: Some("inspect newer history".into()),
                parent_turn_id: None,
            })
            .await
            .unwrap();
        let later_report = swage_protocol::client::context::AssemblyReport {
            assembly_name: "focused".into(),
            total_tokens: 123,
            ..Default::default()
        };
        storage
            .store_snapshot(
                later_turn.id,
                &serde_json::to_string(&later_report).unwrap(),
            )
            .await
            .unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/operator/history")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let changes = body["configuration_changes"].as_array().unwrap();
        assert_eq!(
            changes.len(),
            2,
            "unrelated audit events must be filtered out"
        );
        assert_eq!(changes[0]["resource_kind"], "schedule", "newest first");
        assert_eq!(changes[0]["prior_state"], serde_json::Value::Null);
        assert_eq!(changes[0]["new_state"]["message"], "Daily report");
        assert_eq!(changes[1]["resource_kind"], "provider");
        assert_eq!(changes[1]["resource_id"], "openai");
        assert_eq!(changes[1]["action"], "updated");
        assert_eq!(changes[1]["source"], "settings_ui");
        assert_eq!(changes[1]["session_id"], session.id.to_string());
        assert_eq!(changes[1]["prior_state"]["model"], "old");
        assert_eq!(changes[1]["new_state"]["model"], "new");
        assert_eq!(changes[1]["prior_state"]["api_key"], "[REDACTED]");
        assert!(!body.to_string().contains("must not leak"));

        let variants = body["variant_selections"].as_array().unwrap();
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0]["variant"], "focused", "newest first");
        assert_eq!(variants[0]["reason"], "drawn");
        assert_eq!(variants[1]["turn_id"], turn.id.to_string());
        let assemblies = body["assembly_reports"].as_array().unwrap();
        assert_eq!(assemblies.len(), 2);
        assert_eq!(
            assemblies[0]["turn_id"],
            later_turn.id.to_string(),
            "newest first"
        );
        assert_eq!(assemblies[0]["assembly_name"], "focused");
        assert_eq!(assemblies[1]["turn_id"], turn.id.to_string());
        assert_eq!(assemblies[1]["assembly_name"], "balanced");
        assert!(assemblies[0].get("report").is_none());
        assert_eq!(storage.last_snapshot_list_limit(), 100);
    }

    // -----------------------------------------------------------------------
    // GET /api/turns/:id/context
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_turn_context_returns_404_for_unknown_turn() {
        let (router, _) = make_router();
        let unknown_id = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/turns/{unknown_id}/context"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "not_found");
    }

    // -----------------------------------------------------------------------
    // Helpers specific to shutdown tests
    // -----------------------------------------------------------------------

    /// Variant of `make_router` that also returns the shutdown signal and a
    /// broadcast receiver so shutdown tests can observe both side-effects.
    fn make_shutdown_router() -> (
        Router,
        Arc<MockStorage>,
        CancellationToken,
        broadcast::Receiver<ServerEvent>,
    ) {
        let storage = Arc::new(MockStorage::new());
        let provider = Arc::new(MockProvider::new(vec![]));
        let (broadcast_tx, broadcast_rx) = broadcast::channel(16);
        let shutdown_signal = CancellationToken::new();
        let mut state = AppState::new(
            Arc::clone(&storage) as Arc<dyn Storage>,
            Arc::clone(&storage) as Arc<dyn TopicStore>,
            provider as Arc<dyn swage_provider::LlmProvider>,
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        state.broadcast = broadcast_tx;
        state.shutdown_signal = shutdown_signal.clone();
        (build_router(state), storage, shutdown_signal, broadcast_rx)
    }

    // -----------------------------------------------------------------------
    // POST /api/shutdown
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn shutdown_from_localhost_returns_200() {
        let (router, _, _, _) = make_shutdown_router();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shutdown")
                    .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 8080))))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"].as_str().unwrap(), "shutting_down");
        assert!(body["message"].is_string());
    }

    #[tokio::test]
    async fn shutdown_from_non_localhost_returns_403() {
        let (router, _, _, _) = make_shutdown_router();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shutdown")
                    // Simulate a request from a remote IP.
                    .extension(ConnectInfo(SocketAddr::from(([192, 168, 1, 100], 54321))))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = body_json(resp).await;
        assert_eq!(body["code"].as_str().unwrap(), "forbidden");
    }

    #[tokio::test]
    async fn shutdown_cancels_the_shutdown_signal() {
        let (router, _, shutdown_signal, _) = make_shutdown_router();

        assert!(
            !shutdown_signal.is_cancelled(),
            "signal should not be cancelled before the request"
        );

        router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shutdown")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(
            shutdown_signal.is_cancelled(),
            "signal should be cancelled after the shutdown request"
        );
    }

    #[tokio::test]
    async fn shutdown_broadcasts_shutdown_event() {
        let (router, _, _, mut broadcast_rx) = make_shutdown_router();

        router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/shutdown")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Handler should have sent exactly one Shutdown event to the channel.
        let event = broadcast_rx
            .try_recv()
            .expect("broadcast channel should have received a Shutdown event");
        assert!(
            matches!(event, ServerEvent::Shutdown { .. }),
            "expected a Shutdown event, got: {event:?}"
        );
    }

    #[tokio::test]
    async fn get_turn_context_returns_200_with_report() {
        use swage_storage::store::ContextSnapshotStore;

        let (router, storage) = make_router();
        let turn_id = Uuid::new_v4();
        let report = swage_protocol::client::context::AssemblyReport {
            total_tokens: 1024,
            tool_scope: Some(serde_json::json!({
                "effective_root": "/var/lib/swage/scratch/topics/example/sessions/example",
            })),
            ..Default::default()
        };
        let report_json = serde_json::to_string(&report).unwrap();

        // Store a snapshot directly — simulates what the context engine writes.
        storage.store_snapshot(turn_id, &report_json).await.unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/turns/{turn_id}/context"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["turn_id"].as_str().unwrap(), turn_id.to_string());
        assert_eq!(body["report"]["total_tokens"].as_i64().unwrap(), 1024);
        assert_eq!(
            body["report"]["tool_scope"]["effective_root"],
            "/var/lib/swage/scratch/topics/example/sessions/example"
        );
    }
}
