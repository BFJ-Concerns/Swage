//! Regenerates the committed projections of the wire contract.
//!
//! Running `cargo test -p swage-protocol` rewrites both projections in
//! place, so drift between the Rust types and the committed artifacts
//! shows up as a dirty working tree rather than a silent mismatch:
//!
//! - TypeScript bindings → `frontend/src/lib/protocol/`
//! - JSON Schema (canonical artifact) → `crates/swage-protocol/schema/`

use std::path::{Path, PathBuf};

use ts_rs::TS;

use swage_protocol::{channel, client, device, hosting};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives two levels below the repo root")
        .to_path_buf()
}

#[test]
fn typescript_bindings_are_regenerated() {
    let cfg = ts_rs::Config::new().with_out_dir(repo_root().join("frontend/src/lib/protocol"));

    // Exporting the top-level types pulls in every dependency they
    // reference, so the full surface lands in one pass.
    client::channel::CreateChannelLinkCodeRequest::export_all(&cfg)
        .expect("export channel link code request");
    client::channel::ChannelLinkCodeResponse::export_all(&cfg)
        .expect("export channel link code response");
    client::websocket::ClientEvent::export_all(&cfg).expect("export client events");
    client::websocket::ServerEvent::export_all(&cfg).expect("export server events");
    client::space::CreateSpaceRequest::export_all(&cfg).expect("export create space");
    client::space::UpdateSpaceRequest::export_all(&cfg).expect("export update space");
    client::space::SpaceListResponse::export_all(&cfg).expect("export spaces");
    client::page::CreatePageRequest::export_all(&cfg).expect("export create page");
    client::page::WritePageModuleRequest::export_all(&cfg).expect("export write page module");
    client::page::PageResponse::export_all(&cfg).expect("export page");
    client::page::PageListResponse::export_all(&cfg).expect("export pages");
    client::space::CreateWorkspaceAttachmentRequest::export_all(&cfg)
        .expect("export attach workspace");
    client::space::WorkspaceAttachmentListResponse::export_all(&cfg).expect("export workspaces");
    client::conversation::ConversationListResponse::export_all(&cfg).expect("export conversations");
    client::conversation::ConversationResponse::export_all(&cfg)
        .expect("export conversation response");
    client::conversation::CreateConversationRequest::export_all(&cfg)
        .expect("export create conversation request");
    client::conversation::ConversationDetail::export_all(&cfg).expect("export conversation detail");
    client::conversation::ConversationThreadDetail::export_all(&cfg)
        .expect("export conversation thread");
    client::context::TurnContextResponse::export_all(&cfg).expect("export turn context");
    client::turn::SubmitTurnRequest::export_all(&cfg).expect("export submit turn");
    client::turn::SubmitTurnResponse::export_all(&cfg).expect("export submit turn response");
    client::turn::TurnPollResponse::export_all(&cfg).expect("export turn polling");
    client::turn::SendMessageRequest::export_all(&cfg).expect("export send message");
    client::turn::MessageResponse::export_all(&cfg).expect("export message response");
    client::settings::SettingsResponse::export_all(&cfg).expect("export settings");
    client::settings::SaveSettingsRequest::export_all(&cfg).expect("export save settings");
    client::settings::ModelDiscoveryResponse::export_all(&cfg).expect("export model discovery");
    client::settings::RestartStatusResponse::export_all(&cfg).expect("export restart status");
    client::material::CreateAttachmentRequest::export_all(&cfg).expect("export attachment request");
    client::material::QueuedAttachmentResponse::export_all(&cfg).expect("export queued attachment");
    client::material::AttachmentListResponse::export_all(&cfg).expect("export attachments");
    client::material::CreateMaterialRequest::export_all(&cfg).expect("export material request");
    client::material::PromoteMaterialRequest::export_all(&cfg).expect("export material promotion");
    client::material::MaterialListResponse::export_all(&cfg).expect("export materials");
    client::recovery::RecoveryExportListResponse::export_all(&cfg).expect("export recovery list");
    client::space::UpdateWorkspaceTrustRequest::export_all(&cfg).expect("export workspace trust");
    client::conversation::CreateConversationBranchRequest::export_all(&cfg)
        .expect("export conversation branch request");
    client::voice::TranscribeAudioResponse::export_all(&cfg)
        .expect("export transcription response");
    client::voice::SynthesiseSpeechResponse::export_all(&cfg).expect("export speech response");
    client::knowledge::KnowledgeSearchResponse::export_all(&cfg)
        .expect("export knowledge search response");
    client::knowledge::OrientationFindResponse::export_all(&cfg)
        .expect("export orientation find response");
    client::voice::VoiceCapabilitiesResponse::export_all(&cfg).expect("export voice capabilities");
    client::voice::TranscribeAudioRequest::export_all(&cfg).expect("export transcription");
    client::voice::SynthesiseSpeechRequest::export_all(&cfg).expect("export speech synthesis");
    client::provider_signin::StartProviderSignInRequest::export_all(&cfg)
        .expect("export sign-in request");
    client::provider_signin::StartProviderSignInResponse::export_all(&cfg)
        .expect("export sign-in response");
    client::provider_signin::ProviderSignInState::export_all(&cfg).expect("export sign-in state");
    client::schedule::CreateScheduleRequest::export_all(&cfg).expect("export create");
    client::schedule::UpdateScheduleRequest::export_all(&cfg).expect("export update");
    client::schedule::Schedule::export_all(&cfg).expect("export schedule");
    client::schedule::ScheduleListResponse::export_all(&cfg).expect("export list");
    client::schedule::ScheduleRunListResponse::export_all(&cfg).expect("export runs");
    client::schedule::TriggerScheduleResponse::export_all(&cfg).expect("export trigger");
    client::workflow::CreateWorkflowScriptRequest::export_all(&cfg)
        .expect("export workflow create");
    client::workflow::UpdateWorkflowScriptRequest::export_all(&cfg)
        .expect("export workflow update");
    client::workflow::WorkflowScriptListResponse::export_all(&cfg)
        .expect("export workflow scripts");
    client::workflow::StartWorkflowRunRequest::export_all(&cfg)
        .expect("export workflow run request");
    client::workflow::StartInlineWorkflowRunRequest::export_all(&cfg)
        .expect("export inline workflow run request");
    client::workflow::PostWorkflowRunEventRequest::export_all(&cfg)
        .expect("export workflow event post request");
    client::workflow::PostWorkflowRunEventResponse::export_all(&cfg)
        .expect("export workflow event post response");
    client::workflow::WorkflowDecisionEventPayload::export_all(&cfg)
        .expect("export workflow decision event payload");
    client::workflow::WorkflowRunDetailResponse::export_all(&cfg).expect("export workflow detail");
    client::workflow::WorkflowRunListResponse::export_all(&cfg).expect("export workflow runs");
    client::workflow::WorkflowRunStartedEvent::export_all(&cfg)
        .expect("export workflow started event");
    client::workflow::WorkflowEntryEvent::export_all(&cfg).expect("export workflow entry event");
    client::workflow::WorkflowRunCompletedEvent::export_all(&cfg)
        .expect("export workflow completed event");
    client::workflow::WorkerIsolation::export_all(&cfg).expect("export worker isolation");
    client::skill::SkillListResponse::export_all(&cfg).expect("export skill catalogue");
    hosting::PresenceSnapshotResponse::export_all(&cfg).expect("export hosting presence");
    hosting::MachineHeartbeatRequest::export_all(&cfg).expect("export hosting heartbeat");
    hosting::ApprovalListResponse::export_all(&cfg).expect("export hosting approvals");
    hosting::ApprovalDecisionRequest::export_all(&cfg).expect("export hosting approval decision");
    hosting::OperatorOverviewResponse::export_all(&cfg).expect("export operator overview");
    hosting::OperatorHistoryResponse::export_all(&cfg).expect("export operator history");
    hosting::AgentDefinitionScopeQuery::export_all(&cfg).expect("export agent definition query");
    hosting::CreateAgentDefinitionRequest::export_all(&cfg)
        .expect("export agent definition create");
    hosting::UpdateAgentDefinitionRequest::export_all(&cfg)
        .expect("export agent definition update");
    hosting::AgentDefinitionListResponse::export_all(&cfg).expect("export agent definition list");
    hosting::EffectiveAgentDefinitionListResponse::export_all(&cfg)
        .expect("export effective agent definition list");
    hosting::AgentDefinitionMutationResponse::export_all(&cfg)
        .expect("export agent definition mutation");
    hosting::KnowledgeMirrorRegenerateResponse::export_all(&cfg)
        .expect("export knowledge mirror regenerate");
    device::RegisterDeviceRequest::export_all(&cfg).expect("export device registration");
    device::DeviceListResponse::export_all(&cfg).expect("export device list");
    device::RemoteToolExecutionRequest::export_all(&cfg).expect("export device execution");
    device::RemoteToolExecutionResponse::export_all(&cfg)
        .expect("export device execution response");
    channel::DevDmInboundRequest::export_all(&cfg).expect("export direct-message inbound");
    channel::DevThreadInboundRequest::export_all(&cfg).expect("export thread inbound");
    channel::ChannelInboundResponse::export_all(&cfg).expect("export channel response");
    channel::DeliveryLogQuery::export_all(&cfg).expect("export delivery query");
    channel::DeliveryLogResponse::export_all(&cfg).expect("export delivery log");
    channel::ChannelStatusListResponse::export_all(&cfg).expect("export channel status");

    // ts-rs preserves line-breaking spaces around documented enum fields.
    // Normalise generated files so the committed projection also satisfies
    // the repository whitespace gate.
    trim_generated_typescript(&repo_root().join("frontend/src/lib/protocol"));
}

