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
    client::error::ApiErrorBody::export_all(&cfg).expect("export api error body");
    client::websocket::ClientEvent::export_all(&cfg).expect("export client events");
    client::websocket::ServerEvent::export_all(&cfg).expect("export server events");
    client::topic::CreateTopicRequest::export_all(&cfg).expect("export create topic");
    client::topic::UpdateTopicRequest::export_all(&cfg).expect("export update topic");
    client::topic::TopicListResponse::export_all(&cfg).expect("export topics");
    client::surface::CreateSurfaceRequest::export_all(&cfg).expect("export create surface");
    client::surface::WriteSurfaceModuleRequest::export_all(&cfg)
        .expect("export write surface module");
    client::surface::SurfaceResponse::export_all(&cfg).expect("export surface");
    client::surface::SurfaceListResponse::export_all(&cfg).expect("export surfaces");
    client::surface::CreateSurfaceAnnotationRequest::export_all(&cfg)
        .expect("export create surface annotation");
    client::surface::UpdateSurfaceAnnotationRequest::export_all(&cfg)
        .expect("export update surface annotation");
    client::surface::DeleteSurfaceAnnotationResponse::export_all(&cfg)
        .expect("export delete surface annotation");
    client::surface::SaveSurfaceToLibraryRequest::export_all(&cfg)
        .expect("export save surface to library");
    client::surface::LibraryEntryResponse::export_all(&cfg).expect("export library entry");
    client::surface::LibraryListResponse::export_all(&cfg).expect("export library list");
    client::surface::TopicDefaultSurfaceRequest::export_all(&cfg)
        .expect("export topic default surface");
    client::topic::CreateWorkspaceAttachmentRequest::export_all(&cfg)
        .expect("export attach workspace");
    client::topic::WorkspaceAttachmentListResponse::export_all(&cfg).expect("export workspaces");
    client::conversation::ConversationListResponse::export_all(&cfg).expect("export conversations");
    client::conversation::RecentSessionsResponse::export_all(&cfg).expect("export recent sessions");
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
    client::settings::EffortCapability::export_all(&cfg).expect("export effort capability");
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
    client::topic::UpdateWorkspaceTrustRequest::export_all(&cfg).expect("export workspace trust");
    client::topic::RelinkWorkspaceAttachmentRequest::export_all(&cfg)
        .expect("export workspace relink request");
    client::conversation::CreateConversationBranchRequest::export_all(&cfg)
        .expect("export conversation branch request");
    client::conversation::ConversationBranchResponse::export_all(&cfg)
        .expect("export conversation branch");
    client::conversation::ConversationBranchListResponse::export_all(&cfg)
        .expect("export conversation branch list");
    client::docs::DocumentListResponse::export_all(&cfg).expect("export document list");
    client::docs::DocumentResponse::export_all(&cfg).expect("export document");
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
    client::workflow::WorkflowArgumentContract::export_all(&cfg)
        .expect("export workflow argument contract");
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
    device::DeviceFrame::export_all(&cfg).expect("export device frames");
    device::GatewayFrame::export_all(&cfg).expect("export gateway frames");
    channel::DevDmInboundRequest::export_all(&cfg).expect("export direct-message inbound");
    channel::DevThreadInboundRequest::export_all(&cfg).expect("export thread inbound");
    channel::ChannelInboundResponse::export_all(&cfg).expect("export channel response");
    channel::DeliveryLogQuery::export_all(&cfg).expect("export delivery query");
    channel::DeliveryLogResponse::export_all(&cfg).expect("export delivery log");
    channel::ChannelStatusListResponse::export_all(&cfg).expect("export channel status");

    // ts-rs preserves line-breaking topics around documented enum fields.
    // Normalise generated files so the committed projection also satisfies
    // the repository whitespace gate.
    trim_generated_typescript(&repo_root().join("frontend/src/lib/protocol"));

    let create_topic = std::fs::read_to_string(
        repo_root().join("frontend/src/lib/protocol/CreateTopicRequest.ts"),
    )
    .expect("read generated create-topic binding");
    assert!(create_topic.contains("description?: string"));

    for type_name in ["ProviderSettings", "SaveProviderSettings"] {
        let binding = std::fs::read_to_string(
            repo_root().join(format!("frontend/src/lib/protocol/{type_name}.ts")),
        )
        .expect("read generated provider settings binding");
        assert!(
            binding.contains("worker_staleness_threshold_ms?: bigint | null"),
            "{type_name} must admit the null emitted for an unset threshold: {binding}"
        );
    }

    assert_defaulted_fields_reach_typescript_as_optional();

    assert_option_fields_project_as_optional_nullable(
        &repo_root().join("frontend/src/lib/protocol"),
    );
}

/// A field the wire contract fills in for itself when the sender omits it.
struct DefaultedField {
    type_name: String,
    wire_name: String,
    location: String,
}

/// Every `#[serde(default)]` field must reach TypeScript as an optional
/// property: a sender that legitimately leaves one out is accepted on the
/// wire, so a projection that demands it turns a valid request into a
/// type error.
fn assert_defaulted_fields_reach_typescript_as_optional() {
    let directory = repo_root().join("frontend/src/lib/protocol");
    let fields = defaulted_fields();

    // Guards the reader below against silently matching nothing: the
    // sweep only means something while it still finds a field known to
    // be defaulted.
    assert!(
        fields.iter().any(
            |field| field.type_name == "CreateTopicRequest" && field.wire_name == "description"
        ),
        "the source sweep stopped finding defaulted fields",
    );

    let mut required = Vec::new();
    for field in fields {
        // A type with no binding on disk has no TypeScript projection —
        // the regeneration above never touched it.
        let Ok(binding) =
            std::fs::read_to_string(directory.join(format!("{}.ts", field.type_name)))
        else {
            continue;
        };
        if !binding.contains(&format!("{}?:", field.wire_name)) {
            required.push(format!(
                "{} — {}.{}",
                field.location, field.type_name, field.wire_name
            ));
        }
    }

    assert!(
        required.is_empty(),
        "defaulted fields projected as required TypeScript properties:\n{}",
        required.join("\n"),
    );
}

