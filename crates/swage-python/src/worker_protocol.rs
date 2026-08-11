use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerToolError {
    pub message: String,
    pub result: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutePayload {
    PythonExecute {
        code: String,
        working_directory: String,
    },
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
