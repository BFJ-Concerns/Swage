use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures::stream;
use tokio::sync::{Mutex, Notify, mpsc};

use swage_core::{
    ChatMessage, CompletionRequest, CompletionResponse, DelegateCompletionMode, MessageStatus,
    ProviderError, SandboxError, StreamChunk, ToolCompletionRequest, ToolCompletionResponse,
};
use swage_core::{FinishReason, StorageError};
use swage_hook::{Hook, HookOutcome, HookRegistration, HookRegistry};
use swage_provider::LlmProvider;
use swage_storage::{
    model::{
        ActiveSkillRecord, AgentMessage, AppendMessage, ApprovalRequest, ApprovalState, Attachment,
        AttachmentScopeKind, AttachmentStatus, BackgroundJob, Bench, BenchSlug, ChannelLink,
        EdgeEvaluation, ExecutionHostRegistration, Material, MaterialStatus, Message,
        NewAgentMessage, NewApprovalRequest, NewAttachment, NewBackgroundJob, NewBench,
        NewChannelLink, NewEdgeEvaluation, NewExecutionHostRegistration, NewMaterial, NewMessage,
        NewNodeExecution, NewNodeRetry, NewSchedule, NewScheduleRun, NewSession, NewTeam,
        NewThread, NewToolCall, NewTopologyRun, NewTurn, NewWorkspaceAttachment, NodeExecution,
        NodeExecutionState, NodeRetry, Schedule, ScheduleRun, ScheduleRunState, ScheduleRunUpdate,
        ScheduleStatus, Session, Team, Thread, ThreadState, ToolCallRecord, TopologyRun,
        TopologyRunState, Turn, TurnState, UpdateSchedule, WorkspaceAttachment,
    },
    store::{
        AgentMessageStore, ApprovalStore, AttachmentStore, BackgroundJobStore, BenchStore,
        ChannelLinkStore, ContextSnapshotStore, ExecutionHostStore, MaterialStore, MessageStore,
        ScheduleStore, SessionStore, SkillActivationStore, TeamStore, ThreadStore, ToolCallStore,
        TopologyTraceStore, TurnStore, WorkspaceAttachmentStore,
    },
};

use super::history::parse_model_arguments;

use swage_agent::{AgentEvent, DelegationOutcome};
use swage_sandbox::ToolCallResult;

use super::*;
use crate::{
    state::AppState, subscription::SubscriptionState, test_support::SwageHomeGuard,
    websocket::ServerEvent,
};

// -----------------------------------------------------------------------
// Sandbox guard
// -----------------------------------------------------------------------

struct RetryingShutdownSandbox {
    shutdown_attempts: AtomicUsize,
    failures_before_success: usize,
}

impl RetryingShutdownSandbox {
    fn new(failures_before_success: usize) -> Self {
        Self {
            shutdown_attempts: AtomicUsize::new(0),
            failures_before_success,
        }
    }

    fn shutdown_attempts(&self) -> usize {
        self.shutdown_attempts.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Sandbox for RetryingShutdownSandbox {
    async fn spawn_agent(&mut self) -> Result<(), SandboxError> {
        Ok(())
    }

    async fn execute_tool(
        &self,
        _tool_name: &str,
        _params: serde_json::Value,
    ) -> Result<ToolCallResult, SandboxError> {
        unimplemented!("not used by SandboxGuard tests")
    }

    async fn shutdown(&self) -> Result<(), SandboxError> {
        let attempt = self.shutdown_attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if attempt <= self.failures_before_success {
            return Err(SandboxError::SpawnFailed {
                message: "shutdown failed".to_string(),
            });
        }
        Ok(())
    }
}

#[tokio::test]
async fn sandbox_guard_retries_shutdown_after_failure() {
    let sandbox = Arc::new(RetryingShutdownSandbox::new(1));
    let guard = SandboxGuard::new(sandbox.clone());

    assert!(guard.shutdown().await.is_err());
    assert_eq!(sandbox.shutdown_attempts(), 1);

    guard.shutdown().await.unwrap();
    assert_eq!(sandbox.shutdown_attempts(), 2);

    guard.shutdown().await.unwrap();
    assert_eq!(sandbox.shutdown_attempts(), 2);
}

// -----------------------------------------------------------------------
// Mock LLM providers
// -----------------------------------------------------------------------

/// A mock provider that streams a fixed sequence of chunks.
pub(crate) struct MockProvider {
    chunks: std::sync::Mutex<Vec<StreamChunk>>,
}

impl MockProvider {
    pub(crate) fn new(chunks: Vec<StreamChunk>) -> Self {
        Self {
            chunks: std::sync::Mutex::new(chunks),
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn model_id(&self) -> &str {
        "mock-model"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("not used in streaming tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used in streaming tests")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let chunks = {
            let mut lock = self.chunks.lock().expect("mock lock poisoned");
            std::mem::take(&mut *lock)
        };
        Box::pin(stream::iter(chunks))
    }
}

/// A mock provider supporting multiple streaming rounds.
pub(crate) struct MultiRoundMockProvider {
    rounds: std::sync::Mutex<std::collections::VecDeque<Vec<StreamChunk>>>,
}

impl MultiRoundMockProvider {
    pub(crate) fn new(rounds: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            rounds: std::sync::Mutex::new(rounds.into_iter().collect()),
        }
    }
}

/// A multi-round provider that records every incoming request.
pub(crate) struct CapturingMultiRoundProvider {
    rounds: std::sync::Mutex<std::collections::VecDeque<Vec<StreamChunk>>>,
    captured: Arc<std::sync::Mutex<Vec<CompletionRequest>>>,
}

impl CapturingMultiRoundProvider {
    pub(crate) fn new(
        rounds: Vec<Vec<StreamChunk>>,
    ) -> (Self, Arc<std::sync::Mutex<Vec<CompletionRequest>>>) {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                rounds: std::sync::Mutex::new(rounds.into_iter().collect()),
                captured: Arc::clone(&captured),
            },
            captured,
        )
    }
}

#[async_trait]
impl LlmProvider for CapturingMultiRoundProvider {
    fn model_id(&self) -> &str {
        "mock-capturing-multi-round"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("not used in multi-round tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used in multi-round tests")
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        self.captured.lock().unwrap().push(request);
        let chunks = self
            .rounds
            .lock()
            .expect("mock lock poisoned")
            .pop_front()
            .unwrap_or_default();
        Box::pin(stream::iter(chunks))
    }
}

#[async_trait]
impl LlmProvider for MultiRoundMockProvider {
    fn model_id(&self) -> &str {
        "mock-multi-round"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("not used in multi-round tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used in multi-round tests")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let chunks = self
            .rounds
            .lock()
            .expect("mock lock poisoned")
            .pop_front()
            .unwrap_or_default();
        Box::pin(stream::iter(chunks))
    }
}

struct DetachedGatedProvider {
    child_started: Arc<Notify>,
    child_release: Arc<Notify>,
    initial_parent_request_seen: AtomicUsize,
    captured: Arc<std::sync::Mutex<Vec<String>>>,
}

impl DetachedGatedProvider {
    fn new(
        child_started: Arc<Notify>,
        child_release: Arc<Notify>,
    ) -> (Self, Arc<std::sync::Mutex<Vec<String>>>) {
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                child_started,
                child_release,
                initial_parent_request_seen: AtomicUsize::new(0),
                captured: captured.clone(),
            },
            captured,
        )
    }
}

#[async_trait]
impl LlmProvider for DetachedGatedProvider {
    fn model_id(&self) -> &str {
        "mock-detached-gated"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("streaming provider only")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("streaming provider only")
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        let request_text = request
            .messages
            .iter()
            .map(ChatMessage::text_content)
            .collect::<Vec<_>>()
            .join("\n\n");
        self.captured.lock().unwrap().push(request_text.clone());

        if request_text.contains("Delegated Task\n\nSummarise independently") {
            let started = self.child_started.clone();
            let release = self.child_release.clone();
            return Box::pin(stream::unfold(0_u8, move |state| {
                let started = started.clone();
                let release = release.clone();
                async move {
                    match state {
                        0 => {
                            started.notify_one();
                            release.notified().await;
                            Some((
                                StreamChunk::Delta {
                                    content: "Detached child answer".into(),
                                },
                                1,
                            ))
                        }
                        1 => Some((
                            StreamChunk::Done {
                                finish_reason: None,
                            },
                            2,
                        )),
                        _ => None,
                    }
                }
            }));
        }

        if self
            .initial_parent_request_seen
            .fetch_add(1, Ordering::SeqCst)
            > 0
        {
            return Box::pin(stream::iter(vec![
                StreamChunk::Delta {
                    content: "Parent continued without waiting".into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ]));
        }

        Box::pin(stream::iter(vec![
            StreamChunk::ToolUse {
                call_id: "delegate-detached".into(),
                name: "delegate".into(),
                arguments: serde_json::json!({
                    "target_agent": "agent-b",
                    "task": "Summarise independently",
                    "completion_mode": "detach",
                }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ]))
    }
}

/// A mock provider whose stream blocks forever.
pub(crate) struct PendingProvider;

#[async_trait]
impl LlmProvider for PendingProvider {
    fn model_id(&self) -> &str {
        "pending-model"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!("not used in cancellation tests")
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!("not used in cancellation tests")
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        Box::pin(futures::stream::pending())
    }
}

struct BlockInboundHook;

#[async_trait]
impl Hook for BlockInboundHook {
    fn name(&self) -> &str {
        "block-inbound"
    }

    async fn execute(&self, _event: &HookEvent) -> HookOutcome {
        HookOutcome::Block("blocked by test hook".to_string())
    }
}

// -----------------------------------------------------------------------
// Mock storage — in-memory Vec/HashMap backed, usable in tests.
// -----------------------------------------------------------------------

/// In-memory storage for testing.
pub(crate) struct MockStorage {
    pub(crate) db: Mutex<InMemoryDb>,
    fail_secret_reads: AtomicBool,
}

#[derive(Default)]
pub(crate) struct InMemoryDb {
    pub(crate) benches: HashMap<Uuid, Bench>,
    pub(crate) teams: HashMap<Uuid, Team>,
    pub(crate) thread_branches: Vec<swage_storage::ThreadBranch>,
    pub(crate) topology_runs: HashMap<Uuid, TopologyRun>,
    pub(crate) node_executions: HashMap<Uuid, NodeExecution>,
    pub(crate) edge_evaluations: Vec<EdgeEvaluation>,
    pub(crate) node_retries: Vec<NodeRetry>,
    pub(crate) approval_memory: std::collections::HashSet<(Uuid, String, String)>,
    pub(crate) sessions: HashMap<Uuid, Session>,
    pub(crate) execution_hosts: HashMap<String, ExecutionHostRegistration>,
    pub(crate) channel_links: HashMap<(String, String, String), ChannelLink>,
    pub(crate) threads: HashMap<Uuid, Thread>,
    pub(crate) turns: HashMap<Uuid, Turn>,
    pub(crate) messages: Vec<Message>,
    pub(crate) tool_calls: Vec<ToolCallRecord>,
    pub(crate) snapshots: HashMap<Uuid, (String, chrono::DateTime<chrono::Utc>)>,
    pub(crate) approvals: HashMap<Uuid, ApprovalRequest>,
    pub(crate) audit_events: Vec<swage_storage::AuditEvent>,
    pub(crate) agent_memories: Vec<swage_storage::AgentMemoryRecord>,
    pub(crate) configuration_versions: Vec<swage_storage::ConfigurationVersion>,
    pub(crate) shared_reference_events: Vec<swage_storage::SharedReferenceEvent>,
    pub(crate) agent_messages: HashMap<Uuid, AgentMessage>,
    pub(crate) schedules: HashMap<Uuid, Schedule>,
    pub(crate) schedule_runs: HashMap<Uuid, ScheduleRun>,
    pub(crate) attachments: HashMap<Uuid, Attachment>,
    pub(crate) background_jobs: HashMap<Uuid, BackgroundJob>,
    pub(crate) materials: HashMap<Uuid, Material>,
    pub(crate) workspace_attachments: HashMap<Uuid, WorkspaceAttachment>,
    pub(crate) secrets: HashMap<Uuid, swage_storage::SecretRecord>,
    pub(crate) managed_secret_values: HashMap<Uuid, String>,
    pub(crate) active_lenses: HashMap<Uuid, swage_storage::AppLens>,
    pub(crate) lenses: HashMap<String, swage_storage::LensRecord>,
}

impl MockStorage {
    pub(crate) fn new() -> Self {
        let assistant = swage_storage::LensRecord {
            id: swage_storage::AppLens::assistant(),
            title: "Assistant lens".to_string(),
            summary: "Conversation-first view".to_string(),
            default_surface: "conversation".to_string(),
            ui_modules: vec!["conversation".to_string(), "operator".to_string()],
            prominent_team_ids: vec!["default".to_string()],
            source_kind: swage_storage::LensSourceKind::BuiltIn,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let builder = swage_storage::LensRecord {
            id: swage_storage::AppLens::builder(),
            title: "Builder lens".to_string(),
            summary: "Workspace-first view".to_string(),
            default_surface: "builder".to_string(),
            ui_modules: vec!["builder".to_string(), "conversation".to_string()],
            prominent_team_ids: vec!["workspace-core".to_string()],
            source_kind: swage_storage::LensSourceKind::BuiltIn,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        Self {
            db: Mutex::new(InMemoryDb {
                lenses: HashMap::from([
                    (assistant.id.to_string(), assistant),
                    (builder.id.to_string(), builder),
                ]),
                ..Default::default()
            }),
            fail_secret_reads: AtomicBool::new(false),
        }
    }

    fn fail_secret_reads(&self) {
        self.fail_secret_reads.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl BenchStore for MockStorage {
    async fn create(&self, bench: NewBench) -> Result<Bench, StorageError> {
        let p = Bench {
            id: Uuid::new_v4(),
            name: bench.name,
            description: String::new(),
            is_default: false,
            slug: bench.slug,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        self.db.lock().await.benches.insert(p.id, p.clone());
        Ok(p)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Bench>, StorageError> {
        Ok(self.db.lock().await.benches.get(&id).cloned())
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Bench>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .benches
            .values()
            .find(|p| p.name == name)
            .cloned())
    }

    async fn get_or_create_default(&self) -> Result<Bench, StorageError> {
        if let Some(p) = self
            .db
            .lock()
            .await
            .benches
            .values()
            .find(|bench| bench.is_default)
            .cloned()
        {
            return Ok(p);
        }
        let bench = Bench {
            id: Uuid::new_v4(),
            name: "default".to_string(),
            description: String::new(),
            is_default: true,
            slug: "default".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        self.db.lock().await.benches.insert(bench.id, bench.clone());
        Ok(bench)
    }

    async fn list_all(&self) -> Result<Vec<Bench>, StorageError> {
        let mut benches: Vec<Bench> = self.db.lock().await.benches.values().cloned().collect();
        benches.sort_by_key(|p| p.created_at);
        Ok(benches)
    }

    async fn get_by_slug(&self, slug: &str) -> Result<Option<Bench>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .benches
            .values()
            .find(|p| p.slug == slug)
            .cloned())
    }

    async fn update(
        &self,
        id: Uuid,
        name: Option<String>,
        description: Option<String>,
        slug: Option<BenchSlug>,
    ) -> Result<Bench, StorageError> {
        let mut db = self.db.lock().await;
        match db.benches.get_mut(&id) {
            None => Err(StorageError::NotFound {
                entity: "Bench".into(),
                id: id.to_string(),
            }),
            Some(p) => {
                if let Some(n) = name {
                    p.name = n;
                }
                if let Some(d) = description {
                    p.description = d;
                }
                if let Some(s) = slug {
                    p.slug = s;
                }
                p.updated_at = chrono::Utc::now();
                Ok(p.clone())
            }
        }
    }

    async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
        let mut db = self.db.lock().await;
        match db.benches.get(&id) {
            None => Ok(false),
            Some(p) if p.name == "default" => Err(StorageError::ConstraintViolation(
                "cannot delete the default bench".into(),
            )),
            _ => Ok(db.benches.remove(&id).is_some()),
        }
    }
}

#[async_trait]
impl TeamStore for MockStorage {
    async fn create_team(&self, team: NewTeam) -> Result<Team, StorageError> {
        let now = chrono::Utc::now();
        let members = Team::member_ids_from_agents(&team.agents);
        let created = Team {
            id: Uuid::new_v4(),
            bench_id: team.bench_id,
            name: team.name,
            description: team.description,
            entry_agent_id: team.entry_agent_id,
            members,
            agents: team.agents,
            tasks: team.tasks,
            created_at: now,
            updated_at: now,
        };
        self.db
            .lock()
            .await
            .teams
            .insert(created.id, created.clone());
        Ok(created)
    }

    async fn get_team(&self, id: Uuid) -> Result<Option<Team>, StorageError> {
        Ok(self.db.lock().await.teams.get(&id).cloned())
    }

    async fn get_team_by_name(
        &self,
        bench_id: Uuid,
        name: &str,
    ) -> Result<Option<Team>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .teams
            .values()
            .find(|team| team.bench_id == bench_id && team.name == name)
            .cloned())
    }

    async fn list_teams_by_bench(&self, bench_id: Uuid) -> Result<Vec<Team>, StorageError> {
        let mut teams: Vec<Team> = self
            .db
            .lock()
            .await
            .teams
            .values()
            .filter(|team| team.bench_id == bench_id)
            .cloned()
            .collect();
        teams.sort_by_key(|team| team.created_at);
        Ok(teams)
    }

    async fn update_team(
        &self,
        id: Uuid,
        name: Option<String>,
        description: Option<String>,
        entry_agent_id: Option<String>,
        agents: Option<Vec<swage_storage::model::TeamAgentDefinition>>,
        tasks: Option<Vec<swage_storage::model::TeamTaskDefinition>>,
    ) -> Result<Team, StorageError> {
        let mut db = self.db.lock().await;
        match db.teams.get_mut(&id) {
            None => Err(StorageError::NotFound {
                entity: "Team".into(),
                id: id.to_string(),
            }),
            Some(team) => {
                if let Some(name) = name {
                    team.name = name;
                }
                if let Some(description) = description {
                    team.description = description;
                }
                if let Some(entry_agent_id) = entry_agent_id {
                    team.entry_agent_id = entry_agent_id;
                }
                if let Some(agents) = agents {
                    team.members = Team::member_ids_from_agents(&agents);
                    team.agents = agents;
                }
                if let Some(tasks) = tasks {
                    team.tasks = tasks;
                }
                team.updated_at = chrono::Utc::now();
                Ok(team.clone())
            }
        }
    }

    async fn delete_team(&self, id: Uuid) -> Result<bool, StorageError> {
        Ok(self.db.lock().await.teams.remove(&id).is_some())
    }
}

#[async_trait]
impl TopologyTraceStore for MockStorage {
    async fn create_topology_run(&self, run: NewTopologyRun) -> Result<TopologyRun, StorageError> {
        let now = chrono::Utc::now();
        let created = TopologyRun {
            id: Uuid::new_v4(),
            team_id: run.team_id,
            session_id: run.session_id,
            source_turn_id: run.source_turn_id,
            state: run.state.clone(),
            final_output: run.final_output,
            error_message: run.error_message,
            created_at: now,
            completed_at: if matches!(
                run.state,
                TopologyRunState::Completed
                    | TopologyRunState::Failed
                    | TopologyRunState::Interrupted
            ) {
                Some(now)
            } else {
                None
            },
        };
        self.db
            .lock()
            .await
            .topology_runs
            .insert(created.id, created.clone());
        Ok(created)
    }

    async fn get_topology_run(&self, id: Uuid) -> Result<Option<TopologyRun>, StorageError> {
        Ok(self.db.lock().await.topology_runs.get(&id).cloned())
    }

    async fn list_topology_runs_by_team(
        &self,
        team_id: Uuid,
    ) -> Result<Vec<TopologyRun>, StorageError> {
        let mut runs: Vec<TopologyRun> = self
            .db
            .lock()
            .await
            .topology_runs
            .values()
            .filter(|run| run.team_id == team_id)
            .cloned()
            .collect();
        runs.sort_by_key(|run| run.created_at);
        Ok(runs)
    }

    async fn list_topology_runs_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<TopologyRun>, StorageError> {
        let mut runs: Vec<TopologyRun> = self
            .db
            .lock()
            .await
            .topology_runs
            .values()
            .filter(|run| run.session_id == session_id)
            .cloned()
            .collect();
        runs.sort_by_key(|run| run.created_at);
        Ok(runs)
    }

    async fn update_topology_run(
        &self,
        id: Uuid,
        state: TopologyRunState,
        final_output: Option<&serde_json::Value>,
        error_message: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        let run = db
            .topology_runs
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "TopologyRun".into(),
                id: id.to_string(),
            })?;
        run.state = state.clone();
        if let Some(final_output) = final_output {
            run.final_output = Some(final_output.clone());
        }
        if let Some(error_message) = error_message {
            run.error_message = Some(error_message.to_string());
        }
        if matches!(
            state,
            TopologyRunState::Completed | TopologyRunState::Failed | TopologyRunState::Interrupted
        ) {
            run.completed_at = Some(chrono::Utc::now());
        }
        Ok(())
    }

    async fn create_node_execution(
        &self,
        execution: NewNodeExecution,
    ) -> Result<NodeExecution, StorageError> {
        let now = chrono::Utc::now();
        let created = NodeExecution {
            id: Uuid::new_v4(),
            topology_run_id: execution.topology_run_id,
            node_id: execution.node_id,
            node_kind: execution.node_kind,
            agent_id: execution.agent_id,
            task_name: execution.task_name,
            state: execution.state.clone(),
            input_json: execution.input_json,
            output_json: execution.output_json,
            error_message: execution.error_message,
            created_at: now,
            completed_at: if matches!(
                execution.state,
                NodeExecutionState::Completed
                    | NodeExecutionState::Failed
                    | NodeExecutionState::Skipped
            ) {
                Some(now)
            } else {
                None
            },
        };
        self.db
            .lock()
            .await
            .node_executions
            .insert(created.id, created.clone());
        Ok(created)
    }

    async fn get_node_execution(&self, id: Uuid) -> Result<Option<NodeExecution>, StorageError> {
        Ok(self.db.lock().await.node_executions.get(&id).cloned())
    }

    async fn list_node_executions_by_run(
        &self,
        topology_run_id: Uuid,
    ) -> Result<Vec<NodeExecution>, StorageError> {
        let mut executions: Vec<NodeExecution> = self
            .db
            .lock()
            .await
            .node_executions
            .values()
            .filter(|execution| execution.topology_run_id == topology_run_id)
            .cloned()
            .collect();
        executions.sort_by_key(|execution| execution.created_at);
        Ok(executions)
    }

    async fn update_node_execution(
        &self,
        id: Uuid,
        state: NodeExecutionState,
        output_json: Option<&serde_json::Value>,
        error_message: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        let execution = db
            .node_executions
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "NodeExecution".into(),
                id: id.to_string(),
            })?;
        execution.state = state.clone();
        if let Some(output_json) = output_json {
            execution.output_json = Some(output_json.clone());
        }
        if let Some(error_message) = error_message {
            execution.error_message = Some(error_message.to_string());
        }
        if matches!(
            state,
            NodeExecutionState::Completed
                | NodeExecutionState::Failed
                | NodeExecutionState::Skipped
        ) {
            execution.completed_at = Some(chrono::Utc::now());
        }
        Ok(())
    }

    async fn create_edge_evaluation(
        &self,
        evaluation: NewEdgeEvaluation,
    ) -> Result<EdgeEvaluation, StorageError> {
        let created = EdgeEvaluation {
            id: Uuid::new_v4(),
            topology_run_id: evaluation.topology_run_id,
            source_node_execution_id: evaluation.source_node_execution_id,
            edge_id: evaluation.edge_id,
            target_node_id: evaluation.target_node_id,
            condition_expression: evaluation.condition_expression,
            matched: evaluation.matched,
            payload_json: evaluation.payload_json,
            created_at: chrono::Utc::now(),
        };
        self.db.lock().await.edge_evaluations.push(created.clone());
        Ok(created)
    }

    async fn list_edge_evaluations_by_run(
        &self,
        topology_run_id: Uuid,
    ) -> Result<Vec<EdgeEvaluation>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .edge_evaluations
            .iter()
            .filter(|evaluation| evaluation.topology_run_id == topology_run_id)
            .cloned()
            .collect())
    }

    async fn create_node_retry(&self, retry: NewNodeRetry) -> Result<NodeRetry, StorageError> {
        let created = NodeRetry {
            id: Uuid::new_v4(),
            node_execution_id: retry.node_execution_id,
            attempt_number: retry.attempt_number,
            failure_kind: retry.failure_kind,
            error_message: retry.error_message,
            recovery_guidance: retry.recovery_guidance,
            created_at: chrono::Utc::now(),
        };
        self.db.lock().await.node_retries.push(created.clone());
        Ok(created)
    }

    async fn list_node_retries(
        &self,
        node_execution_id: Uuid,
    ) -> Result<Vec<NodeRetry>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .node_retries
            .iter()
            .filter(|retry| retry.node_execution_id == node_execution_id)
            .cloned()
            .collect())
    }
}

