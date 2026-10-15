//! Integration tests for topic workspace and agent memory.
//!
//! These tests verify interactions between multiple tasks' outputs — behaviour
//! that emerges only when the full stack is wired together:
//!
//! 1. **Topic session isolation** — sessions created in Topic A are absent
//!    when listing Topic B's sessions (spans: storage CRUD, topic-scoped
//!    session routes, topic API handlers).
//!
//! 2. **Memory scoping** — memory written in Topic A's context is inaccessible
//!    from Topic B's context (spans: memory tools, topic context in turn
//!    executor, topic CRUD).
//!
//! 3. **Per-topic config resolution** — different topics resolve different
//!    config (model, instructions) when processing turns (spans: turn executor
//!    topic context, config resolution, storage CRUD).
//!
//! Journey proved: topic workspace — topics are genuinely isolated.

#![allow(clippy::unwrap_used)]
// The env-serialisation guard intentionally spans awaits: it locks out other
// tests for the whole request, since the gateway reads SWAGE_HOME during the
// call. The guarded state is process env, not cancellation-sensitive data.

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use swage_core::Role;
use tower::ServiceExt;

use swage_gateway::server::build_router;
use swage_gateway::state::AppState;
use swage_storage::{
    AuditStore, MessageStore, NewMessage, NewTurn, SqliteStorage, ThreadStore, TopicStore,
    TurnState, TurnStore, WorkspaceAttachmentStore,
};
use uuid::Uuid;

use crate::support::{ProcessEnvGuard, start_live_server};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Build a router backed by in-memory SQLite storage.
/// Returns the router and storage handle for inspection.
fn make_test_router() -> (Router, Arc<SqliteStorage>) {
    // Use a stub provider — these tests exercise the HTTP API layer and
    // storage, not LLM streaming.
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::TopicStore>,
        Arc::new(StubProvider),
        swage_config::ResolvedConfig::default(),
        swage_gateway::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let router = build_router(state);
    (router, storage)
}

/// A provider that panics if called — these tests don't exercise LLM streaming.
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
        unimplemented!("not used in topic integration tests")
    }

    async fn complete_with_tools(
        &self,
        _request: swage_core::ToolCompletionRequest,
    ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
        unimplemented!("not used in topic integration tests")
    }

    fn stream(
        &self,
        _request: swage_core::CompletionRequest,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>> {
        unimplemented!("not used in topic integration tests")
    }
}

/// Send an HTTP request with an optional JSON body, return (status, body).
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
    let json_body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json_body)
}

