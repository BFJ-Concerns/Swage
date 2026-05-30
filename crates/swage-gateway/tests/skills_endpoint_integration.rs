#![allow(
    clippy::await_holding_lock,
    clippy::unwrap_used,
    clippy::disallowed_methods
)]
// These integration tests serialise process-env mutations with a std mutex.
// The guard intentionally spans awaits because the gateway reads env during requests.
//! Integration tests for GET /api/skills — skill listing with hierarchy resolution.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use swage_gateway::server::build_router;
use swage_gateway::state::AppState;
use swage_storage::store::BenchStore;
use swage_storage::{SqliteStorage, model::NewBench};

// Setting SWAGE_HOME is process-level; concurrent tests that mutate it will
// race. Serialise all env-var-dependent tests with this mutex.
static SWAGE_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
fn make_router() -> (Router, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(StubProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
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

/// Unknown bench_id returns 404.
///
/// No file-system setup needed — this tests only the storage lookup path.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_bench_returns_404() {
    let (router, _storage) = make_router();
    let unknown_id = uuid::Uuid::new_v4();

    let (status, body) = get(&router, &format!("/api/skills?bench_id={unknown_id}")).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");
}

/// Known bench but fresh config root → returns 200 with empty skills list.
///
/// When `SWAGE_HOME` points at a fresh directory, the handler has no skills
/// to discover and should degrade gracefully rather than returning an error.
#[tokio::test(flavor = "multi_thread")]
async fn known_bench_fresh_config_root_returns_empty() {
    let (router, storage) = make_router();

    // Create a bench in storage — it exists but has no config directory.
    let bench = storage
        .create(NewBench {
            name: "no-config".into(),
            slug: "no-config".into(),
        })
        .await
        .unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().join("fresh-swage-root");
    // Serialise env-var mutations: SWAGE_HOME is process-level and would race
    // if tests ran concurrently without coordination.
    let _guard = SWAGE_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Safety: test-only env mutation; scoped by this test's execution.
    unsafe { std::env::set_var("SWAGE_HOME", &root) };
    let (status, body) = get(&router, &format!("/api/skills?bench_id={}", bench.id)).await;
    unsafe { std::env::remove_var("SWAGE_HOME") };

    assert_eq!(status, StatusCode::OK);
    let skills = body["skills"]
        .as_array()
        .expect("response should have a skills array");
    assert!(
        skills.is_empty(),
        "fresh config root should produce an empty skill list"
    );
    assert!(root.is_dir(), "skill discovery should auto-create the root");
}

/// Skills at global level appear with level="global".
#[tokio::test(flavor = "multi_thread")]
async fn global_skills_appear_in_listing() {
    let (router, storage) = make_router();

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();

    // Global skill: {root}/skills/formatter/SKILL.md
    create_skill(root, "formatter", "Format code according to standards", &[]);

    let bench = storage
        .create(NewBench {
            name: "test-global".into(),
            slug: "test-global".into(),
        })
        .await
        .unwrap();

    let _guard = SWAGE_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("SWAGE_HOME", root) };
    let (status, body) = get(&router, &format!("/api/skills?bench_id={}", bench.id)).await;
    unsafe { std::env::remove_var("SWAGE_HOME") };

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

/// Bench-level skills appear with level="bench".
#[tokio::test(flavor = "multi_thread")]
async fn bench_skills_appear_in_listing() {
    let (router, storage) = make_router();

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();

    let bench = storage
        .create(NewBench {
            name: "my-proj".into(),
            slug: "my-proj".into(),
        })
        .await
        .unwrap();

    // Bench skill: {root}/benches/my-proj/skills/reviewer/SKILL.md
    let bench_dir = root.join("benches").join("my-proj");
    create_skill(
        &bench_dir,
        "reviewer",
        "Structured code review",
        &["read_file"],
    );

    let _guard = SWAGE_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("SWAGE_HOME", root) };
    let (status, body) = get(&router, &format!("/api/skills?bench_id={}", bench.id)).await;
    unsafe { std::env::remove_var("SWAGE_HOME") };

    assert_eq!(status, StatusCode::OK, "response: {body}");
    let skills = body["skills"].as_array().unwrap();
    let reviewer = skills
        .iter()
        .find(|s| s["name"] == "reviewer")
        .expect("reviewer skill should appear");
    assert_eq!(reviewer["level"], "bench");
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
    let (router, storage) = make_router();

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();

    let bench = storage
        .create(NewBench {
            name: "agent-proj".into(),
            slug: "agent-proj".into(),
        })
        .await
        .unwrap();

    // Agent skill: {root}/benches/agent-proj/agents/gateway/skills/persona/SKILL.md
    let agent_dir = root
        .join("benches")
        .join("agent-proj")
        .join("agents")
        .join("gateway");
    create_skill(&agent_dir, "persona", "Core personality and behaviour", &[]);

    let _guard = SWAGE_HOME_LOCK.lock().unwrap();
    unsafe { std::env::set_var("SWAGE_HOME", root) };
    let (status, body) = get(
        &router,
        &format!("/api/skills?bench_id={}&agent_id=gateway", bench.id),
    )
    .await;
    unsafe { std::env::remove_var("SWAGE_HOME") };

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

/// Same-name skill at bench level shadows global.
///
/// Only the bench version should appear; the listing must not contain duplicates.
#[tokio::test(flavor = "multi_thread")]
async fn bench_skill_shadows_global() {
    let (router, storage) = make_router();

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();

    let bench = storage
        .create(NewBench {
            name: "override-proj".into(),
            slug: "override-proj".into(),
        })
        .await
        .unwrap();

    // Global skill named "code-review".
    create_skill(root, "code-review", "Global code review", &["read_file"]);

    // Bench skill with the same name — should shadow the global.
    let bench_dir = root.join("benches").join("override-proj");
    create_skill(
        &bench_dir,
        "code-review",
        "Bench code review",
        &["read_file", "search"],
    );

    let _guard = SWAGE_HOME_LOCK.lock().unwrap();
    unsafe { std::env::set_var("SWAGE_HOME", root) };
    let (status, body) = get(&router, &format!("/api/skills?bench_id={}", bench.id)).await;
    unsafe { std::env::remove_var("SWAGE_HOME") };

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
        code_reviews[0]["level"], "bench",
        "bench should shadow global"
    );
    assert_eq!(code_reviews[0]["description"], "Bench code review");
}

