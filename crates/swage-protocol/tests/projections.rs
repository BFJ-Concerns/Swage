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
    client::space::CreateWorkspaceAttachmentRequest::export_all(&cfg)
        .expect("export attach workspace");
    client::space::WorkspaceAttachmentListResponse::export_all(&cfg).expect("export workspaces");
    client::conversation::ConversationListResponse::export_all(&cfg).expect("export conversations");
    client::conversation::ConversationResponse::export_all(&cfg)
        .expect("export conversation response");
    client::conversation::ConversationDetail::export_all(&cfg).expect("export conversation detail");
    client::conversation::ConversationThreadDetail::export_all(&cfg)
        .expect("export conversation thread");
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
    client::workflow::WorkflowRunDetailResponse::export_all(&cfg).expect("export workflow detail");
    client::workflow::WorkflowRunListResponse::export_all(&cfg).expect("export workflow runs");
    client::workflow::WorkflowRunStartedEvent::export_all(&cfg)
        .expect("export workflow started event");
    client::workflow::WorkflowEntryEvent::export_all(&cfg).expect("export workflow entry event");
    client::workflow::WorkflowRunCompletedEvent::export_all(&cfg)
        .expect("export workflow completed event");
    hosting::PresenceSnapshotResponse::export_all(&cfg).expect("export hosting presence");
    hosting::MachineHeartbeatRequest::export_all(&cfg).expect("export hosting heartbeat");
    hosting::ApprovalListResponse::export_all(&cfg).expect("export hosting approvals");
    hosting::ApprovalDecisionRequest::export_all(&cfg).expect("export hosting approval decision");
    hosting::RollbackConfigurationRequest::export_all(&cfg)
        .expect("export configuration rollback request");
    hosting::ConfigurationChangeResponse::export_all(&cfg)
        .expect("export configuration change response");
    hosting::OperatorOverviewResponse::export_all(&cfg).expect("export operator overview");
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
