use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::{
    Json,
    extract::{ConnectInfo, Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use swage_agent::approval::ApprovalDecisionValue;
use swage_core::GatewayError;
use swage_storage::SharedReferenceKind;

use crate::{
    attachment::{
        AttachmentListResponse, BackgroundJobResponse, CreateAttachmentRequest,
        QueuedAttachmentResponse,
    },
    audit::{
        AgentMemoryListQuery, AgentMemoryListResponse, OperatorSearchQuery, OperatorSearchResponse,
        OperatorUsageQuery, OperatorUsageResponse,
    },
    authoring_definition::{
        ContextDefinitionListResponse, ContextDefinitionRecord, CreateContextDefinitionRequest,
        CreateTopologyFlowDefinitionRequest, PreviewContextDefinitionRequest,
        PreviewContextDefinitionResponse, TopologyFlowDefinitionListResponse,
        TopologyFlowDefinitionRecord, UpdateContextDefinitionRequest,
        UpdateTopologyFlowDefinitionRequest,
    },
    bench_extension::{
        BenchExtensionListResponse, BenchExtensionResponse, CreateBenchExtensionRequest,
        UpdateBenchExtensionRequest,
    },
    builder_runtime::{
        BuilderApplyRequest, BuilderApplyResponse, BuilderConfigurationService,
        BuilderProposalRequest, BuilderProposalResponse, ConfigurationVersionListResponse,
        ConfigurationVersionQuery, RollbackConfigurationVersionRequest,
        RollbackConfigurationVersionResponse,
    },
    capabilities::WorkflowCapabilitiesResponse,
    channel_gateway::{
        ChannelInboundResponse, ChannelStatusListResponse, DeliveryLogQuery, DeliveryLogResponse,
        DevDmInboundRequest, DevThreadInboundRequest,
    },
    error::ApiError,
    execution_host::{
        ExecutionHostListResponse, ExecutionHostResponse, RegisterExecutionHostRequest,
    },
    heartbeat::{
        CreateHeartbeatRequest, HeartbeatListResponse, HeartbeatResponse, HeartbeatRunListResponse,
        UpdateHeartbeatRequest,
    },
    lens::{
        CreateLensRequest, InstallLensRequest, LensDescriptor, LensListResponse,
        LensRuntimeResponse, UpdateLensRequest, UpdateLensRuntimeRequest,
    },
    material::{
        CreateMaterialRequest, MaterialListResponse, MaterialResponse, PromoteMaterialRequest,
    },
    onboarding::{
        ChannelLinkListResponse, ChannelLinkResponse, CreateChannelLinkRequest,
        CreateNodePairingCodeRequest, ImportGatewayRecoveryRequest, NodePairingCodeResponse,
        RecoveryExportListResponse, RedeemNodePairingRequest,
    },
    operator::OperatorOverviewResponse,
    presence::{MachineHeartbeatRequest, PresenceSnapshotResponse},
    schedule::{
        CreateScheduledJobRequest, ScheduledJobListResponse, ScheduledJobResponse,
        ScheduledRunListResponse, TriggerScheduledJobResponse, UpdateScheduledJobRequest,
    },
    secret::{
        CreateSecretRequest, GenerateSecretRequest, RotateSecretRequest, SecretListResponse,
        SecretResponse, SecretValidationResponse,
    },
    service::{
        BenchListResponse, BenchResponse, BenchService, ConversationBranchResponse,
        ConversationDetail, ConversationResponse, ConversationService, ConversationSummary,
        ConversationThreadDetail, CreateBenchRequest, CreateConversationBranchRequest,
        CreateTeamRequest, CreateTopologyRunRequest, CreateWorkspaceAttachmentRequest,
        TeamListResponse, TeamResponse, TeamService, TopologyRunDetailResponse,
        TopologyRunResponse, TopologyRunService, UpdateBenchRequest, UpdateTeamRequest,
        UpdateWorkspaceTrustRequest, WorkspaceAttachmentListResponse, WorkspaceAttachmentResponse,
        WorkspaceAttachmentService,
    },
    settings::{SaveProviderSettings, SaveSettingsRequest, SettingsResponse},
    shared_reference::{
        CreateSharedReferenceEventRequest, SharedReferenceDetailResponse,
        SharedReferenceInteractionResponse, SharedReferenceListQuery, SharedReferenceListResponse,
        UpdateContextDefinitionReferenceRequest, UpdateOrchestrationFlowReferenceRequest,
        UpdateTeamTopologyReferenceRequest,
    },
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

pub async fn get_settings(State(state): State<AppState>) -> Json<SettingsResponse> {
    Json(crate::settings::load_settings(&state).await)
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

/// `GET /api/benches/:id/lens-runtime` — inspect the active lens for a bench.
pub async fn get_lens_runtime(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
) -> Result<Json<LensRuntimeResponse>, ApiError> {
    Ok(Json(crate::lens::get_lens_runtime(&state, bench_id).await?))
}

/// `GET /api/lenses` — list durable lens records.
pub async fn list_lenses(
    State(state): State<AppState>,
) -> Result<Json<LensListResponse>, ApiError> {
    Ok(Json(crate::lens::list_lenses(&state).await?))
}

/// `POST /api/lenses` — create an authored lens.
pub async fn create_lens(
    State(state): State<AppState>,
    Json(request): Json<CreateLensRequest>,
) -> Result<(StatusCode, Json<LensDescriptor>), ApiError> {
    let service = BuilderConfigurationService::new(state.clone());
    Ok((
        StatusCode::CREATED,
        Json(
            service
                .create_lens(request, None, None, "visual_builder")
                .await?,
        ),
    ))
}

/// `PUT /api/lenses/:id` — update an authored or installed lens.
pub async fn update_lens(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateLensRequest>,
) -> Result<Json<LensDescriptor>, ApiError> {
    let service = BuilderConfigurationService::new(state.clone());
    Ok(Json(
        service
            .update_lens(&id, request, None, None, "visual_builder")
            .await?,
    ))
}

/// `POST /api/lenses/install` — install a premade lens into durable storage.
pub async fn install_lens(
    State(state): State<AppState>,
    Json(request): Json<InstallLensRequest>,
) -> Result<(StatusCode, Json<LensDescriptor>), ApiError> {
    let service = BuilderConfigurationService::new(state.clone());
    Ok((
        StatusCode::CREATED,
        Json(
            service
                .install_lens_preset(request, None, None, "visual_builder")
                .await?,
        ),
    ))
}

/// `PUT /api/benches/:id/lens-runtime` — persist the active lens for a bench.
pub async fn update_lens_runtime(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
    Json(request): Json<UpdateLensRuntimeRequest>,
) -> Result<Json<LensRuntimeResponse>, ApiError> {
    Ok(Json(
        crate::lens::update_lens_runtime(&state, bench_id, request).await?,
    ))
}

/// `GET /api/shared-objects` — list shared reference objects for a bench.
pub async fn list_shared_objects(
    State(state): State<AppState>,
    Query(query): Query<SharedReferenceListQuery>,
) -> Result<Json<SharedReferenceListResponse>, ApiError> {
    Ok(Json(
        crate::shared_reference::list_shared_objects(&state, query).await?,
    ))
}

/// `GET /api/shared-objects/:kind/:id` — inspect one shared reference object.
pub async fn get_shared_object(
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Json<SharedReferenceDetailResponse>, ApiError> {
    Ok(Json(
        crate::shared_reference::get_shared_object(
            &state,
            parse_shared_reference_kind(&kind)?,
            &id,
        )
        .await?,
    ))
}

/// `POST /api/shared-objects/:kind/:id/events` — persist a replayable interaction event.
pub async fn create_shared_object_event(
    State(state): State<AppState>,
    Path((kind, id)): Path<(String, String)>,
    Json(request): Json<CreateSharedReferenceEventRequest>,
) -> Result<(StatusCode, Json<SharedReferenceInteractionResponse>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(
            crate::shared_reference::create_shared_object_event(
                &state,
                parse_shared_reference_kind(&kind)?,
                &id,
                request,
            )
            .await?,
        ),
    ))
}

/// `PUT /api/shared-objects/team_topology/:id` — update the editable team-topology shared object.
pub async fn get_team_topology_reference(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<SharedReferenceDetailResponse>, ApiError> {
    Ok(Json(
        crate::shared_reference::get_shared_object(
            &state,
            swage_storage::SharedReferenceKind::TeamTopology,
            &id.to_string(),
        )
        .await?,
    ))
}

/// `PUT /api/shared-objects/team_topology/:id` — update the editable team-topology shared object.
pub async fn update_team_topology_reference(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(request): Json<UpdateTeamTopologyReferenceRequest>,
) -> Result<Json<SharedReferenceDetailResponse>, ApiError> {
    Ok(Json(
        crate::shared_reference::update_team_topology_reference(&state, id, request).await?,
    ))
}

/// `PUT /api/shared-objects/orchestration_flow/:id` — update a durable orchestration-flow shared object.
pub async fn update_orchestration_flow_reference(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateOrchestrationFlowReferenceRequest>,
) -> Result<Json<SharedReferenceDetailResponse>, ApiError> {
    Ok(Json(
        crate::shared_reference::update_orchestration_flow_reference(&state, &id, request).await?,
    ))
}

/// `GET /api/shared-objects/orchestration_flow/:id` — inspect one durable orchestration-flow shared object.
pub async fn get_orchestration_flow_reference(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SharedReferenceDetailResponse>, ApiError> {
    Ok(Json(
        crate::shared_reference::get_shared_object(
            &state,
            swage_storage::SharedReferenceKind::OrchestrationFlow,
            &id,
        )
        .await?,
    ))
}

/// `PUT /api/shared-objects/context_definition/:id` — update a durable context-definition shared object.
pub async fn update_context_definition_reference(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<UpdateContextDefinitionReferenceRequest>,
) -> Result<Json<SharedReferenceDetailResponse>, ApiError> {
    Ok(Json(
        crate::shared_reference::update_context_definition_reference(&state, &id, request).await?,
    ))
}

/// `GET /api/shared-objects/context_definition/:id` — inspect one durable context-definition shared object.
pub async fn get_context_definition_reference(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SharedReferenceDetailResponse>, ApiError> {
    Ok(Json(
        crate::shared_reference::get_shared_object(
            &state,
            swage_storage::SharedReferenceKind::ContextDefinition,
            &id,
        )
        .await?,
    ))
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

fn parse_shared_reference_kind(kind: &str) -> Result<SharedReferenceKind, ApiError> {
    kind.parse()
        .map_err(|error: String| ApiError::from(GatewayError::ValidationError(error)))
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

/// `GET /api/benches/:id/attachments` — list bench-scoped attachments.
pub async fn list_bench_attachments(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<AttachmentListResponse>, ApiError> {
    Ok(Json(
        crate::attachment::list_bench_attachments(&state, id).await?,
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

/// `GET /api/benches/:id/materials` — list bench-scoped managed materials.
pub async fn list_bench_materials(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<MaterialListResponse>, ApiError> {
    Ok(Json(
        crate::material::list_bench_materials(&state, id).await?,
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

/// `POST /api/onboarding/nodes/pairing-codes` — issue a one-time node pairing code.
pub async fn create_node_pairing_code(
    State(state): State<AppState>,
    Json(req): Json<CreateNodePairingCodeRequest>,
) -> Result<(StatusCode, Json<NodePairingCodeResponse>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(crate::onboarding::create_node_pairing_code(&state, req).await?),
    ))
}

/// `POST /api/onboarding/nodes/redeem` — redeem a pairing code and trust a remote node.
pub async fn redeem_node_pairing_code(
    State(state): State<AppState>,
    Json(req): Json<RedeemNodePairingRequest>,
) -> Result<(StatusCode, Json<ExecutionHostResponse>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(crate::onboarding::redeem_node_pairing_code(&state, req).await?),
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

/// `POST /api/scheduled-jobs` — create a durable scheduled job.
pub async fn create_scheduled_job(
    State(state): State<AppState>,
    Json(req): Json<CreateScheduledJobRequest>,
) -> Result<(StatusCode, Json<ScheduledJobResponse>), ApiError> {
    let response = crate::schedule::create_scheduled_job(&state, req).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// `GET /api/scheduled-jobs` — list all configured scheduled jobs.
pub async fn list_scheduled_jobs(
    State(state): State<AppState>,
) -> Result<Json<ScheduledJobListResponse>, ApiError> {
    Ok(Json(crate::schedule::list_scheduled_jobs(&state).await?))
}

/// `PUT /api/scheduled-jobs/:id` — edit an existing scheduled job.
pub async fn update_scheduled_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateScheduledJobRequest>,
) -> Result<Json<ScheduledJobResponse>, ApiError> {
    Ok(Json(
        crate::schedule::update_scheduled_job(&state, id, req).await?,
    ))
}

/// `POST /api/scheduled-jobs/:id/pause` — pause a scheduled job.
pub async fn pause_scheduled_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ScheduledJobResponse>, ApiError> {
    Ok(Json(
        crate::schedule::pause_scheduled_job(&state, id).await?,
    ))
}

/// `POST /api/scheduled-jobs/:id/resume` — resume a scheduled job.
pub async fn resume_scheduled_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ScheduledJobResponse>, ApiError> {
    Ok(Json(
        crate::schedule::resume_scheduled_job(&state, id).await?,
    ))
}

/// `POST /api/scheduled-jobs/:id/trigger` — execute a job immediately.
pub async fn trigger_scheduled_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<TriggerScheduledJobResponse>), ApiError> {
    Ok((
        StatusCode::ACCEPTED,
        Json(crate::schedule::trigger_scheduled_job(&state, id).await?),
    ))
}

/// `GET /api/scheduled-jobs/:id/runs` — list recorded executions for a job.
pub async fn list_scheduled_runs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ScheduledRunListResponse>, ApiError> {
    Ok(Json(
        crate::schedule::list_scheduled_runs(&state, id).await?,
    ))
}

/// `DELETE /api/scheduled-jobs/:id` — delete a job and its recorded runs.
pub async fn delete_scheduled_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    if crate::schedule::delete_scheduled_job(&state, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("scheduled_job", id))
    }
}

/// `POST /api/heartbeats` — create a durable heartbeat.
pub async fn create_heartbeat(
    State(state): State<AppState>,
    Json(req): Json<CreateHeartbeatRequest>,
) -> Result<(StatusCode, Json<HeartbeatResponse>), ApiError> {
    let response = crate::heartbeat::create_heartbeat(&state, req).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// `GET /api/heartbeats` — list all configured heartbeats.
pub async fn list_heartbeats(
    State(state): State<AppState>,
) -> Result<Json<HeartbeatListResponse>, ApiError> {
    Ok(Json(crate::heartbeat::list_heartbeats(&state).await?))
}

/// `PUT /api/heartbeats/:id` — edit an existing heartbeat.
pub async fn update_heartbeat(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateHeartbeatRequest>,
) -> Result<Json<HeartbeatResponse>, ApiError> {
    Ok(Json(
        crate::heartbeat::update_heartbeat(&state, id, req).await?,
    ))
}

/// `POST /api/heartbeats/:id/enable` — enable heartbeat execution.
pub async fn enable_heartbeat(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<HeartbeatResponse>, ApiError> {
    Ok(Json(crate::heartbeat::enable_heartbeat(&state, id).await?))
}

/// `POST /api/heartbeats/:id/disable` — disable heartbeat execution.
pub async fn disable_heartbeat(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<HeartbeatResponse>, ApiError> {
    Ok(Json(crate::heartbeat::disable_heartbeat(&state, id).await?))
}

/// `GET /api/heartbeats/:id/runs` — list recorded heartbeat runs.
pub async fn list_heartbeat_runs(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<HeartbeatRunListResponse>, ApiError> {
    Ok(Json(
        crate::heartbeat::list_heartbeat_runs(&state, id).await?,
    ))
}

/// `DELETE /api/heartbeats/:id` — delete a heartbeat and its run history.
pub async fn delete_heartbeat(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    if crate::heartbeat::delete_heartbeat(&state, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("heartbeat", id))
    }
}

/// `GET /api/presence` — inspect gateway, client, and machine presence.
pub async fn presence_snapshot(State(state): State<AppState>) -> Json<PresenceSnapshotResponse> {
    Json(crate::presence::snapshot_presence(&state).await)
}

/// `POST /api/presence/machines/heartbeat` — register or refresh a machine heartbeat.
pub async fn machine_heartbeat(
    State(state): State<AppState>,
    Json(req): Json<MachineHeartbeatRequest>,
) -> StatusCode {
    crate::presence::upsert_machine_heartbeat(&state, req).await;
    StatusCode::ACCEPTED
}

/// `DELETE /api/presence/machines/:id` — mark a machine disconnected.
pub async fn disconnect_machine(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if crate::presence::disconnect_machine(&state, &id).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found_str("machine", &id))
    }
}

/// `GET /api/execution-hosts` — list local and attached execution hosts.
pub async fn list_execution_hosts(
    State(state): State<AppState>,
) -> Json<ExecutionHostListResponse> {
    Json(crate::execution_host::list_execution_hosts(&state).await)
}

/// `POST /api/execution-hosts/register` — attach a remote execution host.
pub async fn register_execution_host(
    State(state): State<AppState>,
    Json(request): Json<RegisterExecutionHostRequest>,
) -> Result<(StatusCode, Json<ExecutionHostResponse>), ApiError> {
    if request.host_id.trim().is_empty() {
        return Err(ApiError::validation(
            "execution host id must not be empty".to_string(),
        ));
    }
    if crate::execution_host::ExecutionHostKind::from_str(request.kind.trim()).is_err() {
        return Err(ApiError::validation(format!(
            "unknown execution host kind '{}'",
            request.kind
        )));
    }
    if request.base_url.trim().is_empty() {
        return Err(ApiError::validation(
            "execution host base_url must not be empty".to_string(),
        ));
    }
    if request.shared_token_secret_ref.trim().is_empty() {
        return Err(ApiError::validation(
            "execution host shared_token_secret_ref must not be empty".to_string(),
        ));
    }

    let host = crate::execution_host::register_remote_execution_host(&state, request).await?;
    Ok((
        StatusCode::CREATED,
        Json(ExecutionHostResponse {
            host_id: host.host_id,
            label: host.label,
            kind: host.kind.to_string(),
            base_url: host.base_url,
            workspace_root: host.workspace_root.to_string_lossy().into_owned(),
            workspace_roots: host
                .workspace_roots
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            capabilities: host.capabilities,
            registered_at: host.registered_at,
        }),
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

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// `POST /api/conversations` — create a new conversation.
///
/// Returns 201 Created with the new conversation's metadata.
pub async fn create_conversation(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = ConversationService::new(state.storage, state.bench_store);
    let resp: ConversationResponse = svc.create_conversation().await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/conversations` — list all conversations.
///
/// Returns 200 with `{ conversations: [...] }`.
pub async fn list_conversations(
    State(state): State<AppState>,
) -> Result<Json<ConversationListResponse>, ApiError> {
    let svc = ConversationService::new(state.storage, state.bench_store);
    let conversations = svc.list_conversations().await?;
    Ok(Json(ConversationListResponse { conversations }))
}

/// `GET /api/conversations/:id` — get a conversation with full message history.
///
/// Returns 200 with the conversation detail, or 404 if not found.
pub async fn get_conversation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ConversationDetail>, ApiError> {
    let svc = ConversationService::new(state.storage, state.bench_store);
    let detail = svc.get_conversation(id).await?;
    Ok(Json(detail))
}

/// `GET /api/benches/:bench_id/sessions` — list sessions for a bench.
///
/// Returns 200 with `{ conversations: [...] }`, or 404 if the bench is not found.
pub async fn list_bench_sessions(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
) -> Result<Json<ConversationListResponse>, ApiError> {
    let svc = ConversationService::new(state.storage, state.bench_store);
    let conversations = svc.list_sessions_by_bench(bench_id).await?;
    Ok(Json(ConversationListResponse { conversations }))
}

/// `POST /api/benches/:bench_id/sessions` — create a session in a bench.
///
/// Returns 201 Created with the new session's metadata, or 404 if the bench is not found.
pub async fn create_bench_session(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = ConversationService::new(state.storage, state.bench_store);
    let resp = svc.create_session_in_bench(bench_id).await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `DELETE /api/conversations/:id` — delete a conversation.
///
/// Returns 204 No Content on success, or 404 if not found.
pub async fn delete_conversation(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let svc = ConversationService::new(state.storage, state.bench_store);
    svc.delete_conversation(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Bench handlers
// ---------------------------------------------------------------------------

/// `POST /api/benches` — create a new bench.
///
/// Returns 201 Created with the bench's metadata, or 409/422 on error.
pub async fn create_bench(
    State(state): State<AppState>,
    Json(req): Json<CreateBenchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = BenchService::new(state.bench_store);
    let resp = svc.create_bench(req).await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/benches` — list all benches.
///
/// Returns 200 with `{ benches: [...] }`.
pub async fn list_benches(
    State(state): State<AppState>,
) -> Result<Json<BenchListResponse>, ApiError> {
    let svc = BenchService::new(state.bench_store);
    let resp = svc.list_benches().await?;
    Ok(Json(resp))
}

/// `GET /api/benches/:id` — get a bench by ID.
///
/// Returns 200 with the bench, or 404 if not found.
pub async fn get_bench(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<BenchResponse>, ApiError> {
    let svc = BenchService::new(state.bench_store);
    let resp = svc.get_bench(id).await?;
    Ok(Json(resp))
}

/// `PUT /api/benches/:id` — partially update a bench.
///
/// Returns 200 with the updated bench, 404 if not found, or 422/409 on error.
pub async fn update_bench(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateBenchRequest>,
) -> Result<Json<BenchResponse>, ApiError> {
    let svc = BenchService::new(state.bench_store);
    let resp = svc.update_bench(id, req).await?;
    Ok(Json(resp))
}

/// `DELETE /api/benches/:id` — delete a bench.
///
/// Returns 204 No Content on success, 404 if not found, or 409 if refusing
/// to delete the default bench.
pub async fn delete_bench(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let svc = BenchService::new(state.bench_store);
    svc.delete_bench(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/benches/:id/workspaces` — list workspaces attached to a bench.
pub async fn list_bench_workspaces(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
) -> Result<Json<WorkspaceAttachmentListResponse>, ApiError> {
    let svc = WorkspaceAttachmentService::new(state.storage, state.bench_store);
    Ok(Json(svc.list_workspaces(bench_id).await?))
}

/// `POST /api/benches/:id/workspaces` — attach an external workspace.
pub async fn attach_bench_workspace(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
    Json(req): Json<CreateWorkspaceAttachmentRequest>,
) -> Result<(StatusCode, Json<WorkspaceAttachmentResponse>), ApiError> {
    let svc = WorkspaceAttachmentService::new(state.storage, state.bench_store);
    Ok((
        StatusCode::CREATED,
        Json(svc.attach_workspace(bench_id, req).await?),
    ))
}

/// `PUT /api/benches/:bench_id/workspaces/:workspace_id/trust` — set workspace trust.
pub async fn update_bench_workspace_trust(
    State(state): State<AppState>,
    Path((bench_id, workspace_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateWorkspaceTrustRequest>,
) -> Result<Json<WorkspaceAttachmentResponse>, ApiError> {
    let svc = WorkspaceAttachmentService::new(state.storage, state.bench_store);
    Ok(Json(svc.set_trust(bench_id, workspace_id, req).await?))
}

/// `DELETE /api/benches/:bench_id/workspaces/:workspace_id` — detach a workspace.
pub async fn detach_bench_workspace(
    State(state): State<AppState>,
    Path((bench_id, workspace_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    let svc = WorkspaceAttachmentService::new(state.storage, state.bench_store);
    svc.detach_workspace(bench_id, workspace_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Team handlers
// ---------------------------------------------------------------------------

/// `POST /api/benches/:bench_id/teams` — create a team in a bench.
pub async fn create_team(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
    Json(req): Json<CreateTeamRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = BuilderConfigurationService::new(state.clone());
    let resp = svc
        .create_team(bench_id, req, None, "visual_builder")
        .await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/benches/:bench_id/teams` — list teams in a bench.
pub async fn list_teams(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
) -> Result<Json<TeamListResponse>, ApiError> {
    let svc = TeamService::new(state.storage, state.bench_store);
    let resp = svc.list_teams(bench_id).await?;
    Ok(Json(resp))
}

/// `GET /api/benches/:id/extensions` — list managed and workspace extension candidates.
pub async fn list_bench_extensions(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
) -> Result<Json<BenchExtensionListResponse>, ApiError> {
    Ok(Json(
        crate::bench_extension::list_bench_extensions(&state, bench_id).await?,
    ))
}

/// `POST /api/benches/:id/extensions` — create a managed bench extension.
pub async fn create_bench_extension(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
    Json(request): Json<CreateBenchExtensionRequest>,
) -> Result<(StatusCode, Json<BenchExtensionResponse>), ApiError> {
    let service = BuilderConfigurationService::new(state.clone());
    Ok((
        StatusCode::CREATED,
        Json(
            service
                .create_bench_extension(bench_id, request, None, "visual_builder")
                .await?,
        ),
    ))
}

/// `PUT /api/benches/:id/extensions/:name` — update a managed bench extension.
pub async fn update_bench_extension(
    State(state): State<AppState>,
    Path((bench_id, name)): Path<(Uuid, String)>,
    Json(request): Json<UpdateBenchExtensionRequest>,
) -> Result<Json<BenchExtensionResponse>, ApiError> {
    let service = BuilderConfigurationService::new(state.clone());
    Ok(Json(
        service
            .update_bench_extension(bench_id, &name, request, None, "visual_builder")
            .await?,
    ))
}

/// `POST /api/benches/:id/extensions/:name/approve-workspace` — trust and import a workspace extension candidate.
pub async fn approve_workspace_extension(
    State(state): State<AppState>,
    Path((bench_id, name)): Path<(Uuid, String)>,
) -> Result<Json<BenchExtensionResponse>, ApiError> {
    let service = BuilderConfigurationService::new(state.clone());
    Ok(Json(
        service
            .approve_workspace_extension(bench_id, &name, None, "visual_builder")
            .await?,
    ))
}

/// `GET /api/teams/:id` — fetch a team by ID.
pub async fn get_team(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TeamResponse>, ApiError> {
    let svc = TeamService::new(state.storage, state.bench_store);
    let resp = svc.get_team(id).await?;
    Ok(Json(resp))
}

/// `PUT /api/teams/:id` — partially update a team.
pub async fn update_team(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateTeamRequest>,
) -> Result<Json<TeamResponse>, ApiError> {
    let svc = BuilderConfigurationService::new(state.clone());
    let resp = svc.update_team(id, req, None, "visual_builder").await?;
    Ok(Json(resp))
}

/// `POST /api/benches/:id/builder/proposals` — turn a natural-language request into a reviewed config proposal.
pub async fn create_builder_proposal(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
    Json(request): Json<BuilderProposalRequest>,
) -> Result<Json<BuilderProposalResponse>, ApiError> {
    let service = BuilderConfigurationService::new(state);
    Ok(Json(service.propose_changes(bench_id, request).await?))
}

/// `POST /api/benches/:id/builder/apply` — apply a reviewed builder proposal through the canonical config path.
pub async fn apply_builder_proposal(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
    Json(request): Json<BuilderApplyRequest>,
) -> Result<(StatusCode, Json<BuilderApplyResponse>), ApiError> {
    let service = BuilderConfigurationService::new(state);
    let response = service.apply_operations(bench_id, request).await?;
    let status = if response.success {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    Ok((status, Json(response)))
}

/// `GET /api/benches/:id/config-versions` — inspect builder-managed configuration history.
pub async fn list_configuration_versions(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
    Query(query): Query<ConfigurationVersionQuery>,
) -> Result<Json<ConfigurationVersionListResponse>, ApiError> {
    let service = BuilderConfigurationService::new(state);
    Ok(Json(
        service.list_configuration_versions(bench_id, query).await?,
    ))
}

/// `POST /api/config-versions/:id/rollback` — restore a previous builder-managed configuration version.
pub async fn rollback_configuration_version(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(request): Json<RollbackConfigurationVersionRequest>,
) -> Result<Json<RollbackConfigurationVersionResponse>, ApiError> {
    let service = BuilderConfigurationService::new(state);
    Ok(Json(
        service
            .rollback_configuration_version(id, request.session_id, "operator_api")
            .await?,
    ))
}

/// `DELETE /api/teams/:id` — delete a team.
pub async fn delete_team(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let svc = TeamService::new(state.storage, state.bench_store);
    svc.delete_team(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/conversations/:id/branches` — create a branch thread.
pub async fn create_conversation_branch(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<CreateConversationBranchRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = ConversationService::new(state.storage, state.bench_store);
    let resp = svc.create_branch(session_id, req).await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/conversations/:id/branches` — list branches in a conversation.
pub async fn list_conversation_branches(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<Json<Vec<ConversationBranchResponse>>, ApiError> {
    let svc = ConversationService::new(state.storage, state.bench_store);
    let resp = svc.list_branches(session_id).await?;
    Ok(Json(resp))
}

/// `GET /api/threads/:id` — fetch a single thread with messages and lineage.
pub async fn get_thread_detail(
    State(state): State<AppState>,
    Path(thread_id): Path<Uuid>,
) -> Result<Json<ConversationThreadDetail>, ApiError> {
    let svc = ConversationService::new(state.storage, state.bench_store);
    let resp = svc.get_thread_detail(thread_id).await?;
    Ok(Json(resp))
}

/// `POST /api/teams/:id/runs` — validate and execute a topology run.
pub async fn create_topology_run(
    State(state): State<AppState>,
    Path(team_id): Path<Uuid>,
    Json(req): Json<CreateTopologyRunRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let svc = TopologyRunService::new(state);
    let resp = svc.create_run(team_id, req).await?;
    Ok((StatusCode::CREATED, Json(resp)))
}

/// `GET /api/teams/:id/runs` — list topology runs for a team.
pub async fn list_topology_runs(
    State(state): State<AppState>,
    Path(team_id): Path<Uuid>,
) -> Result<Json<Vec<TopologyRunResponse>>, ApiError> {
    let svc = TopologyRunService::new(state);
    let resp = svc.list_runs(team_id).await?;
    Ok(Json(resp))
}

/// `GET /api/topology-runs/:id` — inspect a topology run and its trace.
pub async fn get_topology_run(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TopologyRunDetailResponse>, ApiError> {
    let svc = TopologyRunService::new(state);
    let resp = svc.get_run(id).await?;
    Ok(Json(resp))
}

/// `GET /api/benches/:id/topology-flows` — list durable topology flow definitions.
pub async fn list_topology_flows(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
) -> Result<Json<TopologyFlowDefinitionListResponse>, ApiError> {
    Ok(Json(
        crate::authoring_definition::list_topology_flows(&state, bench_id).await?,
    ))
}

/// `POST /api/benches/:id/topology-flows` — create a durable topology flow definition.
pub async fn create_topology_flow(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
    Json(request): Json<CreateTopologyFlowDefinitionRequest>,
) -> Result<(StatusCode, Json<TopologyFlowDefinitionRecord>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(crate::authoring_definition::create_topology_flow(&state, bench_id, request).await?),
    ))
}

/// `GET /api/benches/:id/topology-flows/:name` — fetch one durable topology flow definition.
pub async fn get_topology_flow(
    State(state): State<AppState>,
    Path((bench_id, name)): Path<(Uuid, String)>,
) -> Result<Json<TopologyFlowDefinitionRecord>, ApiError> {
    Ok(Json(
        crate::authoring_definition::get_topology_flow(&state, bench_id, &name).await?,
    ))
}

/// `PUT /api/benches/:id/topology-flows/:name` — update a durable topology flow definition.
pub async fn update_topology_flow(
    State(state): State<AppState>,
    Path((bench_id, name)): Path<(Uuid, String)>,
    Json(request): Json<UpdateTopologyFlowDefinitionRequest>,
) -> Result<Json<TopologyFlowDefinitionRecord>, ApiError> {
    Ok(Json(
        crate::authoring_definition::update_topology_flow(&state, bench_id, &name, request).await?,
    ))
}

/// `GET /api/benches/:id/context-definitions` — list durable context definition records.
pub async fn list_context_definitions(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
) -> Result<Json<ContextDefinitionListResponse>, ApiError> {
    Ok(Json(
        crate::authoring_definition::list_context_definitions(&state, bench_id).await?,
    ))
}

/// `POST /api/benches/:id/context-definitions` — create a durable context definition.
pub async fn create_context_definition(
    State(state): State<AppState>,
    Path(bench_id): Path<Uuid>,
    Json(request): Json<CreateContextDefinitionRequest>,
) -> Result<(StatusCode, Json<ContextDefinitionRecord>), ApiError> {
    Ok((
        StatusCode::CREATED,
        Json(
            crate::authoring_definition::create_context_definition(&state, bench_id, request)
                .await?,
        ),
    ))
}

/// `GET /api/benches/:id/context-definitions/:name` — fetch one durable context definition.
pub async fn get_context_definition(
    State(state): State<AppState>,
    Path((bench_id, name)): Path<(Uuid, String)>,
) -> Result<Json<ContextDefinitionRecord>, ApiError> {
    Ok(Json(
        crate::authoring_definition::get_context_definition(&state, bench_id, &name).await?,
    ))
}

/// `PUT /api/benches/:id/context-definitions/:name` — update a durable context definition.
pub async fn update_context_definition(
    State(state): State<AppState>,
    Path((bench_id, name)): Path<(Uuid, String)>,
    Json(request): Json<UpdateContextDefinitionRequest>,
) -> Result<Json<ContextDefinitionRecord>, ApiError> {
    Ok(Json(
        crate::authoring_definition::update_context_definition(&state, bench_id, &name, request)
            .await?,
    ))
}

/// `POST /api/benches/:id/context-definitions/:name/preview` — render a stored context definition.
pub async fn preview_context_definition(
    State(state): State<AppState>,
    Path((bench_id, name)): Path<(Uuid, String)>,
    Json(request): Json<PreviewContextDefinitionRequest>,
) -> Result<Json<PreviewContextDefinitionResponse>, ApiError> {
    Ok(Json(
        crate::authoring_definition::preview_context_definition(&state, bench_id, &name, request)
            .await?,
    ))
}

// ---------------------------------------------------------------------------
// Skills listing
// ---------------------------------------------------------------------------

/// Query parameters for `GET /api/skills`.
#[derive(Debug, Deserialize)]
pub struct SkillsQueryParams {
    /// ID of the bench to list skills for (required).
    pub bench_id: Uuid,
    /// Agent ID to include agent-level skills. When provided, the agent's
    /// directory is searched for a `skills/` folder.
    pub agent_id: Option<String>,
}

/// A single skill entry in the `GET /api/skills` response.
#[derive(Debug, Serialize)]
pub struct SkillItem {
    pub name: String,
    pub display_name: Option<String>,
    pub description: String,
    /// Hierarchy level where this skill was resolved: "agent", "bench",
    /// "team", "lens", or "global".
    pub level: String,
    /// Tool names the skill requests access to.
    pub tools: Vec<String>,
    /// True for agent-level skills — they are always active and cannot be
    /// deactivated by the user.
    pub always_active: bool,
}

/// Response body for `GET /api/skills`.
#[derive(Debug, Serialize)]
pub struct SkillListResponse {
    pub skills: Vec<SkillItem>,
}

/// `GET /api/skills` — list skills available for a bench/agent combination.
///
/// Resolves skills from the Swage config hierarchy (global → bench → agent),
/// applying override rules when the same skill name appears at multiple levels.
/// Returns 200 with `{ skills: [...] }`, sorted agent-first then alphabetically.
///
/// Returns 404 if `bench_id` does not correspond to a known bench.
/// Returns 400 if `bench_id` is missing or not a valid UUID (axum `Query`
/// extractor enforces this automatically).
///
/// When the Swage config root directory does not exist (e.g. on a fresh
/// install or in a test environment without `SWAGE_HOME`), returns an empty
/// skills list rather than an error.
pub async fn list_skills(
    State(state): State<AppState>,
    Query(params): Query<SkillsQueryParams>,
) -> Result<Json<SkillListResponse>, ApiError> {
    use swage_config::skill::discover_skills;
    use swage_config::{HierarchyLevel, resolve_root};

    // Verify the bench exists — return 404 for unknown IDs.
    let bench = state
        .bench_store
        .get(params.bench_id)
        .await
        .map_err(|e| ApiError(GatewayError::Storage(e)))?
        .ok_or({
            ApiError(GatewayError::BenchNotFound {
                id: params.bench_id,
            })
        })?;

    // Resolve the config root. When it doesn't exist (no `~/.swage/` directory
    // and `SWAGE_HOME` unset or pointing at a missing path), degrade gracefully
    // by returning an empty list — skill discovery is a best-effort operation.
    let root = match resolve_root() {
        Ok(r) => r,
        Err(_) => return Ok(Json(SkillListResponse { skills: vec![] })),
    };

    // Build the hierarchy: global always included; bench and agent directories
    // are included only when they exist on disk.
    let bench_dir = root.join("benches").join(&bench.slug);
    let mut levels: Vec<(HierarchyLevel, std::path::PathBuf)> =
        vec![(HierarchyLevel::Global, root.clone())];

    if bench_dir.is_dir() {
        levels.push((HierarchyLevel::Bench, bench_dir.clone()));

        if let Some(ref agent_id) = params.agent_id {
            let agent_dir = bench_dir.join("agents").join(agent_id);
            if agent_dir.is_dir() {
                levels.push((HierarchyLevel::Agent, agent_dir));
            }
        }
    }

    // discover_skills takes borrowed (Level, &Path) pairs — build the refs here.
    let level_refs: Vec<(HierarchyLevel, &std::path::Path)> = levels
        .iter()
        .map(|(l, p)| (l.clone(), p.as_path()))
        .collect();

    let (skills, _warnings) = discover_skills(&level_refs);

    let skill_items: Vec<SkillItem> = skills
        .into_iter()
        .map(|s| {
            let always_active = s.level == HierarchyLevel::Agent;
            let level = match s.level {
                HierarchyLevel::Agent => "agent",
                HierarchyLevel::Team => "team",
                HierarchyLevel::Bench => "bench",
                HierarchyLevel::Global => "global",
            };
            SkillItem {
                name: s.name,
                display_name: s.display_name,
                description: s.description,
                level: level.to_string(),
                tools: s.tools,
                always_active,
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
        .map_err(|e| ApiError(GatewayError::Storage(e)))?;

    match snapshot {
        None => Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "code": "not_found",
                "message": format!("no context snapshot found for turn {turn_id}"),
            })),
        )
            .into_response()),
        Some(report_json) => {
            // Parse back to Value so the HTTP response is clean JSON, not a
            // double-encoded string.
            let report: serde_json::Value = serde_json::from_str(&report_json)
                .map_err(|e| ApiError(GatewayError::ConfigError(e.to_string())))?;
            Ok(Json(serde_json::json!({
                "turn_id": turn_id,
                "report": report,
            }))
            .into_response())
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
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "code": "forbidden",
                "message": "shutdown is only accessible from localhost",
            })),
        )
            .into_response();
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

/// Request body for async turn submission.
#[derive(Debug, Deserialize)]
pub struct SubmitTurnRequest {
    /// The conversation (session) to submit the turn in.
    pub conversation_id: Uuid,
    /// Optional target thread. When absent, the session's root thread is used.
    pub thread_id: Option<Uuid>,
    /// The user message to process.
    pub message: String,
    /// Optional output schema that overrides the agent's configured schema
    /// for this single turn.
    pub output_schema: Option<serde_json::Value>,
}

/// Response for async turn submission.
#[derive(Debug, Serialize)]
pub struct SubmitTurnResponse {
    pub turn_id: Uuid,
    pub status: String,
}

/// Response for turn polling.
#[derive(Debug, Serialize)]
pub struct TurnPollResponse {
    pub turn_id: Uuid,
    pub state: String,
    pub assistant_response: Option<String>,
    pub structured_output: Option<serde_json::Value>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub parent_turn_id: Option<Uuid>,
}

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
    if existing_turns.iter().any(|turn| {
        matches!(
            turn.state,
            // REST submit creates a durable pending placeholder before the
            // background worker has a chance to register `active_turns`, so
            // `Pending` must count as in-flight here to close that race.
            swage_storage::TurnState::Pending
                | swage_storage::TurnState::Processing
                | swage_storage::TurnState::AwaitingApproval
                | swage_storage::TurnState::Delegating
        )
    }) {
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
    let message = req.message;
    let output_schema = req.output_schema;
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

        let result = crate::turn::process_turn_with_existing_turn(
            &bg_state,
            session_id,
            Some(thread_id),
            message,
            ws_tx,
            subscription,
            output_schema,
            Some(turn_id),
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
        parent_turn_id: turn.parent_turn_id,
    }))
}

/// Request body for sending an inter-agent message.
#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    pub sender_agent_id: String,
    pub recipient_agent_id: String,
    pub kind: String,
    pub correlation_id: Option<Uuid>,
    pub bench_id: Option<Uuid>,
    pub sender_turn_id: Option<Uuid>,
    pub payload: serde_json::Value,
}

/// Response for message operations.
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub id: Uuid,
    pub status: String,
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
    let bench_id = req.bench_id;

    if kind == MessageKind::Delegate && bench_id.is_none() {
        return Err(ApiError::validation(
            "delegate messages require bench_id so execution uses the correct bench configuration"
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
        let execution_bench_id = bench_id.expect("delegate bench_id was validated before storage");
        let state_for_task = state.clone();
        let msg_for_task = msg.clone();
        tokio::spawn(async move {
            let message_id = msg_for_task.id;
            if let Err(error) = execute_external_delegate_message(
                state_for_task.clone(),
                msg_for_task,
                execution_bench_id,
                sender_turn_id,
            )
            .await
            {
                tracing::warn!(error = ?error, "external delegate execution failed");
                let router = MessageRouter::new(state_for_task.storage.clone(), None);
                if let Err(fail_error) = router.fail_message(message_id, &error.0.to_string()).await
                {
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
    bench_id: Uuid,
    sender_turn_id: Option<Uuid>,
) -> Result<(), ApiError> {
    use std::sync::Arc;

    use swage_agent::{DelegationExecutor, MessageRouter};

    let bench = state
        .bench_store
        .get(bench_id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found("bench", bench_id))?;

    let target_resolved =
        crate::turn::load_session_config(&state, &bench.slug, &delegate_msg.recipient_agent_id)
            .map_err(|error| ApiError::internal(error.to_string()))?;
    let active_workspace_attachment_id =
        active_workspace_attachment_for_sender_turn(&state, sender_turn_id).await?;
    let target_agent_config = crate::turn::build_agent_config_for_bench_workspace(
        &state,
        delegate_msg.recipient_agent_id.clone(),
        &target_resolved,
        None,
        bench_id,
        sender_turn_id.unwrap_or_else(Uuid::new_v4),
        active_workspace_attachment_id,
    )
    .await;
    let target_context_transfer_config = target_resolved.communication.context_transfer.clone();

    let (silent_ws_tx, silent_ws_rx) = tokio::sync::mpsc::unbounded_channel();
    drop(silent_ws_rx);
    let dispatcher_build = crate::turn::build_gateway_dispatcher_with_unsandboxed_degradation(
        &state,
        crate::turn::GatewayDispatcherRequest {
            config: &target_resolved,
            agent_config: &target_agent_config,
            session_metadata: None,
            bench_id: Some(bench_id),
            active_workspace_attachment_id: None,
            ws_tx: silent_ws_tx,
        },
        "external delegated turn",
    )
    .await
    .map_err(|error| ApiError::internal(error.to_string()))?;

    let router = Arc::new(MessageRouter::new(state.storage.clone(), None));

    let executor = DelegationExecutor {
        storage: state.storage.clone(),
        provider: state.provider.clone(),
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
    };

    let result = executor
        .execute_delegation(
            &delegate_msg,
            sender_turn_id,
            bench_id,
            target_agent_config,
            target_context_transfer_config,
            Some(dispatcher_build.dispatcher),
            tokio_util::sync::CancellationToken::new(),
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

/// Approval listing response.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalListResponse {
    pub approvals: Vec<ApprovalSummary>,
}

/// Summary of a pending approval request.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalSummary {
    pub id: Uuid,
    pub turn_id: Uuid,
    pub agent_id: String,
    pub kind: String,
    pub tool_name: Option<String>,
    pub prompt: String,
    pub timeout_at: Option<String>,
}

/// Query parameters for approval listing.
#[derive(Debug, Deserialize)]
pub struct ApprovalListQuery {
    pub agent_id: Option<String>,
}

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

/// Request body for an approval decision.
#[derive(Debug, Deserialize)]
pub struct ApprovalDecisionRequest {
    pub decision: ApprovalDecisionValue,
    #[serde(default)]
    pub scope: swage_agent::approval::ApprovalDecisionScope,
    pub reason: Option<String>,
}

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
    use std::{collections::HashMap, net::SocketAddr, sync::Arc};

    use axum::{Router, body::to_bytes, extract::ConnectInfo, http::Request};
    use tokio::sync::{Mutex, broadcast};
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use uuid::Uuid;

    use swage_provider::LlmProvider;
    use swage_storage::{
        AppLens,
        store::{BenchStore, MessageStore, SessionStore, Storage, TeamStore},
    };

    use super::*;
    use crate::{
        server::build_router,
        state::AppState,
        turn::tests::{MockProvider, MockStorage, MultiRoundMockProvider},
        websocket::ServerEvent,
    };

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn team_agents(ids: &[&str]) -> Vec<swage_storage::model::TeamAgentDefinition> {
        ids.iter()
            .map(|id| swage_storage::model::TeamAgentDefinition {
                id: (*id).to_string(),
                display_name: (*id).to_string(),
                description: String::new(),
                instructions: String::new(),
                model: None,
            })
            .collect()
    }

    fn team_tasks(names: &[&str]) -> Vec<swage_storage::model::TeamTaskDefinition> {
        names
            .iter()
            .map(|name| swage_storage::model::TeamTaskDefinition {
                name: (*name).to_string(),
                description: String::new(),
                instructions: format!("Task {name}"),
                model: None,
                output_schema: None,
                max_output_retries: None,
                tool_allow: None,
                tool_deny: None,
            })
            .collect()
    }

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
        let (broadcast, _) = tokio::sync::broadcast::channel(16);
        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn Storage>,
            bench_store: Arc::clone(&storage) as Arc<dyn BenchStore>,
            provider,
            voice_provider: Arc::new(swage_provider::UnsupportedVoiceProvider::new(
                "voice is unavailable for tests",
            )) as Arc<dyn swage_provider::VoiceProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast,
            shutdown_signal: tokio_util::sync::CancellationToken::new(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            session_approval_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            presence: Arc::new(Mutex::new(crate::presence::PresenceRuntime::default())),
            execution_hosts: Arc::new(Mutex::new(
                crate::execution_host::ExecutionHostRuntime::default(),
            )),
            pairing_codes: Arc::new(Mutex::new(HashMap::new())),
            secret_manager: Arc::new(crate::secret::SecretManager::new(
                Arc::clone(&storage) as Arc<dyn Storage>
            )),
            recovery_exports: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
            workflow_capabilities: crate::capabilities::WorkflowCapabilities::default(),
            restart_required: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            managed_gateway: false,
        };
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
        let conversation = client
            .create_conversation()
            .await
            .expect("create conversation through live gateway");

        let turn_id = client
            .submit_turn(conversation.id, None, "hello", None)
            .await
            .expect("submit async turn");

        let poll = {
            let mut completed = None;
            for _ in 0..50 {
                let current = client.poll_turn(turn_id).await.expect("poll turn");
                let state = current["state"].as_str().expect("poll state string");
                if matches!(state, "completed" | "failed" | "interrupted") {
                    completed = Some(current);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            completed.expect("turn should reach a terminal state")
        };
        assert_eq!(poll["turn_id"], turn_id.to_string());
        assert_eq!(poll["state"], "completed");
        assert_eq!(poll["assistant_response"], "Hello from the async route");

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

        let default_bench = storage
            .get_or_create_default()
            .await
            .expect("get default bench");
        let sent_message = client
            .send_agent_message_in_bench(
                default_bench.id,
                "agent-a",
                "agent-b",
                "delegate",
                None,
                serde_json::json!({ "task": "summarise" }),
            )
            .await
            .expect("send agent message through live gateway");
        let message_id = sent_message["id"]
            .as_str()
            .expect("message id string")
            .parse::<Uuid>()
            .expect("message id uuid");
        let message_status = {
            let mut terminal = None;
            for _ in 0..50 {
                let current = client
                    .get_message_status(message_id)
                    .await
                    .expect("query message status");
                if matches!(current["status"].as_str(), Some("delivered" | "failed")) {
                    terminal = Some(current);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            terminal.expect("delegate message should leave pending")
        };
        assert!(
            matches!(
                message_status["status"].as_str(),
                Some("delivered" | "failed")
            ),
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
            .decide_approval(approval.id, "approved", Some("looks safe"))
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
        let default_bench = storage
            .get_or_create_default()
            .await
            .expect("get default bench");
        let sent_message = client
            .send_agent_message_in_bench(
                default_bench.id,
                "agent-a",
                "agent-b",
                "delegate",
                None,
                serde_json::json!({ "task": "fail during delegated work" }),
            )
            .await
            .expect("send delegate message through live gateway");
        let message_id = sent_message["id"]
            .as_str()
            .expect("message id string")
            .parse::<Uuid>()
            .expect("message id uuid");

        let message_status = {
            let mut terminal = None;
            for _ in 0..50 {
                let current = client
                    .get_message_status(message_id)
                    .await
                    .expect("query message status");
                if matches!(current["status"].as_str(), Some("delivered" | "failed")) {
                    terminal = Some(current);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            terminal.expect("delegate message should leave pending")
        };

        assert_eq!(message_status["status"], "failed");
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

    #[tokio::test]
    async fn post_delegate_message_requires_bench_id_before_persisting() {
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
                .is_some_and(|message| message.contains("require bench_id")),
            "validation error should explain the missing bench context: {body:?}"
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
            err.0.to_string().contains("result delivery failed"),
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
        assert!(body["bench_id"].is_string());
        assert!(body["created_at"].is_string());
        assert!(body["last_activity"].is_string());
    }

    #[tokio::test]
    async fn get_lens_runtime_defaults_to_assistant() {
        let (router, storage) = make_router();
        let bench = storage
            .get_or_create_default()
            .await
            .expect("default bench");

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/benches/{}/lens-runtime", bench.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["active_lens"], "assistant");
        assert_eq!(body["available_lenses"].as_array().unwrap().len(), 2);
        let stage_modules = body["stage_modules"].as_array().unwrap();
        assert_eq!(stage_modules.len(), 2);
        assert_eq!(stage_modules[0]["kind"], "shared_reference");
        assert_eq!(stage_modules[1]["kind"], "active_team_topology");
    }

    #[tokio::test]
    async fn put_lens_runtime_persists_builder_lens() {
        let (router, storage) = make_router();
        let bench = storage
            .get_or_create_default()
            .await
            .expect("default bench");

        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/benches/{}/lens-runtime", bench.id))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"lens":"builder"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["active_lens"], "builder");
        assert_eq!(body["stage_modules"].as_array().unwrap().len(), 0);

        let stored = swage_storage::store::LensStore::get_active_lens(&*storage, bench.id)
            .await
            .expect("stored lens");
        assert_eq!(stored.active_lens, AppLens::builder());
    }

    #[tokio::test]
    async fn switching_lenses_does_not_change_workflow_capabilities() {
        let (router, storage) = make_router();
        let bench = storage
            .get_or_create_default()
            .await
            .expect("default bench");

        let before = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/capabilities")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let before_body = body_json(before).await;

        let update = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/benches/{}/lens-runtime", bench.id))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"lens":"builder"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(update.status(), StatusCode::OK);

        let after = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/capabilities")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let after_body = body_json(after).await;

        assert_eq!(before_body, after_body);
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
    // POST /api/benches
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_benches_returns_201_with_json() {
        let (router, _) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/benches")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"My Bench"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert!(body["id"].is_string(), "response should have a string id");
        assert!(body["slug"].is_string(), "response should have a slug");
        assert_eq!(body["name"].as_str().unwrap(), "My Bench");
    }

    #[tokio::test]
    async fn post_benches_empty_name_returns_422() {
        let (router, _) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/benches")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_benches_duplicate_name_returns_409() {
        let (router, _) = make_router();

        // First creation succeeds.
        router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/benches")
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
                    .uri("/api/benches")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"Dup"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // -----------------------------------------------------------------------
    // GET /api/benches
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_benches_returns_200_with_list() {
        let (router, _) = make_router();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/benches")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // At least the seeded default bench.
        assert!(body["benches"].is_array());
    }

    // -----------------------------------------------------------------------
    // GET /api/benches/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_bench_by_id_returns_200() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/benches")
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
                    .uri(format!("/api/benches/{id}"))
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
    async fn get_bench_nonexistent_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/benches/{missing}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // PUT /api/benches/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn put_bench_returns_200_with_updated_fields() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/benches")
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
                    .uri(format!("/api/benches/{id}"))
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
    async fn put_bench_nonexistent_returns_404() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/benches/{missing}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"name":"X"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // -----------------------------------------------------------------------
    // DELETE /api/benches/:id
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_bench_returns_204() {
        let (router, _) = make_router();

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/benches")
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
                    .uri(format!("/api/benches/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_default_bench_returns_409() {
        use crate::service::BenchService;

        let (router, storage) = make_router();
        // Get the default bench's ID.
        let svc = BenchService::new(Arc::clone(&storage) as Arc<dyn BenchStore>);
        let default_bench = svc.bench_store.get_or_create_default().await.unwrap();
        let id = default_bench.id;

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/benches/{id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // -----------------------------------------------------------------------
    // GET /api/benches/:bench_id/sessions
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_bench_sessions_returns_200_with_array() {
        let (router, storage) = make_router();

        // Create a named bench directly in storage.
        let bench = storage
            .create(swage_storage::model::NewBench {
                name: "my-bench".into(),
                slug: "my-bench".into(),
            })
            .await
            .unwrap();

        // Add two sessions to that bench.
        storage
            .create_session(swage_storage::model::NewSession {
                bench_id: bench.id,
                agent_id: "gateway".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        storage
            .create_session(swage_storage::model::NewSession {
                bench_id: bench.id,
                agent_id: "gateway".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/benches/{}/sessions", bench.id))
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
    async fn get_bench_sessions_scopes_to_bench() {
        let (router, storage) = make_router();

        let bench_a = storage
            .create(swage_storage::model::NewBench {
                name: "bench-a".into(),
                slug: "bench-a".into(),
            })
            .await
            .unwrap();
        let bench_b = storage
            .create(swage_storage::model::NewBench {
                name: "bench-b".into(),
                slug: "bench-b".into(),
            })
            .await
            .unwrap();

        // One session in A, two in B.
        storage
            .create_session(swage_storage::model::NewSession {
                bench_id: bench_a.id,
                agent_id: "gateway".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        storage
            .create_session(swage_storage::model::NewSession {
                bench_id: bench_b.id,
                agent_id: "gateway".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();
        storage
            .create_session(swage_storage::model::NewSession {
                bench_id: bench_b.id,
                agent_id: "gateway".to_string(),
                metadata: None,
                active_workspace_attachment_id: None,
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/benches/{}/sessions", bench_a.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = body_json(resp).await;
        let list = body["conversations"].as_array().unwrap();
        // Only sessions belonging to bench A.
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn get_bench_sessions_returns_404_for_unknown_bench() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/benches/{missing}/sessions"))
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
    // POST /api/benches/:bench_id/sessions
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_bench_sessions_returns_201_with_json() {
        let (router, storage) = make_router();

        let bench = storage
            .create(swage_storage::model::NewBench {
                name: "my-bench".into(),
                slug: "my-bench".into(),
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/benches/{}/sessions", bench.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert!(body["id"].is_string());
        // Session should be associated with the specified bench.
        assert_eq!(body["bench_id"].as_str().unwrap(), bench.id.to_string());
    }

    #[tokio::test]
    async fn post_bench_sessions_returns_404_for_unknown_bench() {
        let (router, _) = make_router();
        let missing = Uuid::new_v4();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/benches/{missing}/sessions"))
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
    // Team routes
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn post_bench_teams_returns_201_with_json() {
        let (router, storage) = make_router();
        let bench = storage
            .create(swage_storage::model::NewBench {
                name: "team-bench".into(),
                slug: "team-bench".into(),
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/benches/{}/teams", bench.id))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"name":"research","description":"Research team","entry_agent_id":"planner","agents":[{"id":"planner","display_name":"planner"},{"id":"reviewer","display_name":"reviewer"}],"tasks":[]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert_eq!(body["name"].as_str().unwrap(), "research");
        assert_eq!(body["entry_agent_id"].as_str().unwrap(), "planner");
        let members = body["members"].as_array().unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].as_str().unwrap(), "planner");
        assert_eq!(members[1].as_str().unwrap(), "reviewer");
    }

    #[tokio::test]
    async fn get_bench_teams_returns_200_with_array() {
        let (router, storage) = make_router();
        let bench = storage
            .create(swage_storage::model::NewBench {
                name: "team-list-bench".into(),
                slug: "team-list-bench".into(),
            })
            .await
            .unwrap();

        storage
            .create_team(swage_storage::model::NewTeam {
                bench_id: bench.id,
                name: "research".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                agents: team_agents(&["planner"]),
                tasks: vec![],
            })
            .await
            .unwrap();
        storage
            .create_team(swage_storage::model::NewTeam {
                bench_id: bench.id,
                name: "review".into(),
                description: String::new(),
                entry_agent_id: "reviewer".into(),
                agents: team_agents(&["reviewer"]),
                tasks: vec![],
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/benches/{}/teams", bench.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let teams = body["teams"].as_array().unwrap();
        assert_eq!(teams.len(), 2);
    }

    #[tokio::test]
    async fn put_team_returns_200_and_updates_members() {
        let (router, storage) = make_router();
        let bench = storage
            .create(swage_storage::model::NewBench {
                name: "team-update-bench".into(),
                slug: "team-update-bench".into(),
            })
            .await
            .unwrap();
        let team = storage
            .create_team(swage_storage::model::NewTeam {
                bench_id: bench.id,
                name: "research".into(),
                description: "Initial".into(),
                entry_agent_id: "planner".into(),
                agents: team_agents(&["planner"]),
                tasks: vec![],
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/teams/{}", team.id))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        r#"{"entry_agent_id":"reviewer","members":["critic"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["entry_agent_id"].as_str().unwrap(), "reviewer");
        let members = body["members"].as_array().unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].as_str().unwrap(), "reviewer");
        assert_eq!(members[1].as_str().unwrap(), "critic");
    }

    #[tokio::test]
    async fn delete_team_returns_204() {
        let (router, storage) = make_router();
        let bench = storage
            .create(swage_storage::model::NewBench {
                name: "team-delete-bench".into(),
                slug: "team-delete-bench".into(),
            })
            .await
            .unwrap();
        let team = storage
            .create_team(swage_storage::model::NewTeam {
                bench_id: bench.id,
                name: "temporary".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                agents: team_agents(&["planner"]),
                tasks: vec![],
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/teams/{}", team.id))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert!(storage.get_team(team.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn post_team_runs_returns_201_with_trace() {
        let provider = Arc::new(MultiRoundMockProvider::new(vec![
            vec![
                swage_core::StreamChunk::Delta {
                    content: "{\"decision\":\"review\",\"draft\":\"hello\"}".into(),
                },
                swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                    input_tokens: 10,
                    output_tokens: 6,
                }),
                swage_core::StreamChunk::Done {
                    finish_reason: Some(swage_core::FinishReason::Stop),
                },
            ],
            vec![
                swage_core::StreamChunk::Delta {
                    content: "{\"result\":\"approved\"}".into(),
                },
                swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                    input_tokens: 8,
                    output_tokens: 4,
                }),
                swage_core::StreamChunk::Done {
                    finish_reason: Some(swage_core::FinishReason::Stop),
                },
            ],
        ]));
        let storage = Arc::new(MockStorage::new());
        let (router, storage) =
            make_router_with_provider(provider as Arc<dyn LlmProvider>, storage);

        let bench = storage
            .create(swage_storage::model::NewBench {
                name: "team-run-bench".into(),
                slug: "team-run-bench".into(),
            })
            .await
            .unwrap();
        let team = storage
            .create_team(swage_storage::model::NewTeam {
                bench_id: bench.id,
                name: "analysis".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                agents: team_agents(&["planner", "reviewer"]),
                tasks: team_tasks(&["draft-plan", "review-plan"]),
            })
            .await
            .unwrap();

        let topology_source = r#"
import swage_topology as topo

def build_topology():
    return topo.graph(
        "plan",
        [
            topo.llm_node(
                "plan",
                "planner",
                "draft-plan",
                output_schema={
                    "type": "object",
                    "properties": {
                        "decision": {"type": "string"},
                        "draft": {"type": "string"}
                    }
                }
            ),
            topo.llm_node(
                "review",
                "reviewer",
                "review-plan",
                output_schema={
                    "type": "object",
                    "properties": {
                        "result": {"type": "string"}
                    }
                }
            ),
        ],
        [
            topo.conditional_edge(
                "plan-to-review",
                "plan",
                "review",
                "decision",
                "review",
            )
        ],
        max_fan_out=4,
        max_concurrency=2,
        max_total_tokens=100,
    )
"#;

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/teams/{}/runs", team.id))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({
                            "topology_source": topology_source,
                            "message": "hello"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert_eq!(body["run"]["state"].as_str().unwrap(), "completed");
        assert_eq!(body["run"]["final_output"]["result"], "approved");
        assert_eq!(body["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(body["edges"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn get_topology_run_returns_200_with_trace() {
        let provider = Arc::new(MockProvider::new(vec![
            swage_core::StreamChunk::Delta {
                content: "{\"result\":\"ok\"}".into(),
            },
            swage_core::StreamChunk::Usage(swage_core::TokenUsage {
                input_tokens: 4,
                output_tokens: 2,
            }),
            swage_core::StreamChunk::Done {
                finish_reason: Some(swage_core::FinishReason::Stop),
            },
        ]));
        let storage = Arc::new(MockStorage::new());
        let (router, storage) =
            make_router_with_provider(provider as Arc<dyn LlmProvider>, storage);

        let bench = storage
            .create(swage_storage::model::NewBench {
                name: "single-run-bench".into(),
                slug: "single-run-bench".into(),
            })
            .await
            .unwrap();
        let team = storage
            .create_team(swage_storage::model::NewTeam {
                bench_id: bench.id,
                name: "single".into(),
                description: String::new(),
                entry_agent_id: "planner".into(),
                agents: team_agents(&["planner"]),
                tasks: team_tasks(&["draft-plan"]),
            })
            .await
            .unwrap();

        let topology_source = r#"
import swage_topology as topo

def build_topology():
    return topo.graph(
        "plan",
        [
            topo.llm_node(
                "plan",
                "planner",
                "draft-plan",
                output_schema={
                    "type": "object",
                    "properties": {
                        "result": {"type": "string"}
                    }
                }
            )
        ],
        [],
        max_fan_out=1,
        max_concurrency=1,
        max_total_tokens=20,
    )
"#;

        let create_resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/teams/{}/runs", team.id))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({
                            "topology_source": topology_source,
                            "message": "hello"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let create_body = body_json(create_resp).await;
        let run_id = create_body["run"]["id"].as_str().unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/topology-runs/{run_id}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["run"]["id"].as_str().unwrap(), run_id);
        assert_eq!(body["nodes"].as_array().unwrap().len(), 1);
    }

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
        assert_eq!(branches_body.as_array().unwrap().len(), 1);
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
        let state = AppState {
            storage: Arc::clone(&storage) as Arc<dyn Storage>,
            bench_store: Arc::clone(&storage) as Arc<dyn BenchStore>,
            provider: provider as Arc<dyn swage_provider::LlmProvider>,
            voice_provider: Arc::new(swage_provider::UnsupportedVoiceProvider::new(
                "voice is unavailable for tests",
            )) as Arc<dyn swage_provider::VoiceProvider>,
            config: swage_config::ResolvedConfig::default(),
            active_turns: Arc::new(Mutex::new(HashMap::new())),
            active_connections: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tool_dispatcher: Arc::new(crate::tool::ToolDispatcher::allow_all(vec![])),
            hook_registry: None,
            activity_tracker: Arc::new(crate::lifecycle::ActivityTracker::new()),
            broadcast: broadcast_tx,
            shutdown_signal: shutdown_signal.clone(),
            approval_channels: Arc::new(std::sync::Mutex::new(HashMap::new())),
            session_approval_grants: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashSet::new(),
            )),
            channel_routes: Arc::new(Mutex::new(HashMap::new())),
            presence: Arc::new(Mutex::new(crate::presence::PresenceRuntime::default())),
            execution_hosts: Arc::new(Mutex::new(
                crate::execution_host::ExecutionHostRuntime::default(),
            )),
            pairing_codes: Arc::new(Mutex::new(HashMap::new())),
            secret_manager: Arc::new(crate::secret::SecretManager::new(
                Arc::clone(&storage) as Arc<dyn Storage>
            )),
            recovery_exports: std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new())),
            workflow_capabilities: crate::capabilities::WorkflowCapabilities::default(),
            restart_required: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            managed_gateway: false,
        };
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
        let report_json = r#"{"total_tokens":1024,"elements":[]}"#;

        // Store a snapshot directly — simulates what the context engine writes.
        storage.store_snapshot(turn_id, report_json).await.unwrap();

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
    }
}