#[async_trait]
impl SessionStore for MockStorage {
    async fn create_session(&self, session: NewSession) -> Result<Session, StorageError> {
        let s = Session {
            id: Uuid::new_v4(),
            bench_id: session.bench_id,
            agent_id: session.agent_id,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            metadata: session.metadata,
            selected_model_id: None,
            active_workspace_attachment_id: session.active_workspace_attachment_id,
        };
        self.db.lock().await.sessions.insert(s.id, s.clone());
        Ok(s)
    }

    async fn get_session(&self, id: Uuid) -> Result<Option<Session>, StorageError> {
        Ok(self.db.lock().await.sessions.get(&id).cloned())
    }

    async fn list_by_bench(&self, bench_id: Uuid) -> Result<Vec<Session>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .sessions
            .values()
            .filter(|s| s.bench_id == bench_id)
            .cloned()
            .collect())
    }

    async fn list_sessions_by_agent_id(
        &self,
        agent_id: &str,
    ) -> Result<Vec<Session>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .sessions
            .values()
            .filter(|s| s.agent_id == agent_id)
            .cloned()
            .collect())
    }

    async fn delete_session(&self, id: Uuid) -> Result<bool, StorageError> {
        Ok(self.db.lock().await.sessions.remove(&id).is_some())
    }

    async fn update_activity(&self, id: Uuid) -> Result<(), StorageError> {
        if let Some(session) = self.db.lock().await.sessions.get_mut(&id) {
            session.updated_at = chrono::Utc::now();
        }
        Ok(())
    }

    async fn set_session_active_workspace(
        &self,
        id: Uuid,
        workspace_id: Option<Uuid>,
    ) -> Result<Session, StorageError> {
        let mut db = self.db.lock().await;
        let session = db
            .sessions
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            })?;
        session.active_workspace_attachment_id = workspace_id;
        session.updated_at = chrono::Utc::now();
        Ok(session.clone())
    }

    async fn set_session_selected_model(
        &self,
        id: Uuid,
        selected_model_id: Option<String>,
    ) -> Result<Session, StorageError> {
        let mut db = self.db.lock().await;
        let session = db
            .sessions
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "session".to_string(),
                id: id.to_string(),
            })?;
        session.selected_model_id = selected_model_id;
        session.updated_at = chrono::Utc::now();
        Ok(session.clone())
    }

    async fn find_session_by_channel_origin(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<Option<Session>, StorageError> {
        let db = self.db.lock().await;
        Ok(db
            .sessions
            .values()
            .find(|session| {
                let Some(origin) = session
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("channel_origin"))
                else {
                    return false;
                };
                origin.get("integration").and_then(|value| value.as_str()) == Some(integration)
                    && origin.get("account").and_then(|value| value.as_str()) == Some(account)
                    && origin.get("conversation").and_then(|value| value.as_str())
                        == Some(conversation)
            })
            .cloned())
    }
}

#[async_trait]
impl WorkspaceAttachmentStore for MockStorage {
    async fn create_workspace_attachment(
        &self,
        workspace: NewWorkspaceAttachment,
    ) -> Result<WorkspaceAttachment, StorageError> {
        let now = chrono::Utc::now();
        let record = WorkspaceAttachment {
            id: Uuid::new_v4(),
            bench_id: workspace.bench_id,
            label: workspace.label,

            path: workspace.path.clone(),
            canonical_path: workspace.path,
            is_missing: false,
            is_default: false,
            permissions: workspace.permissions,
            is_trusted: workspace.is_trusted,
            created_at: now,
            updated_at: now,
        };
        self.db
            .lock()
            .await
            .workspace_attachments
            .insert(record.id, record.clone());
        Ok(record)
    }

    async fn get_workspace_attachment(
        &self,
        id: Uuid,
    ) -> Result<Option<WorkspaceAttachment>, StorageError> {
        Ok(self.db.lock().await.workspace_attachments.get(&id).cloned())
    }

    async fn list_workspace_attachments_by_bench(
        &self,
        bench_id: Uuid,
    ) -> Result<Vec<WorkspaceAttachment>, StorageError> {
        let mut workspaces = self
            .db
            .lock()
            .await
            .workspace_attachments
            .values()
            .filter(|workspace| workspace.bench_id == bench_id)
            .cloned()
            .collect::<Vec<_>>();
        workspaces.sort_by_key(|workspace| workspace.created_at);
        Ok(workspaces)
    }

    async fn set_workspace_trust(
        &self,
        id: Uuid,
        is_trusted: bool,
    ) -> Result<WorkspaceAttachment, StorageError> {
        let mut db = self.db.lock().await;
        let workspace =
            db.workspace_attachments
                .get_mut(&id)
                .ok_or_else(|| StorageError::NotFound {
                    entity: "workspace_attachment".to_string(),
                    id: id.to_string(),
                })?;
        workspace.is_trusted = is_trusted;
        workspace.updated_at = chrono::Utc::now();
        Ok(workspace.clone())
    }

    async fn set_workspace_missing_state(
        &self,
        id: Uuid,
        is_missing: bool,
    ) -> Result<WorkspaceAttachment, StorageError> {
        let mut db = self.db.lock().await;
        let workspace =
            db.workspace_attachments
                .get_mut(&id)
                .ok_or_else(|| StorageError::NotFound {
                    entity: "workspace_attachment".to_string(),
                    id: id.to_string(),
                })?;
        workspace.is_missing = is_missing;
        workspace.updated_at = chrono::Utc::now();
        Ok(workspace.clone())
    }

    async fn set_bench_default_workspace(
        &self,
        bench_id: Uuid,
        workspace_id: Option<Uuid>,
    ) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        if let Some(workspace_id) = workspace_id {
            let workspace = db.workspace_attachments.get(&workspace_id).ok_or_else(|| {
                StorageError::NotFound {
                    entity: "workspace_attachment".to_string(),
                    id: workspace_id.to_string(),
                }
            })?;
            if workspace.bench_id != bench_id {
                return Err(StorageError::ConstraintViolation(format!(
                    "workspace attachment {workspace_id} does not belong to bench {bench_id}"
                )));
            }
        }
        for workspace in db.workspace_attachments.values_mut() {
            if workspace.bench_id == bench_id {
                workspace.is_default = Some(workspace.id) == workspace_id;
                workspace.updated_at = chrono::Utc::now();
            }
        }
        Ok(())
    }

    async fn delete_workspace_attachment(&self, id: Uuid) -> Result<bool, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .workspace_attachments
            .remove(&id)
            .is_some())
    }
}

#[async_trait]
impl swage_storage::store::LensStore for MockStorage {
    async fn get_active_lens(
        &self,
        bench_id: Uuid,
    ) -> Result<swage_storage::LensPreference, StorageError> {
        let active_lens = self
            .db
            .lock()
            .await
            .active_lenses
            .get(&bench_id)
            .cloned()
            .unwrap_or_else(swage_storage::AppLens::assistant);
        Ok(swage_storage::LensPreference {
            bench_id,
            active_lens,
            updated_at: chrono::Utc::now(),
        })
    }

    async fn set_active_lens(
        &self,
        bench_id: Uuid,
        lens: swage_storage::AppLens,
    ) -> Result<swage_storage::LensPreference, StorageError> {
        let mut db = self.db.lock().await;
        if !db.lenses.contains_key(lens.as_str()) {
            return Err(StorageError::NotFound {
                entity: "lens".to_string(),
                id: lens.to_string(),
            });
        }
        db.active_lenses.insert(bench_id, lens.clone());
        Ok(swage_storage::LensPreference {
            bench_id,
            active_lens: lens,
            updated_at: chrono::Utc::now(),
        })
    }

    async fn list_lenses(&self) -> Result<Vec<swage_storage::LensRecord>, StorageError> {
        Ok(self.db.lock().await.lenses.values().cloned().collect())
    }

    async fn get_lens(
        &self,
        lens: &swage_storage::AppLens,
    ) -> Result<Option<swage_storage::LensRecord>, StorageError> {
        Ok(self.db.lock().await.lenses.get(lens.as_str()).cloned())
    }

    async fn create_lens(
        &self,
        lens: swage_storage::NewLensRecord,
    ) -> Result<swage_storage::LensRecord, StorageError> {
        let mut db = self.db.lock().await;
        if db.lenses.contains_key(lens.id.as_str()) {
            return Err(StorageError::ConstraintViolation(format!(
                "lens {} already exists",
                lens.id
            )));
        }
        let record = swage_storage::LensRecord {
            id: lens.id.clone(),
            title: lens.title,
            summary: lens.summary,
            default_surface: lens.default_surface,
            ui_modules: lens.ui_modules,
            prominent_team_ids: lens.prominent_team_ids,
            source_kind: lens.source_kind,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        db.lenses.insert(record.id.to_string(), record.clone());
        Ok(record)
    }

    async fn update_lens(
        &self,
        lens_id: &swage_storage::AppLens,
        title: String,
        summary: String,
        default_surface: String,
        ui_modules: Vec<String>,
        prominent_team_ids: Vec<String>,
    ) -> Result<swage_storage::LensRecord, StorageError> {
        let mut db = self.db.lock().await;
        let Some(existing) = db.lenses.get_mut(lens_id.as_str()) else {
            return Err(StorageError::NotFound {
                entity: "lens".to_string(),
                id: lens_id.to_string(),
            });
        };
        existing.title = title;
        existing.summary = summary;
        existing.default_surface = default_surface;
        existing.ui_modules = ui_modules;
        existing.prominent_team_ids = prominent_team_ids;
        existing.updated_at = chrono::Utc::now();
        Ok(existing.clone())
    }
}

#[async_trait]
impl ExecutionHostStore for MockStorage {
    async fn upsert_execution_host(
        &self,
        host: NewExecutionHostRegistration,
    ) -> Result<ExecutionHostRegistration, StorageError> {
        let registration = ExecutionHostRegistration {
            host_id: host.host_id,
            label: host.label,
            kind: host.kind,
            base_url: host.base_url,
            shared_token: host.shared_token,
            workspace_root: host.workspace_root,
            workspace_roots: host.workspace_roots,
            capabilities: host.capabilities,
            registered_at: chrono::Utc::now(),
        };
        self.db
            .lock()
            .await
            .execution_hosts
            .insert(registration.host_id.clone(), registration.clone());
        Ok(registration)
    }

    async fn list_execution_hosts(&self) -> Result<Vec<ExecutionHostRegistration>, StorageError> {
        let mut hosts: Vec<ExecutionHostRegistration> = self
            .db
            .lock()
            .await
            .execution_hosts
            .values()
            .cloned()
            .collect();
        hosts.sort_by(|left, right| {
            left.registered_at
                .cmp(&right.registered_at)
                .then_with(|| left.host_id.cmp(&right.host_id))
        });
        Ok(hosts)
    }

    async fn delete_execution_host(&self, host_id: &str) -> Result<bool, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .execution_hosts
            .remove(host_id)
            .is_some())
    }
}

#[async_trait]
impl ChannelLinkStore for MockStorage {
    async fn upsert_channel_link(&self, link: NewChannelLink) -> Result<ChannelLink, StorageError> {
        let now = chrono::Utc::now();
        let key = (
            link.integration.clone(),
            link.account.clone(),
            link.conversation.clone(),
        );
        let linked_at = self
            .db
            .lock()
            .await
            .channel_links
            .get(&key)
            .map(|existing| existing.linked_at)
            .unwrap_or(now);
        let created = ChannelLink {
            integration: link.integration,
            account: link.account,
            conversation: link.conversation,
            bench_id: link.bench_id,
            agent_id: link.agent_id,
            session_id: link.session_id,
            home_target: link.home_target,
            linked_at,
            updated_at: now,
        };
        self.db
            .lock()
            .await
            .channel_links
            .insert(key, created.clone());
        Ok(created)
    }

    async fn get_channel_link(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<Option<ChannelLink>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .channel_links
            .get(&(
                integration.to_string(),
                account.to_string(),
                conversation.to_string(),
            ))
            .cloned())
    }

    async fn list_channel_links(&self) -> Result<Vec<ChannelLink>, StorageError> {
        let mut links: Vec<ChannelLink> = self
            .db
            .lock()
            .await
            .channel_links
            .values()
            .cloned()
            .collect();
        links.sort_by_key(|link| link.linked_at);
        Ok(links)
    }

    async fn delete_channel_link(
        &self,
        integration: &str,
        account: &str,
        conversation: &str,
    ) -> Result<bool, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .channel_links
            .remove(&(
                integration.to_string(),
                account.to_string(),
                conversation.to_string(),
            ))
            .is_some())
    }
}

#[async_trait]
impl ScheduleStore for MockStorage {
    async fn create_schedule(&self, schedule: NewSchedule) -> Result<Schedule, StorageError> {
        let now = chrono::Utc::now();
        let created = Schedule {
            id: Uuid::new_v4(),
            message: schedule.message,
            status: schedule.status,
            kind: schedule.kind,
            expression: schedule.expression,
            session_policy: schedule.session_policy,
            delivery_routing: schedule.delivery_routing,
            gate: schedule.gate,
            next_run_at: schedule.next_run_at,
            last_run_at: schedule.last_run_at,
            created_at: now,
            updated_at: now,
        };
        self.db
            .lock()
            .await
            .schedules
            .insert(created.id, created.clone());
        Ok(created)
    }

    async fn get_schedule(&self, id: Uuid) -> Result<Option<Schedule>, StorageError> {
        Ok(self.db.lock().await.schedules.get(&id).cloned())
    }

    async fn list_schedules(&self) -> Result<Vec<Schedule>, StorageError> {
        let mut schedules: Vec<Schedule> =
            self.db.lock().await.schedules.values().cloned().collect();
        schedules.sort_by_key(|schedule| schedule.created_at);
        Ok(schedules)
    }

    async fn list_due_schedules(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Schedule>, StorageError> {
        let mut schedules: Vec<Schedule> = self
            .db
            .lock()
            .await
            .schedules
            .values()
            .filter(|schedule| {
                schedule.status == ScheduleStatus::Active
                    && schedule.next_run_at.is_some_and(|next| next <= now)
            })
            .cloned()
            .collect();
        schedules.sort_by_key(|schedule| schedule.next_run_at);
        Ok(schedules)
    }

    async fn update_schedule(
        &self,
        id: Uuid,
        update: UpdateSchedule,
    ) -> Result<Schedule, StorageError> {
        let mut db = self.db.lock().await;
        let schedule = db
            .schedules
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Schedule".into(),
                id: id.to_string(),
            })?;
        if let Some(message) = update.message {
            schedule.message = message;
        }
        if let Some(status) = update.status {
            schedule.status = status;
        }
        if let Some(kind) = update.kind {
            schedule.kind = kind;
        }
        if let Some(expression) = update.expression {
            schedule.expression = expression;
        }
        if let Some(delivery_routing) = update.delivery_routing {
            schedule.delivery_routing = delivery_routing;
        }
        if let Some(gate) = update.gate {
            schedule.gate = gate;
        }
        if let Some(next_run_at) = update.next_run_at {
            schedule.next_run_at = next_run_at;
        }
        if let Some(last_run_at) = update.last_run_at {
            schedule.last_run_at = last_run_at;
        }
        schedule.updated_at = chrono::Utc::now();
        Ok(schedule.clone())
    }

    async fn delete_schedule(&self, id: Uuid) -> Result<bool, StorageError> {
        Ok(self.db.lock().await.schedules.remove(&id).is_some())
    }

    async fn create_schedule_run(&self, run: NewScheduleRun) -> Result<ScheduleRun, StorageError> {
        let terminal = matches!(
            run.state,
            ScheduleRunState::Completed
                | ScheduleRunState::CompletedEmpty
                | ScheduleRunState::Failed
                | ScheduleRunState::Skipped
        );
        let created = ScheduleRun {
            id: Uuid::new_v4(),
            schedule_id: run.schedule_id,
            trigger: run.trigger,
            state: run.state,
            skip_reason: run.skip_reason,
            scheduled_for: run.scheduled_for,
            session_id: None,
            turn_id: None,
            gate: None,
            output_text: None,
            error_message: run.error_message,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: terminal.then(chrono::Utc::now),
        };
        self.db
            .lock()
            .await
            .schedule_runs
            .insert(created.id, created.clone());
        Ok(created)
    }

    async fn get_schedule_run(&self, id: Uuid) -> Result<Option<ScheduleRun>, StorageError> {
        Ok(self.db.lock().await.schedule_runs.get(&id).cloned())
    }

    async fn list_schedule_runs(
        &self,
        schedule_id: Uuid,
    ) -> Result<Vec<ScheduleRun>, StorageError> {
        let mut runs: Vec<ScheduleRun> = self
            .db
            .lock()
            .await
            .schedule_runs
            .values()
            .filter(|run| run.schedule_id == schedule_id)
            .cloned()
            .collect();
        runs.sort_by_key(|run| run.created_at);
        Ok(runs)
    }

    async fn update_schedule_run(
        &self,
        id: Uuid,
        update: ScheduleRunUpdate,
    ) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        let run = db
            .schedule_runs
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "ScheduleRun".into(),
                id: id.to_string(),
            })?;
        if let Some(state) = update.state {
            run.state = state;
            if matches!(state, ScheduleRunState::Running) && run.started_at.is_none() {
                run.started_at = Some(chrono::Utc::now());
            }
            if matches!(
                state,
                ScheduleRunState::Completed
                    | ScheduleRunState::CompletedEmpty
                    | ScheduleRunState::Failed
                    | ScheduleRunState::Skipped
            ) {
                run.completed_at = Some(chrono::Utc::now());
            }
        }
        if let Some(skip_reason) = update.skip_reason {
            run.skip_reason = Some(skip_reason);
        }
        if let Some(session_id) = update.session_id {
            run.session_id = Some(session_id);
        }
        if let Some(turn_id) = update.turn_id {
            run.turn_id = Some(turn_id);
        }
        if let Some(gate) = update.gate {
            run.gate = Some(gate);
        }
        if let Some(output_text) = update.output_text {
            run.output_text = Some(output_text);
        }
        if let Some(error_message) = update.error_message {
            run.error_message = Some(error_message);
        }
        Ok(())
    }

    async fn prune_schedule_runs(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        max_age: std::time::Duration,
        max_per_schedule: u32,
    ) -> Result<u64, StorageError> {
        let cutoff = now
            - chrono::Duration::from_std(max_age)
                .map_err(|error| StorageError::QueryFailed(error.to_string()))?;
        let mut db = self.db.lock().await;
        let mut terminal: Vec<(Uuid, Uuid, chrono::DateTime<chrono::Utc>)> = db
            .schedule_runs
            .values()
            .filter(|run| {
                matches!(
                    run.state,
                    ScheduleRunState::Completed
                        | ScheduleRunState::CompletedEmpty
                        | ScheduleRunState::Failed
                        | ScheduleRunState::Skipped
                )
            })
            .map(|run| (run.id, run.schedule_id, run.created_at))
            .collect();

        let mut remove: std::collections::HashSet<Uuid> = terminal
            .iter()
            .filter_map(|(id, _, created_at)| (*created_at < cutoff).then_some(*id))
            .collect();

        terminal.sort_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| right.2.cmp(&left.2))
                .then_with(|| right.0.cmp(&left.0))
        });

        let mut seen_by_schedule: std::collections::HashMap<Uuid, u32> =
            std::collections::HashMap::new();
        for (id, schedule_id, _) in terminal {
            let seen = seen_by_schedule.entry(schedule_id).or_default();
            *seen += 1;
            if *seen > max_per_schedule {
                remove.insert(id);
            }
        }

        let count = remove.len() as u64;
        for id in remove {
            db.schedule_runs.remove(&id);
        }
        Ok(count)
    }

    async fn recover_incomplete_schedule_runs(
        &self,
        error_message: &str,
    ) -> Result<u64, StorageError> {
        let mut db = self.db.lock().await;
        let now = chrono::Utc::now();
        let mut recovered = 0;
        for run in db.schedule_runs.values_mut() {
            if matches!(
                run.state,
                ScheduleRunState::Pending | ScheduleRunState::Running
            ) {
                run.state = ScheduleRunState::Failed;
                run.error_message = Some(error_message.to_string());
                run.completed_at = Some(now);
                recovered += 1;
            }
        }
        Ok(recovered)
    }
}

#[async_trait]
impl AttachmentStore for MockStorage {
    async fn queue_attachment_ingestion(
        &self,
        attachment: NewAttachment,
        job: NewBackgroundJob,
    ) -> Result<(Attachment, BackgroundJob), StorageError> {
        let mut db = self.db.lock().await;
        let now = chrono::Utc::now();
        let byte_count = i64::try_from(attachment.bytes.len()).unwrap_or(i64::MAX);
        let attachment_record = Attachment {
            id: attachment.id,
            scope_kind: attachment.scope_kind,
            session_id: attachment.session_id,
            bench_id: attachment.bench_id,
            kind: attachment.kind,
            status: AttachmentStatus::Pending,
            filename: attachment.filename,
            mime_type: attachment.mime_type,
            bytes: attachment.bytes,
            byte_count,
            created_at: now,
            updated_at: now,
        };
        let job_record = BackgroundJob {
            id: job.id,
            kind: job.kind,
            state: swage_storage::BackgroundJobState::Queued,
            attachment_id: job.attachment_id,
            payload: job.payload,
            result: None,
            error_message: None,
            attempt_count: 0,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
        };
        db.attachments
            .insert(attachment_record.id, attachment_record.clone());
        db.background_jobs.insert(job_record.id, job_record.clone());
        Ok((attachment_record, job_record))
    }

    async fn get_attachment(&self, id: Uuid) -> Result<Option<Attachment>, StorageError> {
        Ok(self.db.lock().await.attachments.get(&id).cloned())
    }

    async fn list_attachments_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Attachment>, StorageError> {
        let mut attachments: Vec<Attachment> = self
            .db
            .lock()
            .await
            .attachments
            .values()
            .filter(|attachment| attachment.session_id == Some(session_id))
            .cloned()
            .collect();
        attachments.sort_by_key(|attachment| attachment.created_at);
        Ok(attachments)
    }

    async fn list_attachments_by_bench(
        &self,
        bench_id: Uuid,
    ) -> Result<Vec<Attachment>, StorageError> {
        let mut attachments: Vec<Attachment> = self
            .db
            .lock()
            .await
            .attachments
            .values()
            .filter(|attachment| attachment.bench_id == Some(bench_id))
            .cloned()
            .collect();
        attachments.sort_by_key(|attachment| attachment.created_at);
        Ok(attachments)
    }

    async fn update_attachment_ingestion(
        &self,
        id: Uuid,
        status: AttachmentStatus,
    ) -> Result<Attachment, StorageError> {
        let mut db = self.db.lock().await;
        let attachment = db
            .attachments
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Attachment".into(),
                id: id.to_string(),
            })?;
        attachment.status = status;
        attachment.updated_at = chrono::Utc::now();
        Ok(attachment.clone())
    }
}

#[async_trait]
impl BackgroundJobStore for MockStorage {
    async fn get_background_job(&self, id: Uuid) -> Result<Option<BackgroundJob>, StorageError> {
        Ok(self.db.lock().await.background_jobs.get(&id).cloned())
    }

    async fn list_runnable_background_jobs(&self) -> Result<Vec<BackgroundJob>, StorageError> {
        let mut jobs: Vec<BackgroundJob> = self
            .db
            .lock()
            .await
            .background_jobs
            .values()
            .filter(|job| {
                matches!(
                    job.state,
                    swage_storage::BackgroundJobState::Queued
                        | swage_storage::BackgroundJobState::Retried
                )
            })
            .cloned()
            .collect();
        jobs.sort_by_key(|job| job.created_at);
        Ok(jobs)
    }

    async fn mark_background_job_running(&self, id: Uuid) -> Result<BackgroundJob, StorageError> {
        let mut db = self.db.lock().await;
        let job = db
            .background_jobs
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "BackgroundJob".into(),
                id: id.to_string(),
            })?;
        let now = chrono::Utc::now();
        job.state = swage_storage::BackgroundJobState::Running;
        job.attempt_count += 1;
        job.started_at = Some(now);
        job.completed_at = None;
        job.updated_at = now;
        Ok(job.clone())
    }

    async fn complete_background_job(
        &self,
        id: Uuid,
        result: Option<&serde_json::Value>,
    ) -> Result<BackgroundJob, StorageError> {
        let mut db = self.db.lock().await;
        let job = db
            .background_jobs
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "BackgroundJob".into(),
                id: id.to_string(),
            })?;
        let now = chrono::Utc::now();
        job.state = swage_storage::BackgroundJobState::Completed;
        job.result = result.cloned();
        job.error_message = None;
        job.completed_at = Some(now);
        job.updated_at = now;
        Ok(job.clone())
    }

    async fn fail_background_job(
        &self,
        id: Uuid,
        error_message: &str,
    ) -> Result<BackgroundJob, StorageError> {
        let mut db = self.db.lock().await;
        let job = db
            .background_jobs
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "BackgroundJob".into(),
                id: id.to_string(),
            })?;
        let now = chrono::Utc::now();
        job.state = swage_storage::BackgroundJobState::Failed;
        job.error_message = Some(error_message.to_string());
        job.completed_at = Some(now);
        job.updated_at = now;
        Ok(job.clone())
    }

    async fn retry_background_job(&self, id: Uuid) -> Result<BackgroundJob, StorageError> {
        let mut db = self.db.lock().await;
        let job = db
            .background_jobs
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "BackgroundJob".into(),
                id: id.to_string(),
            })?;
        job.state = swage_storage::BackgroundJobState::Retried;
        job.result = None;
        job.error_message = None;
        job.started_at = None;
        job.completed_at = None;
        job.updated_at = chrono::Utc::now();
        Ok(job.clone())
    }

    async fn list_background_jobs_by_attachment(
        &self,
        attachment_id: Uuid,
    ) -> Result<Vec<BackgroundJob>, StorageError> {
        let mut jobs: Vec<BackgroundJob> = self
            .db
            .lock()
            .await
            .background_jobs
            .values()
            .filter(|job| job.attachment_id == Some(attachment_id))
            .cloned()
            .collect();
        jobs.sort_by_key(|job| job.created_at);
        Ok(jobs)
    }

    async fn recover_running_background_jobs(
        &self,
        error_message: &str,
    ) -> Result<u64, StorageError> {
        let mut db = self.db.lock().await;
        let now = chrono::Utc::now();
        let mut recovered = 0;
        for job in db.background_jobs.values_mut() {
            if job.state == swage_storage::BackgroundJobState::Running {
                job.state = swage_storage::BackgroundJobState::Retried;
                job.error_message = Some(error_message.to_string());
                job.completed_at = None;
                job.updated_at = now;
                recovered += 1;
            }
        }
        Ok(recovered)
    }
}

