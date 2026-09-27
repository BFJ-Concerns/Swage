//! Cross-task integration tests for inter-agent communication.
//!
//! These tests verify interactions between multiple tasks' outputs — behaviour
//! that emerges only when the full messaging and delegation stack is wired.
//!
//! Tests that require a full LLM provider (delegation round-trip, schema
//! contract enforcement) verify the infrastructure wiring and routing rather
//! than end-to-end execution — the LLM is not invoked.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use swage_core::{MessageKind, MessageStatus};
use swage_storage::SqliteStorage;
use swage_storage::model::{
    NewAgentMessage, NewSession, NewThread, NewTurn, ThreadState, TurnState,
};
use swage_storage::store::{
    AgentMessageStore, SessionStore, Storage, ThreadStore, TopicStore, TurnStore,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Create a minimal storage with a session, thread, and turn for an agent.
async fn setup_agent_turn(agent_id: &str) -> (Arc<SqliteStorage>, Uuid, Uuid, Uuid) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());
    let topic = TopicStore::general_scope(storage.as_ref()).await.unwrap();
    let session = storage
        .create_session(NewSession {
            agent_id: agent_id.to_string(),
            ..swage_storage::fixtures::new_session(topic.id)
        })
        .await
        .unwrap();
    let thread = ThreadStore::create(
        storage.as_ref(),
        NewThread {
            session_id: session.id,
            agent_id: agent_id.to_string(),
        },
    )
    .await
    .unwrap();
    let turn = storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: agent_id.to_string(),
            state: TurnState::Pending,
            user_input: None,
            parent_turn_id: None,
        })
        .await
        .unwrap();
    storage
        .update_turn_state(turn.id, TurnState::Processing)
        .await
        .unwrap();
    storage
        .update_state(thread.id, ThreadState::Processing)
        .await
        .unwrap();
    (storage, session.id, thread.id, turn.id)
}

// ---------------------------------------------------------------------------
// Test 1: Delegate message round-trip through router + storage
// ---------------------------------------------------------------------------

/// Agent A sends a Delegate to Agent B via MessageRouter. The message is
/// persisted with pending status. On delivery, it transitions to delivered.
/// This validates the core messaging infrastructure (router → storage → mailbox).
#[tokio::test(flavor = "multi_thread")]
async fn delegate_and_receive_result() {
    let (storage, _session_id, _thread_id, _sender_turn_id) = setup_agent_turn("agent-a").await;

    let router = swage_agent::MessageRouter::new(storage.clone() as Arc<dyn Storage>, None);

    // Send a Delegate — should be persisted with pending status.
    let delegate = router
        .send_message(NewAgentMessage {
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-b".into(),
            kind: MessageKind::Delegate,
            correlation_id: None,
            payload: serde_json::json!({"task": "summarise the report"}),
        })
        .await
        .unwrap();

    assert_eq!(delegate.kind, MessageKind::Delegate);
    assert_eq!(delegate.status, MessageStatus::Pending);

    // Deliver the message — transitions to Delivered.
    router.deliver_message(&delegate).await.unwrap();
    let updated = storage
        .get_agent_message(delegate.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.status, MessageStatus::Delivered);
    assert!(updated.delivered_at.is_some());

    // Send a correlated Result back.
    let result_msg = router
        .send_message(NewAgentMessage {
            sender_agent_id: "agent-b".into(),
            recipient_agent_id: "agent-a".into(),
            kind: MessageKind::Result,
            correlation_id: Some(delegate.id),
            payload: serde_json::json!({"response": "summary done", "structured_output": {"answer": "42"}}),
        })
        .await
        .unwrap();

    assert_eq!(result_msg.kind, MessageKind::Result);
    assert_eq!(result_msg.correlation_id, Some(delegate.id));

    // Verify correlation chain.
    let chain = storage.list_by_correlation(delegate.id).await.unwrap();
    assert_eq!(chain.len(), 1); // Only the Result has correlation_id set.
    assert_eq!(chain[0].kind, MessageKind::Result);
}

// ---------------------------------------------------------------------------
// Test 2: Delegation creates child turn with parent_turn_id
// ---------------------------------------------------------------------------