/// Reads the defaulted fields out of the crate's own source, so a field
/// gains coverage the day it is declared rather than when someone
/// remembers to extend a list here.
fn defaulted_fields() -> Vec<DefaultedField> {
    let mut sources = Vec::new();
    collect_rust_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut sources,
    );
    sources.sort();

    let mut fields = Vec::new();
    let mut declared = 0usize;

    for path in &sources {
        let source = std::fs::read_to_string(path).expect("read protocol source");
        declared += source.matches("#[serde(default").count();

        let mut owner: Option<String> = None;
        let mut attributes: Vec<&str> = Vec::new();
        for (index, line) in source.lines().enumerate() {
            let line = line.trim();
            if let Some(name) = declared_type(line) {
                owner = Some(name.to_owned());
                attributes.clear();
            } else if line.starts_with("#[") {
                attributes.push(line);
            } else if let Some(name) = declared_field(line) {
                if attributes
                    .iter()
                    .any(|attribute| attribute.starts_with("#[serde(default"))
                {
                    fields.push(DefaultedField {
                        type_name: owner.clone().expect("field declared inside a type"),
                        wire_name: wire_name(name, &attributes),
                        location: format!("{}:{}", path.display(), index + 1),
                    });
                }
                attributes.clear();
            } else if !line.is_empty() && !line.starts_with("//") {
                attributes.clear();
            }
        }
    }

    // Every `#[serde(default)]` in the crate must have landed on a field
    // the reader recognised; a shortfall means the sweep skipped one.
    assert_eq!(
        fields.len(),
        declared,
        "the source sweep missed a #[serde(default)] attribute",
    );
    fields
}

fn collect_rust_sources(directory: &Path, into: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).expect("read protocol source directory") {
        let path = entry.expect("read protocol source entry").path();
        if path.is_dir() {
            collect_rust_sources(&path, into);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            into.push(path);
        }
    }
}

/// The struct or enum a `struct Foo {` / `enum Foo {` line opens.
fn declared_type(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("pub ").unwrap_or(line);
    let rest = rest
        .strip_prefix("struct ")
        .or_else(|| rest.strip_prefix("enum "))?;
    let name = rest
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()?;
    (!name.is_empty()).then_some(name)
}

/// The field a `name: Type,` line declares, in a struct body or an
/// enum variant's.
fn declared_field(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("pub ").unwrap_or(line);
    let (name, tail) = rest.split_once(':')?;
    if tail.starts_with(':') {
        return None;
    }
    let name = name.trim();
    let plain = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    (!name.is_empty() && plain).then_some(name)
}

/// The name the field carries on the wire — its own, unless serde renames it.
fn wire_name(field: &str, attributes: &[&str]) -> String {
    for attribute in attributes {
        if !attribute.starts_with("#[serde(") {
            continue;
        }
        if let Some((_, rest)) = attribute.split_once("rename = \"")
            && let Some((renamed, _)) = rest.split_once('"')
        {
            return renamed.to_owned();
        }
    }
    field.to_owned()
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

/// The OpenAPI document and the TypeScript client are projections of the
/// route manifest; like the schema, they are rewritten in place so drift
/// shows as a dirty tree at the gate.
#[test]
fn client_route_projections_are_regenerated() {
    let document = serde_json::to_string_pretty(&client::openapi::document())
        .expect("openapi document serialises")
        + "\n";
    let document_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/client-openapi.json");
    std::fs::write(&document_path, document).expect("write openapi document");

    let client_path = repo_root().join("frontend/src/lib/protocol/client.ts");
    std::fs::write(&client_path, client::typescript_client::render())
        .expect("write generated client");
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

/// The one projection rule for `Option<T>` fields: the JSON Schema leaves
/// them out of `required` and admits null, so the TypeScript member is
/// `name?: T | null` — never the required-nullable `name: T | null`, which
/// would make the two projections disagree about whether a producer may
/// omit the key. Every struct in the crate carries
/// `#[ts(optional_fields = nullable)]`; this reads the generated bindings
/// back so a struct that forgets it fails here rather than in a client.
fn assert_option_fields_project_as_optional_nullable(bindings: &Path) {
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(bindings).expect("generated bindings directory") {
        let path = entry.expect("binding entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("ts") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read binding");
        let declaration: String = text
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !(trimmed.starts_with("/**")
                    || trimmed.starts_with('*')
                    || trimmed.starts_with("//"))
            })
            .collect::<Vec<_>>()
            .join(" ");
        for member in declaration.split(", ") {
            let Some((name, ty)) = member.split_once(": ") else {
                continue;
            };
            let name = name.trim().trim_start_matches('{').trim();
            let is_identifier =
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if is_identifier
                && ty
                    .trim_end_matches(&[',', ' ', '}', ';'][..])
                    .ends_with("| null")
            {
                offenders.push(format!("{}: {name}", path.display()));
            }
        }
    }
    offenders.sort();
    assert!(
        offenders.is_empty(),
        "{} required-nullable member(s) in the generated bindings; every struct deriving TS carries #[ts(optional_fields = nullable)]:\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}