#[async_trait]
impl MaterialStore for MockStorage {
    async fn create_material(&self, material: NewMaterial) -> Result<Material, StorageError> {
        let now = chrono::Utc::now();
        let record = Material {
            id: material.id,
            scope_kind: material.scope_kind,
            session_id: material.session_id,
            bench_id: material.bench_id,
            source_kind: material.source_kind,
            source_attachment_id: material.source_attachment_id,
            status: MaterialStatus::Active,
            title: material.title,
            relative_path: material.relative_path,
            mime_type: material.mime_type,
            content_text: material.content_text,
            created_at: now,
            updated_at: now,
            archived_at: None,
        };
        self.db
            .lock()
            .await
            .materials
            .insert(record.id, record.clone());
        Ok(record)
    }

    async fn get_material(&self, id: Uuid) -> Result<Option<Material>, StorageError> {
        Ok(self.db.lock().await.materials.get(&id).cloned())
    }

    async fn get_material_by_source_attachment(
        &self,
        attachment_id: Uuid,
    ) -> Result<Option<Material>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .materials
            .values()
            .find(|material| material.source_attachment_id == Some(attachment_id))
            .cloned())
    }

    async fn list_materials_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<Material>, StorageError> {
        let mut materials: Vec<Material> = self
            .db
            .lock()
            .await
            .materials
            .values()
            .filter(|material| material.session_id == Some(session_id))
            .cloned()
            .collect();
        materials.sort_by_key(|material| material.created_at);
        Ok(materials)
    }

    async fn list_materials_by_bench(&self, bench_id: Uuid) -> Result<Vec<Material>, StorageError> {
        let mut materials: Vec<Material> = self
            .db
            .lock()
            .await
            .materials
            .values()
            .filter(|material| {
                material.bench_id == Some(bench_id)
                    && material.scope_kind == AttachmentScopeKind::Bench
            })
            .cloned()
            .collect();
        materials.sort_by_key(|material| material.created_at);
        Ok(materials)
    }

    async fn update_material_content(
        &self,
        id: Uuid,
        content_text: &str,
        mime_type: &str,
    ) -> Result<Material, StorageError> {
        let mut db = self.db.lock().await;
        let material = db
            .materials
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Material".into(),
                id: id.to_string(),
            })?;
        material.content_text = content_text.to_string();
        material.mime_type = mime_type.to_string();
        material.status = MaterialStatus::Active;
        material.archived_at = None;
        material.updated_at = chrono::Utc::now();
        Ok(material.clone())
    }

    async fn archive_material(&self, id: Uuid) -> Result<Material, StorageError> {
        let mut db = self.db.lock().await;
        let material = db
            .materials
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Material".into(),
                id: id.to_string(),
            })?;
        material.status = MaterialStatus::Archived;
        material.archived_at = Some(chrono::Utc::now());
        material.updated_at = chrono::Utc::now();
        Ok(material.clone())
    }

    async fn restore_material(&self, id: Uuid) -> Result<Material, StorageError> {
        let mut db = self.db.lock().await;
        let material = db
            .materials
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Material".into(),
                id: id.to_string(),
            })?;
        material.status = MaterialStatus::Active;
        material.archived_at = None;
        material.updated_at = chrono::Utc::now();
        Ok(material.clone())
    }

    async fn promote_material(
        &self,
        id: Uuid,
        scope_kind: AttachmentScopeKind,
        session_id: Option<Uuid>,
        bench_id: Option<Uuid>,
        relative_path: &str,
    ) -> Result<Material, StorageError> {
        let mut db = self.db.lock().await;
        let material = db
            .materials
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Material".into(),
                id: id.to_string(),
            })?;
        material.scope_kind = scope_kind;
        material.session_id = session_id;
        material.bench_id = bench_id;
        material.relative_path = relative_path.to_string();
        material.status = MaterialStatus::Active;
        material.archived_at = None;
        material.updated_at = chrono::Utc::now();
        Ok(material.clone())
    }
}

#[async_trait]
impl ThreadStore for MockStorage {
    async fn create(&self, thread: NewThread) -> Result<Thread, StorageError> {
        let t = Thread {
            id: Uuid::new_v4(),
            session_id: thread.session_id,
            agent_id: thread.agent_id,
            state: ThreadState::Idle,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        self.db.lock().await.threads.insert(t.id, t.clone());
        Ok(t)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Thread>, StorageError> {
        Ok(self.db.lock().await.threads.get(&id).cloned())
    }

    async fn list_by_session(&self, session_id: Uuid) -> Result<Vec<Thread>, StorageError> {
        let mut threads: Vec<Thread> = self
            .db
            .lock()
            .await
            .threads
            .values()
            .filter(|t| t.session_id == session_id)
            .cloned()
            .collect();
        threads.sort_by_key(|t| t.created_at);
        Ok(threads)
    }

    async fn list_threads_by_agent_id(&self, agent_id: &str) -> Result<Vec<Thread>, StorageError> {
        let mut threads: Vec<Thread> = self
            .db
            .lock()
            .await
            .threads
            .values()
            .filter(|t| t.agent_id == agent_id)
            .cloned()
            .collect();
        threads.sort_by_key(|t| t.created_at);
        Ok(threads)
    }

    async fn update_state(&self, id: Uuid, state: ThreadState) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        match db.threads.get_mut(&id) {
            None => Err(StorageError::NotFound {
                entity: "Thread".into(),
                id: id.to_string(),
            }),
            Some(t) => {
                if !t.state.can_transition_to(&state) {
                    return Err(StorageError::ConstraintViolation(format!(
                        "invalid thread state transition: {:?} → {:?}",
                        t.state, state
                    )));
                }
                t.state = state;
                t.updated_at = chrono::Utc::now();
                Ok(())
            }
        }
    }

    async fn delete(&self, id: Uuid) -> Result<bool, StorageError> {
        Ok(self.db.lock().await.threads.remove(&id).is_some())
    }

    async fn create_branch(
        &self,
        parent_thread_id: Uuid,
        branched_from_message_id: Uuid,
    ) -> Result<swage_storage::ThreadBranch, StorageError> {
        let mut db = self.db.lock().await;
        let parent =
            db.threads
                .get(&parent_thread_id)
                .cloned()
                .ok_or_else(|| StorageError::NotFound {
                    entity: "Thread".into(),
                    id: parent_thread_id.to_string(),
                })?;
        let source_position = db
            .messages
            .iter()
            .find(|message| {
                message.id == branched_from_message_id && message.thread_id == parent_thread_id
            })
            .map(|message| message.position)
            .ok_or_else(|| StorageError::NotFound {
                entity: "Message".into(),
                id: branched_from_message_id.to_string(),
            })?;
        let branch = swage_storage::ThreadBranch {
            child_thread_id: Uuid::new_v4(),
            parent_thread_id,
            branched_from_message_id,
            created_at: chrono::Utc::now(),
        };
        db.threads.insert(
            branch.child_thread_id,
            Thread {
                id: branch.child_thread_id,
                session_id: parent.session_id,
                agent_id: parent.agent_id,
                state: ThreadState::Idle,
                created_at: branch.created_at,
                updated_at: branch.created_at,
            },
        );
        let messages_to_copy: Vec<_> = db
            .messages
            .iter()
            .filter(|message| {
                message.thread_id == parent_thread_id && message.position <= source_position
            })
            .cloned()
            .collect();
        for message in messages_to_copy {
            let message_id = Uuid::new_v4();
            db.messages.push(swage_storage::Message {
                id: message_id,
                thread_id: branch.child_thread_id,
                turn_id: message.turn_id,
                role: message.role,
                content: message.content,
                position: message.position,
                agent_id: message.agent_id,
                tool_call_id: message.tool_call_id,
                tool_name: message.tool_name,
                created_at: message.created_at,
            });
        }
        db.thread_branches.push(branch.clone());
        Ok(branch)
    }

    async fn list_branches_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<swage_storage::ThreadBranch>, StorageError> {
        let db = self.db.lock().await;
        Ok(db
            .thread_branches
            .iter()
            .filter(|branch| {
                db.threads
                    .get(&branch.child_thread_id)
                    .map(|thread| thread.session_id == session_id)
                    .unwrap_or(false)
            })
            .cloned()
            .collect())
    }

    async fn get_branch(
        &self,
        child_thread_id: Uuid,
    ) -> Result<Option<swage_storage::ThreadBranch>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .thread_branches
            .iter()
            .find(|branch| branch.child_thread_id == child_thread_id)
            .cloned())
    }
}

#[async_trait]
impl TurnStore for MockStorage {
    async fn create_turn(&self, turn: NewTurn) -> Result<Turn, StorageError> {
        let t = Turn {
            id: Uuid::new_v4(),
            thread_id: turn.thread_id,
            agent_id: turn.agent_id,
            state: turn.state,
            user_input: turn.user_input,
            inbound_audit: None,
            assistant_response: None,
            input_tokens: None,
            output_tokens: None,
            cost_microcents: None,
            structured_output: None,
            parent_turn_id: turn.parent_turn_id,
            created_at: chrono::Utc::now(),
            completed_at: None,
        };
        self.db.lock().await.turns.insert(t.id, t.clone());
        Ok(t)
    }

    async fn update_structured_output(
        &self,
        id: Uuid,
        output: &serde_json::Value,
    ) -> Result<(), StorageError> {
        if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
            t.structured_output = Some(output.clone());
        }
        Ok(())
    }

    async fn get_turn(&self, id: Uuid) -> Result<Option<Turn>, StorageError> {
        Ok(self.db.lock().await.turns.get(&id).cloned())
    }

    async fn list_turns_by_thread(&self, thread_id: Uuid) -> Result<Vec<Turn>, StorageError> {
        let mut turns: Vec<Turn> = self
            .db
            .lock()
            .await
            .turns
            .values()
            .filter(|t| t.thread_id == thread_id)
            .cloned()
            .collect();
        turns.sort_by_key(|t| t.created_at);
        Ok(turns)
    }

    async fn update_turn_state(&self, id: Uuid, state: TurnState) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        if let Some(turn) = db.turns.get_mut(&id) {
            if matches!(
                state,
                TurnState::Completed | TurnState::Failed | TurnState::Interrupted
            ) {
                turn.completed_at = Some(chrono::Utc::now());
            }
            turn.state = state;
        }
        Ok(())
    }

    async fn find_in_progress_by_agent(&self, agent_id: &str) -> Result<Vec<Turn>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .turns
            .values()
            .filter(|t| {
                t.agent_id == agent_id
                    && matches!(
                        t.state,
                        TurnState::Pending
                            | TurnState::Processing
                            | TurnState::AwaitingApproval
                            | TurnState::Delegating
                    )
            })
            .cloned()
            .collect())
    }

    async fn update_response(&self, id: Uuid, response: &str) -> Result<(), StorageError> {
        if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
            t.assistant_response = Some(response.to_string());
        }
        Ok(())
    }

    async fn update_tokens(&self, id: Uuid, input: i64, output: i64) -> Result<(), StorageError> {
        if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
            t.input_tokens = Some(input);
            t.output_tokens = Some(output);
        }
        Ok(())
    }

    async fn update_cost(&self, id: Uuid, cost: i64) -> Result<(), StorageError> {
        if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
            t.cost_microcents = Some(cost);
        }
        Ok(())
    }

    async fn update_inbound_audit(
        &self,
        id: Uuid,
        audit: &swage_storage::InboundAudit,
    ) -> Result<(), StorageError> {
        if let Some(t) = self.db.lock().await.turns.get_mut(&id) {
            t.inbound_audit = Some(audit.clone());
        }
        Ok(())
    }
}

#[async_trait]
impl ContextSnapshotStore for MockStorage {
    async fn store_snapshot(&self, turn_id: Uuid, report_json: &str) -> Result<(), StorageError> {
        self.db
            .lock()
            .await
            .snapshots
            .insert(turn_id, (report_json.to_string(), chrono::Utc::now()));
        Ok(())
    }

    async fn get_snapshot(&self, turn_id: Uuid) -> Result<Option<String>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .snapshots
            .get(&turn_id)
            .map(|(report_json, _)| report_json.clone()))
    }

    async fn list_snapshots_by_bench(
        &self,
        bench_id: Uuid,
        limit: usize,
    ) -> Result<Vec<swage_storage::ContextSnapshotRecord>, StorageError> {
        let db = self.db.lock().await;
        let mut records = Vec::new();
        for (turn_id, (report_json, created_at)) in &db.snapshots {
            let Some(turn) = db.turns.get(turn_id) else {
                continue;
            };
            let Some(thread) = db.threads.get(&turn.thread_id) else {
                continue;
            };
            let Some(session) = db.sessions.get(&thread.session_id) else {
                continue;
            };
            if session.bench_id != bench_id {
                continue;
            }
            records.push(swage_storage::ContextSnapshotRecord {
                turn_id: *turn_id,
                session_id: session.id,
                bench_id,
                created_at: *created_at,
                report_json: report_json.clone(),
            });
        }
        records.sort_by_key(|record| std::cmp::Reverse(record.created_at));
        records.truncate(limit);
        Ok(records)
    }
}

#[async_trait]
impl swage_storage::store::SharedReferenceStore for MockStorage {
    async fn create_shared_reference_event(
        &self,
        event: swage_storage::NewSharedReferenceEvent,
    ) -> Result<swage_storage::SharedReferenceEvent, StorageError> {
        let created = swage_storage::SharedReferenceEvent {
            id: event.id,
            object_kind: event.object_kind,
            object_id: event.object_id,
            event_kind: event.event_kind,
            payload: event.payload,
            created_at: chrono::Utc::now(),
        };
        self.db
            .lock()
            .await
            .shared_reference_events
            .push(created.clone());
        Ok(created)
    }

    async fn list_shared_reference_events(
        &self,
        object_kind: swage_storage::SharedReferenceKind,
        object_id: &str,
    ) -> Result<Vec<swage_storage::SharedReferenceEvent>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .shared_reference_events
            .iter()
            .filter(|event| event.object_kind == object_kind && event.object_id == object_id)
            .cloned()
            .collect())
    }
}

#[async_trait]
impl MessageStore for MockStorage {
    async fn create_message(&self, message: NewMessage) -> Result<Message, StorageError> {
        let m = Message {
            id: Uuid::new_v4(),
            thread_id: message.thread_id,
            turn_id: message.turn_id,
            role: message.role,
            content: message.content,
            position: message.position,
            agent_id: message.agent_id,
            tool_call_id: None,
            tool_name: None,
            created_at: chrono::Utc::now(),
        };
        self.db.lock().await.messages.push(m.clone());
        Ok(m)
    }

    async fn append_message(&self, message: AppendMessage) -> Result<Message, StorageError> {
        let mut db = self.db.lock().await;
        let position = db
            .messages
            .iter()
            .filter(|existing| existing.thread_id == message.thread_id)
            .map(|existing| existing.position)
            .max()
            .map_or(0, |max_position| max_position + 1);
        let created = Message {
            id: Uuid::new_v4(),
            thread_id: message.thread_id,
            turn_id: message.turn_id,
            role: message.role,
            content: message.content,
            position,
            agent_id: message.agent_id,
            tool_call_id: None,
            tool_name: None,
            created_at: chrono::Utc::now(),
        };
        db.messages.push(created.clone());
        Ok(created)
    }

    async fn list_messages_by_thread(&self, thread_id: Uuid) -> Result<Vec<Message>, StorageError> {
        let db = self.db.lock().await;
        let mut msgs: Vec<Message> = db
            .messages
            .iter()
            .filter(|m| m.thread_id == thread_id)
            .cloned()
            .collect();
        msgs.sort_by_key(|m| m.position);
        Ok(msgs)
    }

    async fn get_latest_for_session(
        &self,
        session_id: Uuid,
    ) -> Result<Option<Message>, StorageError> {
        let db = self.db.lock().await;
        // Collect thread_ids belonging to this session.
        let thread_ids: std::collections::HashSet<Uuid> = db
            .threads
            .values()
            .filter(|t| t.session_id == session_id)
            .map(|t| t.id)
            .collect();
        // Return the message with the highest position across all threads.
        Ok(db
            .messages
            .iter()
            .filter(|m| thread_ids.contains(&m.thread_id))
            .max_by_key(|m| (m.created_at, m.position))
            .cloned())
    }

    async fn get_message(&self, id: Uuid) -> Result<Option<Message>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .messages
            .iter()
            .find(|m| m.id == id)
            .cloned())
    }

    async fn delete_by_thread(&self, thread_id: Uuid) -> Result<(), StorageError> {
        self.db
            .lock()
            .await
            .messages
            .retain(|m| m.thread_id != thread_id);
        Ok(())
    }
}

#[async_trait]
impl ToolCallStore for MockStorage {
    async fn create_tool_call(
        &self,
        tool_call: NewToolCall,
    ) -> Result<ToolCallRecord, StorageError> {
        let record = ToolCallRecord {
            id: Uuid::new_v4(),
            turn_id: tool_call.turn_id,
            agent_id: tool_call.agent_id,
            call_id: tool_call.call_id,
            tool: tool_call.tool,
            arguments: tool_call.arguments,
            model_arguments: tool_call.model_arguments,
            result: tool_call.result,
            model_result: tool_call.model_result,
            status: tool_call.status,
            duration_ms: tool_call.duration_ms,
            truncated: tool_call.truncated,
            total_bytes: tool_call.total_bytes,
            provider_round: tool_call.provider_round,
            position: tool_call.position,
            created_at: chrono::Utc::now(),
            parent_call_id: tool_call.parent_call_id,
        };
        self.db.lock().await.tool_calls.push(record.clone());
        Ok(record)
    }

    async fn get_tool_call(&self, id: Uuid) -> Result<Option<ToolCallRecord>, StorageError> {
        let db = self.db.lock().await;
        Ok(db.tool_calls.iter().find(|tc| tc.id == id).cloned())
    }

    async fn get_tool_call_by_turn_and_call_id(
        &self,
        turn_id: Uuid,
        call_id: &str,
    ) -> Result<Option<ToolCallRecord>, StorageError> {
        let db = self.db.lock().await;
        Ok(db
            .tool_calls
            .iter()
            .find(|tc| tc.turn_id == turn_id && tc.call_id == call_id)
            .cloned())
    }

    async fn update_tool_call_result(
        &self,
        call_id: &str,
        turn_id: Uuid,
        result: &str,
        status: &str,
        duration_ms: i64,
        truncated: bool,
        total_bytes: Option<i64>,
    ) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        if let Some(tc) = db
            .tool_calls
            .iter_mut()
            .find(|tc| tc.call_id == call_id && tc.turn_id == turn_id)
        {
            tc.result = Some(result.to_string());
            tc.model_result = Some(swage_core::compact_tool_result_for_model_with_reference(
                result,
                status != "success",
                swage_core::tool_result_reference(&turn_id.to_string(), call_id),
            ));
            tc.status = Some(status.to_string());
            tc.duration_ms = Some(duration_ms);
            tc.truncated = truncated;
            tc.total_bytes = total_bytes;
        }
        Ok(())
    }

    async fn list_tool_calls_by_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<Vec<ToolCallRecord>, StorageError> {
        let db = self.db.lock().await;
        let mut calls: Vec<ToolCallRecord> = db
            .tool_calls
            .iter()
            .filter(|tc| tc.turn_id == turn_id)
            .cloned()
            .collect();
        calls.sort_by_key(|tc| tc.position);
        Ok(calls)
    }
}

// Stub: skill activation is not exercised in gateway turn tests.
#[async_trait]
impl SkillActivationStore for MockStorage {
    async fn activate_skill(
        &self,
        _session_id: Uuid,
        _skill_name: &str,
    ) -> Result<(), StorageError> {
        Ok(())
    }
    async fn deactivate_skill(
        &self,
        _session_id: Uuid,
        _skill_name: &str,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }
    async fn list_active_skills(
        &self,
        _session_id: Uuid,
    ) -> Result<Vec<ActiveSkillRecord>, StorageError> {
        Ok(vec![])
    }
    async fn clear_active_skills(&self, _session_id: Uuid) -> Result<(), StorageError> {
        Ok(())
    }
}

#[async_trait]
impl ApprovalStore for MockStorage {
    async fn create_approval(
        &self,
        approval: NewApprovalRequest,
    ) -> Result<ApprovalRequest, StorageError> {
        let approval_req = ApprovalRequest {
            id: Uuid::new_v4(),
            turn_id: approval.turn_id,
            agent_id: approval.agent_id,
            kind: approval.kind,
            tool_name: approval.tool_name,
            tool_arguments: approval.tool_arguments,
            hook_name: approval.hook_name,
            prompt: approval.prompt,
            context: approval.context,
            timeout_at: approval.timeout_at,
            timeout_policy: approval.timeout_policy,
            state: ApprovalState::Pending,
            decision_reason: None,
            decided_at: None,
            created_at: chrono::Utc::now(),
        };
        self.db
            .lock()
            .await
            .approvals
            .insert(approval_req.id, approval_req.clone());
        Ok(approval_req)
    }

    async fn get_approval(&self, id: Uuid) -> Result<Option<ApprovalRequest>, StorageError> {
        Ok(self.db.lock().await.approvals.get(&id).cloned())
    }

    async fn list_pending_by_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<ApprovalRequest>, StorageError> {
        let db = self.db.lock().await;
        let mut result: Vec<_> = db
            .approvals
            .values()
            .filter(|a| a.agent_id == agent_id && a.state == ApprovalState::Pending)
            .cloned()
            .collect();
        result.sort_by_key(|a| a.created_at);
        Ok(result)
    }

    async fn list_pending_by_session(&self, _: Uuid) -> Result<Vec<ApprovalRequest>, StorageError> {
        unimplemented!()
    }

    async fn list_all_pending(&self) -> Result<Vec<ApprovalRequest>, StorageError> {
        let db = self.db.lock().await;
        let mut result: Vec<_> = db
            .approvals
            .values()
            .filter(|a| a.state == ApprovalState::Pending)
            .cloned()
            .collect();
        result.sort_by_key(|a| a.created_at);
        Ok(result)
    }

    async fn list_approvals(&self) -> Result<Vec<ApprovalRequest>, StorageError> {
        let db = self.db.lock().await;
        let mut result: Vec<_> = db.approvals.values().cloned().collect();
        result.sort_by_key(|approval| std::cmp::Reverse(approval.created_at));
        Ok(result)
    }

    async fn update_approval_state(
        &self,
        id: Uuid,
        state: ApprovalState,
        reason: Option<&str>,
    ) -> Result<bool, StorageError> {
        let mut db = self.db.lock().await;
        match db.approvals.get_mut(&id) {
            None => Ok(false),
            Some(approval) => {
                if approval.state != ApprovalState::Pending {
                    return Ok(false);
                }
                approval.state = state;
                approval.decision_reason = reason.map(|s| s.to_string());
                approval.decided_at = Some(chrono::Utc::now());
                Ok(true)
            }
        }
    }

    async fn cleanup_orphaned_approvals(&self) -> Result<u64, StorageError> {
        let mut db = self.db.lock().await;
        let expired_ids: Vec<_> = db
            .approvals
            .iter()
            .filter(|(_, a)| !db.turns.contains_key(&a.turn_id))
            .map(|(id, _)| *id)
            .collect();
        let count = expired_ids.len() as u64;
        for id in expired_ids {
            if let Some(approval) = db.approvals.get_mut(&id)
                && approval.state == ApprovalState::Pending
            {
                approval.state = ApprovalState::Expired;
                approval.decided_at = Some(chrono::Utc::now());
            }
        }
        Ok(count)
    }

    async fn remember_approval(
        &self,
        session_id: Uuid,
        tool_name: &str,
        request_fingerprint: &str,
    ) -> Result<(), StorageError> {
        self.db.lock().await.approval_memory.insert((
            session_id,
            tool_name.to_string(),
            request_fingerprint.to_string(),
        ));
        Ok(())
    }

    async fn has_approval_memory(
        &self,
        session_id: Uuid,
        tool_name: &str,
        request_fingerprint: &str,
    ) -> Result<bool, StorageError> {
        Ok(self.db.lock().await.approval_memory.contains(&(
            session_id,
            tool_name.to_string(),
            request_fingerprint.to_string(),
        )))
    }
}

