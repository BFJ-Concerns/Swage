// TODO SQLite AgentMessageStore implementation
// tags: forge:inter-agent-communication
// Implement AgentMessageStore for SqliteStorage.
// Methods:
//   create_message — INSERT with UUID, set status='pending', created_at=now
//   get_message — SELECT by id
//   list_pending_for_agent — SELECT WHERE recipient_agent_id=? AND status='pending'
//     ORDER BY created_at ASC
//   update_message_status — UPDATE status, failure_reason, delivered_at atomically.
//     Only transition from 'pending' to terminal state. Return false if already terminal.
//   list_by_correlation — SELECT WHERE correlation_id=? ORDER BY created_at ASC
//   expire_stale_messages — UPDATE status='failed', failure_reason='delivery timeout'
//     WHERE status='pending' AND created_at < (now - max_age). Return row count.
// Follow the pattern in approval.rs — use with_conn for all operations.
// Success: all CRUD operations work, status transitions are atomic and
// idempotent, pending messages are discoverable by recipient.
// ---
