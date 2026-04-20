use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::Stream;
use serde_json::{Value, json};
use tower::ServiceExt;

use swage_core::{
    CompletionRequest, CompletionResponse, FinishReason, ProviderError, StreamChunk,
    ToolCompletionRequest, ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{
    SqliteStorage,
    store::{AuditStore, ProjectStore, Storage},
};

static SWAGE_HOME_LOCK: Mutex<()> = Mutex::new(());

struct MockProvider {
    responses: Mutex<VecDeque<String>>,
}

impl MockProvider {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "builder-runtime-test"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let content = self
            .responses
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pop_front()
            .expect("mock provider response");
        Ok(CompletionResponse {
            content,
            input_tokens: 100,
            output_tokens: 60,
            finish_reason: FinishReason::Stop,
            cost: None,
        })
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("builder runtime tests do not use tool completion")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> std::pin::Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::iter(vec![StreamChunk::Done {
            finish_reason: None,
        }]))
    }
}

fn test_state(db_path: &Path, provider: Arc<dyn LlmProvider>) -> AppState {
    let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
    AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn ProjectStore>,
        provider,
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
        None,
    )
}

async fn json_response(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            Value::String(String::from_utf8(bytes.to_vec()).expect("utf-8 response"))
        })
    }
}

async fn request_json(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let response = router
        .clone()
        .oneshot(
            builder
                .body(match body {
                    Some(body) => Body::from(body.to_string()),
                    None => Body::empty(),
                })
                .unwrap(),
        )
        .await
        .unwrap();
    (response.status(), json_response(response).await)
}