#[async_trait]
impl swage_storage::store::DeliveryStore for MockStorage {
    async fn create_delivery_record(
        &self,
        delivery: swage_storage::NewDeliveryRecord,
    ) -> Result<swage_storage::DeliveryRecord, StorageError> {
        Ok(swage_storage::DeliveryRecord {
            id: Uuid::new_v4(),
            session_id: delivery.session_id,
            target: delivery.target,
            content: delivery.content,
            status: delivery.status,
            error: delivery.error,
            delivered_at: chrono::Utc::now(),
        })
    }

    async fn list_delivery_records(
        &self,
    ) -> Result<Vec<swage_storage::DeliveryRecord>, StorageError> {
        Ok(Vec::new())
    }

    async fn list_delivery_records_by_session(
        &self,
        _session_id: Uuid,
    ) -> Result<Vec<swage_storage::DeliveryRecord>, StorageError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl swage_storage::store::ChannelQueueStore for MockStorage {
    async fn ingest_channel_inbound(
        &self,
        _messages: Vec<swage_storage::NewChannelInboundMessage>,
        _cursor_integration: &str,
        _cursor_account: &str,
        _cursor: &str,
    ) -> Result<usize, StorageError> {
        Ok(0)
    }

    async fn get_channel_cursor(
        &self,
        _integration: &str,
        _account: &str,
    ) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    async fn list_pending_channel_origins(
        &self,
    ) -> Result<Vec<swage_storage::PendingChannelOrigin>, StorageError> {
        Ok(Vec::new())
    }

    async fn list_pending_by_origin(
        &self,
        _origin: swage_storage::PendingChannelOrigin,
    ) -> Result<Vec<swage_storage::ChannelInboundMessage>, StorageError> {
        Ok(Vec::new())
    }

    async fn begin_channel_turn(
        &self,
        _thread_id: Uuid,
        _agent_id: &str,
        _composed_input: &str,
        _message_ids: Vec<Uuid>,
    ) -> Result<Turn, StorageError> {
        unimplemented!("MockStorage does not support channel queue turn claims")
    }

    async fn discard_channel_messages(
        &self,
        _message_ids: Vec<Uuid>,
        _reason: &str,
    ) -> Result<u64, StorageError> {
        Ok(0)
    }

    async fn requeue_presented_for_inflight_turns(&self) -> Result<u64, StorageError> {
        Ok(0)
    }
}

#[async_trait]
impl swage_storage::store::AuditStore for MockStorage {
    async fn create_audit_event(
        &self,
        event: swage_storage::NewAuditEvent,
    ) -> Result<swage_storage::AuditEvent, StorageError> {
        let event = swage_storage::AuditEvent {
            id: event.id,
            session_id: event.session_id,
            entity_kind: event.entity_kind,
            entity_id: event.entity_id,
            event_kind: event.event_kind,
            summary: event.summary,
            metadata: event.metadata,
            created_at: chrono::Utc::now(),
        };
        self.db.lock().await.audit_events.push(event.clone());
        Ok(event)
    }

    async fn list_audit_events(&self) -> Result<Vec<swage_storage::AuditEvent>, StorageError> {
        Ok(self.db.lock().await.audit_events.clone())
    }

    async fn list_audit_events_by_session(
        &self,
        session_id: Uuid,
    ) -> Result<Vec<swage_storage::AuditEvent>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .audit_events
            .iter()
            .filter(|event| event.session_id == Some(session_id))
            .cloned()
            .collect())
    }
}

#[async_trait]
impl swage_storage::store::AgentMemoryStore for MockStorage {
    async fn create_agent_memory(
        &self,
        record: swage_storage::NewAgentMemoryRecord,
    ) -> Result<swage_storage::AgentMemoryRecord, StorageError> {
        let record = swage_storage::AgentMemoryRecord {
            id: record.id,
            bench_id: record.bench_id,
            session_id: record.session_id,
            turn_id: record.turn_id,
            topology_run_id: record.topology_run_id,
            node_execution_id: record.node_execution_id,
            agent_id: record.agent_id,
            memory_kind: record.memory_kind,
            title: record.title,
            content: record.content,
            tags: record.tags,
            outcome_status: record.outcome_status,
            source_summary: record.source_summary,
            metadata: record.metadata,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        self.db.lock().await.agent_memories.push(record.clone());
        Ok(record)
    }

    async fn get_agent_memory(
        &self,
        id: Uuid,
    ) -> Result<Option<swage_storage::AgentMemoryRecord>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .agent_memories
            .iter()
            .find(|record| record.id == id)
            .cloned())
    }

    async fn list_agent_memories(
        &self,
        bench_id: Uuid,
        agent_id: Option<&str>,
        session_id: Option<Uuid>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<swage_storage::AgentMemoryRecord>, StorageError> {
        let query = query.map(|value| value.to_lowercase());
        let mut records = self
            .db
            .lock()
            .await
            .agent_memories
            .iter()
            .filter(|record| record.bench_id == bench_id)
            .filter(|record| agent_id.is_none_or(|id| record.agent_id == id))
            .filter(|record| session_id.is_none_or(|id| record.session_id == id))
            .filter(|record| {
                query.as_ref().is_none_or(|needle| {
                    record.title.to_lowercase().contains(needle)
                        || record.content.to_lowercase().contains(needle)
                        || record.source_summary.to_lowercase().contains(needle)
                        || record
                            .tags
                            .iter()
                            .any(|tag| tag.to_lowercase().contains(needle))
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by_key(|record| std::cmp::Reverse(record.updated_at));
        records.truncate(limit);
        Ok(records)
    }
}

#[async_trait]
impl swage_storage::store::ConfigurationVersionStore for MockStorage {
    async fn create_configuration_version(
        &self,
        version: swage_storage::NewConfigurationVersion,
    ) -> Result<swage_storage::ConfigurationVersion, StorageError> {
        let record = swage_storage::ConfigurationVersion {
            id: version.id,
            bench_id: version.bench_id,
            session_id: version.session_id,
            entity_kind: version.entity_kind,
            entity_id: version.entity_id,
            version_number: version.version_number,
            operation_kind: version.operation_kind,
            source: version.source,
            summary: version.summary,
            snapshot: version.snapshot,
            diff: version.diff,
            previous_version_id: version.previous_version_id,
            rollback_to_version_id: version.rollback_to_version_id,
            created_at: chrono::Utc::now(),
        };
        self.db
            .lock()
            .await
            .configuration_versions
            .push(record.clone());
        Ok(record)
    }

    async fn get_configuration_version(
        &self,
        id: Uuid,
    ) -> Result<Option<swage_storage::ConfigurationVersion>, StorageError> {
        Ok(self
            .db
            .lock()
            .await
            .configuration_versions
            .iter()
            .find(|version| version.id == id)
            .cloned())
    }

    async fn list_configuration_versions(
        &self,
        bench_id: Option<Uuid>,
    ) -> Result<Vec<swage_storage::ConfigurationVersion>, StorageError> {
        let mut versions = self.db.lock().await.configuration_versions.clone();
        if let Some(bench_id) = bench_id {
            versions.retain(|version| version.bench_id == Some(bench_id));
        }
        versions.sort_by_key(|version| std::cmp::Reverse(version.created_at));
        Ok(versions)
    }

    async fn list_configuration_versions_for_entity(
        &self,
        entity_kind: &str,
        entity_id: &str,
    ) -> Result<Vec<swage_storage::ConfigurationVersion>, StorageError> {
        let mut versions = self
            .db
            .lock()
            .await
            .configuration_versions
            .iter()
            .filter(|version| version.entity_kind == entity_kind && version.entity_id == entity_id)
            .cloned()
            .collect::<Vec<_>>();
        versions.sort_by_key(|version| std::cmp::Reverse(version.version_number));
        Ok(versions)
    }

    async fn get_latest_configuration_version(
        &self,
        entity_kind: &str,
        entity_id: &str,
    ) -> Result<Option<swage_storage::ConfigurationVersion>, StorageError> {
        Ok(self
            .list_configuration_versions_for_entity(entity_kind, entity_id)
            .await?
            .into_iter()
            .next())
    }
}

#[async_trait]
impl swage_storage::store::SecretStore for MockStorage {
    async fn create_secret(
        &self,
        secret: swage_storage::NewSecretRecord,
    ) -> Result<swage_storage::SecretRecord, StorageError> {
        let record = swage_storage::SecretRecord {
            id: secret.id,
            name: secret.name,
            material_kind: secret.material_kind,
            owner_kind: secret.owner_kind,
            backend_kind: secret.backend_kind.clone(),
            scope_kind: secret.scope_kind,
            scope_id: secret.scope_id,
            reference: format!("secret://{}/{}", secret.backend_kind, secret.id),
            external_reference: secret.external_reference,
            metadata: secret.metadata,
            status: "active".into(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            revoked_at: None,
        };
        let mut db = self.db.lock().await;
        if let Some(value) = secret.value {
            db.managed_secret_values.insert(record.id, value);
        }
        db.secrets.insert(record.id, record.clone());
        Ok(record)
    }

    async fn list_secrets(&self) -> Result<Vec<swage_storage::SecretRecord>, StorageError> {
        if self.fail_secret_reads.load(Ordering::SeqCst) {
            return Err(StorageError::ConnectionFailed(
                "injected secret storage failure".into(),
            ));
        }
        Ok(self.db.lock().await.secrets.values().cloned().collect())
    }

    async fn get_secret(
        &self,
        id: Uuid,
    ) -> Result<Option<swage_storage::SecretRecord>, StorageError> {
        Ok(self.db.lock().await.secrets.get(&id).cloned())
    }

    async fn update_secret_value(
        &self,
        id: Uuid,
        value: Option<String>,
        external_reference: Option<String>,
        metadata: serde_json::Value,
    ) -> Result<swage_storage::SecretRecord, StorageError> {
        let mut db = self.db.lock().await;
        if let Some(value) = value {
            db.managed_secret_values.insert(id, value);
        }
        let record = db
            .secrets
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "secret".into(),
                id: id.to_string(),
            })?;
        record.external_reference = external_reference;
        record.metadata = metadata;
        record.updated_at = chrono::Utc::now();
        Ok(record.clone())
    }

    async fn revoke_secret(&self, id: Uuid) -> Result<swage_storage::SecretRecord, StorageError> {
        let mut db = self.db.lock().await;
        let record = db
            .secrets
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "secret".into(),
                id: id.to_string(),
            })?;
        record.status = "revoked".into();
        record.updated_at = chrono::Utc::now();
        record.revoked_at = Some(chrono::Utc::now());
        Ok(record.clone())
    }

    async fn restore_secret(&self, id: Uuid) -> Result<swage_storage::SecretRecord, StorageError> {
        let mut db = self.db.lock().await;
        let record = db
            .secrets
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "secret".into(),
                id: id.to_string(),
            })?;
        record.status = "active".into();
        record.updated_at = chrono::Utc::now();
        record.revoked_at = None;
        Ok(record.clone())
    }

    async fn get_managed_secret_value(&self, id: Uuid) -> Result<Option<String>, StorageError> {
        if self.fail_secret_reads.load(Ordering::SeqCst) {
            return Err(StorageError::ConnectionFailed(
                "injected managed-secret storage failure".into(),
            ));
        }
        Ok(self.db.lock().await.managed_secret_values.get(&id).cloned())
    }
}

#[async_trait]
impl swage_storage::store::AgentMessageStore for MockStorage {
    async fn create_agent_message(
        &self,
        msg: NewAgentMessage,
    ) -> Result<AgentMessage, StorageError> {
        msg.validate()?;

        let now = chrono::Utc::now();
        let record = AgentMessage {
            id: Uuid::new_v4(),
            sender_agent_id: msg.sender_agent_id,
            recipient_agent_id: msg.recipient_agent_id,
            kind: msg.kind,
            correlation_id: msg.correlation_id,
            payload: msg.payload,
            status: swage_core::MessageStatus::Pending,
            failure_reason: None,
            created_at: now,
            delivered_at: None,
        };
        self.db
            .lock()
            .await
            .agent_messages
            .insert(record.id, record.clone());
        Ok(record)
    }
    async fn get_agent_message(&self, id: Uuid) -> Result<Option<AgentMessage>, StorageError> {
        Ok(self.db.lock().await.agent_messages.get(&id).cloned())
    }
    async fn list_pending_for_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<AgentMessage>, StorageError> {
        let mut messages: Vec<_> = self
            .db
            .lock()
            .await
            .agent_messages
            .values()
            .filter(|message| {
                message.recipient_agent_id == agent_id
                    && message.status == swage_core::MessageStatus::Pending
            })
            .cloned()
            .collect();
        messages.sort_by_key(|message| message.created_at);
        Ok(messages)
    }
    async fn update_agent_message_status(
        &self,
        id: Uuid,
        status: swage_core::MessageStatus,
        failure_reason: Option<&str>,
    ) -> Result<bool, StorageError> {
        let mut db = self.db.lock().await;
        let Some(message) = db.agent_messages.get_mut(&id) else {
            return Ok(false);
        };
        if message.status != swage_core::MessageStatus::Pending {
            return Ok(false);
        }

        message.status = status.clone();
        message.failure_reason = failure_reason.map(str::to_string);
        message.delivered_at = if status == swage_core::MessageStatus::Delivered {
            Some(chrono::Utc::now())
        } else {
            None
        };
        Ok(true)
    }
    async fn update_agent_message_payload(
        &self,
        id: Uuid,
        payload: serde_json::Value,
    ) -> Result<(), StorageError> {
        let mut db = self.db.lock().await;
        let message = db
            .agent_messages
            .get_mut(&id)
            .ok_or_else(|| StorageError::NotFound {
                entity: "agent_message".into(),
                id: id.to_string(),
            })?;
        message.payload = payload;
        Ok(())
    }
    async fn list_by_correlation(
        &self,
        correlation_id: Uuid,
    ) -> Result<Vec<AgentMessage>, StorageError> {
        let mut messages: Vec<_> = self
            .db
            .lock()
            .await
            .agent_messages
            .values()
            .filter(|message| message.correlation_id == Some(correlation_id))
            .cloned()
            .collect();
        messages.sort_by_key(|message| message.created_at);
        Ok(messages)
    }
    async fn expire_stale_messages(
        &self,
        max_age: std::time::Duration,
    ) -> Result<u64, StorageError> {
        let now = chrono::Utc::now();
        let mut db = self.db.lock().await;
        let mut expired = 0;
        for message in db.agent_messages.values_mut() {
            if message.status != swage_core::MessageStatus::Pending {
                continue;
            }
            let Ok(age) = now.signed_duration_since(message.created_at).to_std() else {
                continue;
            };
            if age > max_age {
                message.status = swage_core::MessageStatus::Failed;
                message.failure_reason = Some("delivery timeout".into());
                message.delivered_at = None;
                expired += 1;
            }
        }
        Ok(expired)
    }
}

// -----------------------------------------------------------------------
// Test helpers
// -----------------------------------------------------------------------

/// Build an AppState with a pre-populated session and its default thread.
///
/// Returns the state and the session_id. The thread is created automatically
/// so that `process_turn` can look it up via `list_by_session`.
pub(crate) async fn make_state_with_session(provider_chunks: Vec<StreamChunk>) -> (AppState, Uuid) {
    let storage = Arc::new(MockStorage::new());
    make_state_with_storage(provider_chunks, storage).await
}

async fn make_state_with_storage(
    provider_chunks: Vec<StreamChunk>,
    storage: Arc<MockStorage>,
) -> (AppState, Uuid) {
    // Create the default bench via BenchStore.
    let bench = storage
        .get_or_create_default()
        .await
        .expect("create default bench");

    // Create a session.
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    // Create the default thread for this session.
    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let provider = Arc::new(MockProvider::new(provider_chunks));
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        provider as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    (state, session_id)
}

/// Like `make_state_with_session` but accepts an explicit `ResolvedConfig`.
///
/// Used for tests that need non-default config (e.g. output schema configured).
pub(crate) async fn make_state_with_config(
    provider_chunks: Vec<StreamChunk>,
    config: swage_config::ResolvedConfig,
) -> (AppState, Uuid) {
    let storage = Arc::new(MockStorage::new());

    let bench = storage
        .get_or_create_default()
        .await
        .expect("create default bench");

    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let provider = Arc::new(MockProvider::new(provider_chunks));
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        provider as Arc<dyn LlmProvider>,
        config,
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    (state, session_id)
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn full_turn_stores_messages_and_completes() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".to_string(),
        },
        StreamChunk::Delta {
            content: ", world".to_string(),
        },
        StreamChunk::Delta {
            content: "!".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hi".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete without error");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    let token_contents: Vec<String> = events
        .iter()
        .filter_map(|e| {
            if let ServerEvent::Token { content } = e {
                Some(content.clone())
            } else {
                None
            }
        })
        .collect();

    assert_eq!(token_contents, ["Hello", ", world", "!"]);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
    );

    // Get thread_id and verify messages stored in the DB.
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;

    let messages = state
        .storage
        .list_messages_by_thread(thread_id)
        .await
        .unwrap();
    assert_eq!(messages.len(), 2, "user message + assistant message");
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(messages[0].content, "Hi");
    assert_eq!(messages[1].role, Role::Assistant);
    assert_eq!(messages[1].content, "Hello, world!");

    // Verify the turn was marked Completed.
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].state, TurnState::Completed);
}

#[tokio::test]
async fn cancelled_turn_stores_partial_and_marks_interrupted() {
    let storage = Arc::new(MockStorage::new());

    let bench = storage
        .get_or_create_default()
        .await
        .expect("create default bench");

    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let state = Arc::new(AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(PendingProvider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    ));

    let (tx, mut rx) = mpsc::unbounded_channel();

    let state_clone = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        process_turn(
            &state_clone,
            session_id,
            None,
            "Hi".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
    });

    for _ in 0..50 {
        tokio::task::yield_now().await;
        if state.active_turns.lock().await.contains_key(&session_id) {
            break;
        }
    }

    assert!(
        state.active_turns.lock().await.contains_key(&session_id),
        "turn should be active before cancel"
    );

    cancel_turn(&state, session_id)
        .await
        .expect("cancel should succeed");

    handle
        .await
        .expect("task should not panic")
        .expect("process_turn ok");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::Cancelled {})),
        "expected Cancelled event"
    );

    // Verify turn is Interrupted in storage.
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    assert_eq!(turns[0].state, TurnState::Interrupted);
}

#[tokio::test]
async fn provider_error_marks_turn_failed() {
    let (state, session_id) = make_state_with_session(vec![StreamChunk::Error {
        message: "provider overloaded".to_string(),
    }])
    .await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hi".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("error is surfaced via event, not Err return");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    // Agent delegates error categorisation — provider errors surface as agent_error.
    assert!(events.iter().any(|e| matches!(
        e,
        ServerEvent::Error { code, .. } if code == "agent_error"
    )));

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    assert_eq!(turns[0].state, TurnState::Failed);
}

#[tokio::test]
async fn concurrent_turn_rejected_with_turn_in_progress() {
    // Manually create a Processing turn to simulate an active turn.
    let (state, session_id) = make_state_with_session(vec![]).await;

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;

    // Create a turn and advance it to Processing.
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: None,
            parent_turn_id: None,
        })
        .await
        .unwrap();
    state
        .storage
        .update_turn_state(turn.id, TurnState::Processing)
        .await
        .unwrap();

    let (tx, _rx) = mpsc::unbounded_channel();
    let result = process_turn(
        &state,
        session_id,
        None,
        "Another message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await;

    assert!(
        matches!(result, Err(GatewayError::TurnInProgress { .. })),
        "expected TurnInProgress, got: {result:?}"
    );
}

#[tokio::test]
async fn pending_turn_rejected_with_turn_in_progress() {
    let (state, session_id) = make_state_with_session(vec![]).await;

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;

    state
        .storage
        .create_turn(NewTurn {
            thread_id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: None,
            parent_turn_id: None,
        })
        .await
        .unwrap();

    let (tx, _rx) = mpsc::unbounded_channel();
    let result = process_turn(
        &state,
        session_id,
        None,
        "Another message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await;

    assert!(
        matches!(result, Err(GatewayError::TurnInProgress { .. })),
        "expected TurnInProgress for pending turn, got: {result:?}"
    );
}

#[tokio::test]
async fn blocked_inbound_hook_marks_turn_failed_and_clears_active_turn() {
    let (mut state, session_id) = make_state_with_session(vec![]).await;
    let mut hook_registry = HookRegistry::new();
    hook_registry.register(HookRegistration::new(Arc::new(BlockInboundHook)));
    state.hook_registry = Some(Arc::new(hook_registry));

    let (tx, _rx) = mpsc::unbounded_channel();
    let result = process_turn(
        &state,
        session_id,
        None,
        "blocked message".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await;

    assert!(
        matches!(result, Err(GatewayError::ConfigError(ref message)) if message.contains("blocked by test hook")),
        "expected blocked inbound hook error, got: {result:?}"
    );
    assert!(
        !state.active_turns.lock().await.contains_key(&session_id),
        "blocked inbound hook must not leave a stale active_turns entry"
    );

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    assert_eq!(turns.len(), 1, "expected placeholder turn to be created");
    assert_eq!(turns[0].state, TurnState::Failed);
}

// -----------------------------------------------------------------------
// Mock tool for the execution loop
// -----------------------------------------------------------------------

struct EchoMockTool;

#[async_trait::async_trait]
impl crate::tool::Tool for EchoMockTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echo the input back"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        })
    }
    async fn execute(
        &self,
        arguments: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        let text = arguments["text"].as_str().unwrap_or("").to_string();
        Ok(serde_json::json!({"output": text}))
    }
}

struct BlockingMockTool {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait::async_trait]
impl crate::tool::Tool for BlockingMockTool {
    fn name(&self) -> &str {
        "block"
    }
    fn description(&self) -> &str {
        "Block until the test releases execution"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": true
        })
    }
    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        self.started.notify_waiters();
        self.release.notified().await;
        Ok(serde_json::json!({"output": "released"}))
    }
}

/// Build an AppState with a multi-round provider and an EchoMockTool.
async fn make_state_with_tools(rounds: Vec<Vec<StreamChunk>>) -> (AppState, Uuid) {
    let storage = Arc::new(MockStorage::new());

    let bench = storage
        .get_or_create_default()
        .await
        .expect("create default bench");

    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let provider = Arc::new(MultiRoundMockProvider::new(rounds));

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        provider as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );
    (state, session_id)
}

async fn make_state_with_capturing_tools(
    rounds: Vec<Vec<StreamChunk>>,
) -> (
    AppState,
    Uuid,
    Arc<std::sync::Mutex<Vec<CompletionRequest>>>,
) {
    let storage = Arc::new(MockStorage::new());

    let bench = storage
        .get_or_create_default()
        .await
        .expect("create default bench");

    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let (provider, captured) = CapturingMultiRoundProvider::new(rounds);

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    (state, session_id, captured)
}

// -----------------------------------------------------------------------
// Tool execution loop tests
// -----------------------------------------------------------------------

#[tokio::test]
async fn tool_call_sends_tool_start_and_tool_end_events() {
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "call_1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "hello" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "The tool said: hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Use echo".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    assert!(
        events.iter().any(|e| matches!(
            e,
            ServerEvent::ToolStart { tool, call_id, .. }
            if tool == "echo" && call_id == "call_1"
        )),
        "expected ToolStart event, got: {events:?}"
    );

    assert!(
        events.iter().any(|e| matches!(
            e,
            ServerEvent::ToolEnd { tool, call_id, status, .. }
            if tool == "echo" && call_id == "call_1" && status == "success"
        )),
        "expected ToolEnd event, got: {events:?}"
    );

    assert!(
        events.iter().any(|e| matches!(
            e,
            ServerEvent::Token { content } if content.contains("hello")
        )),
        "expected token with 'hello', got: {events:?}"
    );

    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
    );
}

#[tokio::test]
async fn tool_call_stores_tool_call_record() {
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "call_store".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "stored" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Store this".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    let tool_calls = state
        .storage
        .list_tool_calls_by_turn(turns[0].id)
        .await
        .unwrap();
    assert_eq!(tool_calls.len(), 1, "expected 1 tool call record");
    assert_eq!(tool_calls[0].call_id, "call_store");
    assert_eq!(tool_calls[0].tool, "echo");
    assert!(
        tool_calls[0].result.is_some(),
        "tool call should have a result"
    );
    // The agent serialises the full ToolOutput.result JSON as the stored string.
    assert_eq!(
        tool_calls[0].result.as_deref(),
        Some("{\"output\":\"stored\"}")
    );
}

#[tokio::test]
async fn unknown_tool_returns_error_to_model() {
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "call_unknown".into(),
            name: "nonexistent_tool".into(),
            arguments: serde_json::json!({}),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "Sorry, that tool doesn't exist.".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id) = make_state_with_tools(vec![round_1, round_2]).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Use unknown".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should still complete");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    assert!(
        events.iter().any(|e| matches!(
            e,
            ServerEvent::ToolEnd { status, .. } if status == "error"
        )),
        "expected ToolEnd with error status, got: {events:?}"
    );

    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
    );
}

