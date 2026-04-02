use async_trait::async_trait;
use uuid::Uuid;

use swage_core::StorageError;

use crate::model::{
    ActiveSkillRecord, AgentMessage, ApprovalRequest, ApprovalState, EdgeEvaluation, Message,
    NewAgentMessage, NewApprovalRequest, NewEdgeEvaluation, NewMessage, NewNodeExecution,
    NewNodeRetry, NewProject, NewSession, NewTeam, NewThread, NewToolCall, NewTopologyRun,
    NewTurn, NodeExecution, NodeExecutionState, NodeRetry, Project, Session, Team, Thread,
    ThreadState, ToolCallRecord, TopologyRun, TopologyRunState, Turn, TurnState,
};

// ---------------------------------------------------------------------------
// Store sub-traits
// ---------------------------------------------------------------------------

#[async_trait]
pub trait ProjectStore: Send + Sync {
    async fn create(&self, project: NewProject) -> Result<Project, StorageError>;
    async fn get(&self, id: Uuid) -> Result<Option<Project>, StorageError>;
    async fn get_by_name(&self, name: &str) -> Result<Option<Project>, StorageError>;
    /// Get the default project, creating it if it does not exist.
    async fn get_or_create_default(&self) -> Result<Project, StorageError>;
    /// Return all projects ordered by created_at ascending.
    async fn list_all(&self) -> Result<Vec<Project>, StorageError>;
    /// Look up a project by its slug.
    async fn get_by_slug(&self, slug: &str) -> Result<Option<Project>, StorageError>;
    /// Partially update a project. Only `Some` fields are changed.
    ///
    /// Returns the updated project, or `StorageError::NotFound` if `id` is unknown.
    async fn update(
        &self,
        id: Uuid,
        name: Option<String>,
        slug: Option<String>,
        workspace_path: Option<String>,
    ) -> Result<Project, StorageError>;
    /// Delete a project and cascade its sessions.
    ///
    /// Returns `true` if the project existed and was removed, `false` if not found.
    /// Returns `StorageError::ConstraintViolation` if called on the default project.
    async fn delete(&self, id: Uuid) -> Result<bool, StorageError>;
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(&self, session: NewSession) -> Result<Session, StorageError>;
    async fn get_session(&self, id: Uuid) -> Result<Option<Session>, StorageError>;
    /// List sessions in the project ordered by updated_at DESC.
    async fn list_by_project(&self, project_id: Uuid) -> Result<Vec<Session>, StorageError>;
    /// List sessions belonging to an agent, ordered by updated_at DESC.
    async fn list_sessions_by_agent_id(&self, agent_id: &str) -> Result<Vec<Session>, StorageError>;
    /// Delete a session. Returns true if it existed and was removed.
    async fn delete_session(&self, id: Uuid) -> Result<bool, StorageError>;
    /// Bump the session's updated_at to now.
    async fn update_activity(&self, id: Uuid) -> Result<(), StorageError>;
}

#[async_trait]
pub trait TeamStore: Send + Sync {
    async fn create_team(&self, team: NewTeam) -> Result<Team, StorageError>;
    async fn get_team(&self, id: Uuid) -> Result<Option<Team>, StorageError>;
    async fn get_team_by_name(&self, project_id: Uuid, name: &str) -> Result<Option<Team>, StorageError>;
    /// List teams in a project ordered by created_at ascending.
    async fn list_teams_by_project(&self, project_id: Uuid) -> Result<Vec<Team>, StorageError>;
    /// Partially update a team. Only `Some` fields are changed.
    async fn update_team(
        &self,
        id: Uuid,
        name: Option<String>,
        description: Option<String>,
        entry_agent_id: Option<String>,
        members: Option<Vec<String>>,
    ) -> Result<Team, StorageError>;
    /// Delete a team. Returns true if it existed and was removed.
    async fn delete_team(&self, id: Uuid) -> Result<bool, StorageError>;
}

#[async_trait]
pub trait TopologyTraceStore: Send + Sync {
    async fn create_topology_run(&self, run: NewTopologyRun) -> Result<TopologyRun, StorageError>;
    async fn get_topology_run(&self, id: Uuid) -> Result<Option<TopologyRun>, StorageError>;
    async fn list_topology_runs_by_team(&self, team_id: Uuid) -> Result<Vec<TopologyRun>, StorageError>;
    async fn list_topology_runs_by_session(&self, session_id: Uuid) -> Result<Vec<TopologyRun>, StorageError>;
    async fn update_topology_run(
        &self,
        id: Uuid,
        state: TopologyRunState,
        final_output: Option<&serde_json::Value>,
        error_message: Option<&str>,
    ) -> Result<(), StorageError>;

