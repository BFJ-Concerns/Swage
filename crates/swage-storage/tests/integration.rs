//! Cross-task integration tests for Phase 03 (Persistence).
//!
//! These tests verify interactions between multiple tasks' outputs — behaviour
//! that emerges only when the full stack is wired together. Each test documents
//! which motes it spans and which Delivery Closure item it proves.

use uuid::Uuid;

use swage_core::Role;
use swage_storage::model::NewProject;
use swage_storage::store::{
    MessageStore, ProjectStore, SessionStore, ThreadStore, ToolCallStore, TurnStore,
};
use swage_storage::{
    NewMessage, NewSession, NewThread, NewToolCall, NewTurn, SqliteStorage, ThreadState, TurnState,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const AGENT_A: &str = "agent_alpha";
const AGENT_B: &str = "agent_beta";

/// Create an in-memory SqliteStorage with a project and return (storage, project_id).
async fn setup() -> (SqliteStorage, Uuid) {
    let storage = SqliteStorage::in_memory().expect("in-memory should succeed");
    let project = ProjectStore::create(
        &storage,
        NewProject {
            name: "integration-test".into(),
            slug: "integration-test".into(),
            workspace_path: None,
        },
    )
    .await
    .expect("create project");
    (storage, project.id)
}

/// Build a full entity hierarchy: project → session → thread → turn → message + tool call.
/// Returns all IDs for verification.
async fn create_full_hierarchy(
    storage: &SqliteStorage,
    project_id: Uuid,
    agent_id: &str,
) -> (Uuid, Uuid, Uuid, Uuid, Uuid) {
    let session = storage
        .create_session(NewSession {
            project_id,
            agent_id: agent_id.to_string(),
            metadata: None,
        })
        .await
        .expect("create session");

    let thread = ThreadStore::create(
        storage,
        NewThread {
            session_id: session.id,
            agent_id: agent_id.to_string(),
        },
    )
    .await
    .expect("create thread");

    let turn = storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: agent_id.to_string(),
            state: TurnState::Pending,
            user_input: None,
            parent_turn_id: None,
        })
        .await
        .expect("create turn");

    let message = storage
        .create_message(NewMessage {
            thread_id: thread.id,
            turn_id: Some(turn.id),
            agent_id: agent_id.to_string(),
            role: Role::User,
            content: "Hello from integration test".to_string(),
            position: 0,
        })
        .await
        .expect("create message");

    let tool_call = storage
        .create_tool_call(NewToolCall {
            turn_id: turn.id,
            agent_id: agent_id.to_string(),
            call_id: "toolu_int_01".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{"cmd":"echo hi"}"#.to_string(),
            result: Some("hi".to_string()),
            status: Some("success".to_string()),
            duration_ms: Some(5),
            truncated: false,
            total_bytes: Some(2),
            position: 0,
            parent_call_id: None,
        })
        .await
        .expect("create tool call");

    (session.id, thread.id, turn.id, message.id, tool_call.id)
}

