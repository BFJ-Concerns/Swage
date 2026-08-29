//! Page templates and fork-with-lineage, over the gateway's own routes.
//!
//! A template seeds a new page's modules and is recorded as its origin. The
//! fork is a real page from birth: what the fork does to its modules never
//! reaches the template, and the template is never projected into pages
//! already forked from it. Templates are space-local, so a template of
//! another space is not a template this space can fork from.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use swage_gateway::server::build_router;
use swage_gateway::state::AppState;
use swage_storage::SqliteStorage;

fn make_test_router() -> (Router, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::SpaceStore>,
        Arc::new(StubProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    (build_router(state), storage)
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
        unimplemented!("not used in page template tests")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("not used in page template tests")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        unimplemented!("not used in page template tests")
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

async fn make_space(router: &Router, slug: &str) -> String {
    let (status, space) = http_json(
        router,
        "POST",
        "/api/spaces",
        Some(json!({ "name": slug, "slug": slug })),
    )
    .await;
    assert!(status.is_success(), "space create: {space}");
    space["id"].as_str().unwrap().to_string()
}

async fn make_template(router: &Router, space_id: &str) -> String {
    let (status, template) = http_json(
        router,
        "POST",
        &format!("/api/spaces/{space_id}/page-templates"),
        Some(json!({
            "name": "Standup",
            "modules": [
                { "kind": "document", "source": "## Yesterday" },
                { "kind": "document", "source": "## Blockers" }
            ]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "template create: {template}");
    template["id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_created_from_a_template_is_seeded_and_records_its_origin() {
    let (router, _storage) = make_test_router();
    let space_id = make_space(&router, "fork-seed").await;
    let template_id = make_template(&router, &space_id).await;

    let (status, page) = http_json(
        &router,
        "POST",
        &format!("/api/spaces/{space_id}/pages"),
        Some(json!({ "title": "Monday standup", "template_id": template_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "fork: {page}");

    let sources: Vec<&str> = page["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|module| module["source"].as_str().unwrap())
        .collect();
    assert_eq!(
        sources,
        vec!["## Yesterday", "## Blockers"],
        "the create response carries the seeded modules in template order"
    );
    assert_eq!(
        page["forked_from_template_id"].as_str(),
        Some(template_id.as_str()),
        "the page records what it was forked from"
    );

    // The recorded origin survives the read path, not just the create.
    let page_id = page["id"].as_str().unwrap();
    let (status, reread) = http_json(&router, "GET", &format!("/api/pages/{page_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        reread["forked_from_template_id"].as_str(),
        Some(template_id.as_str())
    );
    assert!(reread["forked_at"].is_string(), "the fork records when");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_created_without_a_template_is_blank_and_has_no_lineage() {
    let (router, _storage) = make_test_router();
    let space_id = make_space(&router, "blank-page").await;

    let (status, page) = http_json(
        &router,
        "POST",
        &format!("/api/spaces/{space_id}/pages"),
        Some(json!({ "title": "Scratch" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "blank create: {page}");
    assert!(page["modules"].as_array().unwrap().is_empty());
    assert!(page["forked_from_template_id"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn editing_a_forked_page_leaves_the_template_it_came_from_untouched() {
    let (router, _storage) = make_test_router();
    let space_id = make_space(&router, "fork-divergence").await;
    let template_id = make_template(&router, &space_id).await;

    let (_, page) = http_json(
        &router,
        "POST",
        &format!("/api/spaces/{space_id}/pages"),
        Some(json!({ "title": "Monday standup", "template_id": template_id })),
    )
    .await;
    let page_id = page["id"].as_str().unwrap().to_string();
    let module_id = page["modules"][0]["id"].as_str().unwrap().to_string();

    let (status, _) = http_json(
        &router,
        "POST",
        &format!("/api/pages/{page_id}/modules"),
        Some(json!({ "module_id": module_id, "source": "## Yesterday: shipped it" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, template) = http_json(
        &router,
        "GET",
        &format!("/api/page-templates/{template_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sources: Vec<&str> = template["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|module| module["source"].as_str().unwrap())
        .collect();
    assert_eq!(
        sources,
        vec!["## Yesterday", "## Blockers"],
        "the template is not mutated by an edit to a page forked from it"
    );

    // And nothing flows the other way either: a page already forked keeps
    // its own modules when the template grows a new one.
    let (status, _) = http_json(
        &router,
        "POST",
        &format!("/api/spaces/{space_id}/page-templates"),
        Some(json!({
            "name": "Standup v2",
            "modules": [{ "kind": "document", "source": "## Today" }]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, reread) = http_json(&router, "GET", &format!("/api/pages/{page_id}"), None).await;
    assert_eq!(
        reread["modules"][0]["source"].as_str(),
        Some("## Yesterday: shipped it"),
        "the fork keeps its own edit"
    );
    assert_eq!(reread["modules"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_template_from_another_space_is_rejected_before_a_page_lands() {
    let (router, _storage) = make_test_router();
    let owner = make_space(&router, "template-owner").await;
    let other = make_space(&router, "template-outsider").await;
    let template_id = make_template(&router, &owner).await;

    let (status, body) = http_json(
        &router,
        "POST",
        &format!("/api/spaces/{other}/pages"),
        Some(json!({ "title": "Borrowed", "template_id": template_id })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "templates are space-local: {body}"
    );

    let (_, pages) = http_json(&router, "GET", &format!("/api/spaces/{other}/pages"), None).await;
    assert!(
        pages["pages"].as_array().unwrap().is_empty(),
        "the rejected fork left no page behind"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_template_is_rejected_before_a_page_lands() {
    let (router, _storage) = make_test_router();
    let space_id = make_space(&router, "unknown-template").await;

    let (status, body) = http_json(
        &router,
        "POST",
        &format!("/api/spaces/{space_id}/pages"),
        Some(json!({
            "title": "Nowhere",
            "template_id": "99999999-9999-4999-8999-999999999999"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let (_, pages) = http_json(
        &router,
        "GET",
        &format!("/api/spaces/{space_id}/pages"),
        None,
    )
    .await;
    assert!(pages["pages"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_template_seeding_an_unrenderable_module_kind_is_rejected() {
    let (router, _storage) = make_test_router();
    let space_id = make_space(&router, "bad-kind").await;

    let (status, body) = http_json(
        &router,
        "POST",
        &format!("/api/spaces/{space_id}/page-templates"),
        Some(json!({
            "name": "Broken",
            "modules": [{ "kind": "spreadsheet", "source": "" }]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let (_, templates) = http_json(
        &router,
        "GET",
        &format!("/api/spaces/{space_id}/page-templates"),
        None,
    )
    .await;
    assert!(
        templates["templates"].as_array().unwrap().is_empty(),
        "validation ran before the template row landed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn template_creation_and_the_fork_each_land_an_audit_event() {
    use swage_storage::store::AuditStore;

    let (router, storage) = make_test_router();
    let space_id = make_space(&router, "fork-provenance").await;
    let template_id = make_template(&router, &space_id).await;

    let (_, page) = http_json(
        &router,
        "POST",
        &format!("/api/spaces/{space_id}/pages"),
        Some(json!({ "title": "Monday standup", "template_id": template_id })),
    )
    .await;
    let page_id = page["id"].as_str().unwrap().to_string();

    let events = storage.list_audit_events().await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.entity_kind == "page_template" && event.entity_id == template_id)
            .map(|event| event.event_kind.as_str())
            .collect::<Vec<_>>(),
        vec!["page_template_created"]
    );

    let created = events
        .iter()
        .filter(|event| event.entity_kind == "page" && event.entity_id == page_id)
        .find(|event| event.event_kind == "page_created")
        .expect("the fork is recorded");
    assert_eq!(
        created.metadata["forked_from"]["template_id"].as_str(),
        Some(template_id.as_str()),
        "the trail carries which template the page came from"
    );
    assert_eq!(
        created.metadata["forked_from"]["template_name"].as_str(),
        Some("Standup")
    );
}