#[tokio::test]
async fn cancelling_during_tool_execution_interrupts_without_waiting_for_tool_completion() {
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "call_block".into(),
            name: "block".into(),
            arguments: serde_json::json!({}),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let storage = Arc::new(MockStorage::new());
    let bench = storage
        .get_or_create_default()
        .await
        .expect("create default bench");
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(MultiRoundMockProvider::new(vec![round_1])) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![Arc::new(BlockingMockTool {
            started: started.clone(),
            release: release.clone(),
        }) as Arc<dyn crate::tool::Tool>]),
        None,
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    let state = Arc::new(state);
    let worker_state = state.clone();
    let handle = tokio::spawn(async move {
        process_turn(
            &worker_state,
            session_id,
            None,
            "Run block".to_string(),
            tx,
            Arc::new(Mutex::new(SubscriptionState::new(true))),
            None,
        )
        .await
    });

    started.notified().await;
    cancel_turn(&state, session_id)
        .await
        .expect("cancel should succeed while tool is running");

    handle
        .await
        .expect("task should not panic")
        .expect("process_turn ok");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::Cancelled {})),
        "expected Cancelled event, got: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            ServerEvent::ToolEnd { tool, call_id, status, .. }
            if tool == "block" && call_id == "call_block" && status == "cancelled"
        )),
        "expected cancelled ToolEnd event, got: {events:?}"
    );

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    assert_eq!(turns[0].state, TurnState::Interrupted);

    let thread = ThreadStore::get(&*state.storage, thread_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(thread.state, ThreadState::Idle);

    release.notify_waiters();
}

