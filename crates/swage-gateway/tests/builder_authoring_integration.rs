use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::Stream;
use serde_json::{Value, json};
use tower::ServiceExt;

use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{
    SqliteStorage,
    store::{ProjectStore, Storage},
};

static SWAGE_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "builder-authoring-test"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("builder authoring tests do not call the completion API")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("builder authoring tests do not call the tool completion API")
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

fn test_state(db_path: &Path) -> AppState {
    let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
    AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn ProjectStore>,
        Arc::new(MockProvider),
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
            Value::String(String::from_utf8(bytes.to_vec()).expect("response should be utf-8"))
        })
    }
}

async fn post_json(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    (response.status(), json_response(response).await)
}

async fn put_json(router: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    (response.status(), json_response(response).await)
}

async fn get_json(router: &Router, uri: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    (response.status(), json_response(response).await)
}

#[tokio::test(flavor = "multi_thread")]
async fn builder_authoring_persists_lens_team_and_extension_across_restart() {
    let _guard = SWAGE_HOME_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let swage_home = temp.path().join("swage-home");
    let workspace = temp.path().join("workspace");
    let db_path = temp.path().join("builder-authoring.db");
    std::fs::create_dir_all(&swage_home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe { std::env::set_var("SWAGE_HOME", &swage_home) };

    let state = test_state(&db_path);
    let router = build_router(state);

    let (project_status, project_body) = post_json(
        &router,
        "/api/projects",
        json!({
            "name": "Builder Project",
            "workspace_path": workspace.to_string_lossy()
        }),
    )
    .await;
    assert_eq!(project_status, StatusCode::CREATED);
    let project_id = project_body["id"].as_str().unwrap();
    let project_slug = project_body["slug"].as_str().unwrap();

    let (lens_status, lens_body) =
        put_json(&router, "/api/lens-runtime", json!({ "lens": "builder" })).await;
    assert_eq!(lens_status, StatusCode::OK);
    assert_eq!(lens_body["active_lens"], "builder");

    let (team_status, team_body) = post_json(
        &router,
        &format!("/api/projects/{project_id}/teams"),
        json!({
            "name": "workspace-core",
            "description": "Owns the builder workflow.",
            "entry_agent_id": "default",
            "agents": [
                {
                    "id": "default",
                    "display_name": "Default",
                    "description": "Primary builder agent.",
                    "instructions": "Own the builder workflow.",
                    "model": "claude-sonnet-4-20250514"
                },
                {
                    "id": "reviewer",
                    "display_name": "Reviewer",
                    "description": "Reviews builder mutations.",
                    "instructions": "Review builder changes.",
                    "model": null
                }
            ],
            "tasks": [
                {
                    "name": "review-plan",
                    "description": "Review proposed plans.",
                    "instructions": "Inspect the plan for risk.",
                    "model": null,
                    "output_schema": {
                        "type": "object",
                        "properties": {
                            "result": { "type": "string" }
                        }
                    }
                }
            ]
        }),
    )
    .await;
    assert_eq!(team_status, StatusCode::CREATED);
    let team_id = team_body["id"].as_str().unwrap();

    let (team_update_status, team_update_body) = put_json(
        &router,
        &format!("/api/teams/{team_id}"),
        json!({
            "description": "Owns the durable builder workflow.",
            "agents": [
                {
                    "id": "default",
                    "display_name": "Default",
                    "description": "Primary builder agent.",
                    "instructions": "Own the durable builder workflow.",
                    "model": "claude-sonnet-4-20250514"
                },
                {
                    "id": "reviewer",
                    "display_name": "Reviewer",
                    "description": "Reviews builder mutations.",
                    "instructions": "Review durable changes.",
                    "model": null
                },
                {
                    "id": "qa",
                    "display_name": "QA",
                    "description": "Checks builder regressions.",
                    "instructions": "Catch regressions before apply.",
                    "model": null
                }
            ],
            "tasks": [
                {
                    "name": "review-plan",
                    "description": "Review proposed plans.",
                    "instructions": "Inspect the plan for risk.",
                    "model": null,
                    "output_schema": {
                        "type": "object",
                        "properties": {
                            "result": { "type": "string" }
                        }
                    }
                },
                {
                    "name": "qa-check",
                    "description": "Check builder regressions.",
                    "instructions": "Check regression risk.",
                    "model": null,
                    "output_schema": {
                        "type": "object",
                        "properties": {
                            "status": { "type": "string" }
                        }
                    }
                }
            ]
        }),
    )
    .await;
    assert_eq!(team_update_status, StatusCode::OK);
    assert_eq!(
        team_update_body["description"],
        "Owns the durable builder workflow."
    );
    assert_eq!(team_update_body["members"].as_array().unwrap().len(), 3);
    assert_eq!(team_update_body["agents"].as_array().unwrap().len(), 3);
    assert_eq!(team_update_body["tasks"].as_array().unwrap().len(), 2);

    let (extension_status, extension_body) = post_json(
        &router,
        &format!("/api/projects/{project_id}/extensions"),
        json!({
            "name": "builder-guardrails",
            "display_name": "Builder Guardrails",
            "description": "Adds builder-focused project guidance.",
            "requested_tools": ["shell"],
            "instructions": "Prefer the builder lens and keep authoring state explicit."
        }),
    )
    .await;
    assert_eq!(extension_status, StatusCode::CREATED);
    assert_eq!(extension_body["source_kind"], "managed");

    let (extension_update_status, extension_update_body) = put_json(
        &router,
        &format!("/api/projects/{project_id}/extensions/builder-guardrails"),
        json!({
            "display_name": "Builder Guardrails",
            "description": "Adds durable builder-focused project guidance.",
            "requested_tools": ["shell", "file_read"],
            "instructions": "Prefer the builder lens, keep authoring state explicit, and audit extension changes."
        }),
    )
    .await;
    assert_eq!(extension_update_status, StatusCode::OK);
    assert_eq!(
        extension_update_body["description"],
        "Adds durable builder-focused project guidance."
    );

    let skill_path = swage_home
        .join("projects")
        .join(project_slug)
        .join("skills")
        .join("builder-guardrails")
        .join("SKILL.md");
    assert!(
        skill_path.is_file(),
        "managed extension should write SKILL.md"
    );

    let restarted_router = build_router(test_state(&db_path));

    let (restarted_lens_status, restarted_lens_body) =
        get_json(&restarted_router, "/api/lens-runtime").await;
    assert_eq!(restarted_lens_status, StatusCode::OK);
    assert_eq!(restarted_lens_body["active_lens"], "builder");

    let (restarted_team_status, restarted_team_body) =
        get_json(&restarted_router, &format!("/api/teams/{team_id}")).await;
    assert_eq!(restarted_team_status, StatusCode::OK);
    assert_eq!(
        restarted_team_body["description"],
        "Owns the durable builder workflow."
    );
    assert_eq!(restarted_team_body["members"].as_array().unwrap().len(), 3);
    assert_eq!(restarted_team_body["agents"].as_array().unwrap().len(), 3);
    assert_eq!(restarted_team_body["tasks"].as_array().unwrap().len(), 2);

    let (skills_status, skills_body) = get_json(
        &restarted_router,
        &format!("/api/skills?project_id={project_id}"),
    )
    .await;
    assert_eq!(skills_status, StatusCode::OK);
    let skills = skills_body["skills"].as_array().unwrap();
    let authored_skill = skills
        .iter()
        .find(|skill| skill["name"] == "builder-guardrails")
        .expect("managed extension should load as a project skill after restart");
    assert_eq!(
        authored_skill["description"],
        "Adds durable builder-focused project guidance."
    );
    assert_eq!(authored_skill["tools"], json!(["shell", "file_read"]));

    unsafe { std::env::remove_var("SWAGE_HOME") };
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_extensions_are_not_trusted_until_builder_approves_import() {
    let _guard = SWAGE_HOME_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let swage_home = temp.path().join("swage-home");
    let workspace = temp.path().join("workspace");
    let db_path = temp.path().join("builder-governance.db");
    std::fs::create_dir_all(&swage_home).unwrap();
    std::fs::create_dir_all(workspace.join(".swage/extensions/workspace-skill")).unwrap();
    unsafe { std::env::set_var("SWAGE_HOME", &swage_home) };

    std::fs::write(
        workspace.join(".swage/extensions/workspace-skill/SKILL.md"),
        r#"---
description: Workspace-local extension candidate.
tools:
  - shell
---

Stay local until a builder approves import.
"#,
    )
    .unwrap();

    let router = build_router(test_state(&db_path));
    let (project_status, project_body) = post_json(
        &router,
        "/api/projects",
        json!({
            "name": "Governed Builder Project",
            "workspace_path": workspace.to_string_lossy()
        }),
    )
    .await;
    assert_eq!(project_status, StatusCode::CREATED);
    let project_id = project_body["id"].as_str().unwrap();

    let (extensions_status, extensions_body) =
        get_json(&router, &format!("/api/projects/{project_id}/extensions")).await;
    assert_eq!(extensions_status, StatusCode::OK);
    let workspace_extension = extensions_body["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|extension| extension["name"] == "workspace-skill")
        .expect("workspace candidate should be listed for approval");
    assert_eq!(workspace_extension["status"], "approval_required");
    assert_eq!(workspace_extension["source_kind"], "workspace_candidate");

    let (pre_approval_skills_status, pre_approval_skills_body) =
        get_json(&router, &format!("/api/skills?project_id={project_id}")).await;
    assert_eq!(pre_approval_skills_status, StatusCode::OK);
    assert!(
        pre_approval_skills_body["skills"]
            .as_array()
            .unwrap()
            .iter()
            .all(|skill| skill["name"] != "workspace-skill"),
        "workspace candidate must not activate before approval"
    );

    let (approve_status, approve_body) = post_json(
        &router,
        &format!("/api/projects/{project_id}/extensions/workspace-skill/approve-workspace"),
        json!({}),
    )
    .await;
    assert_eq!(approve_status, StatusCode::OK);
    assert_eq!(approve_body["status"], "active");
    assert_eq!(approve_body["source_kind"], "managed");

    let (post_approval_skills_status, post_approval_skills_body) =
        get_json(&router, &format!("/api/skills?project_id={project_id}")).await;
    assert_eq!(post_approval_skills_status, StatusCode::OK);
    assert!(
        post_approval_skills_body["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["name"] == "workspace-skill"),
        "approved workspace extension should activate through the managed config tree"
    );

    unsafe { std::env::remove_var("SWAGE_HOME") };
}