#[tokio::test(flavor = "multi_thread")]
async fn conversational_builder_proposes_applies_and_survives_restart() {
    let _guard = SWAGE_HOME_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let swage_home = temp.path().join("swage-home");
    let workspace = temp.path().join("workspace");
    let db_path = temp.path().join("builder-runtime.db");
    std::fs::create_dir_all(&swage_home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe { std::env::set_var("SWAGE_HOME", &swage_home) };

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![json!({
        "summary": "Create a review workspace with a new lens, delivery team, and project brief bundle.",
        "operations": [
            {
                "kind": "create_team",
                "name": "delivery-core",
                "description": "Owns delivery reviews.",
                "entry_agent_id": "reviewer",
                "members": ["qa"]
            },
            {
                "kind": "create_lens",
                "id": "delivery",
                "title": "Delivery lens",
                "summary": "Operator-first delivery workspace.",
                "default_surface": "operator",
                "ui_modules": ["operator", "conversation"],
                "prominent_team_ids": ["delivery-core"]
            },
            {
                "kind": "create_project_extension",
                "name": "delivery-brief",
                "display_name": "Delivery Brief",
                "description": "Keeps delivery constraints explicit.",
                "requested_tools": ["file_read"],
                "instructions": "Always restate the brief before editing the project."
            }
        ]
    })
    .to_string()]));

    let router = build_router(test_state(&db_path, provider));

    let (project_status, project_body) = request_json(
        &router,
        "POST",
        "/api/projects",
        Some(json!({
            "name": "Builder Runtime Project",
            "workspace_path": workspace.to_string_lossy()
        })),
    )
    .await;
    assert_eq!(project_status, StatusCode::CREATED);
    let project_id = project_body["id"].as_str().unwrap();
    let project_slug = project_body["slug"].as_str().unwrap();

    let (proposal_status, proposal_body) = request_json(
        &router,
        "POST",
        &format!("/api/projects/{project_id}/builder/proposals"),
        Some(json!({
            "prompt": "Create a delivery workspace with a dedicated lens, team, and brief extension."
        })),
    )
    .await;
    assert_eq!(proposal_status, StatusCode::OK);
    assert_eq!(proposal_body["can_apply"], json!(true));
    assert_eq!(proposal_body["operations"].as_array().unwrap().len(), 3);

    let (apply_status, apply_body) = request_json(
        &router,
        "POST",
        &format!("/api/projects/{project_id}/builder/apply"),
        Some(json!({
            "request_text": "Create a delivery workspace with a dedicated lens, team, and brief extension.",
            "operations": proposal_body["operations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|operation| operation["operation"].clone())
                .collect::<Vec<_>>()
        })),
    )
    .await;
    assert_eq!(apply_status, StatusCode::OK);
    assert_eq!(apply_body["success"], json!(true));
    assert_eq!(apply_body["applied"].as_array().unwrap().len(), 3);

    let restarted_router = build_router(test_state(&db_path, Arc::new(MockProvider::new(vec![]))));

    let (lenses_status, lenses_body) =
        request_json(&restarted_router, "GET", "/api/lenses", None).await;
    assert_eq!(lenses_status, StatusCode::OK);
    assert!(
        lenses_body["lenses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|lens| lens["id"] == "delivery"),
        "created lens should survive restart"
    );

    let (teams_status, teams_body) = request_json(
        &restarted_router,
        "GET",
        &format!("/api/projects/{project_id}/teams"),
        None,
    )
    .await;
    assert_eq!(teams_status, StatusCode::OK);
    assert!(
        teams_body["teams"]
            .as_array()
            .unwrap()
            .iter()
            .any(|team| team["name"] == "delivery-core"),
        "created team should survive restart"
    );

    let (extensions_status, extensions_body) = request_json(
        &restarted_router,
        "GET",
        &format!("/api/projects/{project_id}/extensions"),
        None,
    )
    .await;
    assert_eq!(extensions_status, StatusCode::OK);
    assert!(
        extensions_body["extensions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|extension| extension["name"] == "delivery-brief"),
        "created extension should survive restart"
    );

    let skill_path = swage_home
        .join("projects")
        .join(project_slug)
        .join("skills")
        .join("delivery-brief")
        .join("SKILL.md");
    assert!(
        skill_path.is_file(),
        "managed extension skill file should exist"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn visual_and_conversational_builder_writes_share_canonical_audit_service() {
    let _guard = SWAGE_HOME_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let swage_home = temp.path().join("swage-home");
    let workspace = temp.path().join("workspace");
    let db_path = temp.path().join("builder-audit.db");
    std::fs::create_dir_all(&swage_home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe { std::env::set_var("SWAGE_HOME", &swage_home) };

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![
        json!({
            "summary": "Tighten the existing team description.",
            "operations": [
                {
                    "kind": "update_team",
                    "team_name": "workspace-core",
                    "description": "Owns the conversational builder runtime.",
                    "members": ["default", "reviewer"]
                }
            ]
        })
        .to_string(),
    ]));
    let router = build_router(test_state(&db_path, provider));

    let (project_status, project_body) = request_json(
        &router,
        "POST",
        "/api/projects",
        Some(json!({
            "name": "Builder Audit Project",
            "workspace_path": workspace.to_string_lossy()
        })),
    )
    .await;
    assert_eq!(project_status, StatusCode::CREATED);
    let project_id = project_body["id"].as_str().unwrap();

    let (create_team_status, team_body) = request_json(
        &router,
        "POST",
        &format!("/api/projects/{project_id}/teams"),
        Some(json!({
            "name": "workspace-core",
            "description": "Owns the builder workflow.",
            "entry_agent_id": "default",
            "members": ["reviewer"]
        })),
    )
    .await;
    assert_eq!(create_team_status, StatusCode::CREATED);
    assert_eq!(team_body["name"], "workspace-core");

    let (proposal_status, proposal_body) = request_json(
        &router,
        "POST",
        &format!("/api/projects/{project_id}/builder/proposals"),
        Some(json!({
            "prompt": "Update the workspace-core team so it owns the conversational builder runtime."
        })),
    )
    .await;
    assert_eq!(proposal_status, StatusCode::OK);
    assert_eq!(proposal_body["can_apply"], json!(true));

    let (apply_status, apply_body) = request_json(
        &router,
        "POST",
        &format!("/api/projects/{project_id}/builder/apply"),
        Some(json!({
            "request_text": "Update the workspace-core team so it owns the conversational builder runtime.",
            "operations": proposal_body["operations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|operation| operation["operation"].clone())
                .collect::<Vec<_>>()
        })),
    )
    .await;
    assert_eq!(apply_status, StatusCode::OK);
    assert_eq!(apply_body["success"], json!(true));

    let storage = SqliteStorage::open(&db_path).unwrap();
    let events = storage.list_audit_events().await.unwrap();
    let builder_events: Vec<_> = events
        .iter()
        .filter(|event| event.event_kind == "builder_config_mutation")
        .collect();
    assert!(
        builder_events.iter().any(|event| {
            event.summary.contains("create_team via visual_builder")
                && event.metadata["service"] == "builder_configuration"
        }),
        "visual builder mutation should be recorded by the canonical config service"
    );
    assert!(
        builder_events.iter().any(|event| {
            event
                .summary
                .contains("update_team via conversational_builder")
                && event.metadata["service"] == "builder_configuration"
        }),
        "conversational builder mutation should be recorded by the same canonical config service"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn blocked_builder_request_returns_diagnostics_and_applies_nothing() {
    let _guard = SWAGE_HOME_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let swage_home = temp.path().join("swage-home");
    let workspace = temp.path().join("workspace");
    let db_path = temp.path().join("builder-blocked.db");
    std::fs::create_dir_all(&swage_home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe { std::env::set_var("SWAGE_HOME", &swage_home) };

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new(vec![
        json!({
            "summary": "Create a new specialist agent for the project.",
            "operations": [
                {
                    "kind": "create_agent",
                    "agent_id": "delivery-editor",
                    "team_name": "workspace-core",
                    "instructions": "Owns release edits."
                }
            ]
        })
        .to_string(),
    ]));
    let router = build_router(test_state(&db_path, provider));

    let (project_status, project_body) = request_json(
        &router,
        "POST",
        "/api/projects",
        Some(json!({
            "name": "Blocked Builder Project",
            "workspace_path": workspace.to_string_lossy()
        })),
    )
    .await;
    assert_eq!(project_status, StatusCode::CREATED);
    let project_id = project_body["id"].as_str().unwrap();

    let (proposal_status, proposal_body) = request_json(
        &router,
        "POST",
        &format!("/api/projects/{project_id}/builder/proposals"),
        Some(json!({
            "prompt": "Create a delivery-editor agent for this project."
        })),
    )
    .await;
    assert_eq!(proposal_status, StatusCode::OK);
    assert_eq!(proposal_body["can_apply"], json!(false));
    let operation_diagnostics = proposal_body["operations"][0]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        operation_diagnostics.contains("Agent authoring"),
        "proposal diagnostics should explain the unsupported agent path"
    );

    let (apply_status, apply_body) = request_json(
        &router,
        "POST",
        &format!("/api/projects/{project_id}/builder/apply"),
        Some(json!({
            "request_text": "Create a delivery-editor agent for this project.",
            "operations": proposal_body["operations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|operation| operation["operation"].clone())
                .collect::<Vec<_>>()
        })),
    )
    .await;
    assert_eq!(apply_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(apply_body["success"], json!(false));
    let diagnostics = apply_body["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        diagnostics.contains("Agent authoring"),
        "apply diagnostics should stay explicit"
    );

    let (teams_status, teams_body) = request_json(
        &router,
        "GET",
        &format!("/api/projects/{project_id}/teams"),
        None,
    )
    .await;
    assert_eq!(teams_status, StatusCode::OK);
    assert_eq!(teams_body["teams"], json!([]));
}