fn trim_generated_typescript(directory: &Path) {
    for entry in std::fs::read_dir(directory).expect("read generated TypeScript directory") {
        let path = entry.expect("read generated TypeScript entry").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("ts") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read generated TypeScript file");
        let normalised = source
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(path, normalised).expect("normalise generated TypeScript file");
    }
}

#[test]
fn client_schema_is_regenerated() {
    let schema = schemars::schema_for!(client::ClientSurface);
    let rendered = serde_json::to_string_pretty(&schema).expect("schema serialises") + "\n";

    let schema_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/client.json");
    std::fs::create_dir_all(schema_path.parent().expect("schema dir")).expect("create schema dir");
    std::fs::write(&schema_path, rendered).expect("write schema document");
}

#[test]
fn device_schema_is_regenerated() {
    write_schema::<device::DeviceSurface>("device.json");
}

#[test]
fn channel_schema_is_regenerated() {
    write_schema::<channel::ChannelSurface>("channel.json");
}

#[test]
fn hosting_schema_is_regenerated() {
    write_schema::<hosting::HostingSurface>("hosting.json");
}

fn write_schema<T: schemars::JsonSchema>(name: &str) {
    let schema = schemars::schema_for!(T);
    let rendered = serde_json::to_string_pretty(&schema).expect("schema serialises") + "\n";
    let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schema")
        .join(name);
    std::fs::create_dir_all(schema_path.parent().expect("schema dir")).expect("create schema dir");
    std::fs::write(schema_path, rendered).expect("write schema document");
}
