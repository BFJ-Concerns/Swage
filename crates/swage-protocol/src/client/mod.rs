//! Client surface: types spoken between the gateway and its own clients
//! (web frontend, TUI, desktop shell) over HTTP and WebSocket.

pub mod approval;
pub mod channel;
pub mod context;
pub mod conversation;
pub mod delivery;
pub mod docs;
pub mod error;
pub mod knowledge;
pub mod material;
pub mod memory;
pub mod openapi;
pub mod provider_signin;
pub mod recovery;
pub mod routes;
pub mod schedule;
pub mod settings;
pub mod settings_section;
pub mod skill;
pub mod surface;
pub mod topic;
pub mod turn;
pub mod typescript_client;
pub mod voice;
pub mod websocket;
pub mod workflow;

use schemars::JsonSchema;

/// Anchor type for the client surface's schema document.
///
/// Every wire type on the surface is reachable from here, so a single
/// `schema_for!` call produces one document with the full type graph in
/// `$defs`. The struct itself never crosses the wire — it exists only so
/// the schema generator has a root to walk from.
#[derive(JsonSchema)]
#[allow(dead_code)]
pub struct ClientSurface {
    api_error: error::ApiErrorBody,
    client_event: websocket::ClientEvent,
    server_event: websocket::ServerEvent,
    voice_capabilities: voice::VoiceCapabilitiesResponse,
    transcribe_audio: voice::TranscribeAudioRequest,
    transcribe_audio_response: voice::TranscribeAudioResponse,
    synthesise_speech: voice::SynthesiseSpeechRequest,
    synthesise_speech_response: voice::SynthesiseSpeechResponse,
    create_channel_link_code_request: channel::CreateChannelLinkCodeRequest,
    create_topic_request: topic::CreateTopicRequest,
    create_surface_request: surface::CreateSurfaceRequest,
    memory_page_list: memory::MemoryPageListResponse,
    memory_page: memory::MemoryPageResponse,
    memory_instruction_request: memory::MemoryInstructionRequest,
    memory_instruction_response: memory::MemoryInstructionResponse,
    write_surface_module_request: surface::WriteSurfaceModuleRequest,
    create_surface_annotation_request: surface::CreateSurfaceAnnotationRequest,
    update_surface_annotation_request: surface::UpdateSurfaceAnnotationRequest,
    delete_surface_annotation_response: surface::DeleteSurfaceAnnotationResponse,
    surface: surface::SurfaceResponse,
    surface_list: surface::SurfaceListResponse,
    save_surface_to_library_request: surface::SaveSurfaceToLibraryRequest,
    library_entry: surface::LibraryEntryResponse,
    library_list: surface::LibraryListResponse,
    topic_default_surface_request: surface::TopicDefaultSurfaceRequest,
    update_topic_request: topic::UpdateTopicRequest,
    topic_list: topic::TopicListResponse,
    create_workspace_attachment_request: topic::CreateWorkspaceAttachmentRequest,
    update_workspace_trust_request: topic::UpdateWorkspaceTrustRequest,
    relink_workspace_attachment_request: topic::RelinkWorkspaceAttachmentRequest,
    workspace_attachment_list: topic::WorkspaceAttachmentListResponse,
    channel_link_code_response: channel::ChannelLinkCodeResponse,
    create_conversation_request: conversation::CreateConversationRequest,
    create_conversation_branch_request: conversation::CreateConversationBranchRequest,
    conversation: conversation::ConversationResponse,
    conversation_list: conversation::ConversationListResponse,
    recent_sessions: conversation::RecentSessionsResponse,
    conversation_detail: conversation::ConversationDetail,
    conversation_thread: conversation::ConversationThreadDetail,
    conversation_branch: conversation::ConversationBranchResponse,
    conversation_branch_list: conversation::ConversationBranchListResponse,
    document_list: docs::DocumentListResponse,
    document: docs::DocumentResponse,
    turn_context: context::TurnContextResponse,
    submit_turn: turn::SubmitTurnRequest,
    message_attachment: turn::MessageAttachment,
    submit_turn_response: turn::SubmitTurnResponse,
    turn_poll: turn::TurnPollResponse,
    send_message: turn::SendMessageRequest,
    message_response: turn::MessageResponse,
    settings: settings::SettingsResponse,
    effort_capability: settings::EffortCapability,
    skills: skill::SkillListResponse,
    save_settings: settings::SaveSettingsRequest,
    model_discovery: settings::ModelDiscoveryResponse,
    save_provider_settings: settings::SaveProviderSettings,
    restart_status: settings::RestartStatusResponse,
    provider_sign_in_state: provider_signin::ProviderSignInState,
    start_provider_sign_in_request: provider_signin::StartProviderSignInRequest,
    start_provider_sign_in_response: provider_signin::StartProviderSignInResponse,
    create_attachment_request: material::CreateAttachmentRequest,
    queued_attachment: material::QueuedAttachmentResponse,
    attachment_list: material::AttachmentListResponse,
    create_material: material::CreateMaterialRequest,
    promote_material: material::PromoteMaterialRequest,
    material_list: material::MaterialListResponse,
    recovery_exports: recovery::RecoveryExportListResponse,
    knowledge_search: knowledge::KnowledgeSearchResponse,
    orientation_find: knowledge::OrientationFindResponse,
    create_schedule_request: schedule::CreateScheduleRequest,
    update_schedule_request: schedule::UpdateScheduleRequest,
    schedule: schedule::Schedule,
    schedule_list: schedule::ScheduleListResponse,
    schedule_run: schedule::ScheduleRun,
    schedule_run_list: schedule::ScheduleRunListResponse,
    trigger_schedule_response: schedule::TriggerScheduleResponse,
    create_workflow_script_request: workflow::CreateWorkflowScriptRequest,
    update_workflow_script_request: workflow::UpdateWorkflowScriptRequest,
    workflow_script_list: workflow::WorkflowScriptListResponse,
    workflow_argument_contract: workflow::WorkflowArgumentContract,
    start_workflow_run_request: workflow::StartWorkflowRunRequest,
    start_inline_workflow_run_request: workflow::StartInlineWorkflowRunRequest,
    post_workflow_run_event_request: workflow::PostWorkflowRunEventRequest,
    post_workflow_run_event_response: workflow::PostWorkflowRunEventResponse,
    workflow_decision_event_payload: workflow::WorkflowDecisionEventPayload,
    workflow_run_detail: workflow::WorkflowRunDetailResponse,
    workflow_run_list: workflow::WorkflowRunListResponse,
    workflow_run_started_event: workflow::WorkflowRunStartedEvent,
    workflow_entry_event: workflow::WorkflowEntryEvent,
    workflow_run_completed_event: workflow::WorkflowRunCompletedEvent,
    worker_isolation: workflow::WorkerIsolation,
}