/// Sorting: agent skills appear first, then bench, then global; within each level alphabetically.
#[tokio::test(flavor = "multi_thread")]
async fn skills_are_sorted_by_level_then_alphabetically() {
    let (router, storage) = make_router();

    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();

    let bench = storage
        .create(NewBench {
            name: "sorted-proj".into(),
            slug: "sorted-proj".into(),
        })
        .await
        .unwrap();

    // Global skills.
    create_skill(root, "zebra", "Global zebra skill", &[]);
    create_skill(root, "alpha", "Global alpha skill", &[]);

    // Bench skills.
    let bench_dir = root.join("benches").join("sorted-proj");
    create_skill(&bench_dir, "middle", "Bench middle skill", &[]);

    // Agent skills.
    let agent_dir = root
        .join("benches")
        .join("sorted-proj")
        .join("agents")
        .join("gw");
    create_skill(&agent_dir, "persona", "Agent persona skill", &[]);

    let _guard = SWAGE_HOME_LOCK.lock().unwrap();
    unsafe { std::env::set_var("SWAGE_HOME", root) };
    let (status, body) = get(
        &router,
        &format!("/api/skills?bench_id={}&agent_id=gw", bench.id),
    )
    .await;
    unsafe { std::env::remove_var("SWAGE_HOME") };

    assert_eq!(status, StatusCode::OK, "response: {body}");
    let skills = body["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 4, "all 4 unique skills should appear");

    // Agent comes first.
    assert_eq!(skills[0]["level"], "agent", "agent skill should sort first");
    assert_eq!(skills[0]["name"], "persona");

    // Then bench.
    assert_eq!(
        skills[1]["level"], "bench",
        "bench skill should sort second"
    );
    assert_eq!(skills[1]["name"], "middle");

    // Then global, alphabetical.
    assert_eq!(skills[2]["level"], "global", "global skills sort last");
    assert_eq!(skills[2]["name"], "alpha");
    assert_eq!(skills[3]["level"], "global");
    assert_eq!(skills[3]["name"], "zebra");
}