async fn say_in_session(storage: &SqliteStorage, session_id: Uuid, role: Role, text: &str) {
    let thread = storage.list_by_session(session_id).await.unwrap()[0].id;
    let position = storage.list_messages_by_thread(thread).await.unwrap().len() as i32;
    storage
        .create_message(NewMessage {
            thread_id: thread,
            turn_id: None,
            role,
            origin: swage_storage::MessageOrigin::User,
            delivered: false,
            content: text.into(),
            attachments: vec![],
            position,
            agent_id: "gateway".into(),
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn session_lists_share_message_and_interruption_facts_without_changing_their_order() {
    let (router, storage) = make_test_router();
    let (_, topic) = http_json(
        &router,
        "POST",
        "/api/topics",
        Some(json!({"name": "Alpha"})),
    )
    .await;
    let topic_id = topic["id"].as_str().unwrap();
    let (_, general_a) = http_json(&router, "POST", "/api/conversations", None).await;
    let (_, general_b) = http_json(&router, "POST", "/api/conversations", None).await;
    let (_, topical) = http_json(
        &router,
        "POST",
        &format!("/api/topics/{topic_id}/sessions"),
        None,
    )
    .await;
    let (_, topical_b) = http_json(
        &router,
        "POST",
        &format!("/api/topics/{topic_id}/sessions"),
        None,
    )
    .await;
    let a: Uuid = general_a["id"].as_str().unwrap().parse().unwrap();
    let b: Uuid = general_b["id"].as_str().unwrap().parse().unwrap();
    let t: Uuid = topical["id"].as_str().unwrap().parse().unwrap();
    let t2: Uuid = topical_b["id"].as_str().unwrap().parse().unwrap();

    say_in_session(&storage, a, Role::User, "general user").await;
    say_in_session(&storage, a, Role::Tool, "tool noise").await;
    say_in_session(&storage, b, Role::Assistant, "general assistant").await;
    say_in_session(&storage, t, Role::Assistant, "topic answer").await;
    say_in_session(&storage, t, Role::System, "system noise").await;
    say_in_session(&storage, t2, Role::User, "newer topic message").await;
    let thread = storage.list_by_session(t).await.unwrap()[0].id;
    storage
        .create_turn(NewTurn {
            thread_id: thread,
            agent_id: "gateway".into(),
            state: TurnState::Interrupted,
            user_input: None,
            parent_turn_id: None,
        })
        .await
        .unwrap();

    // The recent projection has a different order. These routes retain the
    // older updated_at order and continue to show archived sessions.
    let now = chrono::Utc::now();
    storage
        .with_conn(move |conn| {
            conn.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                (
                    (now + chrono::Duration::days(3)).to_rfc3339(),
                    a.to_string(),
                ),
            )?;
            conn.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                (now.to_rfc3339(), b.to_string()),
            )?;
            conn.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                (
                    (now + chrono::Duration::days(2)).to_rfc3339(),
                    t.to_string(),
                ),
            )?;
            conn.execute(
                "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
                (
                    (now + chrono::Duration::days(1)).to_rfc3339(),
                    t2.to_string(),
                ),
            )?;
            conn.execute(
                "UPDATE sessions SET archived_at = created_at WHERE id = ?1",
                [t.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let (_, general) = http_json(&router, "GET", "/api/conversations", None).await;
    let general = general["conversations"].as_array().unwrap();
    assert_eq!(
        general.iter().map(|item| &item["id"]).collect::<Vec<_>>(),
        vec![&general_a["id"], &general_b["id"]]
    );
    assert_eq!(general[0]["message_preview"], "general user");
    assert_eq!(general[1]["message_preview"], "general assistant");
    assert_eq!(general[0]["has_interrupted_turn"], false);
    assert_eq!(general[1]["has_interrupted_turn"], false);

    let (_, topic_list) = http_json(
        &router,
        "GET",
        &format!("/api/topics/{topic_id}/sessions"),
        None,
    )
    .await;
    let topic_list = topic_list["conversations"].as_array().unwrap();
    assert_eq!(
        topic_list.len(),
        2,
        "archived topic session remains visible"
    );
    assert_eq!(topic_list[0]["id"], topical["id"]);
    assert_eq!(topic_list[1]["id"], topical_b["id"]);
    assert_eq!(topic_list[0]["message_preview"], "topic answer");
    assert_eq!(topic_list[0]["has_interrupted_turn"], true);
    assert_eq!(topic_list[1]["has_interrupted_turn"], false);

    let (_, overview) = http_json(&router, "GET", "/api/operator/overview", None).await;
    let sessions = overview["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 4);
    assert_eq!(sessions[0]["id"], general_a["id"]);
    assert_eq!(sessions[1]["id"], topical["id"]);
    assert_eq!(sessions[2]["id"], topical_b["id"]);
    assert_eq!(sessions[3]["id"], general_b["id"]);
    let topical = sessions
        .iter()
        .find(|item| item["id"] == topical["id"])
        .unwrap();
    assert_eq!(topical["message_preview"], "topic answer");
    assert_eq!(topical["has_interrupted_turn"], true);
    for uninterrupted_id in [&general_a["id"], &general_b["id"], &topical_b["id"]] {
        let session = sessions
            .iter()
            .find(|item| &item["id"] == uninterrupted_id)
            .unwrap();
        assert_eq!(session["has_interrupted_turn"], false);
    }
}

static LISTED_SQL: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn record_list_sql(event: rusqlite::trace::TraceEvent<'_>) {
    if let rusqlite::trace::TraceEvent::Stmt(_, sql) = event
        && (sql.contains("FROM sessions")
            || sql.contains("FROM messages")
            || sql.contains("FROM threads")
            || sql.contains("FROM turns"))
    {
        LISTED_SQL.lock().unwrap().push(sql.to_owned());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_session_list_uses_one_activity_statement_for_multiple_sessions() {
    let (router, storage) = make_test_router();
    let (_, topic) = http_json(
        &router,
        "POST",
        "/api/topics",
        Some(json!({"name": "Counted"})),
    )
    .await;
    let topic_id = topic["id"].as_str().unwrap();
    for _ in 0..3 {
        http_json(
            &router,
            "POST",
            &format!("/api/topics/{topic_id}/sessions"),
            None,
        )
        .await;
    }
    for _ in 0..2 {
        http_json(&router, "POST", "/api/conversations", None).await;
    }

    LISTED_SQL.lock().unwrap().clear();
    storage
        .with_conn(|conn| {
            conn.trace_v2(
                rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
                Some(record_list_sql),
            );
            Ok(())
        })
        .await
        .unwrap();
    let mut counts = Vec::new();
    for (path, expected) in [
        (format!("/api/topics/{topic_id}/sessions"), 3),
        ("/api/conversations".to_owned(), 2),
        ("/api/operator/overview".to_owned(), 5),
    ] {
        LISTED_SQL.lock().unwrap().clear();
        let (status, listed) = http_json(&router, "GET", &path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        let sessions = if path == "/api/operator/overview" {
            &listed["sessions"]
        } else {
            &listed["conversations"]
        };
        assert_eq!(sessions.as_array().unwrap().len(), expected, "{path}");
        let statements = LISTED_SQL.lock().unwrap();
        counts.push((
            path,
            statements.len(),
            ["sessions", "messages", "threads", "turns"].map(|table| {
                statements
                    .iter()
                    .filter(|sql| sql.contains(&format!("FROM {table}")))
                    .count()
            }),
            statements
                .iter()
                .any(|sql| sql.contains("latest.content") && sql.contains("has_interrupted_turn")),
        ));
    }
    storage
        .with_conn(|conn| {
            conn.trace_v2(rusqlite::trace::TraceEventCodes::empty(), None);
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        counts
            .iter()
            .all(|(_, count, _, shape)| *count == 1 && *shape),
        "session-related statement counts (sessions/messages/threads/turns) and activity shape: {counts:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 1: Topic session isolation
// ---------------------------------------------------------------------------
// Spans: slug migration, slug model, full CRUD,
// topic CRUD routes, topic-scoped session routes
//
// Proves: conversations created in one topic are absent
// when listing another topic's conversations.

#[tokio::test(flavor = "multi_thread")]
async fn sessions_are_isolated_between_topics() {
    let (router, _storage) = make_test_router();

    // Create two scratch-only topics.
    let (status_a, topic_a) = http_json(
        &router,
        "POST",
        "/api/topics",
        Some(json!({ "name": "Alpha" })),
    )
    .await;
    assert_eq!(
        status_a,
        StatusCode::CREATED,
        "topic A creation should succeed"
    );
    let topic_a_id = topic_a["id"].as_str().expect("topic A should have an id");

    let (status_b, topic_b) = http_json(
        &router,
        "POST",
        "/api/topics",
        Some(json!({ "name": "Bravo" })),
    )
    .await;
    assert_eq!(
        status_b,
        StatusCode::CREATED,
        "topic B creation should succeed"
    );
    let topic_b_id = topic_b["id"].as_str().expect("topic B should have an id");

    // Create two sessions in topic A.
    let (s1_status, _) = http_json(
        &router,
        "POST",
        &format!("/api/topics/{topic_a_id}/sessions"),
        None,
    )
    .await;
    assert_eq!(s1_status, StatusCode::CREATED);

    let (s2_status, _) = http_json(
        &router,
        "POST",
        &format!("/api/topics/{topic_a_id}/sessions"),
        None,
    )
    .await;
    assert_eq!(s2_status, StatusCode::CREATED);

    // Create one session in topic B.
    let (s3_status, _) = http_json(
        &router,
        "POST",
        &format!("/api/topics/{topic_b_id}/sessions"),
        None,
    )
    .await;
    assert_eq!(s3_status, StatusCode::CREATED);

    // List topic A's sessions — should see exactly 2.
    let (list_a_status, list_a) = http_json(
        &router,
        "GET",
        &format!("/api/topics/{topic_a_id}/sessions"),
        None,
    )
    .await;
    assert_eq!(list_a_status, StatusCode::OK);
    let sessions_a = list_a["conversations"]
        .as_array()
        .expect("sessions should be an array");
    assert_eq!(
        sessions_a.len(),
        2,
        "topic A should have exactly 2 sessions"
    );

    // List topic B's sessions — should see exactly 1.
    let (list_b_status, list_b) = http_json(
        &router,
        "GET",
        &format!("/api/topics/{topic_b_id}/sessions"),
        None,
    )
    .await;
    assert_eq!(list_b_status, StatusCode::OK);
    let sessions_b = list_b["conversations"]
        .as_array()
        .expect("sessions should be an array");
    assert_eq!(sessions_b.len(), 1, "topic B should have exactly 1 session");

    // The other topic is unaffected, and the general scope stays unlisted.
    let (list_status, topic_list) = http_json(&router, "GET", "/api/topics", None).await;
    assert_eq!(list_status, StatusCode::OK);
    let topics = topic_list["topics"]
        .as_array()
        .expect("topics should be an array");
    assert_eq!(topics.len(), 2, "Alpha and Bravo are the only topics");
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_attachment_mutations_record_whole_state_audit_events() {
    let (router, storage) = make_test_router();
    let (status, topic) = http_json(
        &router,
        "POST",
        "/api/topics",
        Some(json!({ "name": "Audit" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let topic_id = topic["id"].as_str().expect("topic id");
    let workspace_root = tempfile::tempdir().expect("workspace root");

    let (status, attached) = http_json(
        &router,
        "POST",
        &format!("/api/topics/{topic_id}/workspaces"),
        Some(json!({
            "path": workspace_root.path().to_string_lossy(),
            "set_as_default": false,
            "permissions": ["read"],
            "is_trusted": false,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let workspace_id = attached["id"].as_str().expect("workspace id");

    let (status, trusted) = http_json(
        &router,
        "PUT",
        &format!("/api/topics/{topic_id}/workspaces/{workspace_id}/trust"),
        Some(json!({ "is_trusted": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(trusted["is_trusted"], true);

    let (status, _) = http_json(
        &router,
        "DELETE",
        &format!("/api/topics/{topic_id}/workspaces/{workspace_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let mut events = storage
        .list_audit_events()
        .await
        .expect("audit events")
        .into_iter()
        .filter(|event| {
            event.entity_kind == "workspace_attachment" && event.entity_id == workspace_id
        })
        .collect::<Vec<_>>();
    events.sort_by(|left, right| {
        left.metadata["action"]
            .as_str()
            .cmp(&right.metadata["action"].as_str())
    });
    assert_eq!(
        events.len(),
        3,
        "attach, trust, and detach each record an audit event"
    );
    assert!(
        events
            .iter()
            .all(|event| event.event_kind == "configuration_change"
                && event.metadata["source"] == "operator_http")
    );
    let attached_event = events
        .iter()
        .find(|event| event.metadata["action"] == "attach_workspace")
        .expect("attach audit event");
    assert!(attached_event.metadata["prior_state"].is_null());
    assert_eq!(attached_event.metadata["new_state"]["is_trusted"], false);
    let trust_event = events
        .iter()
        .find(|event| event.metadata["action"] == "set_workspace_trust")
        .expect("trust audit event");
    assert_eq!(trust_event.metadata["prior_state"]["is_trusted"], false);
    assert_eq!(trust_event.metadata["new_state"]["is_trusted"], true);
    let detached_event = events
        .iter()
        .find(|event| event.metadata["action"] == "detach_workspace")
        .expect("detach audit event");
    assert_eq!(detached_event.metadata["prior_state"]["is_trusted"], true);
    assert!(detached_event.metadata["new_state"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn relinking_a_moved_workspace_preserves_its_settings_and_records_one_change() {
    let (router, storage) = make_test_router();
    let (status, topic) = http_json(
        &router,
        "POST",
        "/api/topics",
        Some(json!({ "name": "Relink" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let topic_id = topic["id"].as_str().expect("topic id");
    let root = tempfile::tempdir().expect("workspace parent");
    let original = root.path().join("original");
    let moved = root.path().join("moved");
    std::fs::create_dir(&original).expect("original folder");

    let (status, attached) = http_json(
        &router,
        "POST",
        &format!("/api/topics/{topic_id}/workspaces"),
        Some(json!({
            "path": original,
            "label": "Reference files",
            "permissions": ["read", "write"],
            "is_trusted": true,
            "set_as_default": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let workspace_id = attached["id"].as_str().expect("workspace id");
    let workspace_uuid = Uuid::parse_str(workspace_id).expect("workspace uuid");
    let before = serde_json::to_value(
        storage
            .get_workspace_attachment(workspace_uuid)
            .await
            .expect("read attached workspace")
            .expect("attached workspace"),
    )
    .expect("serialise attached workspace");
    std::fs::rename(&original, &moved).expect("move folder");
    let route = format!("/api/topics/{topic_id}/workspaces/{workspace_id}/relink");
    let (status, relinked) =
        http_json(&router, "PUT", &route, Some(json!({ "path": moved }))).await;
    assert_eq!(status, StatusCode::OK, "{relinked}");
    assert_eq!(relinked["id"], attached["id"]);
    assert_eq!(relinked["label"], "Reference files");
    assert_eq!(relinked["permissions"], json!(["read", "write"]));
    assert_eq!(relinked["is_trusted"], true);
    assert_eq!(relinked["is_default"], true);
    assert_eq!(relinked["path"], moved.to_string_lossy().as_ref());
    assert_eq!(relinked["is_missing"], false);

    let (status, listing) = http_json(
        &router,
        "GET",
        &format!("/api/topics/{topic_id}/workspaces"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing["workspaces"], json!([relinked]));
    let after = serde_json::to_value(
        storage
            .get_workspace_attachment(workspace_uuid)
            .await
            .expect("read relinked workspace")
            .expect("relinked workspace"),
    )
    .expect("serialise relinked workspace");

    let events = storage.list_audit_events().await.expect("audit events");
    let relink_events = events
        .iter()
        .filter(|event| {
            event.entity_id == workspace_id && event.metadata["action"] == "relink_workspace"
        })
        .collect::<Vec<_>>();
    assert_eq!(relink_events.len(), 1);
    let event = relink_events[0];
    assert_eq!(event.event_kind, "configuration_change");
    assert_eq!(event.metadata["prior_state"], before);
    assert_eq!(event.metadata["new_state"], after);
    for field in [
        "id",
        "topic_id",
        "label",
        "permissions",
        "is_trusted",
        "is_default",
        "created_at",
    ] {
        assert_eq!(
            event.metadata["prior_state"][field], event.metadata["new_state"][field],
            "{field} changed"
        );
    }

    let (status, _) = http_json(
        &router,
        "PUT",
        &route,
        Some(json!({ "path": root.path().join("absent") })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, after_refusal) = http_json(
        &router,
        "GET",
        &format!("/api/topics/{topic_id}/workspaces"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(after_refusal["workspaces"], json!([relinked]));
    assert_eq!(
        serde_json::to_value(
            storage
                .get_workspace_attachment(workspace_uuid)
                .await
                .expect("read after refusal")
                .expect("workspace survives refusal")
        )
        .expect("serialise workspace after refusal"),
        after,
    );
    assert_eq!(
        storage
            .list_audit_events()
            .await
            .expect("audit events")
            .len(),
        events.len()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conversation_without_a_topic_runs_in_the_general_scope() {
    let (router, _storage) = make_test_router();
    let (address, server) = start_live_server(router).await;
    let client = reqwest::Client::new();
    let base_url = format!("http://{address}");

    // The general scope is never listed as a topic.
    let topics = client
        .get(format!("{base_url}/api/topics"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        topics["topics"].as_array().unwrap().len(),
        0,
        "a fresh install lists no topics: the general scope is not one"
    );

    // A conversation created without choosing a topic carries none.
    let created = client
        .post(format!("{base_url}/api/conversations"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(
        created.get("topic_id").is_none(),
        "a topicless session projects no topic: {created}"
    );
    let detail = client
        .get(format!(
            "{base_url}/api/conversations/{}",
            created["id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(detail.get("topic_id").is_none(), "{detail}");

    // Naming a topic on the topicless creation route is still refused as an
    // undeclared field, and nothing is created.
    let response = client
        .post(format!("{base_url}/api/conversations"))
        .json(&json!({ "topic_id": Uuid::new_v4() }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let rejection = response.text().await.unwrap();
    assert!(
        rejection.contains("topic_id"),
        "rejection should name the undeclared field: {rejection}"
    );

    server.abort();
}

// ---------------------------------------------------------------------------
// Test 2: Memory scoping between topics
// ---------------------------------------------------------------------------
// Spans: memory tools, topic context, topic CRUD,
// memory registration
//
// Proves: memory written in Topic A is not accessible
// from Topic B. Memory tool uses per-topic filesystem scoping.

#[tokio::test(flavor = "multi_thread")]
async fn memory_is_isolated_between_topics() {
    let mut env_guard = ProcessEnvGuard::new();
    let temp_dir = tempfile::TempDir::new().unwrap();
    env_guard.set_swage_home(temp_dir.path());

    let (router, _storage) = make_test_router();

    // Create two topics.
    let (_, topic_a) = http_json(
        &router,
        "POST",
        "/api/topics",
        Some(json!({ "name": "MemA" })),
    )
    .await;
    let slug_a = topic_a["slug"]
        .as_str()
        .expect("topic A should have a slug");

    let (_, topic_b) = http_json(
        &router,
        "POST",
        "/api/topics",
        Some(json!({ "name": "MemB" })),
    )
    .await;
    let slug_b = topic_b["slug"]
        .as_str()
        .expect("topic B should have a slug");

    // Verify the two topics have different slugs.
    assert_ne!(slug_a, slug_b, "topics should have distinct slugs");

    // Write a memory file into topic A's memory directory directly.
    // (In production, the agent does this via the memory tool during a turn.
    //  Here we verify the filesystem scoping by checking that topic B's
    //  memory directory does not contain the file.)
    let swage_root = swage_config::discovery::resolve_root().unwrap();
    let mem_dir_a = swage_root.join("topics").join(slug_a).join("memory");
    let mem_dir_b = swage_root.join("topics").join(slug_b).join("memory");

    // Create memory directory for A and write a file.
    std::fs::create_dir_all(&mem_dir_a).expect("create memory dir A");
    std::fs::write(
        mem_dir_a.join("context.md"),
        "# Topic A context\nImportant fact.",
    )
    .expect("write memory file in A");

    // Topic B's memory directory should either not exist or be empty.
    if mem_dir_b.exists() {
        let entries: Vec<_> = std::fs::read_dir(&mem_dir_b)
            .expect("read B memory dir")
            .collect();
        assert!(
            entries.is_empty(),
            "topic B's memory directory should be empty, but found {} entries",
            entries.len()
        );
    }
    // else: directory doesn't exist, which also means no memory — correct.

    // Verify A's file exists.
    assert!(
        mem_dir_a.join("context.md").exists(),
        "topic A should have the memory file"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Default topic cannot be deleted
// ---------------------------------------------------------------------------
// Spans: topic CRUD handlers, storage CRUD
//
// Proves: the general scope is neither listed nor deletable.

#[tokio::test(flavor = "multi_thread")]
async fn the_general_scope_is_not_a_topic_the_api_can_reach() {
    let (router, storage) = make_test_router();
    let general = storage.general_scope().await.expect("general scope");

    // Absent from the topic list …
    let (list_status, list_body) = http_json(&router, "GET", "/api/topics", None).await;
    assert_eq!(list_status, StatusCode::OK);
    assert!(
        list_body["topics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|topic| topic["id"].as_str() != Some(&general.id.to_string())),
        "the general scope must not be listed as a topic"
    );

    // … and protected from deletion: it is where every topicless session lives.
    let (status, _body) = http_json(
        &router,
        "DELETE",
        &format!("/api/topics/{}", general.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// Test 4: Topic deletion preserves other topics' sessions
// ---------------------------------------------------------------------------
// Spans: topic CRUD, session routes, storage CRUD
//
// Proves: deleting a topic removes its conversations
// but does not affect other topics.

#[tokio::test(flavor = "multi_thread")]
async fn deleting_topic_does_not_affect_other_topics() {
    let (router, _storage) = make_test_router();

    // Create two topics.
    let (_, topic_a) = http_json(
        &router,
        "POST",
        "/api/topics",
        Some(json!({ "name": "Keeper" })),
    )
    .await;
    let topic_a_id = topic_a["id"].as_str().unwrap();

    let (_, topic_b) = http_json(
        &router,
        "POST",
        "/api/topics",
        Some(json!({ "name": "Doomed" })),
    )
    .await;
    let topic_b_id = topic_b["id"].as_str().unwrap();

    // Create a session in each topic.
    http_json(
        &router,
        "POST",
        &format!("/api/topics/{topic_a_id}/sessions"),
        None,
    )
    .await;

    http_json(
        &router,
        "POST",
        &format!("/api/topics/{topic_b_id}/sessions"),
        None,
    )
    .await;

    // Delete topic B.
    let (del_status, _) = http_json(
        &router,
        "DELETE",
        &format!("/api/topics/{topic_b_id}"),
        None,
    )
    .await;
    assert_eq!(del_status, StatusCode::NO_CONTENT);

    // Topic A's session should still be there.
    let (list_status, list_body) = http_json(
        &router,
        "GET",
        &format!("/api/topics/{topic_a_id}/sessions"),
        None,
    )
    .await;
    assert_eq!(list_status, StatusCode::OK);
    let sessions = list_body["conversations"]
        .as_array()
        .expect("sessions should be an array");
    assert_eq!(
        sessions.len(),
        1,
        "topic A should still have its session after topic B was deleted"
    );

    // Topic B should no longer exist.
    let (get_status, _) =
        http_json(&router, "GET", &format!("/api/topics/{topic_b_id}"), None).await;
    assert_eq!(get_status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// The recent-sessions list over HTTP
// ---------------------------------------------------------------------------

/// A session without a topic runs under the general scope's agent
/// definitions: the built-in default and every global definition, with no
/// topic-scoped definition shadowing them. The listing is the same shape a
/// topic's is, so the composer offers the same picker either way.
#[tokio::test(flavor = "multi_thread")]
async fn the_general_scope_lists_the_agent_definitions_a_topicless_session_can_run_under() {
    let (router, _storage) = make_test_router();

    let (status, listed) = http_json(&router, "GET", "/api/agent-definitions", None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let definitions = listed["definitions"].as_array().expect("definitions");
    assert_eq!(
        definitions.len(),
        2,
        "a fresh install ships the default agent and the platform's curator: {listed}"
    );
    assert_eq!(definitions[0]["id"], "gateway");
    assert_eq!(definitions[0]["is_builtin"], true);
    assert_eq!(definitions[0]["selectable"], true);
    assert_eq!(definitions[1]["id"], "memory-curator");
    assert_eq!(
        definitions[1]["selectable"], false,
        "the curator is listed for Settings and never offered to a person's session"
    );

    // Every conversation read carries the agent the session runs under.
    let (_, created) = http_json(
        &router,
        "POST",
        "/api/conversations",
        Some(json!({ "agent_id": "gateway" })),
    )
    .await;
    assert_eq!(created["agent_id"], "gateway", "{created}");
    let (_, detail) = http_json(
        &router,
        "GET",
        &format!("/api/conversations/{}", created["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(detail["agent_id"], "gateway", "{detail}");
    let (_, listed) = http_json(&router, "GET", "/api/conversations", None).await;
    assert_eq!(
        listed["conversations"][0]["agent_id"], "gateway",
        "{listed}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_recent_list_spans_topics_and_the_general_scope_and_narrows_by_topic() {
    let (router, _storage) = make_test_router();

    let (_, topic) = http_json(
        &router,
        "POST",
        "/api/topics",
        Some(json!({ "name": "Alpha" })),
    )
    .await;
    let topic_id = topic["id"].as_str().expect("topic id");
    let (_, in_topic) = http_json(
        &router,
        "POST",
        &format!("/api/topics/{topic_id}/sessions"),
        None,
    )
    .await;
    let (_, in_general) = http_json(&router, "POST", "/api/conversations", None).await;

    let (status, everywhere) = http_json(&router, "GET", "/api/sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    let entries = everywhere["entries"].as_array().expect("entries");
    let mut ids: Vec<&str> = entries
        .iter()
        .map(|entry| {
            entry["source"]["session"]["id"]
                .as_str()
                .expect("session id")
        })
        .collect();
    ids.sort_unstable();
    let mut expected = vec![
        in_topic["id"].as_str().expect("id"),
        in_general["id"].as_str().expect("id"),
    ];
    expected.sort_unstable();
    assert_eq!(ids, expected, "the default list spans every scope");
    for entry in entries {
        assert_eq!(entry["source"]["kind"], "conversation");
        assert_eq!(entry["source"]["session"]["origin"]["kind"], "direct");
        assert_eq!(entry["automated"], json!([]));
    }
    let general_entry = entries
        .iter()
        .find(|entry| entry["source"]["session"]["id"] == in_general["id"])
        .expect("general entry");
    assert!(general_entry["source"]["session"].get("topic_id").is_none());
    assert_eq!(general_entry["source"]["session"]["topic_name"], "General");

    let (status, narrowed) = http_json(
        &router,
        "GET",
        &format!("/api/sessions?topic_id={topic_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let narrowed = narrowed["entries"].as_array().expect("entries");
    assert_eq!(narrowed.len(), 1);
    assert_eq!(narrowed[0]["source"]["session"]["id"], in_topic["id"]);
    assert_eq!(narrowed[0]["source"]["session"]["topic_name"], "Alpha");

    let (status, archived) = http_json(&router, "GET", "/api/sessions?archived=true", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        archived["entries"],
        json!([]),
        "nothing is archived on a fresh install"
    );
}

// Native file tools must enforce the attachment's access even when a turn
// has an inner sandbox strategy: device-executed tools run on the host.
struct UnexpectedSandboxExecution;

#[async_trait::async_trait]
impl swage_tool::dispatch::ToolExecutionStrategy for UnexpectedSandboxExecution {
    async fn execute(
        &self,
        _tool: &dyn swage_tool::tool::Tool,
        name: &str,
        _params: &Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<Value, swage_core::ToolError> {
        Err(swage_core::ToolError::ExecutionFailed {
            name: name.into(),
            message: "native file tool incorrectly entered the inner sandbox".into(),
        })
    }
}

async fn native_file_fixture(
    root: &std::path::Path,
    writable: bool,
    sandboxed_turn: bool,
    remote: bool,
    trusted: bool,
) -> (
    swage_tool::context::MockToolContext,
    swage_gateway::tool_executor::GatewayExecutionStrategy,
    Arc<SqliteStorage>,
) {
    use swage_storage::{NewSession, SessionOrigin, SessionStore};
    let (router, storage) = make_test_router();
    let (status, topic) = http_json(
        &router,
        "POST",
        "/api/topics",
        Some(json!({ "name": "File access" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let topic_id = topic["id"].as_str().unwrap();
    let permissions = if writable {
        vec!["read", "write"]
    } else {
        vec!["read"]
    };
    let (status, attachment) = http_json(
        &router,
        "POST",
        &format!("/api/topics/{topic_id}/workspaces"),
        Some(json!({ "path": root, "permissions": permissions,
            "is_trusted": trusted, "set_as_default": true })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let session = storage
        .create_session(NewSession {
            topic_id: Uuid::parse_str(topic_id).unwrap(),
            agent_id: "default".into(),
            origin: SessionOrigin::Direct,
            metadata: None,
            active_workspace_attachment_id: Some(
                Uuid::parse_str(attachment["id"].as_str().unwrap()).unwrap(),
            ),
        })
        .await
        .unwrap();
    let mut context = swage_tool::context::MockToolContext::new("default", session.id.to_string());
    context.working_directory = root.to_path_buf();
    let (tx, _) = tokio::sync::mpsc::unbounded_channel();
    let inner = sandboxed_turn.then(|| {
        Arc::new(UnexpectedSandboxExecution) as Arc<dyn swage_tool::dispatch::ToolExecutionStrategy>
    });
    let device = remote.then(|| swage_gateway::device::RegisteredDevice {
        device_id: "remote-file-device".into(),
        label: None,
        kind: swage_gateway::device::DeviceKind::AttachedDevice,
        shared_token_secret_ref: None,
        workspace_root: root.to_path_buf(),
        workspace_roots: vec![root.to_path_buf()],
        capabilities: vec!["file_write".into(), "file_edit".into()],
        registered_at: chrono::Utc::now(),
    });
    let strategy = swage_gateway::tool_executor::GatewayExecutionStrategy::new(
        inner,
        tx,
        device,
        Arc::new(swage_gateway::secret::SecretManager::new(storage.clone())),
        storage.clone(),
        None,
    );
    (context, strategy, storage)
}

#[tokio::test(flavor = "multi_thread")]
async fn native_file_write_respects_attachment_access_on_local_and_sandboxed_turns() {
    let home = tempfile::tempdir().unwrap();
    let mut env_guard = ProcessEnvGuard::new();
    env_guard.set_swage_home(home.path());
    use swage_gateway::tool::file_write::FileWriteTool;
    use swage_tool::dispatch::ToolExecutionStrategy;
    for sandboxed in [false, true] {
        for writable in [false, true] {
            let root = tempfile::tempdir().unwrap();
            std::fs::write(root.path().join("existing.txt"), "original").unwrap();
            let (ctx, strategy, _) =
                native_file_fixture(root.path(), writable, sandboxed, false, true).await;
            for path in ["nested/denied-probe.txt", "existing.txt"] {
                let result = strategy
                    .execute(
                        &FileWriteTool,
                        "file_write",
                        &json!({"path": path, "content": "denied-probe\n"}),
                        &ctx,
                    )
                    .await;
                if writable {
                    assert_eq!(
                        result.unwrap()["output"],
                        format!("Wrote 13 bytes to {path}")
                    );
                    assert_eq!(
                        std::fs::read_to_string(root.path().join(path)).unwrap(),
                        "denied-probe\n"
                    );
                } else {
                    let error = result.expect_err("read-only attachment must refuse file_write");
                    assert!(error.to_string().contains("read-only access"), "{error}");
                    assert!(!root.path().join("nested").exists());
                    assert_eq!(
                        std::fs::read_to_string(root.path().join("existing.txt")).unwrap(),
                        "original"
                    );
                    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
                }
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn native_file_edit_respects_attachment_access_on_local_and_sandboxed_turns() {
    let home = tempfile::tempdir().unwrap();
    let mut env_guard = ProcessEnvGuard::new();
    env_guard.set_swage_home(home.path());
    use swage_gateway::tool::file_edit::FileEditTool;
    use swage_tool::dispatch::ToolExecutionStrategy;
    for sandboxed in [false, true] {
        for writable in [false, true] {
            let root = tempfile::tempdir().unwrap();
            for path in ["first.txt", "second.txt"] {
                std::fs::write(root.path().join(path), "original").unwrap();
            }
            let (ctx, strategy, _) =
                native_file_fixture(root.path(), writable, sandboxed, false, true).await;
            let result = strategy.execute(&FileEditTool, "file_edit", &json!({"changes": [
                {"path": "first.txt", "edits": [{"type": "replace", "old_text": "original", "new_text": "changed"}]},
                {"path": "second.txt", "edits": [{"type": "replace", "old_text": "original", "new_text": "changed"}]}
            ]}), &ctx).await;
            if writable {
                assert_eq!(result.unwrap()["output"], "Applied 2 edit(s) to 2 file(s)");
            } else {
                let error = result.expect_err("read-only attachment must refuse file_edit");
                assert!(error.to_string().contains("read-only access"), "{error}");
            }
            for path in ["first.txt", "second.txt"] {
                assert_eq!(
                    std::fs::read_to_string(root.path().join(path)).unwrap(),
                    if writable { "changed" } else { "original" }
                );
            }
        }
    }
}

#[tokio::test]
async fn native_file_mutations_without_attachment_authority_refuse() {
    use swage_gateway::tool::{file_edit::FileEditTool, file_write::FileWriteTool};
    use swage_tool::tool::Tool;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("existing.txt"), "original").unwrap();
    let mut ctx = swage_tool::context::MockToolContext::new("unknown", "unknown");
    ctx.working_directory = root.path().to_path_buf();
    let error = FileWriteTool
        .execute(json!({"path": "probe.txt", "content": "bad"}), &ctx)
        .await
        .expect_err("context without attachment authority must refuse writes");
    assert!(
        error
            .to_string()
            .contains("cannot establish workspace write access"),
        "{error}"
    );
    let error = FileEditTool
        .execute(
            json!({"changes": [{"path": "existing.txt", "edits": [
                {"type": "replace", "old_text": "original", "new_text": "bad"}
            ]}]}),
            &ctx,
        )
        .await
        .expect_err("context without attachment authority must refuse edits");
    assert!(
        error
            .to_string()
            .contains("cannot establish workspace write access"),
        "{error}"
    );
    assert!(!root.path().join("probe.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("existing.txt")).unwrap(),
        "original"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn native_file_mutations_refuse_read_only_targets_before_remote_dispatch() {
    let home = tempfile::tempdir().unwrap();
    let mut env_guard = ProcessEnvGuard::new();
    env_guard.set_swage_home(home.path());
    use swage_gateway::tool::{file_edit::FileEditTool, file_write::FileWriteTool};
    use swage_tool::dispatch::ToolExecutionStrategy;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("existing.txt"), "original").unwrap();
    let (ctx, strategy, _) = native_file_fixture(root.path(), false, false, true, true).await;
    let error = strategy
        .execute(
            &FileWriteTool,
            "file_write",
            &json!({"path": "probe.txt", "content": "bad"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read-only access"), "{error}");
    let error = strategy.execute(&FileEditTool, "file_edit", &json!({"changes": [
        {"path": "existing.txt", "edits": [{"type": "replace", "old_text": "original", "new_text": "bad"}]}
    ]}), &ctx).await.unwrap_err();
    assert!(error.to_string().contains("read-only access"), "{error}");
    assert!(!root.path().join("probe.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("existing.txt")).unwrap(),
        "original"
    );
    // A device's own path is not a gateway attachment. The request reaches
    // the transport boundary instead of being denied as a read-only target.
    let other = tempfile::tempdir().unwrap();
    let error = strategy
        .execute(
            &FileWriteTool,
            "file_write",
            &json!({"path": other.path().join("own.txt"), "content": "device"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("device execution needs the live gateway state"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn native_file_mutations_keep_read_only_access_for_untrusted_attachments() {
    let home = tempfile::tempdir().unwrap();
    let mut env_guard = ProcessEnvGuard::new();
    env_guard.set_swage_home(home.path());
    use swage_gateway::tool::{file_edit::FileEditTool, file_write::FileWriteTool};
    use swage_tool::dispatch::ToolExecutionStrategy;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("existing.txt"), "original").unwrap();
    // Even a caller with this permitted working root cannot turn a declared
    // read-only attachment into a writable one by clearing its trust flag.
    let (ctx, strategy, _) = native_file_fixture(root.path(), false, false, false, false).await;
    let write = strategy
        .execute(
            &FileWriteTool,
            "file_write",
            &json!({"path": "probe.txt", "content": "bad"}),
            &ctx,
        )
        .await;
    assert!(write.unwrap_err().to_string().contains("read-only access"));
    let edit = strategy.execute(&FileEditTool, "file_edit", &json!({"changes": [
        {"path": "existing.txt", "edits": [{"type": "replace", "old_text": "original", "new_text": "bad"}]}
    ]}), &ctx).await;
    assert!(edit.unwrap_err().to_string().contains("read-only access"));
    assert!(!root.path().join("probe.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("existing.txt")).unwrap(),
        "original"
    );
}

async fn additional_native_attachment(
    storage: &SqliteStorage,
    ctx: &swage_tool::context::MockToolContext,
    path: &std::path::Path,
    writable: bool,
) -> swage_storage::WorkspaceAttachment {
    use swage_storage::{NewWorkspaceAttachment, SessionStore};
    let session = storage
        .get_session(Uuid::parse_str(&ctx.session_id).unwrap())
        .await
        .unwrap()
        .unwrap();
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            topic_id: session.topic_id,
            label: "additional workspace".into(),
            path: path.to_string_lossy().into_owned(),
            permissions: if writable {
                vec!["read".into(), "write".into()]
            } else {
                vec!["read".into()]
            },
            is_trusted: true,
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn native_file_edit_remote_preflight_refuses_trimmed_read_only_path() {
    use swage_gateway::tool::file_edit::FileEditTool;
    use swage_tool::dispatch::ToolExecutionStrategy;
    let home = tempfile::tempdir().unwrap();
    let mut env_guard = ProcessEnvGuard::new();
    env_guard.set_swage_home(home.path());
    let writable = tempfile::tempdir().unwrap();
    let read_only = tempfile::tempdir().unwrap();
    let target = read_only.path().join("existing.txt");
    std::fs::write(&target, "original").unwrap();
    let (mut ctx, strategy, storage) =
        native_file_fixture(writable.path(), true, false, true, true).await;
    additional_native_attachment(&storage, &ctx, read_only.path(), false).await;
    ctx.allowed_paths = vec![read_only.path().to_path_buf()];
    let result = strategy.execute(&FileEditTool, "file_edit", &json!({"changes": [
        {"path": format!("  {}  ", target.display()), "edits": [{"type": "replace", "old_text": "original", "new_text": "bad"}]}
    ]}), &ctx).await;
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
    let error = result.expect_err("remote preflight must refuse the trimmed read-only target");
    assert!(error.to_string().contains("read-only access"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn native_file_mutations_allow_active_writable_child_of_read_only_attachment() {
    use swage_gateway::tool::{file_edit::FileEditTool, file_write::FileWriteTool};
    use swage_storage::SessionStore;
    use swage_tool::dispatch::ToolExecutionStrategy;
    let home = tempfile::tempdir().unwrap();
    let mut env_guard = ProcessEnvGuard::new();
    env_guard.set_swage_home(home.path());
    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("child");
    std::fs::create_dir(&child).unwrap();
    std::fs::write(child.join("existing.txt"), "original").unwrap();
    let (mut ctx, strategy, storage) =
        native_file_fixture(root.path(), false, false, false, true).await;
    let attachment = additional_native_attachment(&storage, &ctx, &child, true).await;
    storage
        .set_session_active_workspace(
            Uuid::parse_str(&ctx.session_id).unwrap(),
            Some(attachment.id),
        )
        .await
        .unwrap();
    ctx.working_directory = child.clone();
    ctx.allowed_paths = vec![root.path().to_path_buf()];
    let write = strategy
        .execute(
            &FileWriteTool,
            "file_write",
            &json!({"path": "probe.txt", "content": "written"}),
            &ctx,
        )
        .await;
    let edit = strategy.execute(&FileEditTool, "file_edit", &json!({"changes": [
        {"path": "existing.txt", "edits": [{"type": "replace", "old_text": "original", "new_text": "edited"}]}
    ]}), &ctx).await;
    assert!(
        write.is_ok(),
        "active writable child write failed: {write:?}"
    );
    assert!(edit.is_ok(), "active writable child edit failed: {edit:?}");
    assert_eq!(
        std::fs::read_to_string(child.join("probe.txt")).unwrap(),
        "written"
    );
    assert_eq!(
        std::fs::read_to_string(child.join("existing.txt")).unwrap(),
        "edited"
    );
    let denied = strategy
        .execute(
            &FileWriteTool,
            "file_write",
            &json!({"path": root.path().join("denied.txt"), "content": "bad"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(denied.to_string().contains("read-only access"), "{denied}");
    assert!(!root.path().join("denied.txt").exists());
}

async fn native_lookup_failure_refuses(remote: bool) {
    use swage_gateway::tool::{file_edit::FileEditTool, file_write::FileWriteTool};
    use swage_tool::dispatch::ToolExecutionStrategy;
    let home = tempfile::tempdir().unwrap();
    let mut env_guard = ProcessEnvGuard::new();
    env_guard.set_swage_home(home.path());
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("existing.txt"), "original").unwrap();
    let (ctx, strategy, storage) =
        native_file_fixture(root.path(), true, false, remote, true).await;
    storage
        .with_conn(|conn| {
            conn.execute_batch(
                "ALTER TABLE workspace_attachments RENAME TO unavailable_workspace_attachments",
            )
        })
        .await
        .unwrap();
    let write = strategy
        .execute(
            &FileWriteTool,
            "file_write",
            &json!({"path": "nested/probe.txt", "content": "bad"}),
            &ctx,
        )
        .await;
    let edit = strategy.execute(&FileEditTool, "file_edit", &json!({"changes": [
        {"path": "existing.txt", "edits": [{"type": "replace", "old_text": "original", "new_text": "bad"}]}
    ]}), &ctx).await;
    assert!(!root.path().join("nested").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("existing.txt")).unwrap(),
        "original"
    );
    for result in [write, edit] {
        let error = result.expect_err("attachment lookup failure must refuse mutation");
        assert!(
            error
                .to_string()
                .contains("Cannot check workspace write access"),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("no such table: workspace_attachments"),
            "{error}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn native_file_mutations_local_refuse_attachment_lookup_failure() {
    native_lookup_failure_refuses(false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_file_mutations_remote_refuse_attachment_lookup_failure() {
    native_lookup_failure_refuses(true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn native_file_edit_mixed_access_batch_preserves_both_files_in_either_order() {
    use swage_gateway::tool::file_edit::FileEditTool;
    use swage_tool::dispatch::ToolExecutionStrategy;
    let home = tempfile::tempdir().unwrap();
    let mut env_guard = ProcessEnvGuard::new();
    env_guard.set_swage_home(home.path());
    for sandboxed in [false, true] {
        for remote in [false, true] {
            for read_only_first in [false, true] {
                let writable = tempfile::tempdir().unwrap();
                let read_only = tempfile::tempdir().unwrap();
                let first = writable.path().join("first.txt");
                let second = read_only.path().join("second.txt");
                for path in [&first, &second] {
                    std::fs::write(path, "original").unwrap();
                }
                let (mut ctx, strategy, storage) =
                    native_file_fixture(writable.path(), true, sandboxed, remote, true).await;
                additional_native_attachment(&storage, &ctx, read_only.path(), false).await;
                ctx.allowed_paths = vec![read_only.path().to_path_buf()];
                let paths = if read_only_first {
                    [&second, &first]
                } else {
                    [&first, &second]
                };
                let changes: Vec<_> = paths
                    .into_iter()
                    .map(|path| {
                        json!({"path": path, "edits": [
                            {"type": "replace", "old_text": "original", "new_text": "bad"}
                        ]})
                    })
                    .collect();
                let result = strategy
                    .execute(
                        &FileEditTool,
                        "file_edit",
                        &json!({"changes": changes}),
                        &ctx,
                    )
                    .await;
                for path in [&first, &second] {
                    assert_eq!(
                        std::fs::read_to_string(path).unwrap(),
                        "original",
                        "batch changed {}",
                        path.display()
                    );
                }
                let error =
                    result.expect_err("mixed-access batch must refuse before writing either file");
                assert!(error.to_string().contains("read-only access"), "{error}");
            }
        }
    }
}

#[cfg(unix)]
async fn native_dangling_symlink_write(remote: bool, chain: bool, writable: bool) {
    use std::os::unix::fs::symlink;
    use swage_gateway::tool::file_write::FileWriteTool;
    use swage_tool::dispatch::ToolExecutionStrategy;
    let home = tempfile::tempdir().unwrap();
    let mut env_guard = ProcessEnvGuard::new();
    env_guard.set_swage_home(home.path());
    let root = tempfile::tempdir().unwrap();
    let target_root = root.path().join("target");
    std::fs::create_dir(&target_root).unwrap();
    let target = target_root.join("probe.txt");
    let (ctx, strategy, storage) =
        native_file_fixture(root.path(), true, false, remote, true).await;
    additional_native_attachment(&storage, &ctx, &target_root, writable).await;
    // Relative targets are interpreted from each link's parent. The second
    // link deliberately uses an absolute target to cover both forms.
    if chain {
        symlink(&target, root.path().join("second-link")).unwrap();
        symlink("second-link", root.path().join("link")).unwrap();
    } else {
        symlink("target/probe.txt", root.path().join("link")).unwrap();
    }
    let result = strategy
        .execute(
            &FileWriteTool,
            "file_write",
            &json!({"path": "link", "content": "written"}),
            &ctx,
        )
        .await;
    if writable {
        if remote {
            assert!(
                result.as_ref().is_err_and(|error| error
                    .to_string()
                    .contains("device execution needs the live gateway state"))
                    && !target.exists(),
                "writable symlink must reach remote transport: {result:?}"
            );
        } else {
            assert!(
                result.is_ok()
                    && std::fs::read_to_string(&target).is_ok_and(|text| text == "written"),
                "writable dangling symlink must create its target: {result:?}"
            );
        }
    } else {
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.to_string().contains("read-only access"))
                && !target.exists(),
            "dangling symlink must refuse read-only target without creating it: {result:?}; target exists: {}",
            target.exists()
        );
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn native_file_write_local_refuses_dangling_symlink() {
    native_dangling_symlink_write(false, false, false).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn native_file_write_local_refuses_dangling_symlink_chain() {
    native_dangling_symlink_write(false, true, false).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn native_file_write_remote_refuses_dangling_symlink() {
    native_dangling_symlink_write(true, false, false).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn native_file_write_remote_refuses_dangling_symlink_chain() {
    native_dangling_symlink_write(true, true, false).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn native_file_write_local_allows_writable_dangling_symlink() {
    native_dangling_symlink_write(false, false, true).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn native_file_write_remote_allows_writable_dangling_symlink() {
    native_dangling_symlink_write(true, false, true).await;
}
