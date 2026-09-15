#![allow(clippy::unwrap_used)]
//! Integration tests for GET /api/skills — skill listing with hierarchy resolution.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use swage_gateway::server::build_router;
use swage_gateway::state::AppState;
use swage_storage::store::TopicStore;
use swage_storage::{SqliteStorage, model::NewTopic};

// ---------------------------------------------------------------------------
// Stub provider — skills tests don't exercise LLM streaming
// ---------------------------------------------------------------------------

struct StubProvider;

#[async_trait::async_trait]
impl swage_provider::LlmProvider for StubProvider {
    fn model_id(&self) -> &str {
        "stub"
    }

    async fn complete(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
        unimplemented!("not used in skills endpoint tests")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("not used in skills endpoint tests")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        unimplemented!("not used in skills endpoint tests")
    }
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Build a router backed by in-memory SQLite storage.
fn make_router(config_root: &std::path::Path) -> (Router, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new_with_config_root(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>,
        Arc::new(StubProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
        config_root.to_path_buf(),
    );
    let router = build_router(state);
    (router, storage)
}

/// Send a GET request and return (status, body).
async fn get(router: &Router, uri: &str) -> (StatusCode, Value) {
    let resp = router
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
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// Create a `skills/{name}/SKILL.md` under `base_dir`.
fn create_skill(base_dir: &std::path::Path, name: &str, description: &str, tools: &[&str]) {
    let skill_dir = base_dir.join("skills").join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();

    let tools_yaml = if tools.is_empty() {
        String::new()
    } else {
        let items = tools
            .iter()
            .map(|t| format!("  - {t}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("tools:\n{items}\n")
    };

    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\ndescription: {description}\n{tools_yaml}---\n\nInstructions."),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Unknown topic_id returns 404.
///
/// No file-system setup needed — this tests only the storage lookup path.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_topic_returns_404() {
    let root = tempfile::TempDir::new().unwrap();
    let (router, _storage) = make_router(root.path());
    let unknown_id = uuid::Uuid::new_v4();

    let (status, body) = get(&router, &format!("/api/skills?topic_id={unknown_id}")).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
}

/// Known topic and a fresh config root expose only the compiled catalogue.
///
/// When application state retains a fresh directory, the handler has no
/// authored skills to discover and should return the compiled catalogue.
#[tokio::test(flavor = "multi_thread")]
async fn known_topic_fresh_config_root_returns_shipped_catalogue() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("fresh-swage-root");
    std::fs::create_dir(&root).unwrap();
    let (router, storage) = make_router(&root);

    // Create a topic in storage — it exists but has no config directory.
    let topic = storage
        .create(NewTopic {
            name: "no-config".into(),
            slug: "no-config".into(),
        })
        .await
        .unwrap();

    let (status, body) = get(&router, &format!("/api/skills?topic_id={}", topic.id)).await;

    assert_eq!(status, StatusCode::OK);
    let skills = body["skills"]
        .as_array()
        .expect("response should have a skills array");
    assert_eq!(skills.len(), 7, "fresh roots expose all shipped skills");
    let names = skills
        .iter()
        .map(|skill| skill["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "assembly",
            "commission",
            "maestro",
            "project-docs",
            "research",
            "shakedown",
            "workflow-authoring"
        ]
    );
    assert!(skills.iter().all(|skill| skill["shipped"] == true));
    assert!(skills.iter().all(|skill| skill["id"].as_str().is_some()));
}

/// Skills at global level appear with level="global".
#[tokio::test(flavor = "multi_thread")]
async fn global_skills_appear_in_listing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let (router, storage) = make_router(root);

    // Global skill: {root}/skills/formatter/SKILL.md
    create_skill(root, "formatter", "Format code according to standards", &[]);

    let topic = storage
        .create(NewTopic {
            name: "test-global".into(),
            slug: "test-global".into(),
        })
        .await
        .unwrap();

    let (status, body) = get(&router, &format!("/api/skills?topic_id={}", topic.id)).await;

    assert_eq!(status, StatusCode::OK, "response: {body}");
    let skills = body["skills"].as_array().unwrap();
    let formatter = skills
        .iter()
        .find(|s| s["name"] == "formatter")
        .expect("formatter skill should appear");
    assert_eq!(formatter["level"], "global");
    assert_eq!(
        formatter["description"],
        "Format code according to standards"
    );
    assert!(!formatter["always_active"].as_bool().unwrap());
}

/// Topic-level skills appear with level="topic".
#[tokio::test(flavor = "multi_thread")]
async fn topic_skills_appear_in_listing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let (router, storage) = make_router(root);

    let topic = storage
        .create(NewTopic {
            name: "my-proj".into(),
            slug: "my-proj".into(),
        })
        .await
        .unwrap();

    // Topic skill: {root}/topics/my-proj/skills/reviewer/SKILL.md
    let topic_dir = root.join("topics").join("my-proj");
    create_skill(
        &topic_dir,
        "reviewer",
        "Structured code review",
        &["read_file"],
    );

    let (status, body) = get(&router, &format!("/api/skills?topic_id={}", topic.id)).await;

    assert_eq!(status, StatusCode::OK, "response: {body}");
    let skills = body["skills"].as_array().unwrap();
    let reviewer = skills
        .iter()
        .find(|s| s["name"] == "reviewer")
        .expect("reviewer skill should appear");
    assert_eq!(reviewer["level"], "topic");
    assert!(!reviewer["always_active"].as_bool().unwrap());
    let tools = reviewer["tools"].as_array().unwrap();
    assert!(
        tools.contains(&serde_json::json!("read_file")),
        "tools should include read_file"
    );
}

/// Agent-level skills have always_active=true.
#[tokio::test(flavor = "multi_thread")]
async fn agent_skills_have_always_active_true() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let (router, storage) = make_router(root);

    let topic = storage
        .create(NewTopic {
            name: "agent-proj".into(),
            slug: "agent-proj".into(),
        })
        .await
        .unwrap();

    // Agent skill: {root}/topics/agent-proj/agents/gateway/skills/persona/SKILL.md
    let agent_dir = root
        .join("topics")
        .join("agent-proj")
        .join("agents")
        .join("gateway");
    create_skill(&agent_dir, "persona", "Core personality and behaviour", &[]);

    let (status, body) = get(
        &router,
        &format!("/api/skills?topic_id={}&agent_id=gateway", topic.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "response: {body}");
    let skills = body["skills"].as_array().unwrap();
    let persona = skills
        .iter()
        .find(|s| s["name"] == "persona")
        .expect("persona skill should appear");
    assert_eq!(persona["level"], "agent");
    assert!(
        persona["always_active"].as_bool().unwrap(),
        "agent-level skill should have always_active=true"
    );
}

/// The reserved workflow identity is never an authored agent scope, including
/// on the operator-facing catalogue endpoint.
#[tokio::test(flavor = "multi_thread")]
async fn reserved_workflow_agent_skills_are_not_listed() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let (router, storage) = make_router(root);
    let topic = storage
        .create(NewTopic {
            name: "reserved-agent".into(),
            slug: "reserved-agent".into(),
        })
        .await
        .unwrap();
    let reserved_agent = root
        .join("topics/reserved-agent/agents")
        .join(swage_config::RESERVED_WORKFLOW_AGENT_ID);
    let ordinary_agent = root.join("topics/reserved-agent/agents/reviewer");
    create_skill(&reserved_agent, "injected", "Must not be visible", &[]);
    create_skill(&ordinary_agent, "reviewer", "Must remain visible", &[]);

    let (reserved_status, reserved_body) = get(
        &router,
        &format!(
            "/api/skills?topic_id={}&agent_id={}",
            topic.id,
            swage_config::RESERVED_WORKFLOW_AGENT_ID
        ),
    )
    .await;
    let (ordinary_status, ordinary_body) = get(
        &router,
        &format!("/api/skills?topic_id={}&agent_id=reviewer", topic.id),
    )
    .await;

    assert_eq!(reserved_status, StatusCode::OK, "response: {reserved_body}");
    assert!(
        reserved_body["skills"]
            .as_array()
            .unwrap()
            .iter()
            .all(|skill| skill["name"] != "injected"),
        "the reserved directory must not contribute endpoint catalogue entries"
    );
    assert_eq!(ordinary_status, StatusCode::OK, "response: {ordinary_body}");
    assert!(
        ordinary_body["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["name"] == "reviewer" && skill["always_active"] == true),
        "a normally named agent directory must remain available and always active"
    );
}

/// Same-name skill at topic level shadows global.
///
/// Only the topic version should appear; the listing must not contain duplicates.
#[tokio::test(flavor = "multi_thread")]
async fn topic_skill_shadows_global() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let (router, storage) = make_router(root);

    let topic = storage
        .create(NewTopic {
            name: "override-proj".into(),
            slug: "override-proj".into(),
        })
        .await
        .unwrap();

    // Global skill named "code-review".
    create_skill(root, "code-review", "Global code review", &["read_file"]);

    // Topic skill with the same name — should shadow the global.
    let topic_dir = root.join("topics").join("override-proj");
    create_skill(
        &topic_dir,
        "code-review",
        "Topic code review",
        &["read_file", "search"],
    );

    let (status, body) = get(&router, &format!("/api/skills?topic_id={}", topic.id)).await;

    assert_eq!(status, StatusCode::OK, "response: {body}");
    let skills = body["skills"].as_array().unwrap();

    let code_reviews: Vec<&Value> = skills
        .iter()
        .filter(|s| s["name"] == "code-review")
        .collect();
    assert_eq!(
        code_reviews.len(),
        1,
        "same-name skill should appear once after override"
    );
    assert_eq!(
        code_reviews[0]["level"], "topic",
        "topic should shadow global"
    );
    assert_eq!(code_reviews[0]["description"], "Topic code review");
}

#[tokio::test(flavor = "multi_thread")]
async fn authored_skill_shadows_same_named_shipped_entry() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let (router, storage) = make_router(root);
    let topic = storage
        .create(NewTopic {
            name: "shadow-shipped".into(),
            slug: "shadow-shipped".into(),
        })
        .await
        .unwrap();
    create_skill(root, "commission", "Operator commission", &[]);