    async fn create_node_execution(
        &self,
        execution: NewNodeExecution,
    ) -> Result<NodeExecution, StorageError>;
    async fn get_node_execution(&self, id: Uuid) -> Result<Option<NodeExecution>, StorageError>;
    async fn list_node_executions_by_run(
        &self,
        topology_run_id: Uuid,
    ) -> Result<Vec<NodeExecution>, StorageError>;
    async fn update_node_execution(
        &self,
        id: Uuid,
        state: NodeExecutionState,
        output_json: Option<&serde_json::Value>,
        error_message: Option<&str>,
    ) -> Result<(), StorageError>;

    async fn create_edge_evaluation(
        &self,
        evaluation: NewEdgeEvaluation,
    ) -> Result<EdgeEvaluation, StorageError>;
    async fn list_edge_evaluations_by_run(
        &self,
        topology_run_id: Uuid,
    ) -> Result<Vec<EdgeEvaluation>, StorageError>;

    async fn create_node_retry(&self, retry: NewNodeRetry) -> Result<NodeRetry, StorageError>;
    async fn list_node_retries(
        &self,
        node_execution_id: Uuid,
    ) -> Result<Vec<NodeRetry>, StorageError>;
}

#[async_trait]
pub trait TurnStore: Send + Sync {
    async fn create_turn(&self, turn: NewTurn) -> Result<Turn, StorageError>;
    async fn get_turn(&self, id: Uuid) -> Result<Option<Turn>, StorageError>;
    /// List all turns for a thread ordered by created_at ascending.
    async fn list_turns_by_thread(&self, thread_id: Uuid) -> Result<Vec<Turn>, StorageError>;
    /// Update a turn's state.
    ///
    /// State is consumer-driven — the storage layer records whatever state is
    /// provided. No state-machine enforcement is applied here.
    async fn update_turn_state(&self, id: Uuid, state: TurnState) -> Result<(), StorageError>;
    /// Find all active turns for a given agent (Pending, Processing, or AwaitingApproval).
    ///
    /// Used by startup recovery, shutdown, and cancel to identify in-flight turns.
    async fn find_in_progress_by_agent(&self, agent_id: &str) -> Result<Vec<Turn>, StorageError>;
    /// Persist the assistant's response text onto a completed turn.
    async fn update_response(&self, id: Uuid, response: &str) -> Result<(), StorageError>;
    /// Persist token counts after the LLM call completes.
    async fn update_tokens(&self, id: Uuid, input: i64, output: i64) -> Result<(), StorageError>;
    /// Persist the computed cost after pricing is applied.
    ///
    /// `cost` is in microcents (see `PricingConfig::compute_cost`). Only called
    /// when pricing was configured for the model; absent turns remain NULL.
    async fn update_cost(&self, id: Uuid, cost: i64) -> Result<(), StorageError>;
    /// Persist the validated structured output for a completed turn.
    ///
    /// Called when output schema validation succeeds. The `output` value is
    /// serialised as JSON text for storage. Subsequent `get_turn` calls will
    /// return it deserialised in `Turn::structured_output`.
    async fn update_structured_output(
        &self,
        id: Uuid,
        output: &serde_json::Value,
    ) -> Result<(), StorageError>;
}

/// Store for message CRUD operations.
///
/// Messages belong to threads (not turns). `position` is monotonically
/// increasing within a thread and is used to reconstruct conversation order.
#[async_trait]
pub trait MessageStore: Send + Sync {
    /// Persist a new message. Returns the stored record with its assigned id.
    async fn create_message(&self, message: NewMessage) -> Result<Message, StorageError>;

    /// Fetch all messages in a thread ordered by position ascending.
    async fn list_messages_by_thread(&self, thread_id: Uuid) -> Result<Vec<Message>, StorageError>;

    /// Fetch the most recent message in a session, across all its threads.
    ///
    /// Used for conversation summaries. Returns `None` if the session has
    /// no messages yet.
    async fn get_latest_for_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<Message>, StorageError>;

    /// Fetch a single message by id. Returns `None` if not found.
    async fn get_message(&self, id: Uuid) -> Result<Option<Message>, StorageError>;

