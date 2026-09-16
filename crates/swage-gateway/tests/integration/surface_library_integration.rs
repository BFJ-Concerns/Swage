#![allow(clippy::unwrap_used)]

//! The surface library over HTTP: a session's surface is saved with a
//! description and tags, added to other sessions with content only within
//! its origin topic, designated a topic default that opens in that topic's
//! new sessions, and catalogued without its content. The seeded rows are
//! shown to persist on a database file, as a restarted gateway would find
//! them.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use swage_gateway::server::build_router;
use swage_gateway::state::AppState;
use swage_storage::SqliteStorage;
use swage_storage::store::SurfaceStore;

fn router_over(storage: Arc<SqliteStorage>) -> Router {
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>,
        Arc::new(StubProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    build_router(state)
}

fn make_test_router() -> (Router, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    (router_over(Arc::clone(&storage)), storage)
}

/// A provider that panics if called — these tests exercise HTTP and storage.
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
        unimplemented!("not used in surface library tests")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("not used in surface library tests")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        unimplemented!("not used in surface library tests")
    }
}

async fn http_json(
    router: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let has_body = body.is_some();
    let req_body = match body {
        Some(json) => Body::from(serde_json::to_string(&json).unwrap()),
        None => Body::empty(),
    };
    let mut builder = Request::builder().method(method).uri(uri);
    if has_body {
        builder = builder.header("content-type", "application/json");
    }
    let resp = router
        .clone()
        .oneshot(builder.body(req_body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn make_topic(router: &Router, name: &str) -> String {
    let (status, topic) =
        http_json(router, "POST", "/api/topics", Some(json!({ "name": name }))).await;
    assert!(status.is_success(), "topic create: {topic}");
    topic["id"].as_str().unwrap().to_string()
}

async fn make_session(router: &Router, topic_id: Option<&str>) -> String {
    let uri = match topic_id {
        Some(topic_id) => format!("/api/topics/{topic_id}/sessions"),
        None => "/api/conversations".to_string(),
    };
    let (status, session) = http_json(router, "POST", &uri, None).await;
    assert_eq!(status, StatusCode::CREATED, "session create: {session}");
    session["id"].as_str().unwrap().to_string()
}

/// A surface with two populated modules, the shape every save here copies.
async fn make_populated_surface(router: &Router, session_id: &str) -> String {
    let (status, surface) = http_json(
        router,
        "POST",
        &format!("/api/sessions/{session_id}/surfaces"),
        Some(json!({ "title": "Standup" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "surface create: {surface}");
    let surface_id = surface["id"].as_str().unwrap().to_string();
    for source in ["## Yesterday: shipped the gateway", "## Blockers: none"] {
        let (status, module) = http_json(
            router,
            "POST",
            &format!("/api/surfaces/{surface_id}/modules"),
            Some(json!({ "source": source })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "module write: {module}");
    }
    surface_id
}

async fn save(router: &Router, surface_id: &str, tags: &[&str]) -> String {
    let (status, entry) = http_json(
        router,
        "POST",
        &format!("/api/surfaces/{surface_id}/save"),
        Some(json!({
            "name": "Standup",
            "description": "Daily standup notes — use at the start of a working day.",
            "tags": tags,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save: {entry}");
    entry["id"].as_str().unwrap().to_string()
}

async fn sources_of(router: &Router, surface_id: &str) -> Vec<String> {
    let (status, surface) =
        http_json(router, "GET", &format!("/api/surfaces/{surface_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    surface["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|module| module["source"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_surface_is_the_sessions_own_and_another_session_in_the_topic_does_not_see_it() {
    let (router, _storage) = make_test_router();
    let topic_id = make_topic(&router, "Ownership").await;
    let owner = make_session(&router, Some(&topic_id)).await;
    let neighbour = make_session(&router, Some(&topic_id)).await;
    let surface_id = make_populated_surface(&router, &owner).await;

    let (status, mine) = http_json(
        &router,
        "GET",
        &format!("/api/sessions/{owner}/surfaces"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mine["surfaces"].as_array().unwrap().len(), 1);
    assert_eq!(mine["surfaces"][0]["id"], surface_id);
    let (_, theirs) = http_json(
        &router,
        "GET",
        &format!("/api/sessions/{neighbour}/surfaces"),
        None,
    )
    .await;
    assert_eq!(
        theirs["surfaces"],
        json!([]),
        "a surface is listed for its session alone"
    );
    let (_, read) = http_json(&router, "GET", &format!("/api/surfaces/{surface_id}"), None).await;
    assert_eq!(read["session_id"], owner);
    assert!(read["library_entry_id"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_surface_keeps_its_content_within_its_topic_and_its_shape_beyond() {
    let (router, _storage) = make_test_router();
    let topic_id = make_topic(&router, "Origin").await;
    let other_topic = make_topic(&router, "Elsewhere").await;
    let origin = make_session(&router, Some(&topic_id)).await;
    let surface_id = make_populated_surface(&router, &origin).await;
    let entry_id = save(&router, &surface_id, &["standup"]).await;

    // Saving opened nothing: the origin session still has one surface.
    let (_, listed) = http_json(
        &router,
        "GET",
        &format!("/api/sessions/{origin}/surfaces"),
        None,
    )
    .await;
    assert_eq!(listed["surfaces"].as_array().unwrap().len(), 1);

    let add = |session: String| {
        let router = router.clone();
        let entry_id = entry_id.clone();
        async move {
            let (status, surface) = http_json(
                &router,
                "POST",
                &format!("/api/sessions/{session}/surfaces"),
                Some(json!({ "title": "Monday", "library_entry_id": entry_id })),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "add: {surface}");
            assert_eq!(surface["library_entry_id"], entry_id);
            assert!(surface["seeded_at"].is_string());
            surface["id"].as_str().unwrap().to_string()
        }
    };
    let in_topic = add(make_session(&router, Some(&topic_id)).await).await;
    let in_other = add(make_session(&router, Some(&other_topic)).await).await;
    let in_general = add(make_session(&router, None).await).await;

    assert_eq!(
        sources_of(&router, &in_topic).await,
        vec!["## Yesterday: shipped the gateway", "## Blockers: none"],
        "within the origin topic the populated content is retained"
    );
    assert_eq!(
        sources_of(&router, &in_other).await,
        vec!["", ""],
        "another topic receives the shape and supplies its own data"
    );
    assert_eq!(sources_of(&router, &in_general).await, vec!["", ""]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_catalogue_lists_descriptions_and_tags_and_narrows_by_tag_without_content() {
    let (router, _storage) = make_test_router();
    let topic_id = make_topic(&router, "Catalogue").await;
    let session = make_session(&router, Some(&topic_id)).await;
    let standup = make_populated_surface(&router, &session).await;
    save(&router, &standup, &["standup", "daily"]).await;
    let (_, retro) = http_json(
        &router,
        "POST",
        &format!("/api/sessions/{session}/surfaces"),
        Some(json!({ "title": "Retro" })),
    )
    .await;
    let retro_id = retro["id"].as_str().unwrap();
    let (status, _) = http_json(
        &router,
        "POST",
        &format!("/api/surfaces/{retro_id}/save"),
        Some(json!({
            "name": "Retro",
            "description": "A retrospective board — use at the end of a sprint.",
            "tags": ["sprint"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, whole) = http_json(&router, "GET", "/api/surface-library", None).await;
    assert_eq!(status, StatusCode::OK);
    let entries = whole["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let text = whole.to_string();
    assert!(text.contains("use at the start of a working day"));
    assert!(text.contains("use at the end of a sprint"));
    assert!(
        !text.contains("shipped the gateway"),
        "the catalogue carries no module content"
    );
    assert_eq!(
        entries
            .iter()
            .filter(|e| e["origin_topic_id"] == topic_id)
            .count(),
        2
    );

    let (_, narrowed) = http_json(&router, "GET", "/api/surface-library?tag=sprint", None).await;
    assert_eq!(narrowed["entries"].as_array().unwrap().len(), 1);
    assert_eq!(narrowed["entries"][0]["name"], "Retro");

    let (status, _) = http_json(
        &router,
        "POST",
        &format!("/api/surfaces/{retro_id}/save"),
        Some(json!({ "name": "Nameless", "description": "   " })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a save needs a description"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_topic_default_opens_in_its_new_sessions_and_persists_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("swage.db");
    let (router, storage) = {
        let storage = Arc::new(SqliteStorage::open(&db_path).unwrap());
        (router_over(Arc::clone(&storage)), storage)
    };
    let topic_id = make_topic(&router, "Defaults").await;
    let other_topic = make_topic(&router, "Other").await;
    let session = make_session(&router, Some(&topic_id)).await;
    let surface_id = make_populated_surface(&router, &session).await;
    let entry_id = save(&router, &surface_id, &[]).await;

    let (status, defaults) = http_json(
        &router,
        "POST",
        &format!("/api/topics/{topic_id}/default-surfaces"),
        Some(json!({ "library_entry_id": entry_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "designate: {defaults}");
    assert_eq!(defaults["entries"][0]["id"], entry_id);

    let fresh = make_session(&router, Some(&topic_id)).await;
    let elsewhere = make_session(&router, Some(&other_topic)).await;
    let (_, opened) = http_json(
        &router,
        "GET",
        &format!("/api/sessions/{fresh}/surfaces"),
        None,
    )
    .await;
    assert_eq!(
        opened["surfaces"].as_array().unwrap().len(),
        1,
        "the new session opens with the default"
    );
    assert_eq!(opened["surfaces"][0]["title"], "Standup");
    let opened_id = opened["surfaces"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(
        sources_of(&router, &opened_id).await,
        vec!["## Yesterday: shipped the gateway", "## Blockers: none"]
    );
    let (_, none) = http_json(
        &router,
        "GET",
        &format!("/api/sessions/{elsewhere}/surfaces"),
        None,
    )
    .await;
    assert_eq!(
        none["surfaces"],
        json!([]),
        "a session in another topic did not receive it"
    );

    // The rows are durable: a connection opened after the writer is gone
    // finds them, as a restarted gateway would.
    drop(router);
    drop(storage);
    let reopened = SqliteStorage::open(&db_path).unwrap();
    let modules = reopened
        .list_surface_modules(opened_id.parse().unwrap())
        .await
        .unwrap();
    assert_eq!(modules.len(), 2);
    assert_eq!(
        reopened
            .list_topic_default_entries(topic_id.parse().unwrap())
            .await
            .unwrap()
            .len(),
        1
    );

    let (router, _) = {
        let storage = Arc::new(SqliteStorage::open(&db_path).unwrap());
        (router_over(Arc::clone(&storage)), storage)
    };
    let (status, _) = http_json(
        &router,
        "DELETE",
        &format!("/api/topics/{topic_id}/default-surfaces/{entry_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let later = make_session(&router, Some(&topic_id)).await;
    let (_, none) = http_json(
        &router,
        "GET",
        &format!("/api/sessions/{later}/surfaces"),
        None,
    )
    .await;
    assert_eq!(
        none["surfaces"],
        json!([]),
        "a withdrawn default opens nowhere"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_library_entry_leaves_the_surfaces_added_from_it() {
    let (router, _storage) = make_test_router();
    let topic_id = make_topic(&router, "Removal").await;
    let session = make_session(&router, Some(&topic_id)).await;
    let surface_id = make_populated_surface(&router, &session).await;
    let entry_id = save(&router, &surface_id, &[]).await;
    let (_, added) = http_json(
        &router,
        "POST",
        &format!("/api/sessions/{session}/surfaces"),
        Some(json!({ "title": "Copy", "library_entry_id": entry_id })),
    )
    .await;
    let added_id = added["id"].as_str().unwrap();

    let (status, _) = http_json(
        &router,
        "DELETE",
        &format!("/api/surface-library/{entry_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = http_json(
        &router,
        "GET",
        &format!("/api/surface-library/{entry_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, survivor) = http_json(&router, "GET", &format!("/api/surfaces/{added_id}"), None).await;
    assert_eq!(survivor["modules"].as_array().unwrap().len(), 2);
    assert!(
        survivor["library_entry_id"].is_null(),
        "the origin link clears; the surface stays"
    );
}
