use serde::{Deserialize, Serialize};

use swage_config::HierarchyLevel;
use swage_context::compaction::PendingCompaction;
use swage_context::report::ElementReport;
use swage_core::ChatMessage;
use swage_topology::TopologyEntityCatalog;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerToolError {
    pub message: String,
    pub result: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssemblySearchLevel {
    pub level: HierarchyLevel,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssemblyWorkerData {
    pub prompts_messages: Vec<ChatMessage>,
    pub prompts_token_count: usize,
    pub prompts_source: String,
    pub skills_messages: Vec<ChatMessage>,
    pub skills_token_count: usize,
    pub skills_source: String,
    pub memory_messages: Vec<ChatMessage>,
    pub memory_token_count: usize,
    pub memory_source: String,
    pub history_messages: Vec<ChatMessage>,
    pub history_token_count: usize,
    pub history_source: String,
    pub history_per_message_tokens: Vec<usize>,
    pub search_levels: Vec<AssemblySearchLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssemblyExecutionPayload {
    pub code: String,
    pub path: String,
    pub budget: usize,
    pub model: String,
    pub agent_id: String,
    pub data: AssemblyWorkerData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssemblyExecutionResult {
    pub messages: Vec<ChatMessage>,
    pub element_reports: Vec<ElementReport>,
    pub pending_compactions: Vec<PendingCompaction>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopologyExecutionPayload {
    pub code: String,
    pub path: String,
    pub catalog: TopologyEntityCatalog,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutePayload {
    PythonExecute {
        code: String,
        working_directory: String,
    },
    Assembly(AssemblyExecutionPayload),
    Topology(TopologyExecutionPayload),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostToWorkerMessage {
    Execute {
        request_id: String,
        payload: Box<ExecutePayload>,
    },
    DispatchToolResult {
        request_id: String,
        call_id: String,
        result: Option<serde_json::Value>,
        error: Option<WorkerToolError>,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerToHostMessage {
    Ready,
    DispatchTool {
        request_id: String,
        call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    },
    ExecutionResult {
        request_id: String,
        stdout: String,
        exception_type: Option<String>,
        traceback: Option<String>,
        timed_out: bool,
        result: Option<serde_json::Value>,
    },
    Error {
        message: String,
    },
    ShutdownAck,
}