// ---------------------------------------------------------------------------
// Test 1: Full hierarchy round-trip through file-backed SQLite
//
// Spans: ikjg (migrations), 8cwg (session SQL), 6ay8 (thread SQL),
//        la60 (turn SQL), xf78 (message SQL), 1byf (tool_call SQL)
//
// Proves: "Storage trait hierarchy" — create full entity hierarchy through
//         traits, retrieve through fresh connection.
//         "SQLite persistence" — file-backed write, drop, reconnect, read back.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn full_hierarchy_survives_reconnection() {
    let dir = tempfile::tempdir().expect("temp dir");
    let db_path = dir.path().join("hierarchy.db");

    // Phase 1: create all entities and drop the connection.
    let (session_id, thread_id, turn_id, message_id, tool_call_id) = {
        let storage = SqliteStorage::open(&db_path).expect("open");
        let project = ProjectStore::create(
            &storage,
            NewProject {
                name: "roundtrip".into(),
                slug: "roundtrip".into(),
                workspace_path: None,
            },
        )
        .await
        .expect("create project");

        let ids = create_full_hierarchy(&storage, project.id, AGENT_A).await;
        // Storage dropped here — connection closed.
        ids
    };

    // Phase 2: reconnect and verify everything is readable.
    let storage = SqliteStorage::open(&db_path).expect("reopen");

    let session = storage.get_session(session_id).await.expect("get session");
    assert!(session.is_some(), "session should survive reconnection");
    assert_eq!(session.unwrap().agent_id, AGENT_A);

    let thread = ThreadStore::get(&storage, thread_id)
        .await
        .expect("get thread");
    assert!(thread.is_some(), "thread should survive reconnection");
    let thread = thread.unwrap();
    assert_eq!(thread.session_id, session_id);
    assert_eq!(thread.state, ThreadState::Idle);

    let turn = storage.get_turn(turn_id).await.expect("get turn");
    assert!(turn.is_some(), "turn should survive reconnection");
    assert_eq!(turn.unwrap().thread_id, thread_id);

    let message = storage.get_message(message_id).await.expect("get message");
    assert!(message.is_some(), "message should survive reconnection");
    assert_eq!(message.unwrap().thread_id, thread_id);

    let tool_call = storage
        .get_tool_call(tool_call_id)
        .await
        .expect("get tool_call");
    assert!(tool_call.is_some(), "tool call should survive reconnection");
    assert_eq!(tool_call.unwrap().agent_id, AGENT_A);
}

// ---------------------------------------------------------------------------
// Test 2: Agent_id isolation across all entity types
//
// Spans: brbf (session agent_id), p30a (thread model), 8157 (turn model),
//        xlrb (message model), 0znc (tool_call model), plus all SQL impls
//
// Proves: "Data model with agent_id" — querying by agent_id returns only
//         that agent's records across all conversation entity types.
//         Guards against: "agent_id field exists but queries return all records."
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn agent_id_isolation_across_entity_types() {
    let (storage, project_id) = setup().await;

    // Create hierarchies for two different agents.
    create_full_hierarchy(&storage, project_id, AGENT_A).await;
    create_full_hierarchy(&storage, project_id, AGENT_B).await;

    // Sessions: each agent should see only their own.
    let sessions_a = storage
        .list_sessions_by_agent_id(AGENT_A)
        .await
        .expect("list sessions A");
    let sessions_b = storage
        .list_sessions_by_agent_id(AGENT_B)
        .await
        .expect("list sessions B");
    assert_eq!(sessions_a.len(), 1, "agent_a should see 1 session");
    assert_eq!(sessions_b.len(), 1, "agent_b should see 1 session");
    assert_ne!(sessions_a[0].id, sessions_b[0].id, "different sessions");

    // Threads: each agent should see only their own.
    let threads_a = storage
        .list_threads_by_agent_id(AGENT_A)
        .await
        .expect("list threads A");
    let threads_b = storage
        .list_threads_by_agent_id(AGENT_B)
        .await
        .expect("list threads B");
    assert_eq!(threads_a.len(), 1, "agent_a should see 1 thread");
    assert_eq!(threads_b.len(), 1, "agent_b should see 1 thread");

    // Turn recovery: each agent should find only their own non-terminal turns.
    let orphans_a = storage
        .find_in_progress_by_agent(AGENT_A)
        .await
        .expect("find A");
    let orphans_b = storage
        .find_in_progress_by_agent(AGENT_B)
        .await
        .expect("find B");
    assert_eq!(orphans_a.len(), 1, "agent_a should see 1 pending turn");
    assert_eq!(orphans_b.len(), 1, "agent_b should see 1 pending turn");
    assert_ne!(orphans_a[0].id, orphans_b[0].id, "different turns");
}