    let (status, body) = get(&router, &format!("/api/skills?topic_id={}", topic.id)).await;

    assert_eq!(status, StatusCode::OK, "response: {body}");
    let matching = body["skills"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|skill| skill["name"] == "commission")
        .collect::<Vec<_>>();
    assert_eq!(matching.len(), 1);
    assert_eq!(matching[0]["description"], "Operator commission");
    assert_eq!(matching[0]["shipped"], false);
    assert!(matching[0]["id"].is_null());
}

/// Sorting: agent skills appear first, then topic, then global; within each level alphabetically.
#[tokio::test(flavor = "multi_thread")]
async fn skills_are_sorted_by_level_then_alphabetically() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let (router, storage) = make_router(root);

    let topic = storage
        .create(NewTopic {
            name: "sorted-proj".into(),
            slug: "sorted-proj".into(),
        })
        .await
        .unwrap();

    // Global skills.
    create_skill(root, "zebra", "Global zebra skill", &[]);
    create_skill(root, "alpha", "Global alpha skill", &[]);

    // Topic skills.
    let topic_dir = root.join("topics").join("sorted-proj");
    create_skill(&topic_dir, "middle", "Topic middle skill", &[]);

    // Agent skills.
    let agent_dir = root
        .join("topics")
        .join("sorted-proj")
        .join("agents")
        .join("gw");
    create_skill(&agent_dir, "persona", "Agent persona skill", &[]);

    let (status, body) = get(
        &router,
        &format!("/api/skills?topic_id={}&agent_id=gw", topic.id),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "response: {body}");
    let skills = body["skills"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|skill| skill["shipped"] == false)
        .collect::<Vec<_>>();
    assert_eq!(skills.len(), 4, "all 4 authored skills should appear");

    // Agent comes first.
    assert_eq!(skills[0]["level"], "agent", "agent skill should sort first");
    assert_eq!(skills[0]["name"], "persona");

    // Then topic.
    assert_eq!(
        skills[1]["level"], "topic",
        "topic skill should sort second"
    );
    assert_eq!(skills[1]["name"], "middle");

    // Then global, alphabetical.
    assert_eq!(skills[2]["level"], "global", "global skills sort last");
    assert_eq!(skills[2]["name"], "alpha");
    assert_eq!(skills[3]["level"], "global");
    assert_eq!(skills[3]["name"], "zebra");
}