    /// Delete all messages belonging to a thread.
    ///
    /// Called when the thread itself is deleted, as a complement to the
    /// database's ON DELETE CASCADE (useful when callers need to know the
    /// count of removed rows or trigger application-level cleanup).
    async fn delete_by_thread(&self, thread_id: Uuid) -> Result<(), StorageError>;
}

#[async_trait]
pub trait ToolCallStore: Send + Sync {
    /// Insert a new tool call record. Returns the persisted record with its assigned id.
    async fn create_tool_call(
        &self,
        tool_call: NewToolCall,
    ) -> Result<ToolCallRecord, StorageError>;

    /// Fetch a single tool call by id. Returns `None` if not found.
    async fn get_tool_call(&self, id: Uuid) -> Result<Option<ToolCallRecord>, StorageError>;

    /// Write the completed result back onto an in-progress tool call.
    ///
    /// Matched by `call_id` scoped to `turn_id` to avoid cross-turn collisions.
    async fn update_tool_call_result(
        &self,
        call_id: &str,
        turn_id: Uuid,
        result: &str,
        status: &str,
        duration_ms: i64,
        truncated: bool,
        total_bytes: Option<i64>,
    ) -> Result<(), StorageError>;

    /// Fetch all tool calls for a turn, ordered by position ascending.
    async fn list_tool_calls_by_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<Vec<ToolCallRecord>, StorageError>;
}

/// Store for context assembly report snapshots.
///
/// One snapshot per turn — stores the serialised JSON of an `AssemblyReport`
/// produced by the context assembly engine. Using raw JSON avoids coupling the
/// storage layer to the `AssemblyReport` type.
#[async_trait]
pub trait ContextSnapshotStore: Send + Sync {
    /// Persist an assembly report for `turn_id`. Replaces any existing entry.
    async fn store_snapshot(&self, turn_id: Uuid, report_json: &str) -> Result<(), StorageError>;
    /// Fetch the raw report JSON for `turn_id`. Returns `None` if no snapshot
    /// was stored for that turn.
    async fn get_snapshot(&self, turn_id: Uuid) -> Result<Option<String>, StorageError>;
}

/// Store for thread CRUD operations.
///
/// Threads scope a single agent's execution within a session. Each agent
/// invocation gets its own thread; the thread's state machine tracks whether
/// the agent is currently active.
#[async_trait]
pub trait ThreadStore: Send + Sync {
    /// Persist a new thread. Returns the stored record with its assigned id.
    async fn create(&self, thread: NewThread) -> Result<Thread, StorageError>;

    /// Fetch a thread by id. Returns `None` if not found.
    async fn get(&self, id: Uuid) -> Result<Option<Thread>, StorageError>;

    /// List all threads for a session ordered by created_at ascending.
    async fn list_by_session(&self, session_id: Uuid) -> Result<Vec<Thread>, StorageError>;

    /// List all threads owned by an agent, ordered by created_at ascending.
    async fn list_threads_by_agent_id(&self, agent_id: &str) -> Result<Vec<Thread>, StorageError>;

    /// Transition a thread to a new state.
    ///
    /// Implementations must fetch the current state and reject invalid
    /// transitions with `StorageError::ConstraintViolation`. Returns
    /// `StorageError::NotFound` if `id` is unknown.
    async fn update_state(&self, id: Uuid, state: ThreadState) -> Result<(), StorageError>;

    /// Delete a thread. Returns `true` if it existed and was removed.
    async fn delete(&self, id: Uuid) -> Result<bool, StorageError>;
}

/// Store for per-session skill activation state.
///
/// Tracks which skills are currently active for a session. State survives
/// client disconnections and gateway restarts because it is persisted in
/// the `active_skills` table, which cascades on session delete.
#[async_trait]
pub trait SkillActivationStore: Send + Sync {
    /// Mark a skill as active for a session. Idempotent — calling this for
    /// an already-active skill is a no-op (INSERT OR IGNORE).
    async fn activate_skill(&self, session_id: Uuid, skill_name: &str) -> Result<(), StorageError>;

    /// Remove a skill from the active set.
    ///
    /// Returns `true` if the skill was active and has been removed, or `false`
    /// if the skill was not in the active set (nothing to remove).
    async fn deactivate_skill(
        &self,
        session_id: Uuid,
        skill_name: &str,
    ) -> Result<bool, StorageError>;

