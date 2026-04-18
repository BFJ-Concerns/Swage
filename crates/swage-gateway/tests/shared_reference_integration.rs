use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use futures::Stream;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use swage_context::report::{AssemblyReport, ElementReport};
use swage_core::{
    CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
    ToolCompletionResponse,
};
use swage_gateway::{server::build_router, state::AppState};
use swage_provider::LlmProvider;
use swage_storage::{
    NewTurn, SqliteStorage, TurnState,
    store::{ContextSnapshotStore, ProjectStore, SessionStore, Storage, ThreadStore, TurnStore},
};

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "shared-reference-mock"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unreachable!("shared reference tests do not call the completion API")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!("shared reference tests do not call the tool completion API")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::iter(vec![StreamChunk::Done {
            finish_reason: None,
        }]))
    }
}

fn test_state(db_path: &Path) -> (AppState, Arc<SqliteStorage>) {
    let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn Storage>,
        Arc::clone(&storage) as Arc<dyn ProjectStore>,
        Arc::new(MockProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
        None,
    );
    (state, storage)
}

async fn json_response(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
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

async fn create_conversation(router: &Router) -> Uuid {
    let (status, body) = post_json(router, "/api/conversations", json!({})).await;
    assert_eq!(status, StatusCode::CREATED);
    body["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn shared_reference_objects_round_trip_visual_selection_and_team_updates() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("shared-reference.db");
    let (state, storage) = test_state(&db_path);
    let router = build_router(state);

    let session_id = create_conversation(&router).await;
    let session = storage.get_session(session_id).await.unwrap().unwrap();
    let project_id = session.project_id;
    let thread_id = storage.list_by_session(session_id).await.unwrap()[0].id;

    let turn = storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "builder".into(),
            state: TurnState::Completed,
            user_input: Some("show the context".into()),
            parent_turn_id: None,
        })
        .await
        .unwrap();

    let report = AssemblyReport {
        assembly_name: "builder".into(),
        elements: vec![ElementReport {
            name: "history".into(),
            source: "session".into(),
            token_count: 128,
            preview: Some("Recent build notes".into()),
            priority: 10,
            named_assembly: None,
        }],
        total_tokens: 128,
        budget: Some(512),
        budget_remaining: Some(384),
        compaction_applied: Vec::new(),
        warnings: vec!["Near the configured token budget".into()],
        errors: Vec::new(),
        duration: Duration::from_secs(2),
    };
    storage
        .store_snapshot(turn.id, &serde_json::to_string(&report).unwrap())
        .await
        .unwrap();

    let (team_status, team_body) = post_json(
        &router,
        &format!("/api/projects/{project_id}/teams"),
        json!({
            "name": "builders",
            "description": "Shapes the builder graph.",
            "entry_agent_id": "builder",
            "members": ["reviewer", "qa"]
        }),
    )
    .await;
    assert_eq!(team_status, StatusCode::CREATED);
    let team_id = team_body["id"].as_str().unwrap();

    let (list_status, list_body) = get_json(
        &router,
        &format!("/api/shared-objects?project_id={project_id}"),
    )
    .await;
    assert_eq!(list_status, StatusCode::OK, "body: {list_body:?}");
    let objects = list_body["objects"].as_array().unwrap();
    assert!(
        objects
            .iter()
            .any(|object| { object["kind"] == "team_topology" && object["id"] == team_id }),
        "expected team topology object in {objects:?}"
    );
    assert!(
        objects.iter().any(|object| {
            object["kind"] == "context_snapshot" && object["id"] == turn.id.to_string()
        }),
        "expected context snapshot object in {objects:?}"
    );

    let (snapshot_event_status, snapshot_event_body) = post_json(
        &router,
        &format!("/api/shared-objects/context_snapshot/{}/events", turn.id),
        json!({
            "event_kind": "selected",
            "payload": {
                "element_id": "history:0",
                "source": "builder-lens"
            }
        }),
    )
    .await;
    assert_eq!(snapshot_event_status, StatusCode::CREATED);
    assert_eq!(snapshot_event_body["event_kind"], "selected");

    let (snapshot_status, snapshot_body) = get_json(
        &router,
        &format!("/api/shared-objects/context_snapshot/{}", turn.id),
    )
    .await;
    assert_eq!(snapshot_status, StatusCode::OK);
    assert_eq!(
        snapshot_body["object"]["payload"]["assembly_name"],
        "builder"
    );
    assert_eq!(
        snapshot_body["object"]["payload"]["elements"][0]["preview"],
        "Recent build notes"
    );
    assert_eq!(snapshot_body["interactions"].as_array().unwrap().len(), 1);

    let (team_event_status, team_event_body) = post_json(
        &router,
        &format!("/api/shared-objects/team_topology/{team_id}/events"),
        json!({
            "event_kind": "selected",
            "payload": {
                "agent_id": "reviewer",
                "source": "builder-lens"
            }
        }),
    )
    .await;
    assert_eq!(team_event_status, StatusCode::CREATED);
    assert_eq!(team_event_body["payload"]["agent_id"], "reviewer");

    let (topology_update_status, topology_update_body) = put_json(
        &router,
        &format!("/api/shared-objects/team_topology/{team_id}"),
        json!({
            "entry_agent_id": "reviewer",
            "members": ["builder", "reviewer", "qa", "docs"]
        }),
    )
    .await;
    assert_eq!(topology_update_status, StatusCode::OK);
    assert_eq!(
        topology_update_body["object"]["payload"]["entry_agent_id"],
        "reviewer"
    );
    assert_eq!(
        topology_update_body["interactions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let (team_detail_status, team_detail_body) =
        get_json(&router, &format!("/api/teams/{team_id}")).await;
    assert_eq!(team_detail_status, StatusCode::OK);
    assert_eq!(team_detail_body["entry_agent_id"], "reviewer");
    assert_eq!(team_detail_body["members"].as_array().unwrap().len(), 4);
}