/// When a child turn is created for a delegation, it carries parent_turn_id
/// linking back to the sender's turn. Root turns have parent_turn_id = None.
#[tokio::test(flavor = "multi_thread")]
async fn delegation_creates_child_turn_with_parent() {
    let (storage, _session_id, _thread_id, sender_turn_id) = setup_agent_turn("agent-a").await;

    // Root turn should have parent_turn_id = None.
    let root_turn = storage.get_turn(sender_turn_id).await.unwrap().unwrap();
    assert!(root_turn.parent_turn_id.is_none());

    // Create a child turn for agent-b with parent_turn_id = sender's turn.
    let topic = TopicStore::general_scope(storage.as_ref()).await.unwrap();
    let child_session = storage
        .create_session(NewSession {
            agent_id: "agent-b".into(),
            ..swage_storage::fixtures::new_session(topic.id)
        })
        .await
        .unwrap();
    let child_thread = ThreadStore::create(
        storage.as_ref(),
        NewThread {
            session_id: child_session.id,
            agent_id: "agent-b".into(),
        },
    )
    .await
    .unwrap();
    let child_turn = storage
        .create_turn(NewTurn {
            thread_id: child_thread.id,
            agent_id: "agent-b".into(),
            state: TurnState::Pending,
            user_input: None,
            parent_turn_id: Some(sender_turn_id),
        })
        .await
        .unwrap();
    storage
        .update_turn_state(child_turn.id, TurnState::Processing)
        .await
        .unwrap();
    storage
        .update_state(child_thread.id, ThreadState::Processing)
        .await
        .unwrap();

    // Verify the child turn references the parent.
    let fetched_child = storage.get_turn(child_turn.id).await.unwrap().unwrap();
    assert_eq!(fetched_child.parent_turn_id, Some(sender_turn_id));
    assert_eq!(fetched_child.agent_id, "agent-b");
}

// ---------------------------------------------------------------------------
// Test 3: Context transfer — explicit mode restricts fields
// ---------------------------------------------------------------------------

/// Explicit context transfer filters by allowed fields. Only matching
/// message roles pass through; the rest are excluded.
#[tokio::test(flavor = "multi_thread")]
async fn context_transfer_explicit_restricts_fields() {
    use swage_agent::context_transfer::transfer_context;
    use swage_config::{ContextTransferConfig, ContextTransferField, ContextTransferMode};
    use swage_core::{ChatMessage, Role};

    let config = ContextTransferConfig {
        mode: ContextTransferMode::Explicit,
        allowed_fields: vec![ContextTransferField::System],
        summary_budget: None,
        targets: std::collections::HashMap::new(),
    };

    let context = vec![
        ChatMessage::new(Role::System, "You are a helpful assistant."),
        ChatMessage::new(Role::User, "Summarise the report."),
        ChatMessage::new(Role::Assistant, "Here is the summary..."),
    ];

    let transferred = transfer_context(&config, "agent-b", &context);

    // Only the system message should pass through.
    assert_eq!(transferred.messages.len(), 1);
    assert_eq!(transferred.messages[0].role(), Role::System);
}

// ---------------------------------------------------------------------------
// Test 4: Store-and-forward — delivered on instantiation
// ---------------------------------------------------------------------------

/// Messages sent to an offline agent are stored with pending status.
/// When the agent checks its mailbox, the messages are claimed and delivered.
#[tokio::test(flavor = "multi_thread")]
async fn store_and_forward_delivers_on_instantiation() {
    let (storage, _session_id, _thread_id, _sender_turn_id) = setup_agent_turn("agent-a").await;

    let router = swage_agent::MessageRouter::new(storage.clone() as Arc<dyn Storage>, None);

    // Send a Delegate to agent-b (not running).
    let msg = router
        .send_message(NewAgentMessage {
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-b".into(),
            kind: MessageKind::Delegate,
            correlation_id: None,
            payload: serde_json::json!({"task": "work"}),
        })
        .await
        .unwrap();
    assert_eq!(msg.status, MessageStatus::Pending);

    // Agent-b checks mailbox on instantiation.
    let mailbox = swage_agent::check_mailbox(&(storage.clone() as Arc<dyn Storage>), "agent-b")
        .await
        .unwrap();

    assert_eq!(mailbox.pending_delegates.len(), 1);
    assert!(!mailbox.should_shutdown);

    // Message should now be delivered.
    let updated = storage.get_agent_message(msg.id).await.unwrap().unwrap();
    assert_eq!(updated.status, MessageStatus::Delivered);
}