    /// List all skills currently active for a session, ordered by activation
    /// time ascending (the order in which they were activated).
    async fn list_active_skills(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<ActiveSkillRecord>, StorageError>;

    /// Remove all active skills for a session in one operation.
    ///
    /// Used when resetting or closing a session without deleting it.
    async fn clear_active_skills(&self, session_id: Uuid) -> Result<(), StorageError>;
}

/// Store for human-in-the-loop approval requests.
///
/// Each approval request is created when an agent turn hits an approval gate
/// (tool dispatch or hook). The turn is suspended in
/// `TurnState::AwaitingApproval` until the request reaches a terminal state
/// (Approved / Rejected / Expired).
#[async_trait]
pub trait ApprovalStore: Send + Sync {
    /// Persist a new approval request. Assigns `id`, sets `state = Pending`,
    /// and leaves `decided_at` as `None`. Returns the stored record.
    async fn create_approval(
        &self,
        approval: NewApprovalRequest,
    ) -> Result<ApprovalRequest, StorageError>;

    /// Fetch a single approval request by id. Returns `None` if not found.
    async fn get_approval(&self, id: Uuid) -> Result<Option<ApprovalRequest>, StorageError>;

    /// List all pending approval requests for a specific agent.
    ///
    /// Used by the agent runtime to check which of its own turns are
    /// suspended. Ordered by `created_at` ascending.
    async fn list_pending_by_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<ApprovalRequest>, StorageError>;

    /// List pending approval requests scoped to a specific session.
    ///
    /// Joins through turns → threads → session to return only approvals whose
    /// turn belongs to the given session. Used by the WebSocket handler to
    /// re-emit approvals without leaking cross-session data.
    async fn list_pending_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<ApprovalRequest>, StorageError>;

    /// List all pending approval requests across all agents.
    ///
    /// Used during startup recovery and by the gateway to re-emit pending
    /// approvals to newly connected clients.
    async fn list_all_pending(&self) -> Result<Vec<ApprovalRequest>, StorageError>;

    /// Transition an approval request from Pending to `state`.
    ///
    /// Sets `decision_reason` and `decided_at` atomically. Returns `true` if
    /// the update succeeded (the request was in Pending state), or `false` if
    /// the request was already in a terminal state (concurrent decision). Only
    /// non-Pending → terminal transitions are valid.
    async fn update_approval_state(
        &self,
        id: Uuid,
        state: ApprovalState,
        reason: Option<&str>,
    ) -> Result<bool, StorageError>;

    /// Mark as Expired any pending approval whose parent turn no longer exists.
    ///
    /// Called during startup recovery to handle the case where an agent was
    /// killed with approvals in flight. Returns the number of rows updated.
    async fn cleanup_orphaned_approvals(&self) -> Result<u64, StorageError>;
}

/// Persistent mailbox for inter-agent messages.
///
/// Follows actor-model semantics: messages are store-and-forward for durable
/// kinds (Delegate, Result, Cancel, Shutdown) and at-most-once for Notify.
/// Status transitions are atomic — only one transition from Pending to a
/// terminal state succeeds.
#[async_trait]
pub trait AgentMessageStore: Send + Sync {
    /// Persist a new agent message with status = Pending.
    async fn create_agent_message(
        &self,
        msg: NewAgentMessage,
    ) -> Result<AgentMessage, StorageError>;

    /// Retrieve a single message by ID.
    async fn get_agent_message(&self, id: Uuid) -> Result<Option<AgentMessage>, StorageError>;

    /// List all pending messages for a recipient agent, ordered by creation time.
    ///
    /// Used by the mailbox on agent instantiation to discover queued work.
    async fn list_pending_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<AgentMessage>, StorageError>;

    /// Atomically transition a message to a terminal status (Delivered or Failed).
    ///
    /// Returns `true` if the transition succeeded (message was Pending), `false`
    /// if the message was already in a terminal state (concurrent delivery).
    /// Sets `delivered_at` when transitioning to Delivered.
    async fn update_agent_message_status(
        &self,
        id: Uuid,
        status: swage_core::MessageStatus,
        failure_reason: Option<&str>,
    ) -> Result<bool, StorageError>;

    /// Update the payload of an existing message.
    ///
    /// Used by `DelegationExecutor` to annotate delegate messages with the
    /// child thread_id after creation, enabling scoped cancel fallback.
    async fn update_agent_message_payload(
        &self,
        id: Uuid,
        payload: serde_json::Value,
    ) -> Result<(), StorageError>;