// ---------------------------------------------------------------------------
// Test 3: Cascade deletion from project level
//
// Spans: ikjg (migrations with FK cascades), all store impls
//
// Proves: "Data model" — deleting a project cascades through
//         sessions → threads → turns → messages and tool calls.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn cascade_deletion_from_project() {
    let (storage, project_id) = setup().await;
    let (session_id, thread_id, turn_id, message_id, tool_call_id) =
        create_full_hierarchy(&storage, project_id, AGENT_A).await;

    // Delete the project — everything underneath should cascade.
    let deleted = ProjectStore::delete(&storage, project_id)
        .await
        .expect("delete project");
    assert!(deleted, "project should exist and be deleted");

    // Verify all children are gone.
    assert!(
        storage
            .get_session(session_id)
            .await
            .expect("get")
            .is_none(),
        "session should be cascade-deleted"
    );
    assert!(
        ThreadStore::get(&storage, thread_id)
            .await
            .expect("get")
            .is_none(),
        "thread should be cascade-deleted"
    );
    assert!(
        storage.get_turn(turn_id).await.expect("get").is_none(),
        "turn should be cascade-deleted"
    );
    assert!(
        storage
            .get_message(message_id)
            .await
            .expect("get")
            .is_none(),
        "message should be cascade-deleted"
    );
    assert!(
        storage
            .get_tool_call(tool_call_id)
            .await
            .expect("get")
            .is_none(),
        "tool call should be cascade-deleted"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Thread state machine enforced through SQLite
//
// Spans: p30a (ThreadState), r8b1 (ThreadStore trait), 6ay8 (ThreadStore SQL)
//
// Proves: "Thread state machine" — walk through Idle → Processing → Idle →
//         Processing → Completed, then verify Completed → Processing rejected.
//         Guards against: "update_state accepts any value without validation."
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn thread_state_machine_enforced() {
    let (storage, project_id) = setup().await;
    let session = storage
        .create_session(NewSession {
            project_id,
            agent_id: AGENT_A.to_string(),
            metadata: None,
        })
        .await
        .unwrap();

    let thread = ThreadStore::create(
        &storage,
        NewThread {
            session_id: session.id,
            agent_id: AGENT_A.to_string(),
        },
    )
    .await
    .unwrap();

    // Initial state is Idle.
    assert_eq!(thread.state, ThreadState::Idle);

    // Idle → Processing (valid)
    storage
        .update_state(thread.id, ThreadState::Processing)
        .await
        .unwrap();
    let t = ThreadStore::get(&storage, thread.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.state, ThreadState::Processing);

    // Processing → Idle (valid — turn completed, thread continues)
    storage
        .update_state(thread.id, ThreadState::Idle)
        .await
        .unwrap();
    let t = ThreadStore::get(&storage, thread.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.state, ThreadState::Idle);

    // Idle → Processing → Completed (valid sequence)
    storage
        .update_state(thread.id, ThreadState::Processing)
        .await
        .unwrap();
    storage
        .update_state(thread.id, ThreadState::Completed)
        .await
        .unwrap();
    let t = ThreadStore::get(&storage, thread.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.state, ThreadState::Completed);

    // Completed → Processing (INVALID — must be rejected)
    let err = storage
        .update_state(thread.id, ThreadState::Processing)
        .await
        .unwrap_err();
    assert!(
        matches!(err, swage_core::StorageError::ConstraintViolation(_)),
        "invalid transition should return ConstraintViolation, got: {err:?}"
    );

    // State should still be Completed after rejection.
    let t = ThreadStore::get(&storage, thread.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        t.state,
        ThreadState::Completed,
        "state should not change on rejection"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Turn recovery finds both Pending and Processing turns
//
// Spans: h6xf (TurnState update), ztv4 (TurnStore trait), la60 (TurnStore SQL)
//
// Proves: "Turn lifecycle states" — find_in_progress returns turns in both
//         Pending and Processing states.
//         Guards against: "find_in_progress misses Pending turns."
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn turn_recovery_finds_pending_and_processing() {
    let (storage, project_id) = setup().await;
    let session = storage
        .create_session(NewSession {
            project_id,
            agent_id: AGENT_A.to_string(),
            metadata: None,
        })
        .await
        .unwrap();

    let thread = ThreadStore::create(
        &storage,
        NewThread {
            session_id: session.id,
            agent_id: AGENT_A.to_string(),
        },
    )
    .await
    .unwrap();

    // Create one Pending turn and one Processing turn.
    let pending_turn = storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: AGENT_A.to_string(),
            state: TurnState::Pending,
            user_input: None,
            parent_turn_id: None,
        })
        .await
        .unwrap();

    let processing_turn = storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: AGENT_A.to_string(),
            state: TurnState::Processing,
            user_input: None,
            parent_turn_id: None,
        })
        .await
        .unwrap();

    // Create a Completed turn — should NOT appear in recovery.
    let completed_turn = storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: AGENT_A.to_string(),
            state: TurnState::Completed,
            user_input: None,
            parent_turn_id: None,
        })
        .await
        .unwrap();

    // Recovery query should find both Pending and Processing.
    let in_progress = storage
        .find_in_progress_by_agent(AGENT_A)
        .await
        .expect("find_in_progress should succeed");

    assert_eq!(
        in_progress.len(),
        2,
        "should find both Pending and Processing turns"
    );

    let ids: Vec<Uuid> = in_progress.iter().map(|t| t.id).collect();
    assert!(
        ids.contains(&pending_turn.id),
        "should include Pending turn"
    );
    assert!(
        ids.contains(&processing_turn.id),
        "should include Processing turn"
    );
    assert!(
        !ids.contains(&completed_turn.id),
        "should NOT include Completed turn"
    );
}

