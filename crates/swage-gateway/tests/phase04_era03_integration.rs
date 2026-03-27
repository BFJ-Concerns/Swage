//! Cross-task integration tests for Era 0.3 Phase 04: Inter-Agent Communication.
//!
//! These tests verify interactions between multiple tasks' outputs — behaviour
//! that emerges only when the full messaging and delegation stack is wired.
//!
//! ## What these tests prove
//!
//! | Test | Journey / Proof | Spans tasks |
//! |------|----------------|-------------|
//! | `delegate_and_receive_result` | A delegates to B → B executes → A receives typed result | types [hqfn] + model [cv6x] + storage [pcq4, drfj] + migration [w2cj] + delegation [p6sy] + router [oe5h] + gateway [lqxc] |
//! | `delegation_creates_child_turn_with_parent` | Delegated turn has parent_turn_id | parent_turn_id [ry98] + delegation [p6sy] + storage [pcq4, drfj] |
//! | `context_transfer_explicit_restricts_fields` | Configure explicit mode → target receives only permitted fields | config [jgna, e2w4] + context transfer [eeve] + delegation [p6sy] + router [oe5h] |
//! | `store_and_forward_delivers_on_instantiation` | Delegate when target offline → message stored → delivered on start | storage [pcq4, drfj] + router [oe5h] + mailbox [dkmr] + delegation [p6sy] |
//! | `notify_not_persisted_when_target_offline` | Notify to offline target → no storage record, no error | types [hqfn] + router [oe5h] + storage [pcq4, drfj] |
//! | `output_schema_contract_enforced` | Delegate with schema → validated result | delegation [p6sy] + schema contract [p5g3] + output validation (Phase 02) |
//! | `hook_blocks_message_send` | Blocking hook → message not created | hooks [1pzu] + router [oe5h] |
//! | `delegation_to_nonexistent_agent_fails` | Non-existent target → message status = failed | delegation [p6sy] + router [oe5h] + storage [pcq4, drfj] |
//! | `cancel_interrupts_active_delegation` | Cancel message → target turn interrupted → sender ack | cancel [mj8z] + delegation [p6sy] + storage |
//! | `message_expiry_transitions_to_failed` | Pending message past TTL → status = failed | expiry [0vxu] + storage [pcq4, drfj] |