// ---------------------------------------------------------------------------
// Test 5: Notify — not persisted when target offline
// ---------------------------------------------------------------------------

/// Notify messages to offline targets are silently discarded — no storage
/// record, no error.
#[tokio::test(flavor = "multi_thread")]
async fn notify_not_persisted_when_target_offline() {
    let (storage, _session_id, _thread_id, _sender_turn_id) = setup_agent_turn("agent-a").await;

    let router = swage_agent::MessageRouter::new(storage.clone() as Arc<dyn Storage>, None);

    // Send a Notify to agent-b (not running).
    let msg = router
        .send_message(NewAgentMessage {
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-b".into(),
            kind: MessageKind::Notify,
            correlation_id: None,
            payload: serde_json::json!({"progress": 50}),
        })
        .await
        .unwrap();

    // Notify returns as "delivered" but is not in storage.
    assert_eq!(msg.status, MessageStatus::Delivered);
    let pending = storage.list_pending_for_agent("agent-b").await.unwrap();
    assert!(pending.is_empty(), "Notify should not be persisted");
}

// ---------------------------------------------------------------------------
// Test 6: Output schema as inter-agent contract
// ---------------------------------------------------------------------------

/// Schema contract resolver: delegate-specified schema overrides target config,
/// absent delegate schema falls back to target config, neither = no validation.
#[tokio::test(flavor = "multi_thread")]
async fn output_schema_contract_enforced() {
    use swage_agent::schema_contract::resolve_delegation_schema;
    use swage_config::OutputConfig;

    let target_config = OutputConfig {
        schema: Some(serde_json::json!({"type": "number"})),
        max_retries: 3,
    };

    // Delegate with its own schema — overrides target.
    let delegate_schema = serde_json::json!({"type": "object", "properties": {"answer": {"type": "string"}}, "required": ["answer"]});
    let payload = serde_json::json!({"output_schema": delegate_schema, "task": "work"});
    let result = resolve_delegation_schema(&payload, &target_config);
    assert_eq!(result.schema.unwrap(), delegate_schema);

    // Delegate without schema — falls back to target.
    let payload_no_schema = serde_json::json!({"task": "work"});
    let result2 = resolve_delegation_schema(&payload_no_schema, &target_config);
    assert_eq!(
        result2.schema.unwrap(),
        serde_json::json!({"type": "number"})
    );

    // Neither has schema — no validation.
    let empty_config = OutputConfig {
        schema: None,
        max_retries: 3,
    };
    let result3 = resolve_delegation_schema(&payload_no_schema, &empty_config);
    assert!(result3.schema.is_none());
}

// ---------------------------------------------------------------------------
// Test 7: Hook blocks message send
// ---------------------------------------------------------------------------

/// A MessageSend hook that returns Block prevents the message from being
/// created. No storage record exists after the blocked send.
#[tokio::test(flavor = "multi_thread")]
async fn hook_blocks_message_send() {
    use swage_hook::{Hook, HookEvent, HookOutcome, HookRegistry};

    struct BlockHook;

    #[async_trait::async_trait]
    impl Hook for BlockHook {
        fn name(&self) -> &str {
            "block-hook"
        }
        async fn execute(&self, _event: &HookEvent) -> HookOutcome {
            HookOutcome::Block("policy: no delegation allowed".into())
        }
    }

    let storage: Arc<dyn Storage> = Arc::new(SqliteStorage::in_memory().unwrap());
    let mut registry = HookRegistry::new();

    // Event filter function for MessageSend events
    fn is_message_send(event: &HookEvent) -> bool {
        matches!(event, HookEvent::MessageSend { .. })
    }

    registry.register(swage_hook::HookRegistration {
        hook: Arc::new(BlockHook),
        event_filter: Some(is_message_send),
        matcher: None,
        priority: 0,
        failure_mode: swage_hook::FailureMode::FailOpen,
        timeout: std::time::Duration::from_secs(30),
    });

    let registry = Arc::new(registry);
    let router = swage_agent::MessageRouter::new(storage.clone(), Some(registry));

    let result = router
        .send_message(NewAgentMessage {
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-b".into(),
            kind: MessageKind::Delegate,
            correlation_id: None,
            payload: serde_json::json!({"task": "blocked"}),
        })
        .await;

    assert!(result.is_err(), "Send should fail when hook blocks");

    // No message should have been persisted.
    let pending = storage.list_pending_for_agent("agent-b").await.unwrap();
    assert!(pending.is_empty());
}