// ---------------------------------------------------------------------------
// Test 6: Default project cannot be deleted
//
// Spans: ikjg (migrations/seeding), project SQL impl
//
// Proves: "Data model" — deleting the default project is rejected.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn default_project_cannot_be_deleted() {
    let storage = SqliteStorage::in_memory().expect("in-memory");
    let default = storage.get_or_create_default().await.expect("get default");

    let err = ProjectStore::delete(&storage, default.id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, swage_core::StorageError::ConstraintViolation(_)),
        "deleting default project should return ConstraintViolation, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 7: Messages ordered within thread, not within turns
//
// Spans: xlrb (message model), 19ie (MessageStore trait), xf78 (message SQL)
//
// Proves: "Gateway adaptation" — messages are ordered within the thread as
//         a flat sequence. Position is thread-level, not turn-level.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn messages_ordered_within_thread() {
    let (storage, project_id) = setup().await;
    let session = storage
        .create_session(NewSession {
            project_id,
            agent_id: AGENT_A.to_string(),
            metadata: None,
        })
        .await
        .unwrap();

    let thread = ThreadStore::create(
        &storage,
        NewThread {
            session_id: session.id,
            agent_id: AGENT_A.to_string(),
        },
    )
    .await
    .unwrap();

    // Create messages at different positions — insert out of order.
    storage
        .create_message(NewMessage {
            thread_id: thread.id,
            turn_id: None,
            agent_id: AGENT_A.to_string(),
            role: Role::User,
            content: "second".into(),
            position: 1,
        })
        .await
        .unwrap();

    storage
        .create_message(NewMessage {
            thread_id: thread.id,
            turn_id: None,
            agent_id: AGENT_A.to_string(),
            role: Role::User,
            content: "first".into(),
            position: 0,
        })
        .await
        .unwrap();

    storage
        .create_message(NewMessage {
            thread_id: thread.id,
            turn_id: None,
            agent_id: AGENT_A.to_string(),
            role: Role::Assistant,
            content: "third".into(),
            position: 2,
        })
        .await
        .unwrap();

    // list_messages_by_thread should return messages in position order.
    let messages = storage
        .list_messages_by_thread(thread.id)
        .await
        .expect("list messages");
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].content, "first");
    assert_eq!(messages[0].position, 0);
    assert_eq!(messages[1].content, "second");
    assert_eq!(messages[1].position, 1);
    assert_eq!(messages[2].content, "third");
    assert_eq!(messages[2].position, 2);
}