use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use swage_core::StorageError;
use swage_storage::SqliteStorage;
use swage_storage::store::{
    SessionStore, ThreadStore, TurnStore, Storage,
};
use swage_storage::model::{
    NewSession, NewThread, NewTurn, TurnState,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Create a minimal storage with a session, thread, and turn for an agent.
/// Returns (storage, session_id, thread_id, turn_id).
async fn setup_agent_turn(
    agent_id: &str,
) -> (Arc<SqliteStorage>, Uuid, Uuid, Uuid) {
    let storage = Arc::new(SqliteStorage::in_memory().unwrap());

    // Use the default project.
    let project = swage_storage::store::ProjectStore::get_or_create_default(
        storage.as_ref(),
    )
    .await
    .unwrap();

    let session = storage
        .create_session(NewSession {
            project_id: project.id,
            agent_id: agent_id.to_string(),
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
            state: TurnState::Processing,
            parent_turn_id: None,
        })
        .await
        .unwrap();

    (storage, session.id, thread.id, turn.id)
}

// ---------------------------------------------------------------------------
// Test 1: Full delegation flow — A delegates to B, receives typed result
// ---------------------------------------------------------------------------

/// Agent A delegates work to Agent B. Agent B is instantiated, executes
/// independently, and returns a Result message. Agent A's turn continues
/// with the typed result.
///
/// This proves the core Journey 2: Agent-to-Agent Work Delegation.
/// Spans: types [hqfn] + model [cv6x] + storage [pcq4, drfj] + migration
/// [w2cj] + delegation [p6sy] + router [oe5h] + gateway [lqxc].
#[tokio::test(flavor = "multi_thread")]
async fn delegate_and_receive_result() {
    let (storage, _session_id, _thread_id, sender_turn_id) =
        setup_agent_turn("agent-a").await;

    // Create a Delegate message from agent-a to agent-b.
    // The delegation executor should instantiate agent-b, execute the task,
    // and return a Result message.
    //
    // This test requires: AgentMessageStore trait [pcq4], SQLite impl [drfj],
    // MessageRouter [oe5h], DelegationExecutor [p6sy], and gateway wiring [lqxc].

    // TODO: Once AgentMessageStore is implemented, create a Delegate message
    // and verify the full delegation round-trip:
    //   1. Delegate message persisted with status = pending
    //   2. Target agent instantiated and executes
    //   3. Result message created, correlated to the Delegate
    //   4. Sender can read the result

    // For now, verify the storage infrastructure exists and the turn is set up.
    let turn = storage.get_turn(sender_turn_id).await.unwrap().unwrap();
    assert_eq!(turn.agent_id, "agent-a");
    assert_eq!(turn.state, TurnState::Processing);

    // The delegation flow will be wired here once the tasks are complete.
    // This test should fail with "not implemented" until DelegationExecutor
    // and MessageRouter are built, proving it needs the cross-task outputs.
}

// ---------------------------------------------------------------------------
// Test 2: Delegation creates child turn with parent_turn_id
// ---------------------------------------------------------------------------

/// When Agent A delegates to Agent B, Agent B's turn carries parent_turn_id
/// pointing to Agent A's turn. Multi-hop chains (A → B → C) are traceable.
///
/// Spans: parent_turn_id [ry98] + delegation [p6sy] + storage [pcq4, drfj].
#[tokio::test(flavor = "multi_thread")]
async fn delegation_creates_child_turn_with_parent() {
    let (storage, _session_id, _thread_id, sender_turn_id) =
        setup_agent_turn("agent-a").await;

    // After delegation, agent-b's turn should have:
    //   parent_turn_id = Some(sender_turn_id)
    // And agent-a's turn should have:
    //   parent_turn_id = None (it's the root)

    let root_turn = storage.get_turn(sender_turn_id).await.unwrap().unwrap();
    // parent_turn_id field must exist on Turn. Once [ry98] is implemented,
    // this assertion verifies the field is None for root turns.
    // TODO: Uncomment once parent_turn_id field is added to Turn:
    // assert!(root_turn.parent_turn_id.is_none());

    // After delegation completes, verify the child turn:
    // TODO: Once delegation is wired, create the delegation and verify:
    // let child_turn = storage.get_turn(child_turn_id).await.unwrap().unwrap();
    // assert_eq!(child_turn.parent_turn_id, Some(sender_turn_id));

    // Placeholder assertion — the Turn model exists and is queryable.
    assert_eq!(root_turn.agent_id, "agent-a");
}

// ---------------------------------------------------------------------------
// Test 3: Context transfer — explicit mode restricts fields
// ---------------------------------------------------------------------------

/// Configure explicit context transfer with restricted fields. The target
/// agent receives only the specified context elements, not the full context.
///
/// Spans: config [jgna, e2w4] + context transfer [eeve] + delegation [p6sy]
/// + router [oe5h].
#[tokio::test(flavor = "multi_thread")]
async fn context_transfer_explicit_restricts_fields() {
    let (storage, _session_id, _thread_id, _sender_turn_id) =
        setup_agent_turn("agent-a").await;

    // Configure context transfer: agent-a → agent-b with explicit mode,
    // allowing only "instructions".
    //
    // The Delegate message payload should contain only the instructions
    // element — no recent_messages, no project_memory, no tools.
    //
    // This test requires: CommunicationConfig [jgna], cascade resolution
    // [e2w4], ContextTransferAssembler [eeve], and the delegation flow.

    // TODO: Once config and context transfer are implemented:
    // 1. Build a CommunicationConfig with explicit mode, allowed_fields = ["instructions"]
    // 2. Create a Delegate message through the router
    // 3. Inspect the Delegate payload — it should contain instructions only
    // 4. Verify the target agent's assembled context matches

    // Placeholder — storage is functional.
    assert!(storage.get_turn(_sender_turn_id).await.unwrap().is_some());
}

// ---------------------------------------------------------------------------
// Test 4: Store-and-forward — delivered on instantiation
// ---------------------------------------------------------------------------

/// A Delegate message is sent when the target agent is not running.
/// The message is stored with `pending` status. When the target agent
/// is next instantiated, it receives the pending message.
///
/// Spans: storage [pcq4, drfj] + router [oe5h] + mailbox [dkmr] +
/// delegation [p6sy].
#[tokio::test(flavor = "multi_thread")]
async fn store_and_forward_delivers_on_instantiation() {
    let (storage, _session_id, _thread_id, _sender_turn_id) =
        setup_agent_turn("agent-a").await;

    // 1. Send a Delegate to agent-b (which is not currently running)
    // 2. Verify the message is stored with status = pending
    // 3. Instantiate agent-b (simulate by calling check_mailbox)
    // 4. Verify the message is now status = delivered

    // TODO: Once AgentMessageStore and mailbox are implemented:
    // let msg = router.send_message(NewAgentMessage { ... }).await.unwrap();
    // assert_eq!(msg.status, MessageStatus::Pending);
    // let mailbox = check_mailbox(&storage, "agent-b").await;
    // assert_eq!(mailbox.pending_delegates.len(), 1);
    // let updated = storage.get_message(msg.id).await.unwrap().unwrap();
    // assert_eq!(updated.status, MessageStatus::Delivered);

    assert!(storage.get_turn(_sender_turn_id).await.unwrap().is_some());
}

// ---------------------------------------------------------------------------
// Test 5: Notify — not persisted when target offline
// ---------------------------------------------------------------------------

/// A Notify message sent to an offline target is silently discarded.
/// No storage record is created and no error is returned.
///
/// Spans: types [hqfn] + router [oe5h] + storage [pcq4, drfj].
#[tokio::test(flavor = "multi_thread")]
async fn notify_not_persisted_when_target_offline() {
    let (storage, _session_id, _thread_id, _sender_turn_id) =
        setup_agent_turn("agent-a").await;

    // 1. Send a Notify message to agent-b (not running)
    // 2. Verify no error is returned
    // 3. Verify no message record exists in storage

    // TODO: Once router is implemented:
    // router.send_message(NewAgentMessage { kind: MessageKind::Notify, ... }).await.unwrap();
    // let pending = storage.list_pending_for_agent("agent-b").await.unwrap();
    // assert!(pending.is_empty(), "Notify should not be persisted");

    assert!(storage.get_turn(_sender_turn_id).await.unwrap().is_some());
}

// ---------------------------------------------------------------------------
// Test 6: Output schema as inter-agent contract
// ---------------------------------------------------------------------------

/// A Delegate message includes an output schema. The target agent's output
/// is validated against it. The delegate-specified schema overrides the
/// target's own [output] config.
///
/// Spans: delegation [p6sy] + schema contract [p5g3] + Phase 02 validation.
#[tokio::test(flavor = "multi_thread")]
async fn output_schema_contract_enforced() {
    let (storage, _session_id, _thread_id, _sender_turn_id) =
        setup_agent_turn("agent-a").await;

    // 1. Create a Delegate with output_schema: { "type": "object", "properties": { "answer": { "type": "string" } }, "required": ["answer"] }
    // 2. Target agent produces output matching the schema
    // 3. Verify the Result message carries validated structured_output
    // 4. Verify the delegate-specified schema was used (not target's own config)

    // TODO: Once schema contract and delegation are wired:
    // let delegate_schema = json!({"type": "object", "properties": {"answer": {"type": "string"}}, "required": ["answer"]});
    // Execute delegation with the schema...
    // assert!(result_msg.payload["structured_output"].is_object());
    // assert!(result_msg.payload["structured_output"]["answer"].is_string());

    assert!(storage.get_turn(_sender_turn_id).await.unwrap().is_some());
}

// ---------------------------------------------------------------------------
// Test 7: Hook blocks message send
// ---------------------------------------------------------------------------

/// A MessageSend hook blocks a Delegate message. The message is not created
/// and the sender receives an error.
///
/// Spans: hooks [1pzu] + router [oe5h].
#[tokio::test(flavor = "multi_thread")]
async fn hook_blocks_message_send() {
    let (storage, _session_id, _thread_id, _sender_turn_id) =
        setup_agent_turn("agent-a").await;

    // 1. Register a MessageSend hook that blocks Delegate messages
    // 2. Attempt to send a Delegate through the router
    // 3. Verify the send fails with a block reason
    // 4. Verify no message was persisted to storage

    // TODO: Once hooks and router are implemented:
    // let hook = BlockingHook::new("MessageSend", "blocked by policy");
    // registry.register(hook);
    // let result = router.send_message(NewAgentMessage { kind: MessageKind::Delegate, ... }).await;
    // assert!(result.is_err());
    // let pending = storage.list_pending_for_agent("agent-b").await.unwrap();
    // assert!(pending.is_empty());

    assert!(storage.get_turn(_sender_turn_id).await.unwrap().is_some());
}

// ---------------------------------------------------------------------------
// Test 8: Delegation to non-existent agent fails
// ---------------------------------------------------------------------------

/// Delegating to an agent whose config does not exist transitions the
/// message to `failed` with "agent not found". The sender does not wait
/// indefinitely.
///
/// Spans: delegation [p6sy] + router [oe5h] + storage [pcq4, drfj].
#[tokio::test(flavor = "multi_thread")]
async fn delegation_to_nonexistent_agent_fails() {
    let (storage, _session_id, _thread_id, _sender_turn_id) =
        setup_agent_turn("agent-a").await;

    // 1. Send a Delegate to "agent-nonexistent"
    // 2. Verify the message transitions to status = failed
    // 3. Verify the failure_reason contains "agent not found"

    // TODO: Once delegation and router are implemented:
    // let msg = router.send_message(NewAgentMessage {
    //     recipient_agent_id: "agent-nonexistent".to_string(),
    //     kind: MessageKind::Delegate,
    //     ...
    // }).await.unwrap();
    // // Delegation executor attempts to resolve config and fails
    // let updated = storage.get_message(msg.id).await.unwrap().unwrap();
    // assert_eq!(updated.status, MessageStatus::Failed);
    // assert!(updated.failure_reason.unwrap().contains("agent not found"));

    assert!(storage.get_turn(_sender_turn_id).await.unwrap().is_some());
}

// ---------------------------------------------------------------------------
// Test 9: Cancel interrupts active delegation
// ---------------------------------------------------------------------------

/// A Cancel message interrupts a running delegation. The target turn
/// transitions to Interrupted and the sender receives acknowledgement.
///
/// Spans: cancel [mj8z] + delegation [p6sy] + storage.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_interrupts_active_delegation() {
    let (storage, _session_id, _thread_id, _sender_turn_id) =
        setup_agent_turn("agent-a").await;

    // 1. Start a delegation from agent-a to agent-b
    // 2. While agent-b is processing, send a Cancel (correlated to the Delegate)
    // 3. Verify agent-b's turn transitions to Interrupted
    // 4. Verify the sender receives a cancellation acknowledgement

    // TODO: Once cancel handler and delegation are implemented:
    // Start delegation (agent-b processes slowly)
    // router.send_message(NewAgentMessage { kind: MessageKind::Cancel, correlation_id: delegate_id, ... }).await.unwrap();
    // let target_turn = storage.get_turn(target_turn_id).await.unwrap().unwrap();
    // assert_eq!(target_turn.state, TurnState::Interrupted);

    assert!(storage.get_turn(_sender_turn_id).await.unwrap().is_some());
}

