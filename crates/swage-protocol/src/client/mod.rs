//! Client surface: types spoken between the gateway and its own clients
//! (web frontend, TUI, desktop shell) over HTTP and WebSocket.

pub mod approval;
pub mod channel;
pub mod context;
pub mod conversation;
pub mod delivery;
pub mod knowledge;
pub mod material;
pub mod page;
pub mod provider_signin;
pub mod recovery;
pub mod schedule;
pub mod settings;
pub mod skill;
pub mod space;
pub mod turn;
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
    client_event: websocket::ClientEvent,
    server_event: websocket::ServerEvent,
    voice_capabilities: voice::VoiceCapabilitiesResponse,
    transcribe_audio: voice::TranscribeAudioRequest,
    transcribe_audio_response: voice::TranscribeAudioResponse,
    synthesise_speech: voice::SynthesiseSpeechRequest,
    synthesise_speech_response: voice::SynthesiseSpeechResponse,
    create_channel_link_code_request: channel::CreateChannelLinkCodeRequest,
    create_space_request: space::CreateSpaceRequest,
    create_page_request: page::CreatePageRequest,
    write_page_module_request: page::WritePageModuleRequest,
    create_page_annotation_request: page::CreatePageAnnotationRequest,
    update_page_annotation_request: page::UpdatePageAnnotationRequest,
    page: page::PageResponse,
    page_list: page::PageListResponse,
    create_page_template_request: page::CreatePageTemplateRequest,
    page_template: page::PageTemplateResponse,
    page_template_list: page::PageTemplateListResponse,
    update_space_request: space::UpdateSpaceRequest,
    space_list: space::SpaceListResponse,
    create_workspace_attachment_request: space::CreateWorkspaceAttachmentRequest,
    update_workspace_trust_request: space::UpdateWorkspaceTrustRequest,
    workspace_attachment_list: space::WorkspaceAttachmentListResponse,
    channel_link_code_response: channel::ChannelLinkCodeResponse,
    create_conversation_request: conversation::CreateConversationRequest,
    create_conversation_branch_request: conversation::CreateConversationBranchRequest,
    conversation: conversation::ConversationResponse,
    conversation_list: conversation::ConversationListResponse,
    conversation_detail: conversation::ConversationDetail,
    conversation_thread: conversation::ConversationThreadDetail,
    turn_context: context::TurnContextResponse,
    submit_turn: turn::SubmitTurnRequest,
    submit_turn_response: turn::SubmitTurnResponse,
    turn_poll: turn::TurnPollResponse,
    send_message: turn::SendMessageRequest,
    message_response: turn::MessageResponse,
    settings: settings::SettingsResponse,
    skills: skill::SkillListResponse,
    save_settings: settings::SaveSettingsRequest,
    model_discovery: settings::ModelDiscoveryResponse,
    restart_status: settings::RestartStatusResponse,
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
    start_provider_sign_in_request: provider_signin::StartProviderSignInRequest,
    start_provider_sign_in_response: provider_signin::StartProviderSignInResponse,
    provider_sign_in_state: provider_signin::ProviderSignInState,
    create_workflow_script_request: workflow::CreateWorkflowScriptRequest,
    update_workflow_script_request: workflow::UpdateWorkflowScriptRequest,
    workflow_script_list: workflow::WorkflowScriptListResponse,
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