#[tokio::test]
async fn multi_round_tool_calling() {
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "first" }),
        },
        StreamChunk::ToolUse {
            call_id: "c2".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "second" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::ToolUse {
            call_id: "c3".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "third" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_3 = vec![
        StreamChunk::Delta {
            content: "All done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id) = make_state_with_tools(vec![round_1, round_2, round_3]).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Multi-round".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    let tool_start_count = events
        .iter()
        .filter(|e| matches!(e, ServerEvent::ToolStart { .. }))
        .count();
    let tool_end_count = events
        .iter()
        .filter(|e| matches!(e, ServerEvent::ToolEnd { .. }))
        .count();
    assert_eq!(tool_start_count, 3, "expected 3 ToolStart events");
    assert_eq!(tool_end_count, 3, "expected 3 ToolEnd events");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    let tool_calls = state
        .storage
        .list_tool_calls_by_turn(turns[0].id)
        .await
        .unwrap();
    assert_eq!(tool_calls.len(), 3, "expected 3 tool call records");

    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
    );
}

#[tokio::test]
async fn second_tool_round_request_keeps_structured_tool_messages() {
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "first" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id, captured) =
        make_state_with_capturing_tools(vec![round_1, round_2]).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Use echo".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2, "expected two provider requests");

    let second = &requests[1];
    assert_eq!(second.messages.len(), 3);
    assert_eq!(second.messages[1].role(), Role::Assistant);
    assert!(
        second.messages[1].tool_replay().is_some(),
        "assistant tool-use message should retain canonical replay"
    );
    assert_eq!(second.messages[2].role(), Role::User);
    assert!(
        second.messages[2].tool_replay().is_some(),
        "tool-result message should retain canonical replay"
    );
}

#[tokio::test]
async fn second_tool_round_request_does_not_replay_reasoning_by_default() {
    let round_1 = vec![
        StreamChunk::Reasoning {
            content: "Private provider reasoning".into(),
            details: Some(serde_json::json!({ "source": "test" })),
        },
        StreamChunk::ToolUse {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "first" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id, captured) =
        make_state_with_capturing_tools(vec![round_1, round_2]).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Use echo".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2, "expected two provider requests");
    assert!(
        !requests[1].messages.iter().any(|message| {
            message.tool_replay().is_some_and(|replay| {
                replay
                    .iter()
                    .any(|part| matches!(part, swage_core::ToolReplayPart::Reasoning { .. }))
            })
        }),
        "reasoning side-channels must stay out of replay unless the model opts in"
    );
}

#[tokio::test]
async fn second_tool_round_request_replays_reasoning_when_model_capability_opts_in() {
    let round_1 = vec![
        StreamChunk::Reasoning {
            content: "Private provider reasoning".into(),
            details: Some(serde_json::json!({ "source": "test" })),
        },
        StreamChunk::ToolUse {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "first" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id, captured) =
        make_state_with_capturing_tools(vec![round_1, round_2]).await;
    let mut config = swage_config::ResolvedConfig::default();
    config.models.default = "reasoner".to_string();
    config.models.entries = [(
        "reasoner".to_string(),
        swage_config::ModelEndpointConfig {
            provider_id: "mock".to_string(),
            model_id: "mock-reasoning".to_string(),
            aliases: Vec::new(),
            context_window: None,
            generation_preset: None,
            capabilities: swage_config::ModelCapabilitiesConfig {
                reasoning_replay: true,
            },
        },
    )]
    .into_iter()
    .collect();
    state.replace_runtime(
        config,
        state.active_provider(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
    );
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Use echo".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2, "expected two provider requests");
    let assistant_replay = requests[1]
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::Assistant
                && message.tool_replay().is_some_and(|replay| {
                    replay
                        .iter()
                        .any(|part| matches!(part, swage_core::ToolReplayPart::Reasoning { .. }))
                })
        })
        .and_then(|message| message.tool_replay())
        .expect("opted-in model should receive reasoning replay");

    assert!(matches!(
        assistant_replay.first(),
        Some(swage_core::ToolReplayPart::Reasoning { content, details })
            if content == "Private provider reasoning"
                && details.as_ref().is_some_and(|details| details["source"] == "test")
    ));
    assert!(
        assistant_replay
            .iter()
            .any(|part| matches!(part, swage_core::ToolReplayPart::ToolCall { .. })),
        "reasoning replay should travel with the assistant tool-use replay"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn later_turn_rebuilds_tool_replay_from_persisted_compact_rows() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let bench = storage
        .get_or_create_default()
        .await
        .expect("create default bench");
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let first_provider = Arc::new(MultiRoundMockProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "c1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "first" }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "first complete".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]));

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        first_provider as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Use echo".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("first turn should complete");

    let (second_provider, captured) = CapturingProvider::new(vec![
        StreamChunk::Delta {
            content: "second complete".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]);
    state.replace_runtime(
        state.active_config(),
        Arc::new(second_provider) as Arc<dyn LlmProvider>,
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Continue".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("second turn should complete");

    let captured = captured
        .lock()
        .unwrap()
        .clone()
        .expect("second provider request should be captured");
    let assistant_tool_message = captured
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::Assistant
                && message.tool_replay().is_some_and(|replay| {
                    replay
                        .iter()
                        .any(|part| matches!(part, swage_core::ToolReplayPart::ToolCall { .. }))
                })
        })
        .expect("later turn should include assistant tool-use replay");
    let tool_use = assistant_tool_message
        .tool_replay()
        .unwrap()
        .iter()
        .find_map(|part| match part {
            swage_core::ToolReplayPart::ToolCall {
                call_id,
                name,
                arguments,
            } => Some((call_id, name, arguments)),
            _ => None,
        })
        .expect("tool-use replay");
    assert_eq!(tool_use.0, "c1");
    assert_eq!(tool_use.1, "echo");
    assert_eq!(tool_use.2["kind"], "tool_arguments");
    assert_eq!(tool_use.2["preview"], r#"{"text":"first"}"#);

    let tool_result_message = captured
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::User
                && message.tool_replay().is_some_and(|replay| {
                    replay
                        .iter()
                        .any(|part| matches!(part, swage_core::ToolReplayPart::ToolResult { .. }))
                })
        })
        .expect("later turn should include user tool-result replay");
    let tool_result = tool_result_message
        .tool_replay()
        .unwrap()
        .iter()
        .find_map(|part| match part {
            swage_core::ToolReplayPart::ToolResult {
                call_id, content, ..
            } => Some((call_id, content)),
            _ => None,
        })
        .expect("tool-result replay");
    assert_eq!(tool_result.0, "c1");
    let envelope: swage_core::CompactToolReplay = serde_json::from_str(tool_result.1)
        .expect("tool result content should be compact replay JSON");
    assert_eq!(envelope.kind, "tool_result");
    assert_eq!(envelope.preview, r#"{"output":"first"}"#);
    assert!(
        envelope
            .full_result_ref
            .as_deref()
            .is_some_and(|reference| reference.ends_with(":c1:result")),
        "persisted replay should include a durable full-result reference"
    );
    assert_eq!(envelope.is_error, Some(false));
}

#[tokio::test(flavor = "multi_thread")]
async fn later_turn_rebuilds_multi_round_tool_replay_as_separate_pairs() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let bench = storage
        .get_or_create_default()
        .await
        .expect("create default bench");
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create default thread");

    let first_provider = Arc::new(MultiRoundMockProvider::new(vec![
        vec![
            StreamChunk::ToolUse {
                call_id: "c1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "first" }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::ToolUse {
                call_id: "c2".into(),
                name: "echo".into(),
                arguments: serde_json::json!({ "text": "second" }),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
        vec![
            StreamChunk::Delta {
                content: "done".into(),
            },
            StreamChunk::Done {
                finish_reason: None,
            },
        ],
    ]));

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        first_provider as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Use echo twice".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("first turn should complete");

    let threads = storage
        .list_by_session(session_id)
        .await
        .expect("list threads");
    let turns = storage
        .list_turns_by_thread(threads[0].id)
        .await
        .expect("list turns");
    let first_turn_calls = storage
        .list_tool_calls_by_turn(turns[0].id)
        .await
        .expect("list first-turn tool calls");
    assert_eq!(
        first_turn_calls
            .iter()
            .map(|call| (call.call_id.as_str(), call.provider_round))
            .collect::<Vec<_>>(),
        vec![("c1", 1), ("c2", 2)]
    );

    let (second_provider, captured) = CapturingProvider::new(vec![
        StreamChunk::Delta {
            content: "second complete".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]);
    state.replace_runtime(
        state.active_config(),
        Arc::new(second_provider) as Arc<dyn LlmProvider>,
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Continue".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("second turn should complete");

    let captured = captured
        .lock()
        .unwrap()
        .clone()
        .expect("second provider request should be captured");
    let replay_tool_use_ids: Vec<Vec<String>> = captured
        .messages
        .iter()
        .filter(|message| message.role() == Role::Assistant)
        .filter_map(|message| message.tool_replay())
        .map(|replay| {
            replay
                .iter()
                .filter_map(|part| match part {
                    swage_core::ToolReplayPart::ToolCall { call_id, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .filter(|ids| !ids.is_empty())
        .collect();

    assert_eq!(
        replay_tool_use_ids,
        vec![vec!["c1".to_string()], vec!["c2".to_string()]],
        "later-turn replay must preserve provider tool-round boundaries"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn branch_turn_rebuilds_tool_replay_from_copied_history() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let bench = storage
        .get_or_create_default()
        .await
        .expect("create default bench");
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    let parent_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create parent thread");
    let parent_turn = storage
        .create_turn(NewTurn {
            thread_id: parent_thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Completed,
            user_input: Some("Use echo".to_string()),
            parent_turn_id: None,
        })
        .await
        .expect("create parent turn");
    storage
        .create_message(NewMessage {
            thread_id: parent_thread.id,
            turn_id: Some(parent_turn.id),
            role: Role::User,
            content: "Use echo".to_string(),
            position: 0,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create parent user message");
    let assistant_message = storage
        .create_message(NewMessage {
            thread_id: parent_thread.id,
            turn_id: Some(parent_turn.id),
            role: Role::Assistant,
            content: "Echo complete".to_string(),
            position: 1,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create parent assistant message");
    storage
        .create_tool_call(NewToolCall {
            turn_id: parent_turn.id,
            agent_id: "gateway".to_string(),
            call_id: "call_1".to_string(),
            tool: "echo".to_string(),
            arguments: r#"{"text":"first"}"#.to_string(),
            model_arguments: swage_core::compact_tool_arguments_for_model(
                &serde_json::json!({ "text": "first" }),
            ),
            result: Some(r#"{"output":"first"}"#.to_string()),
            model_result: Some(swage_core::compact_tool_result_for_model_with_reference(
                r#"{"output":"first"}"#,
                false,
                "turn:parent:call_1:result",
            )),
            status: Some("success".to_string()),
            duration_ms: Some(12),
            truncated: false,
            total_bytes: Some(18),
            provider_round: 1,
            position: 0,
            parent_call_id: None,
        })
        .await
        .expect("create parent tool call");

    let branch = storage
        .create_branch(parent_thread.id, assistant_message.id)
        .await
        .expect("create branch");
    let child_turn = storage
        .list_turns_by_thread(branch.child_thread_id)
        .await
        .expect("list child turns")
        .into_iter()
        .next()
        .expect("branch should copy parent turn");
    let (provider, captured) = CapturingProvider::new(vec![
        StreamChunk::Delta {
            content: "branch complete".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        Some(branch.child_thread_id),
        "Continue from branch".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("branch turn should complete");

    let captured = captured
        .lock()
        .unwrap()
        .clone()
        .expect("provider request should be captured");
    let assistant_tool_replay = captured
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::Assistant
                && message.tool_replay().is_some_and(|replay| {
                    replay
                        .iter()
                        .any(|part| matches!(part, swage_core::ToolReplayPart::ToolCall { .. }))
                })
        })
        .and_then(|message| message.tool_replay())
        .expect("branch history should rebuild assistant tool replay");
    assert!(assistant_tool_replay.iter().any(|part| matches!(
        part,
        swage_core::ToolReplayPart::ToolCall { call_id, name, arguments }
            if call_id == "call_1"
                && name == "echo"
                && arguments["kind"] == "tool_arguments"
                && arguments["preview"] == r#"{"text":"first"}"#
    )));

    let tool_result_replay = captured
        .messages
        .iter()
        .find(|message| {
            message.role() == Role::User
                && message.tool_replay().is_some_and(|replay| {
                    replay
                        .iter()
                        .any(|part| matches!(part, swage_core::ToolReplayPart::ToolResult { .. }))
                })
        })
        .and_then(|message| message.tool_replay())
        .expect("branch history should rebuild tool result replay");
    let result_content = tool_result_replay
        .iter()
        .find_map(|part| match part {
            swage_core::ToolReplayPart::ToolResult {
                call_id, content, ..
            } if call_id == "call_1" => Some(content),
            _ => None,
        })
        .expect("tool result replay for copied branch call");
    let envelope: swage_core::CompactToolReplay =
        serde_json::from_str(result_content).expect("tool result replay should stay compact");
    assert_eq!(envelope.preview, r#"{"output":"first"}"#);
    let expected_ref = swage_core::tool_result_reference(&child_turn.id.to_string(), "call_1");
    assert_eq!(
        envelope.full_result_ref.as_deref(),
        Some(expected_ref.as_str())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn later_turn_rebuilds_failed_tool_replay_as_error() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let bench = storage
        .get_or_create_default()
        .await
        .expect("create default bench");
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,

            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    let thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create thread");
    let failed_turn = storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Completed,
            user_input: Some("Run the command".to_string()),
            parent_turn_id: None,
        })
        .await
        .expect("create failed-call turn");
    storage
        .create_message(NewMessage {
            thread_id: thread.id,
            turn_id: Some(failed_turn.id),
            role: Role::User,
            content: "Run the command".to_string(),
            position: 0,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create user message");
    storage
        .create_message(NewMessage {
            thread_id: thread.id,
            turn_id: Some(failed_turn.id),
            role: Role::Assistant,
            content: "Trying the command".to_string(),
            position: 1,
            agent_id: "gateway".to_string(),
        })
        .await
        .expect("create assistant message");
    storage
        .create_tool_call(NewToolCall {
            turn_id: failed_turn.id,
            agent_id: "gateway".to_string(),
            call_id: "call_fail".to_string(),
            tool: "shell".to_string(),
            arguments: r#"{"command":"missing-command"}"#.to_string(),
            model_arguments: swage_core::compact_tool_arguments_for_model(
                &serde_json::json!({ "command": "missing-command" }),
            ),
            result: Some("command not found".to_string()),
            model_result: Some(swage_core::compact_tool_result_for_model_with_reference(
                "command not found",
                true,
                "turn:failed:call_fail:result",
            )),
            status: Some("error".to_string()),
            duration_ms: Some(7),
            truncated: false,
            total_bytes: Some(17),
            provider_round: 1,
            position: 0,
            parent_call_id: None,
        })
        .await
        .expect("create failed tool call");

    let (provider, captured) = CapturingProvider::new(vec![
        StreamChunk::Delta {
            content: "next answer".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        Some(thread.id),
        "Continue".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("later turn should complete");

    let captured = captured
        .lock()
        .unwrap()
        .clone()
        .expect("provider request should be captured");
    let failed_replay = captured
        .messages
        .iter()
        .find_map(|message| {
            message.tool_replay().and_then(|replay| {
                replay.iter().find_map(|part| match part {
                    swage_core::ToolReplayPart::ToolResult {
                        call_id,
                        content,
                        is_error,
                    } if call_id == "call_fail" => Some((content, *is_error)),
                    _ => None,
                })
            })
        })
        .expect("failed tool result should be replayed");

    assert!(failed_replay.1, "failed calls must replay as error results");
    let envelope: swage_core::CompactToolReplay =
        serde_json::from_str(failed_replay.0).expect("failed tool replay should stay compact");
    assert_eq!(envelope.preview, "command not found");
    assert_eq!(envelope.is_error, Some(true));
    assert_eq!(
        envelope.full_result_ref.as_deref(),
        Some("turn:failed:call_fail:result")
    );
}

#[test]
fn compact_tool_replay_messages_includes_nested_compact_rows() {
    let turn_id = Uuid::new_v4();
    let top_level_call_id = "python_parent".to_string();
    let records = vec![
        ToolCallRecord {
            id: Uuid::new_v4(),
            turn_id,
            agent_id: "gateway".to_string(),
            call_id: top_level_call_id.clone(),
            tool: "python_execute".to_string(),
            arguments: r#"{"code":"run()"}"#.to_string(),
            model_arguments: swage_core::compact_tool_arguments_for_model(
                &serde_json::json!({"code": "run()"}),
            ),
            result: Some(r#"{"ok":true}"#.to_string()),
            model_result: Some(swage_core::compact_tool_result_for_model(
                r#"{"ok":true}"#,
                false,
            )),
            status: Some("success".to_string()),
            duration_ms: Some(20),
            truncated: false,
            total_bytes: Some(11),
            provider_round: 2,
            position: 0,
            created_at: chrono::Utc::now(),
            parent_call_id: None,
        },
        ToolCallRecord {
            id: Uuid::new_v4(),
            turn_id,
            agent_id: "gateway".to_string(),
            call_id: "nested_read".to_string(),
            tool: "file_read".to_string(),
            arguments: r#"{"path":"Cargo.toml"}"#.to_string(),
            model_arguments: swage_core::compact_tool_arguments_for_model(
                &serde_json::json!({"path": "Cargo.toml"}),
            ),
            result: Some(r#"{"content":"workspace"}"#.to_string()),
            model_result: Some(swage_core::compact_tool_result_for_model(
                r#"{"content":"workspace"}"#,
                false,
            )),
            status: Some("success".to_string()),
            duration_ms: Some(3),
            truncated: false,
            total_bytes: Some(23),
            provider_round: 0,
            position: 1,
            created_at: chrono::Utc::now(),
            parent_call_id: Some(top_level_call_id),
        },
    ];

    let replay = compact_tool_replay_messages(&records)
        .expect("complete nested replay records should be valid");
    assert_eq!(replay.len(), 2);

    let assistant_replay = replay[0]
        .tool_replay()
        .expect("assistant replay should use canonical tool replay");
    let tool_use_ids: Vec<_> = assistant_replay
        .iter()
        .filter_map(|part| match part {
            swage_core::ToolReplayPart::ToolCall { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_use_ids, vec!["python_parent", "nested_read"]);

    let result_replay = replay[1]
        .tool_replay()
        .expect("tool result replay should use canonical tool replay");
    let result_ids: Vec<_> = result_replay
        .iter()
        .filter_map(|part| match part {
            swage_core::ToolReplayPart::ToolResult { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(result_ids, vec!["python_parent", "nested_read"]);

    let nested_result = result_replay
        .iter()
        .find_map(|part| match part {
            swage_core::ToolReplayPart::ToolResult {
                call_id, content, ..
            } if call_id == "nested_read" => Some(content),
            _ => None,
        })
        .expect("nested tool result should be present");
    let envelope: swage_core::CompactToolReplay = serde_json::from_str(nested_result)
        .expect("nested result should contain compact replay JSON");
    assert_eq!(envelope.preview, r#"{"content":"workspace"}"#);
}

fn complete_replay_record(call_id: &str) -> ToolCallRecord {
    ToolCallRecord {
        id: Uuid::new_v4(),
        turn_id: Uuid::new_v4(),
        agent_id: "gateway".into(),
        call_id: call_id.into(),
        tool: "file_read".into(),
        arguments: r#"{"path":"Cargo.toml"}"#.into(),
        model_arguments: swage_core::compact_tool_arguments_for_model(
            &serde_json::json!({"path": "Cargo.toml"}),
        ),
        result: Some("workspace".into()),
        model_result: Some(swage_core::compact_tool_result_for_model(
            "workspace",
            false,
        )),
        status: Some("success".into()),
        duration_ms: Some(1),
        truncated: false,
        total_bytes: Some(9),
        provider_round: 1,
        position: 0,
        created_at: chrono::Utc::now(),
        parent_call_id: None,
    }
}

#[test]
fn replay_uses_valid_raw_arguments_when_compact_arguments_are_unreadable() {
    let mut record = complete_replay_record("legacy-call");
    record.model_arguments = "not-json".into();

    let arguments = parse_model_arguments(&record)
        .expect("valid raw arguments are a safe legacy replay fallback");

    assert_eq!(arguments["path"], "Cargo.toml");
}

#[test]
fn replay_omits_calls_without_model_safe_results() {
    let mut record = complete_replay_record("incomplete-call");
    record.result = Some("raw result must not be replayed".into());
    record.model_result = None;

    let replay = compact_tool_replay_messages(&[record])
        .expect("an incomplete call should be omitted rather than replaying raw data");

    assert!(replay.is_empty());
}

#[test]
fn corrupt_replay_arguments_are_rejected() {
    let mut record = complete_replay_record("corrupt-call");
    record.model_arguments = "{not-json".into();
    record.arguments = "{also-not-json".into();

    let error = compact_tool_replay_messages(&[record])
        .expect_err("corrupt replay arguments must not be sent to the provider");

    assert!(matches!(error, StorageError::Corruption { .. }));
}

#[test]
fn orphaned_nested_replay_is_rejected() {
    let mut record = complete_replay_record("nested-call");
    record.parent_call_id = Some("missing-parent".into());

    let error = compact_tool_replay_messages(&[record])
        .expect_err("orphaned nested replay must not be regrouped silently");

    assert!(matches!(error, StorageError::Corruption { .. }));
}

#[tokio::test]
async fn approval_resumption_keeps_structured_tool_messages() {
    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "c1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({ "text": "first" }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (state, session_id, captured) =
        make_state_with_capturing_tools(vec![round_1, round_2]).await;
    let mut runtime_config = state.active_config();
    runtime_config.tools.permissions.require_approval =
        vec![swage_config::model::RequireApprovalEntry::Simple(
            "echo".into(),
        )];
    state.replace_runtime(
        runtime_config,
        state.active_provider(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(EchoMockTool) as Arc<dyn crate::tool::Tool>
        ]),
    );

    let subscription = Arc::new(Mutex::new(SubscriptionState::new(true)));
    let (tx, _rx) = mpsc::unbounded_channel();
    let approval_channels = Arc::clone(&state.approval_channels);

    let turn_task = tokio::spawn(async move {
        process_turn(
            &state,
            session_id,
            None,
            "Use echo".to_string(),
            tx,
            subscription,
            None,
        )
        .await
    });

    let approval_sender = loop {
        let maybe_sender = {
            let mut locked = approval_channels.lock().unwrap();
            let approval_id = locked.keys().next().copied();
            approval_id.and_then(|id| locked.remove(&id))
        };
        if let Some(sender) = maybe_sender {
            break sender;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };

    approval_sender
        .send(swage_agent::ApprovalDecision {
            decision: swage_agent::approval::ApprovalDecisionValue::Approved,
            scope: swage_agent::approval::ApprovalDecisionScope::Once,
            reason: None,
        })
        .expect("approval decision should be delivered");

    turn_task
        .await
        .expect("turn task should finish")
        .expect("turn should complete");

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2, "expected two provider requests");

    let second = &requests[1];
    assert_eq!(second.messages.len(), 3);
    assert_eq!(second.messages[1].role(), Role::Assistant);
    assert!(
        second.messages[1].tool_replay().is_some(),
        "assistant tool-use message should retain canonical replay after approval"
    );
    assert_eq!(second.messages[2].role(), Role::User);
    assert!(
        second.messages[2].tool_replay().is_some(),
        "tool-result message should retain canonical replay after approval"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn delegate_tool_executes_child_agent_and_resumes_parent_turn() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let bench = storage
        .get_or_create_default()
        .await
        .expect("default bench");
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;
    let root_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create root thread");

    let parent_round = vec![
        StreamChunk::ToolUse {
            call_id: "delegate-1".into(),
            name: "delegate".into(),
            arguments: serde_json::json!({
                "target_agent": "agent-b",
                "task": "Summarise this request",
            }),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let child_round = vec![
        StreamChunk::Delta {
            content: "Delegate answer".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let parent_resume_round = vec![
        StreamChunk::Delta {
            content: "Parent resumed after delegation".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let (provider, captured_requests) =
        CapturingMultiRoundProvider::new(vec![parent_round, child_round, parent_resume_round]);

    let swage_home = tempfile::TempDir::new().expect("temp swage home");
    let bench_skill_dir = swage_home
        .path()
        .join("benches/default/skills/foreground-only");
    std::fs::create_dir_all(&bench_skill_dir).expect("create foreground skill directory");
    std::fs::write(
        bench_skill_dir.join("SKILL.md"),
        "---\ndescription: Foreground only\n---\nUse the parent foreground skill.\n",
    )
    .expect("write foreground skill");
    let target_agent_dir = swage_home.path().join("benches/default/agents/agent-b");
    std::fs::create_dir_all(target_agent_dir.join("skills/delegated-specialist"))
        .expect("create delegated agent directory");
    std::fs::write(
            target_agent_dir.join("skills/delegated-specialist/SKILL.md"),
            "---\ndescription: Delegated specialist behaviour\n---\nUse the delegated specialist skill.\n",
        )
        .expect("write delegated agent skill");
    let _guard = SwageHomeGuard::set_to(swage_home.path());
    storage
        .activate_skill(session_id, "foreground-only")
        .await
        .expect("activate foreground skill");

    let dispatcher =
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(swage_tool::delegate::DelegateTool) as Arc<dyn crate::tool::Tool>
        ]);

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig {
            communication: swage_config::CommunicationConfig {
                context_transfer: swage_config::ContextTransferConfig {
                    mode: swage_config::ContextTransferMode::Explicit,
                    allowed_fields: vec![swage_config::ContextTransferField::User],
                    summary_budget: None,
                    targets: std::collections::HashMap::new(),
                },
                ..Default::default()
            },
            ..Default::default()
        },
        dispatcher,
        None,
    );

    let (tx, mut rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Delegate this".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    assert!(
        events.iter().any(|event| matches!(
            event,
            ServerEvent::DelegationStarted { target_agent_id, parent_turn_id, .. }
            if target_agent_id == "agent-b" && *parent_turn_id != Uuid::nil()
        )),
        "expected DelegationStarted event, got: {events:?}"
    );

    assert!(
        events.iter().any(|event| matches!(
            event,
            ServerEvent::DelegationCompleted { target_agent_id, success, .. }
            if target_agent_id == "agent-b" && *success
        )),
        "expected DelegationCompleted event, got: {events:?}"
    );

    let root_turns = state
        .storage
        .list_turns_by_thread(root_thread.id)
        .await
        .expect("list root turns");
    assert_eq!(root_turns.len(), 1, "expected exactly one root turn");
    let root_turn = &root_turns[0];
    assert_eq!(root_turn.state, TurnState::Completed);
    assert_eq!(
        root_turn.assistant_response.as_deref(),
        Some("Parent resumed after delegation"),
    );

    let child_sessions = state
        .storage
        .list_sessions_by_agent_id("agent-b")
        .await
        .expect("list child sessions");
    assert_eq!(child_sessions.len(), 1, "expected one child session");

    let child_threads = state
        .storage
        .list_by_session(child_sessions[0].id)
        .await
        .expect("list child threads");
    assert_eq!(child_threads.len(), 1, "expected one child thread");

    let child_turns = state
        .storage
        .list_turns_by_thread(child_threads[0].id)
        .await
        .expect("list child turns");
    assert_eq!(child_turns.len(), 1, "expected one child turn");
    assert_eq!(child_turns[0].state, TurnState::Completed);
    assert_eq!(child_turns[0].parent_turn_id, Some(root_turn.id));
    assert_eq!(
        child_turns[0].assistant_response.as_deref(),
        Some("Delegate answer")
    );

    let captured_requests = captured_requests.lock().unwrap();
    assert_eq!(
        captured_requests.len(),
        3,
        "expected parent, child, and resumed-parent provider requests"
    );
    let parent_request_text = captured_requests[0]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n\n");
    assert!(
        parent_request_text.contains("Use the parent foreground skill."),
        "parent provider request should include the activated foreground skill: {parent_request_text}"
    );
    let child_request_text = captured_requests[1]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n\n");
    assert!(
        child_request_text.contains("Inherited Background Context"),
        "delegated provider request should include transferred context: {child_request_text}"
    );
    assert!(
        child_request_text.contains("Delegate this"),
        "explicit user context transfer should carry the parent request: {child_request_text}"
    );
    assert!(
        child_request_text.contains("Delegated Task\n\nSummarise this request"),
        "delegated provider request should fence the task separately: {child_request_text}"
    );
    assert!(
        child_request_text.contains("Active Skills"),
        "delegated provider request should include target agent active skills: {child_request_text}"
    );
    assert!(
        child_request_text.contains("Use the delegated specialist skill."),
        "delegated provider request should include target agent-level skill instructions: {child_request_text}"
    );
    assert!(
        !child_request_text.contains("Use the parent foreground skill."),
        "delegated provider request must not inherit user-activated parent skills: {child_request_text}"
    );
    assert!(
        !child_request_text.contains("Parent resumed after delegation"),
        "child provider request must not inherit future parent continuation"
    );
    let resumed_parent_request_text = captured_requests[2]
        .messages
        .iter()
        .map(ChatMessage::text_content)
        .collect::<Vec<_>>()
        .join("\n\n");
    assert!(
        !resumed_parent_request_text.contains("Use the delegated specialist skill."),
        "target agent skill instructions must not bleed into the resumed parent request"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn delegate_tool_detaches_child_and_persists_result_after_release() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let bench = storage
        .get_or_create_default()
        .await
        .expect("default bench");
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;
    let root_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create root thread");

    let child_started = Arc::new(Notify::new());
    let child_release = Arc::new(Notify::new());
    let (provider, captured_requests) =
        DetachedGatedProvider::new(child_started.clone(), child_release.clone());

    let dispatcher =
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(swage_tool::delegate::DelegateTool) as Arc<dyn crate::tool::Tool>
        ]);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        dispatcher,
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Delegate independently".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("detached parent turn should complete before child release");

    tokio::time::timeout(std::time::Duration::from_secs(1), child_started.notified())
        .await
        .expect("detached child provider stream should have started");

    let root_turns = state
        .storage
        .list_turns_by_thread(root_thread.id)
        .await
        .expect("list root turns");
    assert_eq!(root_turns.len(), 1, "expected one root turn");
    let root_turn = &root_turns[0];
    assert_eq!(root_turn.state, TurnState::Completed);
    assert_eq!(
        root_turn.assistant_response.as_deref(),
        Some("Parent continued without waiting"),
    );

    let tool_calls = state
        .storage
        .list_tool_calls_by_turn(root_turn.id)
        .await
        .expect("list tool calls");
    let delegate_tool_call = tool_calls
        .iter()
        .find(|call| call.tool == "delegate")
        .expect("delegate tool call should be stored");
    let detached_output: serde_json::Value = serde_json::from_str(
        delegate_tool_call
            .result
            .as_deref()
            .expect("delegate tool result should be stored"),
    )
    .expect("delegate tool result should be JSON");
    assert_eq!(detached_output["status"], "detached");
    assert_eq!(
        detached_output["result_delivery"],
        "The delegated work continues independently. Its durable result message will be stored with correlation_id equal to delegate_message_id when the child finishes."
    );

    let delegate_message_id = detached_output["delegate_message_id"]
        .as_str()
        .expect("delegate message id should be a string")
        .parse::<Uuid>()
        .expect("delegate message id should be a uuid");
    let delegate_message = state
        .storage
        .get_agent_message(delegate_message_id)
        .await
        .expect("get delegate message")
        .expect("delegate message should exist");
    assert_eq!(delegate_message.status, MessageStatus::Delivered);
    assert_eq!(
        state
            .storage
            .list_by_correlation(delegate_message_id)
            .await
            .expect("list correlated messages before release")
            .len(),
        0,
        "child result should not exist until the gated provider is released"
    );

    child_release.notify_one();

    let mut result_messages = Vec::new();
    for _ in 0..20 {
        result_messages = state
            .storage
            .list_by_correlation(delegate_message_id)
            .await
            .expect("list correlated messages after release");
        if !result_messages.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert_eq!(result_messages.len(), 1, "expected one result message");
    let result_message = &result_messages[0];
    assert_eq!(result_message.status, MessageStatus::Delivered);
    assert_eq!(result_message.payload["response"], "Detached child answer");

    let child_session_id = detached_output["child_session_id"]
        .as_str()
        .expect("child session id should be a string")
        .parse::<Uuid>()
        .expect("child session id should be a uuid");
    let child_threads = state
        .storage
        .list_by_session(child_session_id)
        .await
        .expect("list child threads");
    assert_eq!(child_threads.len(), 1, "expected one child thread");
    let child_turns = state
        .storage
        .list_turns_by_thread(child_threads[0].id)
        .await
        .expect("list child turns");
    assert_eq!(child_turns.len(), 1, "expected one child turn");
    assert_eq!(child_turns[0].state, TurnState::Completed);
    assert_eq!(child_turns[0].parent_turn_id, Some(root_turn.id));

    let captured = captured_requests.lock().unwrap();
    assert!(
        captured.len() >= 3,
        "expected initial parent, detached child, and resumed parent requests: {captured:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn detached_delegate_can_be_cancelled_through_active_registry() {
    let storage = Arc::new(swage_storage::SqliteStorage::in_memory().expect("in-memory storage"));
    let bench = storage
        .get_or_create_default()
        .await
        .expect("default bench");
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let root_thread = ThreadStore::create(
        &*storage,
        NewThread {
            session_id: session.id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("create root thread");
    let parent_turn = storage
        .create_turn(NewTurn {
            thread_id: root_thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Processing,
            user_input: Some("start detached work".to_string()),
            parent_turn_id: None,
        })
        .await
        .expect("create parent turn");

    let child_started = Arc::new(Notify::new());
    let child_release = Arc::new(Notify::new());
    let (provider, _captured_requests) =
        DetachedGatedProvider::new(child_started.clone(), child_release);
    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let handler = GatewayDelegationHandler::new(state.clone());

    let outcome = handler
        .handle_delegation(
            "agent-b",
            "Summarise independently",
            None,
            "gateway",
            parent_turn.id,
            bench.id,
            CancellationToken::new(),
            DelegateCompletionMode::Detach,
        )
        .await
        .expect("detached delegation should start");
    let detached = match outcome {
        DelegationOutcome::Detached(detached) => detached,
        DelegationOutcome::Completed(_) => panic!("expected detached outcome"),
    };

    tokio::time::timeout(std::time::Duration::from_secs(1), child_started.notified())
        .await
        .expect("detached child should enter provider stream");

    assert!(
        handler
            .active_delegations
            .cancellation_target(detached.delegate_message_id)
            .await
            .is_some(),
        "detached child should be registered for precise cancellation"
    );

    let cancel = storage
        .create_agent_message(NewAgentMessage {
            sender_agent_id: "gateway".to_string(),
            recipient_agent_id: "agent-b".to_string(),
            kind: swage_core::MessageKind::Cancel,
            correlation_id: Some(detached.delegate_message_id),
            payload: serde_json::json!({}),
        })
        .await
        .expect("create cancel message");
    swage_agent::cancel::handle_cancel(
        storage.as_ref(),
        &cancel,
        Some(&handler.active_delegations),
    )
    .await
    .expect("active-registry cancel should succeed");

    let mut child_turns = Vec::new();
    for _ in 0..20 {
        child_turns = state
            .storage
            .list_turns_by_thread(detached.child_thread_id)
            .await
            .expect("list child turns");
        if child_turns
            .iter()
            .any(|turn| turn.state == TurnState::Interrupted)
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert_eq!(child_turns.len(), 1, "expected one child turn");
    assert_eq!(child_turns[0].state, TurnState::Interrupted);

    let delegate = state
        .storage
        .get_agent_message(detached.delegate_message_id)
        .await
        .expect("get delegate message")
        .expect("delegate message should exist");
    assert_eq!(delegate.status, MessageStatus::Delivered);

    let mut result_messages = Vec::new();
    for _ in 0..20 {
        result_messages = state
            .storage
            .list_by_correlation(detached.delegate_message_id)
            .await
            .expect("list correlated messages after cancellation");
        if result_messages
            .iter()
            .any(|message| message.payload["error"] == "turn cancelled")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let cancellation_results = result_messages
        .iter()
        .filter(|message| message.kind == swage_core::MessageKind::Result)
        .collect::<Vec<_>>();
    assert_eq!(
        cancellation_results.len(),
        1,
        "expected one cancellation result"
    );
    assert_eq!(cancellation_results[0].payload["error"], "turn cancelled");

    for _ in 0..20 {
        if handler
            .active_delegations
            .cancellation_target(detached.delegate_message_id)
            .await
            .is_none()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("detached delegation registry entry should be removed after cancellation");
}

// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// Bench context and config resolution tests
// -----------------------------------------------------------------------

struct CapturingProvider {
    captured: Arc<std::sync::Mutex<Option<CompletionRequest>>>,
    chunks: Vec<StreamChunk>,
}

impl CapturingProvider {
    fn new(chunks: Vec<StreamChunk>) -> (Self, Arc<std::sync::Mutex<Option<CompletionRequest>>>) {
        let captured = Arc::new(std::sync::Mutex::new(None));
        (
            Self {
                captured: Arc::clone(&captured),
                chunks,
            },
            captured,
        )
    }
}

#[async_trait]
impl LlmProvider for CapturingProvider {
    fn model_id(&self) -> &str {
        "capturing-model"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        unimplemented!()
    }

    async fn complete_with_tools(
        &self,
        _request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unimplemented!()
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        *self.captured.lock().unwrap() = Some(request);
        Box::pin(stream::iter(self.chunks.clone()))
    }
}

struct ContextCaptureTool {
    captured: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
}

#[async_trait::async_trait]
impl crate::tool::Tool for ContextCaptureTool {
    fn name(&self) -> &str {
        "capture_context"
    }
    fn description(&self) -> &str {
        "Captures the working_directory from the tool context"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn execute(
        &self,
        _: serde_json::Value,
        ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        *self.captured.lock().unwrap() = Some(ctx.working_directory().to_path_buf());
        Ok(serde_json::json!({"output": "captured"}))
    }
}

/// Create a storage with a session + thread and return (storage, session_id).
async fn make_storage_with_session() -> (Arc<MockStorage>, Uuid) {
    let storage = Arc::new(MockStorage::new());
    let bench = storage.get_or_create_default().await.expect("bench");
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("session");
    let session_id = session.id;
    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("thread");
    (storage, session_id)
}

#[tokio::test]
async fn process_turn_passes_tool_context_to_tools() {
    let captured = Arc::new(std::sync::Mutex::new(None));
    let capture_tool = Arc::new(ContextCaptureTool {
        captured: Arc::clone(&captured),
    });

    let (storage, session_id) = make_storage_with_session().await;

    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "cc_1".into(),
            name: "capture_context".into(),
            arguments: serde_json::json!({}),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let provider = Arc::new(MultiRoundMockProvider::new(vec![round_1, round_2]));

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        provider as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![capture_tool as Arc<dyn crate::tool::Tool>]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "capture".into(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    let working_dir = captured
        .lock()
        .unwrap()
        .take()
        .expect("ToolContext should have been passed during tool execution");

    assert!(
        working_dir.is_absolute(),
        "working_directory should be absolute: {working_dir:?}"
    );
}

#[tokio::test]
async fn process_turn_resolves_config_per_bench_and_completes_turn() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);

    let storage = Arc::new(MockStorage::new());
    let bench = BenchStore::create(
        &*storage,
        swage_storage::model::NewBench {
            name: "Test Config Bench".to_string(),
            slug: "test-config-resolution-no-bench-toml".into(),
        },
    )
    .await
    .expect("create test bench");
    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("session");
    let session_id = session.id;
    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("thread");

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "hi".into(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    // Verify the provider was invoked. No configured model exists in this
    // test, so the request should still leave provider defaults in charge.
    let req = captured_request.lock().unwrap().take();
    assert!(
        req.is_some(),
        "provider should have been invoked for config-resolved turn"
    );
    assert_eq!(req.and_then(|request| request.model), None);
}

#[tokio::test]
async fn process_turn_uses_hot_swapped_runtime_config_for_next_turn() {
    let _guard = SwageHomeGuard::isolate();
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);
    let (storage, session_id) = make_storage_with_session().await;

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );
    let runtime_config = swage_config::ResolvedConfig {
        models: swage_config::ModelCatalogConfig {
            default: "fast".to_string(),
            entries: [(
                "fast".to_string(),
                swage_config::ModelEndpointConfig {
                    provider_id: "local".to_string(),
                    model_id: "fast-wire".to_string(),
                    aliases: vec![],
                    context_window: None,
                    generation_preset: Some("quick".to_string()),
                    capabilities: swage_config::ModelCapabilitiesConfig::default(),
                },
            )]
            .into_iter()
            .collect(),
        },
        generation: swage_config::GenerationConfig {
            presets: [(
                "quick".to_string(),
                swage_config::GenerationPresetConfig {
                    max_tokens: Some(123),
                    temperature: Some(0.2),
                    stop_sequences: vec!["STOP".to_string()],
                    metadata: Some(serde_json::json!({"preset": "quick"})),
                },
            )]
            .into_iter()
            .collect(),
        },
        context: swage_config::ContextConfig {
            assembly: "default".to_string(),
            timeout: 7,
            budget: Some(2048),
        },
        ..Default::default()
    };
    state.replace_runtime(
        runtime_config,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        crate::tool::ToolDispatcher::allow_all(vec![]),
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "hi".into(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    let request = captured_request
        .lock()
        .unwrap()
        .take()
        .expect("provider should receive the new-turn request");
    assert_eq!(request.model.as_deref(), Some("fast-wire"));
    assert_eq!(request.max_tokens, Some(123));
    assert_eq!(request.temperature, Some(0.2));
    assert_eq!(request.stop_sequences, Some(vec!["STOP".to_string()]));
    assert_eq!(
        request.metadata,
        Some(serde_json::json!({"preset": "quick"}))
    );
}

#[tokio::test]
async fn process_turn_uses_hot_swapped_runtime_tool_policy_for_next_turn() {
    let _guard = SwageHomeGuard::isolate();
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);
    let (storage, session_id) = make_storage_with_session().await;

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(MockProvider::new(vec![])) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![]),
        None,
    );

    let mut runtime_config = swage_config::ResolvedConfig::default();
    runtime_config.tools.permissions.deny = vec!["file_read".to_string()];
    state.replace_runtime(
        runtime_config,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(WorkspaceGatedReadTool) as Arc<dyn crate::tool::Tool>
        ]),
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "hello".into(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    let request = captured_request
        .lock()
        .unwrap()
        .take()
        .expect("provider should receive the new-turn request");
    assert!(
        request.tool_definitions.is_empty(),
        "hot-applied deny policy should hide file_read from the next turn"
    );
}

#[tokio::test]
async fn process_turn_stream_request_includes_registered_tool_definitions() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);
    let (storage, session_id) = make_storage_with_session().await;

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(WorkingDirReadTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "hello".into(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    let req = captured_request
        .lock()
        .unwrap()
        .take()
        .expect("provider request should be captured");
    assert_eq!(req.tool_definitions.len(), 1);
    assert_eq!(req.tool_definitions[0]["name"], "working_dir_read");
}

#[test]
fn effective_session_config_uses_known_selected_model_as_default() {
    let config = swage_config::ResolvedConfig {
        models: swage_config::ModelCatalogConfig {
            default: "global".to_string(),
            entries: [
                (
                    "global".to_string(),
                    swage_config::ModelEndpointConfig {
                        provider_id: "openai".to_string(),
                        model_id: "global-wire".to_string(),
                        aliases: vec![],
                        context_window: None,
                        generation_preset: None,
                        capabilities: swage_config::ModelCapabilitiesConfig::default(),
                    },
                ),
                (
                    "conversation".to_string(),
                    swage_config::ModelEndpointConfig {
                        provider_id: "openai".to_string(),
                        model_id: "conversation-wire".to_string(),
                        aliases: vec![],
                        context_window: None,
                        generation_preset: None,
                        capabilities: swage_config::ModelCapabilitiesConfig::default(),
                    },
                ),
            ]
            .into_iter()
            .collect(),
        },
        ..Default::default()
    };

    let effective = effective_session_config(config, Some("conversation"));

    assert_eq!(effective.models.default, "conversation");
    assert_eq!(effective.default_model_id(), "conversation-wire");
}

#[tokio::test]
async fn process_turn_stream_request_inherits_global_permissions_without_workspace_grants() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (provider, captured_request) = CapturingProvider::new(chunks);
    let (storage, session_id) = make_storage_with_session().await;

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        Arc::new(provider) as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(WorkspaceGatedReadTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "hello".into(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn ok");

    let req = captured_request
        .lock()
        .unwrap()
        .take()
        .expect("provider request should be captured");
    assert_eq!(req.tool_definitions.len(), 1);
    assert_eq!(req.tool_definitions[0]["name"], "file_read");
}

#[tokio::test]
async fn text_only_turn_still_works_with_tool_registry() {
    let (state, session_id) = make_state_with_tools(vec![vec![
        StreamChunk::Delta {
            content: "Just text".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ]])
    .await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::Token { content } if content == "Just text"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ServerEvent::TurnComplete { .. }))
    );

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ServerEvent::ToolStart { .. }))
    );
}

// -----------------------------------------------------------------------
// ToolContext propagation tests
// -----------------------------------------------------------------------

struct WorkingDirReadTool;

#[async_trait::async_trait]
impl crate::tool::Tool for WorkingDirReadTool {
    fn name(&self) -> &str {
        "working_dir_read"
    }
    fn description(&self) -> &str {
        "Returns the working_directory from the ToolContext"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn execute(
        &self,
        _arguments: serde_json::Value,
        ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        let path = ctx.working_directory().to_string_lossy().into_owned();
        Ok(serde_json::json!({"output": path}))
    }
}

struct WorkspaceGatedReadTool;

#[async_trait::async_trait]
impl crate::tool::Tool for WorkspaceGatedReadTool {
    fn name(&self) -> &str {
        "file_read"
    }
    fn description(&self) -> &str {
        "Represents a workspace-read tool in agent-facing schema tests"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _ctx: &dyn swage_tool::context::ToolContext,
    ) -> Result<serde_json::Value, swage_core::ToolError> {
        Ok(serde_json::json!({}))
    }
}

fn registered_remote_host(
    capabilities: Vec<String>,
    workspace_roots: Vec<std::path::PathBuf>,
) -> crate::execution_host::RegisteredExecutionHost {
    crate::execution_host::RegisteredExecutionHost {
        host_id: "node-a".into(),
        label: None,
        kind: crate::execution_host::ExecutionHostKind::AttachedNode,
        base_url: Some("http://127.0.0.1:32001".into()),
        shared_token_secret_ref: Some("secret://execution-host/node-a".into()),
        workspace_root: workspace_roots
            .first()
            .cloned()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp")),
        workspace_roots,
        capabilities,
        registered_at: chrono::Utc::now(),
    }
}

#[test]
fn gateway_advertisement_filter_hides_tools_absent_from_remote_host_capabilities() {
    let workspace = tempfile::TempDir::new().expect("workspace");
    let agent_config = AgentConfig::new("agent")
        .with_workspace(workspace.path(), vec![workspace.path().to_path_buf()]);
    let host = registered_remote_host(
        vec!["other_tool".into()],
        vec![workspace.path().to_path_buf()],
    );
    let filter = build_gateway_advertisement_filter(Some(&host), &agent_config)
        .expect("remote host should produce an advertisement filter");

    assert!(
        !filter(&WorkingDirReadTool),
        "tools the selected remote host cannot execute must not be advertised"
    );
}

#[test]
fn gateway_advertisement_filter_keeps_remote_capability_with_matching_workspace() {
    let workspace = tempfile::TempDir::new().expect("workspace");
    let agent_config = AgentConfig::new("agent")
        .with_workspace(workspace.path(), vec![workspace.path().to_path_buf()]);
    let host = registered_remote_host(
        vec!["working_dir_read".into()],
        vec![workspace.path().to_path_buf()],
    );
    let filter = build_gateway_advertisement_filter(Some(&host), &agent_config)
        .expect("remote host should produce an advertisement filter");

    assert!(
        filter(&WorkingDirReadTool),
        "remote-capable tools inside the host workspace should remain advertised"
    );
}

#[tokio::test]
async fn tool_context_working_directory_is_passed_during_tool_execution() {
    let storage = Arc::new(MockStorage::new());
    let bench = BenchStore::create(
        &*storage,
        NewBench {
            name: "my-bench".to_string(),
            slug: "my-bench".into(),
        },
    )
    .await
    .expect("create bench");

    let session = storage
        .create_session(NewSession {
            bench_id: bench.id,
            agent_id: "gateway".to_string(),
            metadata: None,
            active_workspace_attachment_id: None,
        })
        .await
        .expect("create session");
    let session_id = session.id;

    ThreadStore::create(
        &*storage,
        NewThread {
            session_id,
            agent_id: "gateway".to_string(),
        },
    )
    .await
    .expect("thread");

    let round_1 = vec![
        StreamChunk::ToolUse {
            call_id: "ctx-check".into(),
            name: "working_dir_read".into(),
            arguments: serde_json::json!({}),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];

    let provider = Arc::new(MultiRoundMockProvider::new(vec![round_1, round_2]));

    let state = AppState::new(
        Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
        Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
        provider as Arc<dyn LlmProvider>,
        swage_config::ResolvedConfig::default(),
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(WorkingDirReadTool) as Arc<dyn crate::tool::Tool>
        ]),
        None,
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Check context".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    let tool_calls = state
        .storage
        .list_tool_calls_by_turn(turns[0].id)
        .await
        .unwrap();
    assert_eq!(tool_calls.len(), 1);
    // The agent stores the full JSON-serialised tool output.
    let result = tool_calls[0].result.as_deref().unwrap_or("");
    let result_json: serde_json::Value =
        serde_json::from_str(result).expect("tool result should be valid JSON");
    let path_str = result_json["output"]
        .as_str()
        .expect("output field should be a string");
    assert!(
        std::path::Path::new(path_str).is_absolute(),
        "working_directory should be an absolute path, got: {path_str}"
    );
}

// -----------------------------------------------------------------------
// Turn ID correlation span test
// -----------------------------------------------------------------------

/// The `process_turn` function creates a tracing span named "turn" carrying
/// `turn_id` and `session_id` fields so that all log entries within the turn
/// are correlated by the same identifiers.
///
/// Uses a custom `Layer` that records span metadata to an in-process buffer,
/// avoiding external log capture and filesystem dependencies.
#[test]
fn process_turn_creates_correlation_span() {
    use std::sync::{Arc as StdArc, Mutex as StdMutex};
    use tracing_subscriber::{Registry, layer::SubscriberExt};

    // Accumulates (span_name, [field_names]) for every span created
    // during process_turn. Shared between the subscriber and the assertions.
    type RecordedFields = Vec<(String, Vec<String>)>;

    let recorded: StdArc<StdMutex<RecordedFields>> = StdArc::new(StdMutex::new(Vec::new()));
    let captured = StdArc::clone(&recorded);

    // Minimal Layer that records span names and field names on creation.
    struct SpanFieldRecorder(StdArc<StdMutex<RecordedFields>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SpanFieldRecorder {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let name = attrs.metadata().name().to_string();
            let mut fields = Vec::new();

            // Collect field names from metadata — this captures both
            // value-filled and Empty (deferred) fields, so turn_id is
            // visible even though it's recorded after span creation.
            attrs.metadata().fields().iter().for_each(|f| {
                fields.push(f.name().to_string());
            });
            self.0.lock().unwrap().push((name, fields));
        }
    }

    let subscriber = Registry::default().with(SpanFieldRecorder(captured));

    // Scope the subscriber around the entire async execution via with_default.
    // This is more reliable than set_default inside #[tokio::test] because
    // with_default brackets the runtime creation itself, ensuring the subscriber
    // is active when tracing rebuilds callsite interest caches (which happens
    // when SCOPED_COUNT transitions 0→1). A gap between set_default and
    // process_turn can let concurrent tests drain SCOPED_COUNT to 0, causing
    // tracing to rebuild with a noop subscriber and cache "never" interest —
    // so spans are silently dropped before reaching our recorder.
    tracing::subscriber::with_default(subscriber, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let chunks = vec![
                    StreamChunk::Delta {
                        content: "ok".to_string(),
                    },
                    StreamChunk::Done {
                        finish_reason: None,
                    },
                ];
                let (state, session_id) = make_state_with_session(chunks).await;
                let (tx, _rx) = mpsc::unbounded_channel();

                // Re-seed callsite interest immediately before process_turn.
                // In a parallel test run, concurrent tests can trigger
                // rebuild_interest_cache() on their own threads (when their
                // scoped subscriber guards drop), resetting our callsite's
                // interest to "never". Calling it here — with our subscriber
                // still active — re-establishes "sometimes" interest just
                // before the span would be created.
                tracing::callsite::rebuild_interest_cache();

                process_turn(
                    &state,
                    session_id,
                    None,
                    "test".to_string(),
                    tx,
                    Arc::new(Mutex::new(SubscriptionState::new(true))),
                    None,
                )
                .await
                .expect("turn should complete");
            });
    });

    let spans = recorded.lock().unwrap();
    let turn_spans: Vec<_> = spans.iter().filter(|(name, _)| name == "turn").collect();

    assert!(
        !turn_spans.is_empty(),
        "expected a 'turn' span during process_turn; saw spans: {:?}",
        spans.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
    );

    let (_, fields) = &turn_spans[0];
    assert!(
        fields.contains(&"turn_id".to_string()),
        "turn span should have 'turn_id' field, got: {fields:?}",
    );
    assert!(
        fields.contains(&"session_id".to_string()),
        "turn span should have 'session_id' field, got: {fields:?}",
    );
}

// -----------------------------------------------------------------------
// Token usage structured log tests
// -----------------------------------------------------------------------

/// Verifies that process_turn emits an INFO-level tracing event at turn
/// completion that includes input_tokens, output_tokens, and
/// cost_microcents as structured fields. This event extends the turn span
/// with cost-visibility for operators reading JSON logs.
#[test]
fn process_turn_logs_token_usage_at_completion() {
    use std::sync::{Arc as StdArc, Mutex as StdMutex};
    use swage_core::TokenUsage;
    use tracing_subscriber::{Registry, layer::SubscriberExt};

    // Accumulates (level_str, [field_names]) for every event emitted
    // during process_turn. Shared between the layer and the assertions.
    type RecordedFields = Vec<(String, Vec<String>)>;

    let recorded: StdArc<StdMutex<RecordedFields>> = StdArc::new(StdMutex::new(Vec::new()));
    let captured = StdArc::clone(&recorded);

    struct EventFieldRecorder(StdArc<StdMutex<RecordedFields>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for EventFieldRecorder {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let level = event.metadata().level().to_string();
            let fields: Vec<String> = event
                .metadata()
                .fields()
                .iter()
                .map(|f| f.name().to_string())
                .collect();
            self.0.lock().unwrap().push((level, fields));
        }
    }

    let subscriber = Registry::default().with(EventFieldRecorder(captured));

    tracing::subscriber::with_default(subscriber, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let chunks = vec![
                    StreamChunk::Delta {
                        content: "ok".to_string(),
                    },
                    StreamChunk::Usage(TokenUsage {
                        input_tokens: 100,
                        output_tokens: 50,
                    }),
                    StreamChunk::Done {
                        finish_reason: None,
                    },
                ];
                let (state, session_id) = make_state_with_session(chunks).await;
                let (tx, _rx) = mpsc::unbounded_channel();

                tracing::callsite::rebuild_interest_cache();

                process_turn(
                    &state,
                    session_id,
                    None,
                    "test".to_string(),
                    tx,
                    Arc::new(Mutex::new(SubscriptionState::new(true))),
                    None,
                )
                .await
                .expect("turn should complete");
            });
    });

    let events = recorded.lock().unwrap();

    // Find an INFO event that carries token usage fields.
    let usage_events: Vec<_> = events
        .iter()
        .filter(|(level, fields)| level == "INFO" && fields.contains(&"input_tokens".to_string()))
        .collect();

    assert!(
        !usage_events.is_empty(),
        "expected an INFO event with input_tokens field; got events: {events:?}",
    );

    let (_, fields) = &usage_events[0];
    assert!(
        fields.contains(&"output_tokens".to_string()),
        "usage event should have output_tokens field, got: {fields:?}",
    );
    assert!(
        fields.contains(&"cost_microcents".to_string()),
        "usage event should have cost_microcents field, got: {fields:?}",
    );
}

// -----------------------------------------------------------------------
// Context assembly tests
// -----------------------------------------------------------------------

// Imports from super::* via `use super::*` — ContextElement, HistoryElement,
// PromptsElement, TiktokenTokeniser, PromptRule are all available.

/// Assembly from a PromptRule and HistoryElement produces a system message
/// first, followed by history, with a report showing non-zero token counts.
#[test]
fn assembly_with_instructions_produces_system_message_first() {
    let rule = PromptRule {
        priority: 0,
        content: "You are a helpful coding assistant.".to_string(),
        source_level: swage_config::HierarchyLevel::Global,
        file_path: std::path::PathBuf::from("test.md"),
    };
    let prompts = PromptsElement::new(vec![rule]);
    let history = HistoryElement::new(vec![ChatMessage::new(Role::User, "Hello")]);

    let elements: Vec<Box<dyn ContextElement>> = vec![Box::new(prompts), Box::new(history)];

    let config = swage_config::ContextConfig::default();
    let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");

    let output = swage_context::assembly::default_assembly(&elements, &config, &tokeniser);

    // System message from instructions appears first.
    assert_eq!(
        output.messages[0].role(),
        Role::System,
        "first message should be system instructions"
    );
    // User message from history follows.
    assert_eq!(
        output.messages[1].role(),
        Role::User,
        "second message should be user history"
    );

    // Report should have non-zero token counts for both elements.
    let prompts_report = output
        .report
        .elements
        .iter()
        .find(|e| e.name == "prompts")
        .expect("report should include prompts element");
    assert!(
        prompts_report.token_count > 0,
        "prompts element should have non-zero token count"
    );

    let history_report = output
        .report
        .elements
        .iter()
        .find(|e| e.name == "history")
        .expect("report should include history element");
    assert!(
        history_report.token_count > 0,
        "history element should have non-zero token count"
    );
}

/// Assembly with empty instructions produces no system message — only
/// history messages.
#[test]
fn assembly_without_instructions_omits_system_message() {
    let prompts = PromptsElement::new(vec![]);
    let history = HistoryElement::new(vec![ChatMessage::new(Role::User, "Hello")]);

    let elements: Vec<Box<dyn ContextElement>> = vec![Box::new(prompts), Box::new(history)];

    let config = swage_config::ContextConfig::default();
    let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");

    let output = swage_context::assembly::default_assembly(&elements, &config, &tokeniser);

    // No system message when instructions are empty.
    assert!(
        output.messages.iter().all(|m| m.role() != Role::System),
        "expected no system message when prompts are empty"
    );
    assert_eq!(
        output.messages.len(),
        1,
        "should only contain the user message"
    );
}

/// Constructing a PromptRule from the config.instructions string produces
/// a PromptsElement that generates equivalent output to the old flat prepend.
#[test]
fn prompt_rule_from_instructions_string_matches_flat_prepend() {
    let instructions = "You are a helpful assistant.\n\nBe concise.";

    // Old approach: flat system message prepend.
    let flat_system = ChatMessage::new(Role::System, instructions);

    // New approach: PromptRule → PromptsElement → assembly.
    let rule = PromptRule {
        priority: 0,
        content: instructions.to_string(),
        source_level: swage_config::HierarchyLevel::Global,
        file_path: std::path::PathBuf::from("instructions"),
    };
    let prompts = PromptsElement::new(vec![rule]);
    let content = prompts.content();

    assert_eq!(
        content.len(),
        1,
        "should produce exactly one system message"
    );
    assert_eq!(content[0].role(), flat_system.role());
    // Content should be identical — single rule, no joining needed.
    assert_eq!(content[0].content(), flat_system.content());
}

// -----------------------------------------------------------------------
// Workspace permission gate
// -----------------------------------------------------------------------

/// Convenience for invoking the private build_permission_config from tests
/// without having to repeat the full ResolvedConfig wiring.
fn build_permission_config_for_test(config: &swage_config::ResolvedConfig) -> PermissionConfig {
    super::build_permission_config(config, config, None, &std::collections::HashSet::new())
}

#[test]
fn bench_scoped_approval_grant_only_removes_gate_for_matching_bench() {
    let matching_bench_id = Uuid::new_v4();
    let other_bench_id = Uuid::new_v4();
    let mut config = swage_config::ResolvedConfig::default();
    config.tools.permissions.require_approval =
        vec![swage_config::model::RequireApprovalEntry::Simple(
            "file_write".into(),
        )];

    let grants = std::collections::HashSet::from([crate::state::ApprovalGrantKey {
        bench_id: Some(matching_bench_id),
        kind: swage_storage::model::ApprovalKind::Tool,
        name: "file_write".into(),
        access: None,
    }]);

    let matching =
        super::build_permission_config(&config, &config, Some(matching_bench_id), &grants);
    let other = super::build_permission_config(&config, &config, Some(other_bench_id), &grants);
    let no_bench = super::build_permission_config(&config, &config, None, &grants);

    assert!(matching.require_approval.is_empty());
    assert_eq!(other.require_approval.len(), 1);
    assert_eq!(no_bench.require_approval.len(), 1);
}

#[tokio::test]
async fn workspace_summary_is_empty_when_no_attachments_exist() {
    let storage: Arc<dyn swage_storage::store::Storage> = Arc::new(MockStorage::new());
    let bench_id = Uuid::new_v4();

    let summary = summarise_trusted_workspace_permissions(&storage, bench_id).await;

    assert!(!summary.has_read);
    assert!(!summary.has_write);
}

#[tokio::test]
async fn workspace_summary_ignores_untrusted_attachments() {
    let storage: Arc<dyn swage_storage::store::Storage> = Arc::new(MockStorage::new());
    let bench_id = Uuid::new_v4();
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            bench_id,
            label: "untrusted".into(),
            path: "/tmp/untrusted".into(),
            permissions: vec!["read".into(), "write".into()],
            is_trusted: false,
        })
        .await
        .unwrap();

    let summary = summarise_trusted_workspace_permissions(&storage, bench_id).await;

    assert!(
        !summary.has_read,
        "untrusted attachment must not grant read"
    );
    assert!(
        !summary.has_write,
        "untrusted attachment must not grant write"
    );
}

#[tokio::test]
async fn workspace_summary_combines_grants_across_trusted_attachments() {
    let storage: Arc<dyn swage_storage::store::Storage> = Arc::new(MockStorage::new());
    let bench_id = Uuid::new_v4();
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            bench_id,
            label: "read-only".into(),
            path: "/tmp/read".into(),
            permissions: vec!["read".into()],
            is_trusted: true,
        })
        .await
        .unwrap();
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            bench_id,
            label: "write-only".into(),
            path: "/tmp/write".into(),
            permissions: vec!["write".into()],
            is_trusted: true,
        })
        .await
        .unwrap();

    let summary = summarise_trusted_workspace_permissions(&storage, bench_id).await;

    assert!(summary.has_read);
    assert!(summary.has_write);
}

#[tokio::test]
async fn workspace_summary_only_aggregates_target_bench() {
    let storage: Arc<dyn swage_storage::store::Storage> = Arc::new(MockStorage::new());
    let target_bench = Uuid::new_v4();
    let other_bench = Uuid::new_v4();
    storage
        .create_workspace_attachment(NewWorkspaceAttachment {
            bench_id: other_bench,
            label: "other".into(),
            path: "/tmp/other".into(),
            permissions: vec!["read".into(), "write".into()],
            is_trusted: true,
        })
        .await
        .unwrap();

    let summary = summarise_trusted_workspace_permissions(&storage, target_bench).await;

    assert!(!summary.has_read);
    assert!(!summary.has_write);
}

#[tokio::test]
async fn delegation_rejects_missing_sender_turn_lineage() {
    let (state, _) = make_state_with_session(vec![]).await;
    let handler = GatewayDelegationHandler::new(state);
    let missing_turn_id = Uuid::new_v4();

    let error = handler
        .active_workspace_attachment_for_turn(missing_turn_id)
        .await
        .expect_err("a missing sender turn must not look like an unattached workspace");

    assert!(matches!(
        error,
        AgentError::StorageFailed(StorageError::NotFound { entity, id })
            if entity == "turn" && id == missing_turn_id.to_string()
    ));
}

#[test]
fn sandbox_config_skips_missing_trusted_workspace_paths() {
    let temp_dir = tempfile::tempdir().unwrap();
    let working_directory = temp_dir.path().join("work");
    let existing_workspace = temp_dir.path().join("existing");
    let missing_workspace = temp_dir.path().join("missing");
    std::fs::create_dir_all(&working_directory).unwrap();
    std::fs::create_dir_all(&existing_workspace).unwrap();

    let agent_config = AgentConfig::new("agent")
        .with_workspace(&working_directory, vec![working_directory.clone()]);
    let policy = TrustedWorkspacePolicy {
        permissions: WorkspacePermissionSummary {
            has_read: true,
            has_write: true,
        },
        active_workspace: Some(TrustedWorkspacePath {
            path: working_directory,
            can_read: true,
            can_write: true,
        }),
        attachments: vec![
            TrustedWorkspacePath {
                path: existing_workspace.clone(),
                can_read: true,
                can_write: false,
            },
            TrustedWorkspacePath {
                path: missing_workspace.clone(),
                can_read: true,
                can_write: true,
            },
        ],
    };

    let sandbox_config = build_sandbox_config(&agent_config, Some(&policy)).unwrap();
    let existing_workspace = sandbox_path(&existing_workspace);

    assert!(
        sandbox_config
            .read_only_mounts
            .contains(&existing_workspace)
    );
    assert!(
        sandbox_config
            .landlock_read_paths
            .contains(&existing_workspace)
    );
    assert!(
        !sandbox_config
            .read_write_mounts
            .contains(&missing_workspace)
    );
    assert!(
        !sandbox_config
            .landlock_write_paths
            .contains(&missing_workspace)
    );
}

#[test]
fn agent_workspace_uses_active_workspace_and_attachment_grants() {
    let temp_dir = tempfile::tempdir().unwrap();
    let active_workspace = temp_dir.path().join("active");
    let attachment_workspace = temp_dir.path().join("attachment");
    std::fs::create_dir_all(&active_workspace).unwrap();
    std::fs::create_dir_all(&attachment_workspace).unwrap();

    let policy = TrustedWorkspacePolicy {
        permissions: WorkspacePermissionSummary {
            has_read: true,
            has_write: true,
        },
        active_workspace: Some(TrustedWorkspacePath {
            path: active_workspace.clone(),
            can_read: true,
            can_write: true,
        }),
        attachments: vec![TrustedWorkspacePath {
            path: attachment_workspace.clone(),
            can_read: true,
            can_write: false,
        }],
    };

    let (workspace_root, allowed_paths) =
        agent_workspace_from_policy(&policy, Uuid::new_v4(), Uuid::new_v4());

    assert_eq!(workspace_root, active_workspace);
    assert_eq!(allowed_paths, vec![active_workspace, attachment_workspace]);
}

#[test]
fn permission_config_inherits_global_defaults_when_workspace_grants_are_missing() {
    let config = swage_config::ResolvedConfig::default();
    let policy = build_permission_config_for_test(&config);

    assert!(
        policy.deny.is_empty(),
        "missing bench workspace grants must inherit global permissions rather than denying"
    );
}

#[test]
fn permission_config_does_not_duplicate_already_denied_tools() {
    let mut config = swage_config::ResolvedConfig::default();
    config.tools.permissions.deny = vec!["file_write".into()];

    let policy = build_permission_config_for_test(&config);

    let file_write_count = policy
        .deny
        .iter()
        .filter(|entry| entry.as_str() == "file_write")
        .count();
    assert_eq!(file_write_count, 1, "file_write should appear once in deny");
}

/// The build_assembly_elements helper constructs the correct elements for
/// the turn's context assembly. This function is tested directly to verify
/// the wiring before it's used in process_turn.
#[test]
fn build_assembly_elements_creates_prompts_skills_memory_and_history() {
    let instructions = "Be helpful.";
    let history = vec![
        ChatMessage::new(Role::User, "Hi"),
        ChatMessage::new(Role::Assistant, "Hello!"),
    ];

    let elements = build_assembly_elements(
        instructions,
        vec![],
        history,
        String::new(),
        "My Bench".to_string(),
    );

    // Should produce four elements: prompts, skills (empty), memory, and history.
    assert_eq!(elements.len(), 4);

    let names: Vec<&str> = elements.iter().map(|e| e.name()).collect();
    assert!(names.contains(&"prompts"), "should include prompts element");
    assert!(names.contains(&"skills"), "should include skills element");
    assert!(names.contains(&"memory"), "should include memory element");
    assert!(names.contains(&"history"), "should include history element");
}

/// When instructions are empty, build_assembly_elements still includes the
/// prompts element (which will produce no system message), the skills
/// element, memory, and history.
#[test]
fn build_assembly_elements_with_empty_instructions() {
    let elements = build_assembly_elements(
        "",
        vec![],
        vec![ChatMessage::new(Role::User, "Hello")],
        String::new(),
        "My Bench".to_string(),
    );

    assert_eq!(elements.len(), 4);
    // PromptsElement with empty rules produces no messages.
    let prompts = elements.iter().find(|e| e.name() == "prompts").unwrap();
    assert!(prompts.content().is_empty());
}

/// When memory_content is non-empty, the memory element produces a system
/// message containing that content.
#[test]
fn build_assembly_elements_memory_element_contains_provided_content() {
    let elements = build_assembly_elements(
        "Be helpful.",
        vec![],
        vec![],
        "Important bench context".to_string(),
        "My Bench".to_string(),
    );

    let memory = elements.iter().find(|e| e.name() == "memory").unwrap();
    let messages = memory.content();

    assert_eq!(
        messages.len(),
        1,
        "non-empty memory should produce one message"
    );
    // content is Vec<ContentPart> — extract the text to check the value.
    let text = messages[0].content().iter().find_map(|p| {
        if let swage_core::ContentPart::Text { text } = p {
            Some(text.as_str())
        } else {
            None
        }
    });
    assert!(
        text.is_some_and(|t| t.contains("Important bench context")),
        "memory message should contain the provided content"
    );
}

/// load_bench_memory returns an empty string when the bench has no
/// memory directory.
#[tokio::test]
async fn load_bench_memory_returns_empty_when_dir_absent() {
    let dir = tempfile::tempdir().unwrap();
    // No memory directory created — the helper should not panic.
    let result = load_bench_memory(dir.path(), "no-such-bench").await;
    assert!(
        result.is_empty(),
        "should return empty string when memory dir is absent"
    );
}

/// load_bench_memory reads all .md files from the memory directory and
/// joins them in alphabetical filename order.
#[tokio::test]
async fn load_bench_memory_concatenates_md_files_alphabetically() {
    let dir = tempfile::tempdir().unwrap();
    let memory_dir = dir.path().join("benches").join("my-bench").join("memory");
    std::fs::create_dir_all(&memory_dir).unwrap();

    // b_notes sorts after a_context.
    std::fs::write(memory_dir.join("b_notes.md"), "Second file content").unwrap();
    std::fs::write(memory_dir.join("a_context.md"), "First file content").unwrap();
    // Non-.md file must not be included.
    std::fs::write(memory_dir.join("other.txt"), "Should be ignored").unwrap();

    let result = load_bench_memory(dir.path(), "my-bench").await;

    assert!(
        result.contains("First file content"),
        "should include first .md file"
    );
    assert!(
        result.contains("Second file content"),
        "should include second .md file"
    );
    assert!(
        !result.contains("Should be ignored"),
        "should not include non-.md files"
    );

    // a_context.md sorts before b_notes.md alphabetically.
    let first_pos = result.find("First file content").unwrap();
    let second_pos = result.find("Second file content").unwrap();
    assert!(
        first_pos < second_pos,
        "files should appear in alphabetical order"
    );
}

/// load_bench_memory skips files with only whitespace content.
#[tokio::test]
async fn load_bench_memory_skips_empty_files() {
    let dir = tempfile::tempdir().unwrap();
    let memory_dir = dir.path().join("benches").join("my-bench").join("memory");
    std::fs::create_dir_all(&memory_dir).unwrap();

    std::fs::write(memory_dir.join("empty.md"), "   \n  ").unwrap();
    std::fs::write(memory_dir.join("content.md"), "Real content").unwrap();

    let result = load_bench_memory(dir.path(), "my-bench").await;

    assert!(result.contains("Real content"));
    assert!(
        !result.contains("   "),
        "whitespace-only files should be excluded"
    );
}

// -----------------------------------------------------------------------
// Token usage persistence tests
// -----------------------------------------------------------------------

/// When the provider returns a Usage chunk, the turn record should have
/// non-null input_tokens and output_tokens in storage after completion.
#[tokio::test]
async fn token_usage_persisted_to_turn_record() {
    use swage_core::TokenUsage;

    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".to_string(),
        },
        StreamChunk::Usage(TokenUsage {
            input_tokens: 42,
            output_tokens: 17,
        }),
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hi".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();
    assert_eq!(turns.len(), 1);

    let turn = &turns[0];
    assert_eq!(
        turn.input_tokens,
        Some(42),
        "input_tokens should be persisted from provider usage"
    );
    assert_eq!(
        turn.output_tokens,
        Some(17),
        "output_tokens should be persisted from provider usage"
    );
}

/// When the provider does not return a Usage chunk, token fields should
/// remain NULL (None) in the turn record.
#[tokio::test]
async fn token_usage_remains_null_when_provider_sends_no_usage() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hi".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = state.storage.list_turns_by_thread(thread_id).await.unwrap();

    // The agent always persists token counts — zero when the provider
    // sends no Usage chunk (the old gateway left them NULL, the agent
    // stores the accumulated total which starts at 0).
    let turn = &turns[0];
    assert_eq!(
        turn.input_tokens,
        Some(0),
        "input_tokens should be zero when no usage reported"
    );
    assert_eq!(
        turn.output_tokens,
        Some(0),
        "output_tokens should be zero when no usage reported"
    );
}

// -----------------------------------------------------------------------
// ContextAssembled event and snapshot persistence tests
// -----------------------------------------------------------------------

/// ContextAssembled must be the first event before any Token events.
#[tokio::test]
async fn context_assembled_precedes_first_token() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hello".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hi".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    // Find positions of ContextAssembled and first Token.
    let assembled_pos = events
        .iter()
        .position(|e| matches!(e, ServerEvent::ContextAssembled { .. }))
        .expect("expected a ContextAssembled event");
    let first_token_pos = events
        .iter()
        .position(|e| matches!(e, ServerEvent::Token { .. }))
        .expect("expected at least one Token event");

    assert!(
        assembled_pos < first_token_pos,
        "ContextAssembled (pos {assembled_pos}) must precede first Token (pos {first_token_pos})"
    );

    // Verify the event carries the correct turn_id.
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    let turn_id = turns[0].id;

    match &events[assembled_pos] {
        ServerEvent::ContextAssembled {
            turn_id: event_turn_id,
            report,
        } => {
            assert_eq!(*event_turn_id, turn_id, "turn_id should match");
            // Report should have an "elements" array from the assembly engine.
            assert!(
                report.get("elements").is_some(),
                "report should contain elements key"
            );
        }
        other => panic!("expected ContextAssembled, got: {other:?}"),
    }
}

/// The assembly report should be persisted as a context snapshot.
#[tokio::test]
async fn assembly_report_persisted_as_context_snapshot() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hi".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    let turn_id = turns[0].id;

    let snapshot = state
        .storage
        .get_snapshot(turn_id)
        .await
        .expect("get_snapshot should succeed");

    assert!(
        snapshot.is_some(),
        "context snapshot should be persisted for the turn"
    );

    // Verify it parses as valid JSON with the expected structure.
    let parsed: serde_json::Value =
        serde_json::from_str(&snapshot.unwrap()).expect("snapshot should be valid JSON");
    assert!(
        parsed.get("elements").is_some(),
        "snapshot should contain elements key"
    );
    let provider_request = &parsed["provider_visible"]["requests"][0];
    assert_eq!(provider_request["round"], serde_json::json!(1));
    assert_eq!(
        provider_request["request"]["stream"],
        serde_json::json!(true)
    );
    assert!(
        provider_request["request"]["body"]["messages"].is_array(),
        "provider-visible snapshot should include converted request messages"
    );
}

#[tokio::test]
async fn existing_placeholder_turn_receives_provider_visible_snapshot() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Async answer".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let thread = state
        .storage
        .list_by_session(session_id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("session should have a thread");
    let placeholder = state
        .storage
        .create_turn(NewTurn {
            thread_id: thread.id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: Some("Async hello".to_string()),
            parent_turn_id: None,
        })
        .await
        .expect("create placeholder turn");
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn_with_existing_turn(
        &state,
        session_id,
        Some(thread.id),
        "Async hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
        Some(placeholder.id),
    )
    .await
    .expect("placeholder turn should complete");

    let turns = state
        .storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("list turns");
    assert_eq!(turns.len(), 1, "existing placeholder must be reused");
    assert_eq!(turns[0].id, placeholder.id);
    assert_eq!(turns[0].state, TurnState::Completed);
    assert_eq!(turns[0].assistant_response.as_deref(), Some("Async answer"));

    let snapshot = state
        .storage
        .get_snapshot(placeholder.id)
        .await
        .expect("get snapshot")
        .expect("provider-visible snapshot should be persisted on placeholder turn");
    let parsed: serde_json::Value =
        serde_json::from_str(&snapshot).expect("snapshot should be valid JSON");
    let provider_request = &parsed["provider_visible"]["requests"][0];
    assert_eq!(provider_request["round"], serde_json::json!(1));
    assert!(
        provider_request["request"]["body"]["messages"].is_array(),
        "provider-visible request should be attached to the reused turn"
    );
}

#[tokio::test]
async fn provider_reasoning_is_persisted_in_context_snapshot_not_visible_response() {
    let chunks = vec![
        StreamChunk::Reasoning {
            content: "private thought".to_string(),
            details: Some(serde_json::json!({"kind": "summary"})),
        },
        StreamChunk::Delta {
            content: "Visible answer".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, _rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "Hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let messages = state
        .storage
        .list_messages_by_thread(threads[0].id)
        .await
        .unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.role == Role::Assistant && message.content == "Visible answer"),
        "visible assistant transcript should contain only visible content"
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.content.contains("private thought")),
        "reasoning side channel must not be persisted as transcript text"
    );

    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    let snapshot = state
        .storage
        .get_snapshot(turns[0].id)
        .await
        .expect("get_snapshot should succeed")
        .expect("snapshot should be persisted");
    let parsed: serde_json::Value =
        serde_json::from_str(&snapshot).expect("snapshot should be valid JSON");
    let reasoning = &parsed["provider_visible"]["reasoning"][0];
    assert_eq!(reasoning["round"], serde_json::json!(1));
    assert_eq!(reasoning["content"], "private thought");
    assert_eq!(reasoning["details"]["kind"], "summary");
}

#[tokio::test]
async fn secret_refs_stay_out_of_snapshots_and_persisted_tool_arguments() {
    struct SecretEchoTool;

    #[async_trait::async_trait]
    impl crate::tool::Tool for SecretEchoTool {
        fn name(&self) -> &str {
            "secret_echo"
        }

        fn description(&self) -> &str {
            "returns a resolved secret"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "secret": { "type": "string" }
                },
                "required": ["secret"]
            })
        }

        fn sensitive_keys(&self) -> &[&str] {
            &["secret"]
        }

        fn host_secret_keys(&self) -> &[&str] {
            &["secret"]
        }

        async fn execute(
            &self,
            arguments: serde_json::Value,
            _ctx: &dyn swage_tool::context::ToolContext,
        ) -> Result<serde_json::Value, swage_core::ToolError> {
            Ok(serde_json::json!({
                "resolved": arguments["secret"].as_str().unwrap_or_default()
            }))
        }
    }

    let secret_value = "host-only-secret";
    let round_2 = vec![
        StreamChunk::Delta {
            content: "done".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(vec![]).await;
    let secret = state
        .secret_manager
        .create_secret(crate::secret::CreateSecretRequest {
            name: "Turn secret".into(),
            material_kind: "plugin_secret".into(),
            owner_kind: "service".into(),
            backend_kind: "managed".into(),
            scope_kind: "plugin".into(),
            scope_id: Some("turn-test".into()),
            metadata: None,
            value: Some(secret_value.into()),
            external_reference: None,
        })
        .await
        .unwrap();
    state.replace_runtime(
        state.active_config(),
        Arc::new(MultiRoundMockProvider::new(vec![
            vec![
                StreamChunk::ToolUse {
                    call_id: "secret-call".into(),
                    name: "secret_echo".into(),
                    arguments: serde_json::json!({
                        "secret": secret.reference
                    }),
                },
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::ToolUse),
                },
            ],
            round_2.clone(),
        ])) as Arc<dyn LlmProvider>,
        crate::tool::ToolDispatcher::allow_all(vec![
            Arc::new(SecretEchoTool) as Arc<dyn crate::tool::Tool>
        ]),
    );

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        "Use the governed secret".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    let tool_calls = state
        .storage
        .list_tool_calls_by_turn(turns[0].id)
        .await
        .unwrap();
    let snapshot = state
        .storage
        .get_snapshot(turns[0].id)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(tool_calls.len(), 1);
    assert!(tool_calls[0].arguments.contains("secret://"));
    assert!(!tool_calls[0].arguments.contains(secret_value));
    assert!(!snapshot.contains(secret_value));
}

#[tokio::test]
async fn provider_visible_capture_fails_closed_when_secret_inventory_is_unavailable() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Safe response".into(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let storage = Arc::new(MockStorage::new());
    let (state, session_id) = make_state_with_storage(chunks, Arc::clone(&storage)).await;
    let secret_value = "must-not-reach-context-snapshot";
    state
        .secret_manager
        .create_secret(crate::secret::CreateSecretRequest {
            name: "Snapshot guard".into(),
            material_kind: "provider_secret".into(),
            owner_kind: "provider".into(),
            backend_kind: "managed".into(),
            scope_kind: "global".into(),
            scope_id: None,
            metadata: None,
            value: Some(secret_value.into()),
            external_reference: None,
        })
        .await
        .expect("create managed secret");
    storage.fail_secret_reads();

    let (tx, _rx) = mpsc::unbounded_channel();
    process_turn(
        &state,
        session_id,
        None,
        format!("Never persist {secret_value}"),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("redaction degradation should not abort the model turn");

    let thread = state
        .storage
        .list_by_session(session_id)
        .await
        .expect("list threads")
        .into_iter()
        .next()
        .expect("session should have a thread");
    let turn = state
        .storage
        .list_turns_by_thread(thread.id)
        .await
        .expect("list turns")
        .into_iter()
        .next()
        .expect("turn should exist");
    let snapshot = state
        .storage
        .get_snapshot(turn.id)
        .await
        .expect("get snapshot")
        .expect("safe degradation marker should be persisted");
    let parsed: serde_json::Value =
        serde_json::from_str(&snapshot).expect("snapshot should remain valid JSON");

    assert!(!snapshot.contains(secret_value));
    assert_eq!(
        parsed["provider_visible"]["capture_errors"][0]["kind"],
        "secret_redaction_unavailable"
    );
    assert_eq!(
        parsed["provider_visible"]["capture_errors"][0]["round"],
        serde_json::json!(1)
    );
}

/// When subscription is off, ContextAssembled is not delivered but
/// snapshot persistence still happens.
#[tokio::test]
async fn context_assembled_not_sent_when_unsubscribed() {
    let chunks = vec![
        StreamChunk::Delta {
            content: "Hi".to_string(),
        },
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    // force_on = false, no channels subscribed.
    process_turn(
        &state,
        session_id,
        None,
        "Hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(false))),
        None,
    )
    .await
    .expect("turn should complete");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    // No ContextAssembled in the event stream.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, ServerEvent::ContextAssembled { .. })),
        "ContextAssembled should not be sent when unsubscribed"
    );

    // But snapshot should still be persisted.
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turns = state
        .storage
        .list_turns_by_thread(threads[0].id)
        .await
        .unwrap();
    let snapshot = state
        .storage
        .get_snapshot(turns[0].id)
        .await
        .expect("get_snapshot should succeed");
    assert!(
        snapshot.is_some(),
        "snapshot should still be persisted even without subscription"
    );
}

// -----------------------------------------------------------------------
// TurnComplete usage field tests
// -----------------------------------------------------------------------

/// When subscribed to context channel, TurnComplete includes token usage.
#[tokio::test]
async fn turn_complete_includes_usage_when_context_subscribed() {
    use swage_core::TokenUsage;

    let chunks = vec![
        StreamChunk::Delta {
            content: "Hi".to_string(),
        },
        StreamChunk::Usage(TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
        }),
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    // force_on = true means all channels are subscribed.
    process_turn(
        &state,
        session_id,
        None,
        "Hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    let complete = events
        .iter()
        .find(|e| matches!(e, ServerEvent::TurnComplete { .. }))
        .expect("expected TurnComplete event");

    match complete {
        ServerEvent::TurnComplete {
            input_tokens,
            output_tokens,
            ..
        } => {
            assert_eq!(*input_tokens, Some(100), "input_tokens should be populated");
            assert_eq!(
                *output_tokens,
                Some(50),
                "output_tokens should be populated"
            );
        }
        other => panic!("expected TurnComplete, got: {other:?}"),
    }
}

// -----------------------------------------------------------------------
// Output schema wiring tests
// -----------------------------------------------------------------------

/// When the config has an output schema and the provider returns valid JSON,
/// TurnComplete carries structured_output and the turn record is persisted.
#[tokio::test]
async fn turn_complete_carries_structured_output_when_schema_passes() {
    let _guard = SwageHomeGuard::isolate();
    use swage_config::OutputConfig;
    use swage_core::FinishReason;

    let chunks = vec![
        StreamChunk::Delta {
            content: r#"{"result":"ok"}"#.to_string(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ];

    // Build a config with an output schema requiring {"result": <string>}.
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "result": { "type": "string" } },
        "required": ["result"]
    });
    let config = swage_config::ResolvedConfig {
        output: OutputConfig {
            schema: Some(schema),
            max_retries: 1,
        },
        ..Default::default()
    };

    let (state, session_id) = make_state_with_config(chunks, config).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "give me json".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(true))),
        None,
    )
    .await
    .expect("turn should complete");

    let events: Vec<ServerEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();

    let complete = events
        .iter()
        .find(|e| matches!(e, ServerEvent::TurnComplete { .. }))
        .expect("expected TurnComplete");

    match complete {
        ServerEvent::TurnComplete {
            structured_output, ..
        } => {
            assert!(
                structured_output.is_some(),
                "TurnComplete should carry structured_output when schema passes"
            );
            assert_eq!(structured_output.as_ref().unwrap()["result"], "ok");
        }
        other => panic!("expected TurnComplete, got: {other:?}"),
    }
}

/// After a valid schema turn, the turn record in storage has structured_output.
#[tokio::test]
async fn structured_output_persisted_on_turn_after_schema_validation() {
    let _guard = SwageHomeGuard::isolate();
    use swage_config::OutputConfig;
    use swage_core::FinishReason;
    use swage_storage::store::TurnStore;

    let chunks = vec![
        StreamChunk::Delta {
            content: r#"{"x":42}"#.to_string(),
        },
        StreamChunk::Done {
            finish_reason: Some(FinishReason::Stop),
        },
    ];

    let schema = serde_json::json!({
        "type": "object",
        "properties": { "x": { "type": "integer" } },
        "required": ["x"]
    });
    let config = swage_config::ResolvedConfig {
        output: OutputConfig {
            schema: Some(schema),
            max_retries: 1,
        },
        ..Default::default()
    };

    let (state, session_id) = make_state_with_config(chunks, config).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    process_turn(
        &state,
        session_id,
        None,
        "give me x".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(false))),
        None,
    )
    .await
    .expect("turn should complete");

    // Drain events to let the turn finish.
    while rx.try_recv().is_ok() {}

    // Look up the turn in storage and verify structured_output was persisted.
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let thread_id = threads[0].id;
    let turns = TurnStore::list_turns_by_thread(&*state.storage, thread_id)
        .await
        .unwrap();
    assert_eq!(turns.len(), 1);
    assert!(
        turns[0].structured_output.is_some(),
        "turn record should have structured_output persisted"
    );
    assert_eq!(turns[0].structured_output.as_ref().unwrap()["x"], 42);
}

/// When NOT subscribed to context channel, TurnComplete omits usage fields.
#[tokio::test]
async fn turn_complete_omits_usage_when_not_subscribed() {
    use swage_core::TokenUsage;

    let chunks = vec![
        StreamChunk::Delta {
            content: "Hi".to_string(),
        },
        StreamChunk::Usage(TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
        }),
        StreamChunk::Done {
            finish_reason: None,
        },
    ];
    let (state, session_id) = make_state_with_session(chunks).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    // force_on = false, no subscriptions.
    process_turn(
        &state,
        session_id,
        None,
        "Hello".to_string(),
        tx,
        Arc::new(Mutex::new(SubscriptionState::new(false))),
        None,
    )
    .await
    .expect("turn should complete");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    let complete = events
        .iter()
        .find(|e| matches!(e, ServerEvent::TurnComplete { .. }))
        .expect("expected TurnComplete event");

    match complete {
        ServerEvent::TurnComplete {
            input_tokens,
            output_tokens,
            cost_microcents,
            ..
        } => {
            assert_eq!(
                *input_tokens, None,
                "input_tokens should be None when unsubscribed"
            );
            assert_eq!(
                *output_tokens, None,
                "output_tokens should be None when unsubscribed"
            );
            assert_eq!(
                *cost_microcents, None,
                "cost_microcents should be None when unsubscribed"
            );
        }
        other => panic!("expected TurnComplete, got: {other:?}"),
    }
}

// -----------------------------------------------------------------------
// Approval event routing tests [byi8]
// -----------------------------------------------------------------------

#[tokio::test]
async fn sandbox_degradation_approval_uses_resolved_hitl_settings() {
    let (state, session_id) = make_state_with_session(vec![]).await;
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id: threads[0].id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: Some("hello".to_string()),
            parent_turn_id: None,
        })
        .await
        .unwrap();

    let mut turn_config = state.config.clone();
    turn_config.hitl.timeout = 7;
    turn_config.hitl.timeout_policy = swage_core::ApprovalTimeoutPolicy::Interrupt;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel_token = CancellationToken::new();
    let approval_channels = Arc::clone(&state.approval_channels);
    let state_for_task = state.clone();
    let task = tokio::spawn(async move {
        request_sandbox_degradation_approval(
            &state_for_task,
            SandboxDegradationApprovalRequest {
                config: &turn_config,
                ws_tx: &tx,
                turn_id: turn.id,
                agent_id: "gateway",
                prompt: "Sandbox setup failed. Approve unsandboxed execution?".to_string(),
                context: serde_json::json!({ "degradation": "unsandboxed_execution" }),
                cancel_token: &cancel_token,
            },
        )
        .await
    });

    let approval_id = match rx.recv().await.expect("approval event should be sent") {
        ServerEvent::ApprovalRequired {
            approval_id,
            timeout_policy,
            ..
        } => {
            assert_eq!(timeout_policy, "interrupt");
            Uuid::parse_str(&approval_id).unwrap()
        }
        other => panic!("expected ApprovalRequired, got: {other:?}"),
    };

    let approval = state
        .storage
        .get_approval(approval_id)
        .await
        .unwrap()
        .expect("approval should be stored");
    assert_eq!(
        approval.timeout_policy,
        swage_core::ApprovalTimeoutPolicy::Interrupt
    );

    approval_channels
        .lock()
        .unwrap()
        .remove(&approval_id)
        .expect("approval channel should be registered")
        .send(swage_agent::ApprovalDecision {
            decision: swage_agent::approval::ApprovalDecisionValue::Approved,
            scope: swage_agent::approval::ApprovalDecisionScope::Once,
            reason: None,
        })
        .expect("approval decision should be delivered");

    let outcome = task
        .await
        .expect("approval task should finish")
        .expect("approval should succeed");
    assert_eq!(outcome, SandboxDegradationApproval::Approved);
}

#[tokio::test]
async fn sandbox_degradation_approval_preserves_cancelled_wait() {
    let (state, session_id) = make_state_with_session(vec![]).await;
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id: threads[0].id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: Some("hello".to_string()),
            parent_turn_id: None,
        })
        .await
        .unwrap();

    let turn_config = state.config.clone();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel_token = CancellationToken::new();
    let cancel_for_task = cancel_token.clone();
    let state_for_task = state.clone();
    let task = tokio::spawn(async move {
        request_sandbox_degradation_approval(
            &state_for_task,
            SandboxDegradationApprovalRequest {
                config: &turn_config,
                ws_tx: &tx,
                turn_id: turn.id,
                agent_id: "gateway",
                prompt: "Sandbox setup failed. Approve unsandboxed execution?".to_string(),
                context: serde_json::json!({ "degradation": "unsandboxed_execution" }),
                cancel_token: &cancel_for_task,
            },
        )
        .await
    });

    match rx.recv().await.expect("approval event should be sent") {
        ServerEvent::ApprovalRequired { .. } => {}
        other => panic!("expected ApprovalRequired, got: {other:?}"),
    }

    cancel_token.cancel();

    let outcome = task
        .await
        .expect("approval task should finish")
        .expect("approval should complete as cancelled");
    assert_eq!(outcome, SandboxDegradationApproval::Interrupted);

    let stored_turn = state
        .storage
        .get_turn(turn.id)
        .await
        .unwrap()
        .expect("turn should exist");
    assert_eq!(stored_turn.state, TurnState::AwaitingApproval);
}

#[tokio::test]
async fn sandbox_degradation_timeout_interrupts_and_cleans_channel() {
    let (state, session_id) = make_state_with_session(vec![]).await;
    let threads = state.storage.list_by_session(session_id).await.unwrap();
    let turn = state
        .storage
        .create_turn(NewTurn {
            thread_id: threads[0].id,
            agent_id: "gateway".to_string(),
            state: TurnState::Pending,
            user_input: Some("hello".to_string()),
            parent_turn_id: None,
        })
        .await
        .unwrap();

    let mut turn_config = state.config.clone();
    turn_config.hitl.timeout = 0;
    turn_config.hitl.timeout_policy = ApprovalTimeoutPolicy::Interrupt;

    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel_token = CancellationToken::new();
    let state_for_task = state.clone();
    let task = tokio::spawn(async move {
        request_sandbox_degradation_approval(
            &state_for_task,
            SandboxDegradationApprovalRequest {
                config: &turn_config,
                ws_tx: &tx,
                turn_id: turn.id,
                agent_id: "gateway",
                prompt: "Sandbox setup failed. Approve unsandboxed execution?".to_string(),
                context: serde_json::json!({ "degradation": "unsandboxed_execution" }),
                cancel_token: &cancel_token,
            },
        )
        .await
    });

    let approval_id = match rx.recv().await.expect("approval event should be sent") {
        ServerEvent::ApprovalRequired { approval_id, .. } => Uuid::parse_str(&approval_id).unwrap(),
        other => panic!("expected ApprovalRequired, got: {other:?}"),
    };

    let outcome = task
        .await
        .expect("approval task should finish")
        .expect("approval should complete as interrupted");
    assert_eq!(outcome, SandboxDegradationApproval::Interrupted);

    assert!(
        state
            .approval_channels
            .lock()
            .unwrap()
            .get(&approval_id)
            .is_none(),
        "timed-out degradation approvals must not leave stale decision channels"
    );
    let approval = state
        .storage
        .get_approval(approval_id)
        .await
        .unwrap()
        .expect("approval should exist");
    assert_eq!(approval.state, ApprovalState::Expired);
}

#[test]
fn approval_required_routes_to_ws_tx() {
    // When the agent emits ApprovalRequired, the session-scoped ws_tx
    // should receive the corresponding ServerEvent — not the broadcast channel.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let approval_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();

    route_approval_agent_event(
        AgentEvent::ApprovalRequired {
            approval_id,
            turn_id,
            agent_id: "agent-1".to_string(),
            kind: "tool".to_string(),
            tool_name: Some("shell".to_string()),
            tool_arguments: Some(serde_json::json!({"cmd": "ls"})),
            hook_name: None,
            prompt: "Allow shell?".to_string(),
            context: None,
            timeout_at: "2026-01-01T00:00:00Z".to_string(),
            timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
        },
        &tx,
    );

    match rx.try_recv().expect("ws_tx must receive the event") {
        ServerEvent::ApprovalRequired {
            approval_id: aid,
            turn_id: tid,
            agent_id,
            kind,
            tool_name,
            ..
        } => {
            assert_eq!(aid, approval_id.to_string());
            assert_eq!(tid, turn_id.to_string());
            assert_eq!(agent_id, "agent-1");
            assert_eq!(kind, "tool");
            assert_eq!(tool_name, Some("shell".to_string()));
        }
        other => panic!("expected ApprovalRequired, got: {other:?}"),
    }
}

#[test]
fn approval_confirmed_routes_to_ws_tx() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let approval_id = Uuid::new_v4();

    route_approval_agent_event(
        AgentEvent::ApprovalConfirmed {
            approval_id,
            decision: swage_agent::approval::ApprovalDecisionValue::Approved,
            reason: Some("looks safe".to_string()),
        },
        &tx,
    );

    match rx.try_recv().expect("ws_tx must receive the event") {
        ServerEvent::ApprovalConfirmed {
            approval_id: aid,
            decision,
            reason,
        } => {
            assert_eq!(aid, approval_id.to_string());
            assert_eq!(decision, "approved");
            assert_eq!(reason, Some("looks safe".to_string()));
        }
        other => panic!("expected ApprovalConfirmed, got: {other:?}"),
    }
}

#[test]
fn approval_expired_routes_to_ws_tx() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let approval_id = Uuid::new_v4();

    route_approval_agent_event(
        AgentEvent::ApprovalExpired {
            approval_id,
            timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
        },
        &tx,
    );

    match rx.try_recv().expect("ws_tx must receive the event") {
        ServerEvent::ApprovalExpired {
            approval_id: aid,
            timeout_policy,
        } => {
            assert_eq!(aid, approval_id.to_string());
            assert_eq!(timeout_policy, "reject");
        }
        other => panic!("expected ApprovalExpired, got: {other:?}"),
    }
}

#[test]
fn approval_events_routed_to_ws_tx_not_broadcast() {
    // Two separate mpsc channels simulating two different sessions.
    // Only the channel passed to route_approval_agent_event should receive
    // the event — the other remains empty, proving session-scoped routing.
    let (tx_a, mut rx_a) = mpsc::unbounded_channel::<ServerEvent>();
    let (_tx_b, mut rx_b) = mpsc::unbounded_channel::<ServerEvent>();

    route_approval_agent_event(
        AgentEvent::ApprovalRequired {
            approval_id: Uuid::new_v4(),
            turn_id: Uuid::new_v4(),
            agent_id: "agent-a".to_string(),
            kind: "tool".to_string(),
            tool_name: None,
            tool_arguments: None,
            hook_name: None,
            prompt: "Allow?".to_string(),
            context: None,
            timeout_at: "2026-01-01T00:00:00Z".to_string(),
            timeout_policy: swage_core::ApprovalTimeoutPolicy::Reject,
        },
        &tx_a, // only agent-a's channel
    );

    // agent-a's client should receive the event.
    assert!(
        rx_a.try_recv().is_ok(),
        "agent-a's ws_tx must receive the ApprovalRequired event"
    );
    // agent-b's client must NOT receive it — routing is session-scoped.
    assert!(
        rx_b.try_recv().is_err(),
        "agent-b's ws_tx must not receive agent-a's approval event"
    );
}