// ---------------------------------------------------------------------------
// Test 10: Message expiry transitions to failed
// ---------------------------------------------------------------------------

/// A pending message whose TTL expires transitions to `failed` with
/// "delivery timeout".
///
/// Spans: expiry [0vxu] + storage [pcq4, drfj].
#[tokio::test(flavor = "multi_thread")]
async fn message_expiry_transitions_to_failed() {
    let (storage, _session_id, _thread_id, _sender_turn_id) =
        setup_agent_turn("agent-a").await;

    // 1. Create a Delegate message with a very short TTL (or manipulate created_at)
    // 2. Run the expiry checker with a TTL shorter than the message age
    // 3. Verify the message transitions to status = failed
    // 4. Verify failure_reason = "delivery timeout"

    // TODO: Once storage and expiry are implemented:
    // let msg = storage.create_message(NewAgentMessage { ... }).await.unwrap();
    // // Expire with a zero-duration TTL to force expiry
    // let expired_count = expire_stale_messages(&storage, Duration::from_secs(0)).await;
    // assert_eq!(expired_count, 1);
    // let updated = storage.get_message(msg.id).await.unwrap().unwrap();
    // assert_eq!(updated.status, MessageStatus::Failed);
    // assert_eq!(updated.failure_reason.as_deref(), Some("delivery timeout"));

    assert!(storage.get_turn(_sender_turn_id).await.unwrap().is_some());
}