    /// List all messages sharing a correlation ID, ordered by creation time.
    ///
    /// Used to trace delegation chains (Delegate → Result/Cancel).
    async fn list_by_correlation(
        &self,
        correlation_id: Uuid,
    ) -> Result<Vec<AgentMessage>, StorageError>;

    /// Expire pending messages older than `max_age`, transitioning them to
    /// Failed with reason "delivery timeout". Returns the count of expired messages.
    async fn expire_stale_messages(
        &self,
        max_age: std::time::Duration,
    ) -> Result<u64, StorageError>;
}

// ---------------------------------------------------------------------------
// Storage supertrait
// ---------------------------------------------------------------------------

/// Combined storage interface implemented by `SqliteStorage`.
///
/// `ProjectStore` is not included — it is independently available and consumers
/// that need project operations import it separately.
pub trait Storage:
    TeamStore
    + SessionStore
    + ThreadStore
    + TurnStore
    + MessageStore
    + ToolCallStore
    + ContextSnapshotStore
    + SkillActivationStore
    + ApprovalStore
    + AgentMessageStore
    + TopologyTraceStore
    + Send
    + Sync
{
}

/// Blanket implementation so any type satisfying all sub-traits gets Storage for free.
impl<T> Storage for T where
    T: TeamStore
        + SessionStore
        + ThreadStore
        + TurnStore
        + MessageStore
        + ToolCallStore
        + ContextSnapshotStore
        + SkillActivationStore
        + ApprovalStore
        + AgentMessageStore
        + TopologyTraceStore
        + Send
        + Sync
{
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewToolCall, ToolCallRecord};
    use crate::store::ContextSnapshotStore;

    /// Compile-time check: a type implementing all sub-traits satisfies Storage.
    ///
    /// This struct exists only to verify the trait bounds compile correctly.
    /// It panics if any method is actually called.
    struct StubStorage;