// ---------------------------------------------------------------------------
// Test 8: Delegation to non-existent agent — message fails
// ---------------------------------------------------------------------------

/// When a Delegate message is sent to a non-existent agent, the router
/// persists it but the delegation executor would fail it. Here we verify
/// the manual fail path works correctly.
#[tokio::test(flavor = "multi_thread")]
async fn delegation_to_nonexistent_agent_fails() {
    let (storage, _session_id, _thread_id, _sender_turn_id) = setup_agent_turn("agent-a").await;

    let router = swage_agent::MessageRouter::new(storage.clone() as Arc<dyn Storage>, None);

    // Send a Delegate to a non-existent agent.
    let msg = router
        .send_message(NewAgentMessage {
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-nonexistent".into(),
            kind: MessageKind::Delegate,
            correlation_id: None,
            payload: serde_json::json!({"task": "work"}),
        })
        .await
        .unwrap();

    // Simulate: delegation executor fails to resolve config → fail the message.
    router
        .fail_message(msg.id, "agent not found: agent-nonexistent")
        .await
        .unwrap();

    let updated = storage.get_agent_message(msg.id).await.unwrap().unwrap();
    assert_eq!(updated.status, MessageStatus::Failed);
    assert!(
        updated
            .failure_reason
            .as_deref()
            .unwrap()
            .contains("agent not found"),
        "failure_reason should mention 'agent not found'"
    );
}

// ---------------------------------------------------------------------------
// Test 9: Cancel interrupts active delegation
// ---------------------------------------------------------------------------

/// A Cancel message correlated to a Delegate causes the target's active
/// turn to be interrupted.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_interrupts_active_delegation() {
    let (storage, _session_id, _thread_id, target_turn_id) = setup_agent_turn("agent-b").await;

    // Create a Delegate in the correlation chain.
    let delegate = storage
        .create_agent_message(NewAgentMessage {
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-b".into(),
            kind: MessageKind::Delegate,
            correlation_id: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();

    // Build a Cancel message referencing the Delegate.
    let cancel = swage_storage::model::AgentMessage {
        id: Uuid::new_v4(),
        sender_agent_id: "agent-a".into(),
        recipient_agent_id: "agent-b".into(),
        kind: MessageKind::Cancel,
        correlation_id: Some(delegate.id),
        payload: serde_json::json!({}),
        status: MessageStatus::Delivered,
        failure_reason: None,
        created_at: chrono::Utc::now(),
        delivered_at: Some(chrono::Utc::now()),
    };

    // Fallback path — no active delegations registry.
    swage_agent::cancel::handle_cancel(storage.as_ref(), &cancel, None)
        .await
        .unwrap();

    // The target's turn should now be interrupted.
    let turn = storage.get_turn(target_turn_id).await.unwrap().unwrap();
    assert_eq!(turn.state, TurnState::Interrupted);
}

// ---------------------------------------------------------------------------
// Test 10: Message expiry transitions to failed
// ---------------------------------------------------------------------------

/// Pending messages older than the TTL are transitioned to Failed with
/// "delivery timeout" by the expiry checker.
#[tokio::test(flavor = "multi_thread")]
async fn message_expiry_transitions_to_failed() {
    let (storage, _session_id, _thread_id, _sender_turn_id) = setup_agent_turn("agent-a").await;

    // Create a Delegate message.
    let msg = storage
        .create_agent_message(NewAgentMessage {
            sender_agent_id: "agent-a".into(),
            recipient_agent_id: "agent-b".into(),
            kind: MessageKind::Delegate,
            correlation_id: None,
            payload: serde_json::json!({"task": "expired work"}),
        })
        .await
        .unwrap();
    assert_eq!(msg.status, MessageStatus::Pending);

    // Expire with zero TTL — forces immediate expiry.
    let expired = swage_agent::expiry::expire_stale_messages(
        &(storage.clone() as Arc<dyn Storage>),
        Duration::from_secs(0),
    )
    .await
    .unwrap();
    assert_eq!(expired, 1);

    let updated = storage.get_agent_message(msg.id).await.unwrap().unwrap();
    assert_eq!(updated.status, MessageStatus::Failed);
    assert_eq!(updated.failure_reason.as_deref(), Some("delivery timeout"));
}