    #[async_trait]
    impl ProjectStore for StubStorage {
        async fn create(&self, _: NewProject) -> Result<Project, StorageError> {
            unimplemented!()
        }
        async fn get(&self, _: Uuid) -> Result<Option<Project>, StorageError> {
            unimplemented!()
        }
        async fn get_by_name(&self, _: &str) -> Result<Option<Project>, StorageError> {
            unimplemented!()
        }
        async fn get_or_create_default(&self) -> Result<Project, StorageError> {
            unimplemented!()
        }
        async fn list_all(&self) -> Result<Vec<Project>, StorageError> {
            unimplemented!()
        }
        async fn get_by_slug(&self, _: &str) -> Result<Option<Project>, StorageError> {
            unimplemented!()
        }
        async fn update(&self, _: Uuid, _: Option<String>, _: Option<String>, _: Option<String>) -> Result<Project, StorageError> {
            unimplemented!()
        }
        async fn delete(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl SessionStore for StubStorage {
        async fn create_session(&self, _: NewSession) -> Result<Session, StorageError> {
            unimplemented!()
        }
        async fn get_session(&self, _: Uuid) -> Result<Option<Session>, StorageError> {
            unimplemented!()
        }
        async fn list_by_project(&self, _: Uuid) -> Result<Vec<Session>, StorageError> {
            unimplemented!()
        }
        async fn list_sessions_by_agent_id(&self, _: &str) -> Result<Vec<Session>, StorageError> {
            unimplemented!()
        }
        async fn delete_session(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn update_activity(&self, _: Uuid) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl TeamStore for StubStorage {
        async fn create_team(&self, _: NewTeam) -> Result<Team, StorageError> {
            unimplemented!()
        }
        async fn get_team(&self, _: Uuid) -> Result<Option<Team>, StorageError> {
            unimplemented!()
        }
        async fn get_team_by_name(&self, _: Uuid, _: &str) -> Result<Option<Team>, StorageError> {
            unimplemented!()
        }
        async fn list_teams_by_project(&self, _: Uuid) -> Result<Vec<Team>, StorageError> {
            unimplemented!()
        }
        async fn update_team(
            &self,
            _: Uuid,
            _: Option<String>,
            _: Option<String>,
            _: Option<String>,
            _: Option<Vec<String>>,
        ) -> Result<Team, StorageError> {
            unimplemented!()
        }
        async fn delete_team(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl TopologyTraceStore for StubStorage {
        async fn create_topology_run(&self, _: NewTopologyRun) -> Result<TopologyRun, StorageError> {
            unimplemented!()
        }
        async fn get_topology_run(&self, _: Uuid) -> Result<Option<TopologyRun>, StorageError> {
            unimplemented!()
        }
        async fn list_topology_runs_by_team(&self, _: Uuid) -> Result<Vec<TopologyRun>, StorageError> {
            unimplemented!()
        }
        async fn list_topology_runs_by_session(&self, _: Uuid) -> Result<Vec<TopologyRun>, StorageError> {
            unimplemented!()
        }
        async fn update_topology_run(
            &self,
            _: Uuid,
            _: TopologyRunState,
            _: Option<&serde_json::Value>,
            _: Option<&str>,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn create_node_execution(
            &self,
            _: NewNodeExecution,
        ) -> Result<NodeExecution, StorageError> {
            unimplemented!()
        }
        async fn get_node_execution(&self, _: Uuid) -> Result<Option<NodeExecution>, StorageError> {
            unimplemented!()
        }
        async fn list_node_executions_by_run(
            &self,
            _: Uuid,
        ) -> Result<Vec<NodeExecution>, StorageError> {
            unimplemented!()
        }
        async fn update_node_execution(
            &self,
            _: Uuid,
            _: NodeExecutionState,
            _: Option<&serde_json::Value>,
            _: Option<&str>,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn create_edge_evaluation(
            &self,
            _: NewEdgeEvaluation,
        ) -> Result<EdgeEvaluation, StorageError> {
            unimplemented!()
        }
        async fn list_edge_evaluations_by_run(
            &self,
            _: Uuid,
        ) -> Result<Vec<EdgeEvaluation>, StorageError> {
            unimplemented!()
        }
        async fn create_node_retry(&self, _: NewNodeRetry) -> Result<NodeRetry, StorageError> {
            unimplemented!()
        }
        async fn list_node_retries(&self, _: Uuid) -> Result<Vec<NodeRetry>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl TurnStore for StubStorage {
        async fn create_turn(&self, _: NewTurn) -> Result<Turn, StorageError> {
            unimplemented!()
        }
        async fn get_turn(&self, _: Uuid) -> Result<Option<Turn>, StorageError> {
            unimplemented!()
        }
        async fn list_turns_by_thread(&self, _thread_id: Uuid) -> Result<Vec<Turn>, StorageError> {
            unimplemented!()
        }
        async fn update_turn_state(&self, _: Uuid, _: TurnState) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn find_in_progress_by_agent(&self, _agent_id: &str) -> Result<Vec<Turn>, StorageError> {
            unimplemented!()
        }
        async fn update_response(&self, _id: Uuid, _response: &str) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn update_tokens(&self, _id: Uuid, _input: i64, _output: i64) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn update_cost(&self, _id: Uuid, _cost: i64) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn update_structured_output(
            &self,
            _id: Uuid,
            _output: &serde_json::Value,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ContextSnapshotStore for StubStorage {
        async fn store_snapshot(&self, _turn_id: Uuid, _report_json: &str) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn get_snapshot(&self, _turn_id: Uuid) -> Result<Option<String>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl MessageStore for StubStorage {
        async fn create_message(&self, _: NewMessage) -> Result<Message, StorageError> {
            unimplemented!()
        }
        async fn list_messages_by_thread(&self, _: Uuid) -> Result<Vec<Message>, StorageError> {
            unimplemented!()
        }
        async fn get_latest_for_session(&self, _: Uuid) -> Result<Option<Message>, StorageError> {
            unimplemented!()
        }
        async fn get_message(&self, _: Uuid) -> Result<Option<Message>, StorageError> {
            unimplemented!()
        }
        async fn delete_by_thread(&self, _: Uuid) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ThreadStore for StubStorage {
        async fn create(&self, _: NewThread) -> Result<Thread, StorageError> {
            unimplemented!()
        }
        async fn get(&self, _: Uuid) -> Result<Option<Thread>, StorageError> {
            unimplemented!()
        }
        async fn list_by_session(&self, _: Uuid) -> Result<Vec<Thread>, StorageError> {
            unimplemented!()
        }
        async fn list_threads_by_agent_id(&self, _: &str) -> Result<Vec<Thread>, StorageError> {
            unimplemented!()
        }
        async fn update_state(&self, _: Uuid, _: ThreadState) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn delete(&self, _: Uuid) -> Result<bool, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ToolCallStore for StubStorage {
        async fn create_tool_call(
            &self,
            _: NewToolCall,
        ) -> Result<ToolCallRecord, StorageError> {
            unimplemented!()
        }
        async fn get_tool_call(
            &self,
            _: Uuid,
        ) -> Result<Option<ToolCallRecord>, StorageError> {
            unimplemented!()
        }
        async fn update_tool_call_result(
            &self,
            _call_id: &str,
            _turn_id: Uuid,
            _result: &str,
            _status: &str,
            _duration_ms: i64,
            _truncated: bool,
            _total_bytes: Option<i64>,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn list_tool_calls_by_turn(
            &self,
            _: Uuid,
        ) -> Result<Vec<ToolCallRecord>, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl SkillActivationStore for StubStorage {
        async fn activate_skill(&self, _: Uuid, _: &str) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn deactivate_skill(&self, _: Uuid, _: &str) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn list_active_skills(&self, _: Uuid) -> Result<Vec<ActiveSkillRecord>, StorageError> {
            unimplemented!()
        }
        async fn clear_active_skills(&self, _: Uuid) -> Result<(), StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl ApprovalStore for StubStorage {
        async fn create_approval(
            &self,
            _: NewApprovalRequest,
        ) -> Result<ApprovalRequest, StorageError> {
            unimplemented!()
        }
        async fn get_approval(&self, _: Uuid) -> Result<Option<ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn list_pending_by_agent(
            &self,
            _: &str,
        ) -> Result<Vec<ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn list_pending_by_session(
            &self,
            _: Uuid,
        ) -> Result<Vec<ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn list_all_pending(&self) -> Result<Vec<ApprovalRequest>, StorageError> {
            unimplemented!()
        }
        async fn update_approval_state(
            &self,
            _: Uuid,
            _: ApprovalState,
            _: Option<&str>,
        ) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn cleanup_orphaned_approvals(&self) -> Result<u64, StorageError> {
            unimplemented!()
        }
    }

    #[async_trait]
    impl AgentMessageStore for StubStorage {
        async fn create_agent_message(
            &self,
            _: NewAgentMessage,
        ) -> Result<AgentMessage, StorageError> {
            unimplemented!()
        }
        async fn get_agent_message(&self, _: Uuid) -> Result<Option<AgentMessage>, StorageError> {
            unimplemented!()
        }
        async fn list_pending_for_agent(
            &self,
            _: &str,
        ) -> Result<Vec<AgentMessage>, StorageError> {
            unimplemented!()
        }
        async fn update_agent_message_status(
            &self,
            _: Uuid,
            _: swage_core::MessageStatus,
            _: Option<&str>,
        ) -> Result<bool, StorageError> {
            unimplemented!()
        }
        async fn update_agent_message_payload(
            &self,
            _: Uuid,
            _: serde_json::Value,
        ) -> Result<(), StorageError> {
            unimplemented!()
        }
        async fn list_by_correlation(
            &self,
            _: Uuid,
        ) -> Result<Vec<AgentMessage>, StorageError> {
            unimplemented!()
        }
        async fn expire_stale_messages(
            &self,
            _: std::time::Duration,
        ) -> Result<u64, StorageError> {
            unimplemented!()
        }
    }

    // --- MessageStore: thread-ownership API compile checks ---

    /// list_messages_by_thread exists on MessageStore and is callable via T: MessageStore.
    #[test]
    fn message_store_list_messages_by_thread_compile_check() {
        fn _assert<T: MessageStore>(t: &T) {
            let _ = t.list_messages_by_thread(Uuid::new_v4());
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// get_message exists on MessageStore.
    #[test]
    fn message_store_get_message_compile_check() {
        fn _assert<T: MessageStore>(t: &T) {
            let _ = t.get_message(Uuid::new_v4());
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// delete_by_thread exists on MessageStore.
    #[test]
    fn message_store_delete_by_thread_compile_check() {
        fn _assert<T: MessageStore>(t: &T) {
            let _ = t.delete_by_thread(Uuid::new_v4());
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// get_latest_for_session (renamed from get_latest_message_for_session) exists.
    #[test]
    fn message_store_get_latest_for_session_compile_check() {
        fn _assert<T: MessageStore>(t: &T) {
            let _ = t.get_latest_for_session(Uuid::new_v4());
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Verify that a type implementing all sub-traits satisfies the Storage supertrait.
    #[test]
    fn storage_supertrait_includes_tool_call_store() {
        // This is a compile-time check: if Storage doesn't require ToolCallStore,
        // the blanket impl and this constraint would be inconsistent.
        fn _assert_is_storage<T: Storage>(_: &T) {}
        let stub = StubStorage;
        _assert_is_storage(&stub);
    }

    /// Compile-time check: StubStorage implements ThreadStore.
    #[test]
    fn thread_store_compile_check() {
        fn _assert_is_thread_store<T: ThreadStore>(_: &T) {}
        let stub = StubStorage;
        _assert_is_thread_store(&stub);
    }

    /// Compile-time check: Storage implies ThreadStore.
    ///
    /// `update_state` is unique to `ThreadStore`. Calling it via a
    /// `T: Storage` bound only compiles when Storage ⊇ ThreadStore.
    #[test]
    fn storage_supertrait_includes_thread_store() {
        fn _assert<T: Storage>(t: &T) {
            let _ = t.update_state(Uuid::new_v4(), ThreadState::Processing);
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: TurnStore has list_turns_by_thread and find_in_progress_by_agent.
    #[test]
    fn turn_store_new_methods_compile_check() {
        fn _assert_turn_store<T: TurnStore>(_: &T) {}
        let stub = StubStorage;
        _assert_turn_store(&stub);
        // The existence of the impl above is enough — if the new methods are missing
        // from StubStorage, the trait impl won't compile.
    }

    /// Compile-time check: TurnStore has update_cost.
    #[test]
    fn turn_store_update_cost_compile_check() {
        fn _assert<T: TurnStore>(t: &T) {
            let _ = t.update_cost(Uuid::new_v4(), 42);
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: ContextSnapshotStore is accessible and has the expected methods.
    #[test]
    fn context_snapshot_store_compile_check() {
        fn _assert<T: ContextSnapshotStore>(t: &T) {
            let _ = t.store_snapshot(Uuid::new_v4(), "{\"total_tokens\":100}");
            let _ = t.get_snapshot(Uuid::new_v4());
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: SkillActivationStore has the expected methods.
    #[test]
    fn skill_activation_store_compile_check() {
        fn _assert<T: SkillActivationStore>(t: &T) {
            let _ = t.activate_skill(Uuid::new_v4(), "forge");
            let _ = t.deactivate_skill(Uuid::new_v4(), "forge");
            let _ = t.list_active_skills(Uuid::new_v4());
            let _ = t.clear_active_skills(Uuid::new_v4());
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: Storage implies SkillActivationStore.
    #[test]
    fn storage_supertrait_includes_skill_activation_store() {
        fn _assert<T: Storage>(t: &T) {
            let _ = t.activate_skill(Uuid::new_v4(), "forge");
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: TopologyTraceStore has the expected methods.
    #[test]
    fn topology_trace_store_compile_check() {
        fn _assert<T: TopologyTraceStore>(t: &T) {
            let _ = t.get_topology_run(Uuid::new_v4());
            let _ = t.list_topology_runs_by_team(Uuid::new_v4());
            let _ = t.list_topology_runs_by_session(Uuid::new_v4());
            let _ = t.get_node_execution(Uuid::new_v4());
            let _ = t.list_node_executions_by_run(Uuid::new_v4());
            let _ = t.list_edge_evaluations_by_run(Uuid::new_v4());
            let _ = t.list_node_retries(Uuid::new_v4());
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: Storage implies TopologyTraceStore.
    #[test]
    fn storage_supertrait_includes_topology_trace_store() {
        fn _assert<T: Storage>(t: &T) {
            let _ = t.list_topology_runs_by_team(Uuid::new_v4());
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: ApprovalStore has the expected methods.
    #[test]
    fn approval_store_compile_check() {
        fn _assert<T: ApprovalStore>(t: &T) {
            let _ = t.get_approval(Uuid::new_v4());
            let _ = t.list_pending_by_agent("agent-1");
            let _ = t.list_all_pending();
            let _ = t.cleanup_orphaned_approvals();
        }
        let stub = StubStorage;
        _assert(&stub);
    }

    /// Compile-time check: Storage implies ApprovalStore.
    ///
    /// `list_all_pending` is unique to ApprovalStore. Calling it via a
    /// `T: Storage` bound only compiles when Storage ⊇ ApprovalStore.
    #[test]
    fn storage_supertrait_includes_approval_store() {
        fn _assert<T: Storage>(t: &T) {
            let _ = t.list_all_pending();
        }
        let stub = StubStorage;
        _assert(&stub);
    }
}
