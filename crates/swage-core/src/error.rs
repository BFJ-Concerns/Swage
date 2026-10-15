//! Error types for the Swage framework.
//!
//! Provides a structured error hierarchy with retriability semantics to enable
//! intelligent retry logic in agent orchestration. Each error variant carries
//! structured fields for programmatic error handling.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Top-level error
// ---------------------------------------------------------------------------

/// Unified error type for the Swage framework.
///
/// Wraps all domain-specific error categories and delegates `is_retriable()`
/// to each inner type, enabling callers to decide retry policy uniformly.
#[derive(Debug, Error)]
pub enum SwageError {
    /// LLM provider communication failure.
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// Persistent storage failure.
    #[error(transparent)]
    Storage(#[from] StorageError),

    /// Tool execution failure.
    #[error(transparent)]
    Tool(#[from] ToolError),

    /// Hook execution failure.
    #[error(transparent)]
    Hook(#[from] HookError),

    /// Safety subsystem failure.
    #[error(transparent)]
    Safety(#[from] SafetyError),

    /// Sandbox subsystem failure.
    #[error(transparent)]
    Sandbox(#[from] SandboxError),

    /// Tokio runtime failure.
    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    /// Secret retrieval failure.
    #[error(transparent)]
    Secret(#[from] SecretError),

    /// Gateway-level failure (routing, session management, validation).
    #[error(transparent)]
    Gateway(#[from] GatewayError),
}

impl SwageError {
    /// Returns true if the error represents a transient failure that may
    /// succeed on retry.
    pub fn is_retriable(&self) -> bool {
        match self {
            SwageError::Provider(e) => e.is_retriable(),
            SwageError::Storage(e) => e.is_retriable(),
            SwageError::Tool(e) => e.is_retriable(),
            SwageError::Hook(e) => e.is_retriable(),
            SwageError::Safety(e) => e.is_retriable(),
            SwageError::Sandbox(e) => e.is_retriable(),
            SwageError::Runtime(e) => e.is_retriable(),
            SwageError::Secret(e) => e.is_retriable(),
            SwageError::Gateway(e) => e.is_retriable(),
        }
    }
}

// ---------------------------------------------------------------------------
// Provider errors
// ---------------------------------------------------------------------------

/// Errors from LLM provider interactions.
#[derive(Debug, Clone, Serialize, Deserialize, Error)]
pub enum ProviderError {
    /// The provider could not serve the request because it was overloaded.
    #[error("provider {provider} overloaded: {message}")]
    Overloaded {
        provider: String,
        message: String,
        retry_after: Option<Duration>,
    },
    /// Rate limit exceeded (transient).
    #[error("rate limited by provider {provider}")]
    RateLimited {
        provider: String,
        retry_after: Option<Duration>,
    },

    /// Request timed out (transient).
    #[error("request to provider {provider} timed out after {duration:?}")]
    Timeout {
        provider: String,
        duration: Duration,
    },

    /// Network-level failure (transient).
    #[error("network error communicating with provider {provider}: {message}")]
    Network { provider: String, message: String },

    /// Authentication or authorisation failure (permanent).
    #[error("authentication failed for provider {provider}")]
    Auth { provider: String },

    /// Invalid provider configuration (permanent).
    #[error("invalid provider configuration: {message}")]
    InvalidConfig { message: String },

    /// Malformed request rejected by the provider (permanent).
    #[error("invalid request: {message}")]
    InvalidRequest { message: String },

    /// Model-specific error (permanent).
    #[error("model error for {model}: {message}")]
    ModelError { model: String, message: String },

    /// Raw HTTP error from the provider (permanent — callers may inspect
    /// `status` for retry decisions on 5xx, but the default is non-retriable).
    #[error("HTTP error {status}: {body}")]
    HttpError { status: u16, body: String },

    /// Error reading or parsing a streaming response (transient).
    #[error("stream error: {0}")]
    StreamError(String),

    /// A provider response completed successfully at the transport layer but
    /// contained no usable model output (transient).
    #[error("provider response anomaly: {0}")]
    ProviderAnomaly(String),

    /// Provider returned a well-formed response with unexpected content (permanent).
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

impl ProviderError {
    /// Returns true if the error represents a transient failure that may
    /// succeed on retry.
    pub fn is_retriable(&self) -> bool {
        // HTTP errors stay conservative by default; callers can inspect
        // `status` for finer retry decisions when duplicate side effects are safe.
        matches!(
            self,
            ProviderError::RateLimited { .. }
                | ProviderError::Overloaded { .. }
                | ProviderError::Timeout { .. }
                | ProviderError::Network { .. }
                | ProviderError::StreamError(_)
                | ProviderError::ProviderAnomaly(_)
        )
    }
}

// ---------------------------------------------------------------------------
// Storage errors
// ---------------------------------------------------------------------------

/// Errors from persistent storage operations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Underlying I/O failure (permanent — caller should inspect the inner error).
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Serialisation or deserialisation failure (permanent).
    #[error("serialisation error: {message}")]
    Serialisation { message: String },

    /// Requested entity not found (permanent).
    #[error("{entity} not found: {id}")]
    NotFound { entity: String, id: String },

    /// Data corruption detected (permanent).
    #[error("data corruption: {message}")]
    Corruption { message: String },

    /// Storage capacity exceeded (transient — may succeed after cleanup).
    #[error("capacity exceeded: {message}")]
    CapacityExceeded { message: String },

    /// Database connection could not be established (transient).
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// SQL or query-level failure (permanent — likely a programming error).
    #[error("query failed: {0}")]
    QueryFailed(String),

    /// Schema migration failed (permanent).
    #[error("migration failed: {0}")]
    MigrationFailed(String),

    /// The store's recorded schema history is not one this build produced
    /// (permanent for this binary): it names a migration this build does not
    /// know, or skips one this build applies before a later one it records.
    /// The store belongs to another build and is refused before any migration
    /// runs, so nothing is half-applied.
    #[error("store schema is not one this build knows: {message}")]
    UnknownSchema { message: String },

    /// Unique constraint or foreign key violation (permanent).
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),
}

impl StorageError {
    /// Returns true if the error represents a transient failure that may
    /// succeed on retry.
    pub fn is_retriable(&self) -> bool {
        matches!(
            self,
            StorageError::CapacityExceeded { .. } | StorageError::ConnectionFailed(_)
        )
    }
}

// ---------------------------------------------------------------------------
// Tool errors
// ---------------------------------------------------------------------------

/// Policy applied when an approval request times out.
///
/// Determines what happens to the pending tool call (and the turn) when the
/// human reviewer does not respond within the configured timeout window.
// Serialise as lowercase strings ("reject", "interrupt") to match the Display
// and FromStr implementations used for storage and TOML config round-trips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalTimeoutPolicy {
    /// Treat the timeout as a rejection: the agent receives feedback that the
    /// approval was not granted, and the turn continues normally.
    #[default]
    Reject,
    /// Mark the turn as Interrupted: no further tool calls or completions are
    /// issued, and the turn ends with an Interrupted state.
    Interrupt,
}

impl std::fmt::Display for ApprovalTimeoutPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApprovalTimeoutPolicy::Reject => write!(f, "reject"),
            ApprovalTimeoutPolicy::Interrupt => write!(f, "interrupt"),
        }
    }
}

impl std::str::FromStr for ApprovalTimeoutPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "reject" => Ok(ApprovalTimeoutPolicy::Reject),
            "interrupt" => Ok(ApprovalTimeoutPolicy::Interrupt),
            other => Err(format!("unknown approval timeout policy: {other:?}")),
        }
    }
}

/// The reason a tool was denied by the permission resolver.
///
/// Distinguishes between two denial modes so callers can construct
/// useful error messages and audit logs without parsing display strings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDeniedReason {
    /// The tool was not found in any allow entry — default-deny applies.
    NotAllowed,
    /// The tool was found in the explicit deny list.
    ExplicitlyDenied,
}

/// How a delegate tool call should complete from the parent turn's point of view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegateCompletionMode {
    /// Wait for the child to finish and return its result as the tool output.
    #[default]
    Wait,
    /// Return once the child has been started; the child continues independently.
    Detach,
}

/// Errors from tool execution.
#[derive(Debug, Error)]
pub enum ToolError {
    #[error("{source}")]
    AfterExecution { source: Box<ToolError> },
    /// Tool not found in registry (permanent).
    #[error("tool not found: {name}")]
    NotFound { name: String },

    /// Tool execution failed (transient — external command may have been
    /// interrupted or returned a transient error).
    #[error("tool {name} execution failed: {message}")]
    ExecutionFailed { name: String, message: String },

    /// Invalid parameters supplied to tool (permanent).
    #[error("invalid parameters for tool {name}: {message}")]
    InvalidParameters { name: String, message: String },

    /// Tool execution timed out (transient).
    #[error("tool {name} timed out after {duration:?}")]
    Timeout { name: String, duration: Duration },

    /// Tool access denied by policy (permanent).
    ///
    /// The `reason` field distinguishes between default-deny (not in any allow
    /// entry) and an explicit deny-list hit, enabling precise audit logging.
    #[error("permission denied for tool {name}: {reason}")]
    PermissionDenied {
        name: String,
        reason: PermissionDeniedReason,
    },

    /// Tool requires human approval before it may execute.
    ///
    /// Returned by the permission check when an allowed tool also appears in
    /// `require_approval`. This is not a retriable error — approval is a distinct
    /// control flow handled by the HITL subsystem.
    #[error("tool {name} requires approval before execution")]
    RequiresApproval {
        name: String,
        /// Optional timeout in seconds; `None` means wait indefinitely.
        timeout_secs: Option<u64>,
        /// Policy applied if the timeout elapses without a decision.
        timeout_policy: Option<ApprovalTimeoutPolicy>,
    },

    /// Tool requested access to a filesystem location that is not currently
    /// granted to this turn.
    #[error("tool {name} requires {access} approval for location {path}")]
    RequiresLocationApproval {
        name: String,
        path: String,
        access: String,
        timeout_secs: Option<u64>,
        timeout_policy: Option<ApprovalTimeoutPolicy>,
    },

    /// Tool requests delegation to another agent.
    ///
    /// Returned by the delegate tool to signal that the agent turn loop should
    /// suspend, run the target agent via the delegation handler, and return the
    /// result as this tool's output. Like `RequiresApproval`, this is control
    /// flow, not an error — the agent handles it by orchestrating delegation.
    #[error("tool {} requires delegation to {}", request.name, request.target_agent_id)]
    RequiresDelegation {
        /// The delegation the tool asked for. Boxed so that this control-flow
        /// variant does not widen every `Result<_, ToolError>` in the tree.
        request: Box<DelegationRequest>,
    },
}

/// Everything the turn loop needs to run a delegated child agent.
#[derive(Debug, Clone)]
pub struct DelegationRequest {
    /// The tool that asked for delegation.
    pub name: String,
    /// The agent to delegate work to.
    pub target_agent_id: String,
    /// Task description for the delegate agent.
    pub task: String,
    /// Optional durable task definition resolved by the runtime.
    pub task_definition: Option<String>,
    /// Optional context to transfer to the delegate agent.
    pub context: Option<serde_json::Value>,
    /// Caller-selected child working directory.
    pub cwd: Option<String>,
    /// Optional runtime-managed child isolation.
    pub isolation: Option<swage_protocol::client::workflow::WorkerIsolation>,
    /// Named skills selected for this child dispatch.
    pub skills: Option<Vec<String>>,
    /// The caller's name for this child. Naming a child that this session has
    /// already delegated to continues with that child — same session, same
    /// thread, everything it was told before — instead of starting a fresh
    /// one. An unused name opens a child under it.
    pub child: Option<String>,
    /// Call-scoped inputs used to compose the child's context.
    pub composition: Box<crate::DispatchContextComposition>,
    /// Whether the parent turn should wait for the child result.
    pub completion_mode: DelegateCompletionMode,
}

impl std::fmt::Display for PermissionDeniedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PermissionDeniedReason::NotAllowed => {
                write!(f, "not in any allow entry")
            }
            PermissionDeniedReason::ExplicitlyDenied => {
                write!(f, "explicitly denied")
            }
        }
    }
}

impl ToolError {
    pub fn after_execution(self) -> Self {
        match self {
            Self::RequiresApproval { .. }
            | Self::RequiresLocationApproval { .. }
            | Self::RequiresDelegation { .. }
            | Self::AfterExecution { .. } => self,
            _ => Self::AfterExecution {
                source: Box::new(self),
            },
        }
    }

    pub fn execution(&self) -> swage_protocol::client::conversation::ToolExecution {
        use swage_protocol::client::conversation::{
            ToolExecution, ToolExecutionStage, ToolOutcome,
        };
        match self {
            Self::AfterExecution { source } => ToolExecution {
                outcome: if matches!(source.as_ref(), Self::InvalidParameters { .. }) {
                    ToolOutcome::Rejected
                } else {
                    ToolOutcome::Failed
                },
                stage: ToolExecutionStage::Finished,
                ..ToolExecution::failed()
            },
            Self::ExecutionFailed { .. } | Self::Timeout { .. } => ToolExecution {
                outcome: ToolOutcome::Failed,
                stage: ToolExecutionStage::Unknown,
                completion: swage_protocol::client::conversation::ToolCompletion::Unknown,
            },
            Self::InvalidParameters { .. } => ToolExecution {
                outcome: ToolOutcome::Rejected,
                ..ToolExecution::refused()
            },
            _ => ToolExecution::refused(),
        }
    }

    /// Returns true if the error represents a transient failure that may
    /// succeed on retry.
    pub fn is_retriable(&self) -> bool {
        if let Self::AfterExecution { source } = self {
            return source.is_retriable();
        }
        // Approval and delegation variants are control flow, not transient failures.
        matches!(
            self,
            ToolError::ExecutionFailed { .. } | ToolError::Timeout { .. }
        )
    }
}

// ---------------------------------------------------------------------------
// Hook errors
// ---------------------------------------------------------------------------

/// Errors from hook execution.
#[derive(Debug, Error)]
pub enum HookError {
    /// Hook script or handler failed (permanent).
    #[error("hook {hook_name} execution failed: {message}")]
    ExecutionFailed { hook_name: String, message: String },

    /// Hook timed out (permanent — hooks should complete promptly).
    #[error("hook {hook_name} timed out after {duration:?}")]
    Timeout {
        hook_name: String,
        duration: Duration,
    },

    /// Hook matcher pattern is invalid (permanent).
    #[error("invalid matcher pattern '{pattern}': {message}")]
    InvalidMatcher { pattern: String, message: String },
}

impl HookError {
    /// Returns true if the error represents a transient failure that may
    /// succeed on retry.
    pub fn is_retriable(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Safety errors
// ---------------------------------------------------------------------------

/// Errors from the safety subsystem (leak detection, redaction).
#[derive(Debug, Error)]
pub enum SafetyError {
    /// Invalid detection pattern (permanent).
    #[error("invalid safety pattern: {message}")]
    InvalidPattern { message: String },

    /// Redaction process failed (permanent).
    #[error("redaction failed: {message}")]
    RedactionFailed { message: String },
}

impl SafetyError {
    /// Returns true if the error represents a transient failure that may
    /// succeed on retry.
    pub fn is_retriable(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Sandbox errors
// ---------------------------------------------------------------------------

/// Errors from the sandboxed execution environment.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// Failed to spawn sandbox process (permanent).
    #[error("sandbox spawn failed: {message}")]
    SpawnFailed { message: String },

    /// Inter-process communication failure (permanent).
    #[error("sandbox IPC error: {message}")]
    IpcError { message: String },

    /// Sandbox environment is not available (permanent).
    #[error("sandbox unavailable: {message}")]
    Unavailable { message: String },
}

impl SandboxError {
    /// Returns true if the error represents a transient failure that may
    /// succeed on retry.
    pub fn is_retriable(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Runtime errors
// ---------------------------------------------------------------------------

/// Errors that can occur during runtime operations.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// Attempted to create a nested Tokio runtime (permanent).
    #[error("cannot create nested runtime: already inside a Tokio runtime context")]
    NestedRuntime,

    /// Failed to create Tokio runtime (permanent).
    #[error("failed to create runtime: {0}")]
    CreationFailed(String),
}

impl RuntimeError {
    /// Returns true if the error represents a transient failure that may succeed on retry.
    pub fn is_retriable(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Secret errors
// ---------------------------------------------------------------------------

/// Errors that can occur during secret retrieval.
#[derive(Debug, Error)]
pub enum SecretError {
    /// Secret not found (permanent).
    #[error("secret not found: {key}")]
    NotFound { key: String },

    /// Access to secret denied (permanent).
    #[error("access denied for secret: {key}")]
    AccessDenied { key: String },

    /// Secret value contains invalid encoding (permanent).
    #[error("secret has invalid encoding: {key}")]
    InvalidEncoding { key: String },
}

impl SecretError {
    /// Returns true if the error represents a transient failure that may succeed on retry.
    pub fn is_retriable(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Gateway errors
// ---------------------------------------------------------------------------

/// Errors from the HTTP/WebSocket gateway layer (routing, sessions, validation).
#[derive(Debug, Error)]
pub enum GatewayError {
    /// Invalid or missing configuration (permanent).
    #[error("config error: {0}")]
    ConfigError(String),

    /// Failed to bind the listening socket (permanent).
    #[error("bind error: {0}")]
    BindError(String),

    /// Upstream provider failure (delegates retriability to inner error).
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    /// Upstream storage failure (delegates retriability to inner error).
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    /// WebSocket-level error (transient — connections can be re-established).
    #[error("websocket error: {0}")]
    WebSocketError(String),

    /// Shutdown signalling failed (permanent).
    #[error("shutdown error: {0}")]
    ShutdownError(String),

    /// Another turn is already in progress for this session (transient —
    /// the client can retry once the active turn completes).
    #[error("turn already in progress for session {session_id}")]
    TurnInProgress { session_id: Uuid },

    /// Conversation does not exist (permanent).
    #[error("conversation {id} not found")]
    ConversationNotFound { id: Uuid },

    /// Topic does not exist (permanent).
    #[error("topic {id} not found")]
    TopicNotFound { id: Uuid },

    /// Topic name or slug conflicts with an existing topic (permanent).
    #[error("topic conflict: {0}")]
    TopicConflict(String),

    /// Request body or parameters failed validation (permanent).
    #[error("validation error: {0}")]
    ValidationError(String),

    /// The selected provider does not expose the requested sign-in flow.
    #[error("provider sign-in is unsupported: {0}")]
    SignInUnsupported(String),

    /// The provider rejected or could not complete a sign-in operation.
    #[error("provider sign-in failed: {0}")]
    SignInFailed(String),

    /// The provider runtime could not be started or reached.
    #[error("provider runtime unavailable: {0}")]
    ProviderRuntimeUnavailable(String),
}

impl GatewayError {
    /// Returns true if the error represents a transient failure that may
    /// succeed on retry.
    pub fn is_retriable(&self) -> bool {
        match self {
            GatewayError::Provider(e) => e.is_retriable(),
            GatewayError::Storage(e) => e.is_retriable(),
            GatewayError::WebSocketError(_) | GatewayError::TurnInProgress { .. } => true,
            GatewayError::ConfigError(_)
            | GatewayError::BindError(_)
            | GatewayError::ShutdownError(_)
            | GatewayError::ConversationNotFound { .. }
            | GatewayError::TopicNotFound { .. }
            | GatewayError::TopicConflict(_)
            | GatewayError::ValidationError(_)
            | GatewayError::SignInUnsupported(_)
            | GatewayError::SignInFailed(_)
            | GatewayError::ProviderRuntimeUnavailable(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Provider errors ------------------------------------------------------

    #[test]
    fn provider_rate_limited_is_retriable() {
        let err = ProviderError::RateLimited {
            provider: "openai".to_string(),
            retry_after: Some(Duration::from_secs(30)),
        };
        assert!(err.is_retriable());
    }

    #[test]
    fn provider_rate_limited_without_retry_after_is_retriable() {
        let err = ProviderError::RateLimited {
            provider: "anthropic".to_string(),
            retry_after: None,
        };
        assert!(err.is_retriable());
    }

    #[test]
    fn provider_timeout_is_retriable() {
        let err = ProviderError::Timeout {
            provider: "openai".to_string(),
            duration: Duration::from_mins(1),
        };
        assert!(err.is_retriable());
    }

    #[test]
    fn provider_network_is_retriable() {
        let err = ProviderError::Network {
            provider: "google".to_string(),
            message: "connection reset".to_string(),
        };
        assert!(err.is_retriable());
    }

    #[test]
    fn provider_auth_is_permanent() {
        let err = ProviderError::Auth {
            provider: "openai".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn provider_invalid_config_is_permanent() {
        let err = ProviderError::InvalidConfig {
            message: "missing API key".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn provider_invalid_request_is_permanent() {
        let err = ProviderError::InvalidRequest {
            message: "bad JSON".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn provider_model_error_is_permanent() {
        let err = ProviderError::ModelError {
            model: "gpt-4".to_string(),
            message: "context window exceeded".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn provider_http_error_is_permanent() {
        let err = ProviderError::HttpError {
            status: 429,
            body: "too many requests".to_string(),
        };
        // Conservative default: non-retriable; callers inspect status for retry logic.
        assert!(!err.is_retriable());
    }

    #[test]
    fn provider_stream_error_is_retriable() {
        let err = ProviderError::StreamError("disconnect".to_string());
        assert!(err.is_retriable());
    }

    #[test]
    fn provider_invalid_response_is_permanent() {
        let err = ProviderError::InvalidResponse("unexpected format".to_string());
        assert!(!err.is_retriable());
    }

    // -- Storage errors -------------------------------------------------------

    #[test]
    fn storage_io_is_permanent() {
        let err = StorageError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "file gone",
        ));
        assert!(!err.is_retriable());
    }

    #[test]
    fn storage_serialisation_is_permanent() {
        let err = StorageError::Serialisation {
            message: "invalid UTF-8".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn storage_not_found_is_permanent() {
        let err = StorageError::NotFound {
            entity: "session".to_string(),
            id: "abc-123".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn storage_corruption_is_permanent() {
        let err = StorageError::Corruption {
            message: "checksum mismatch".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn storage_capacity_exceeded_is_retriable() {
        let err = StorageError::CapacityExceeded {
            message: "disk full".to_string(),
        };
        assert!(err.is_retriable());
    }

    #[test]
    fn storage_connection_failed_is_retriable() {
        let err = StorageError::ConnectionFailed("ECONNREFUSED".to_string());
        assert!(err.is_retriable());
    }

    #[test]
    fn storage_query_failed_is_permanent() {
        let err = StorageError::QueryFailed("syntax error near WHERE".to_string());
        assert!(!err.is_retriable());
    }

    #[test]
    fn storage_migration_failed_is_permanent() {
        let err = StorageError::MigrationFailed("schema mismatch".to_string());
        assert!(!err.is_retriable());
    }

    #[test]
    fn storage_constraint_violation_is_permanent() {
        let err = StorageError::ConstraintViolation("UNIQUE constraint failed".to_string());
        assert!(!err.is_retriable());
    }

    // -- Tool errors ----------------------------------------------------------

    #[test]
    fn tool_not_found_is_permanent() {
        let err = ToolError::NotFound {
            name: "calculator".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn tool_execution_failed_is_retriable() {
        let err = ToolError::ExecutionFailed {
            name: "web_search".to_string(),
            message: "503 service unavailable".to_string(),
        };
        assert!(err.is_retriable());
    }

    #[test]
    fn tool_invalid_parameters_is_permanent() {
        let err = ToolError::InvalidParameters {
            name: "calculator".to_string(),
            message: "missing operand".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn tool_timeout_is_retriable() {
        let err = ToolError::Timeout {
            name: "web_search".to_string(),
            duration: Duration::from_secs(30),
        };
        assert!(err.is_retriable());
    }

    #[test]
    fn tool_permission_denied_is_permanent() {
        let err = ToolError::PermissionDenied {
            name: "shell".to_string(),
            reason: PermissionDeniedReason::NotAllowed,
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn permission_denied_not_allowed_display_includes_reason() {
        let err = ToolError::PermissionDenied {
            name: "shell".to_string(),
            reason: PermissionDeniedReason::NotAllowed,
        };
        let msg = format!("{err}");
        assert!(msg.contains("shell"), "display must include tool name");
        assert!(
            msg.contains("not allowed") || msg.contains("not in any allow"),
            "display must indicate not-allowed denial mode; got: {msg}"
        );
    }

    #[test]
    fn permission_denied_explicitly_denied_display_includes_reason() {
        let err = ToolError::PermissionDenied {
            name: "shell".to_string(),
            reason: PermissionDeniedReason::ExplicitlyDenied,
        };
        let msg = format!("{err}");
        assert!(msg.contains("shell"), "display must include tool name");
        assert!(
            msg.contains("explicitly denied") || msg.contains("deny list"),
            "display must indicate explicit-denial mode; got: {msg}"
        );
    }

    // -- RequiresApproval -----------------------------------------------

    #[test]
    fn requires_approval_constructible() {
        let err = ToolError::RequiresApproval {
            name: "deploy".to_string(),
            timeout_secs: Some(300),
            timeout_policy: Some(ApprovalTimeoutPolicy::Reject),
        };
        if let ToolError::RequiresApproval {
            name,
            timeout_secs,
            timeout_policy,
        } = &err
        {
            assert_eq!(name, "deploy");
            assert_eq!(*timeout_secs, Some(300));
            assert_eq!(*timeout_policy, Some(ApprovalTimeoutPolicy::Reject));
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn requires_approval_optional_fields_none() {
        let err = ToolError::RequiresApproval {
            name: "bash".to_string(),
            timeout_secs: None,
            timeout_policy: None,
        };
        if let ToolError::RequiresApproval {
            timeout_secs,
            timeout_policy,
            ..
        } = &err
        {
            assert!(timeout_secs.is_none());
            assert!(timeout_policy.is_none());
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn requires_approval_is_not_retriable() {
        let err = ToolError::RequiresApproval {
            name: "deploy".to_string(),
            timeout_secs: Some(60),
            timeout_policy: Some(ApprovalTimeoutPolicy::Interrupt),
        };
        assert!(
            !err.is_retriable(),
            "approval is distinct control flow, not retriable"
        );
    }

    #[test]
    fn requires_approval_display_includes_tool_name() {
        let err = ToolError::RequiresApproval {
            name: "deploy".to_string(),
            timeout_secs: Some(300),
            timeout_policy: None,
        };
        let msg = format!("{err}");
        assert!(msg.contains("deploy"), "display must include tool name");
        assert!(
            msg.to_lowercase().contains("approval"),
            "display must mention approval; got: {msg}"
        );
    }

    #[test]
    fn permission_denied_reason_serde_round_trip() {
        let reason = PermissionDeniedReason::NotAllowed;
        let json = serde_json::to_string(&reason).unwrap();
        let decoded: PermissionDeniedReason = serde_json::from_str(&json).unwrap();
        assert_eq!(reason, decoded);

        let reason2 = PermissionDeniedReason::ExplicitlyDenied;
        let json2 = serde_json::to_string(&reason2).unwrap();
        let decoded2: PermissionDeniedReason = serde_json::from_str(&json2).unwrap();
        assert_eq!(reason2, decoded2);
    }

    #[test]
    fn permission_denied_reasons_are_distinct() {
        assert_ne!(
            PermissionDeniedReason::NotAllowed,
            PermissionDeniedReason::ExplicitlyDenied
        );
    }

    // -- Hook errors ----------------------------------------------------------

    #[test]
    fn hook_execution_failed_is_permanent() {
        let err = HookError::ExecutionFailed {
            hook_name: "pre-commit".to_string(),
            message: "exit code 1".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn hook_timeout_is_permanent() {
        let err = HookError::Timeout {
            hook_name: "lint".to_string(),
            duration: Duration::from_secs(10),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn hook_invalid_matcher_is_permanent() {
        let err = HookError::InvalidMatcher {
            pattern: "[invalid".to_string(),
            message: "unclosed bracket".to_string(),
        };
        assert!(!err.is_retriable());
    }

    // -- Safety errors --------------------------------------------------------

    #[test]
    fn safety_invalid_pattern_is_permanent() {
        let err = SafetyError::InvalidPattern {
            message: "regex syntax error".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn safety_redaction_failed_is_permanent() {
        let err = SafetyError::RedactionFailed {
            message: "replacement failed".to_string(),
        };
        assert!(!err.is_retriable());
    }

    // -- Sandbox errors -------------------------------------------------------

    #[test]
    fn sandbox_spawn_failed_is_permanent() {
        let err = SandboxError::SpawnFailed {
            message: "no bubblewrap".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn sandbox_ipc_error_is_permanent() {
        let err = SandboxError::IpcError {
            message: "pipe broken".to_string(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn sandbox_unavailable_is_permanent() {
        let err = SandboxError::Unavailable {
            message: "sandboxing not supported on this platform".to_string(),
        };
        assert!(!err.is_retriable());
    }

    // -- SwageError delegation ------------------------------------------------

    #[test]
    fn swage_error_delegates_retriable_for_transient_provider() {
        let err: SwageError = ProviderError::RateLimited {
            provider: "openai".to_string(),
            retry_after: Some(Duration::from_secs(5)),
        }
        .into();
        assert!(err.is_retriable());
    }

    #[test]
    fn swage_error_delegates_retriable_for_permanent_provider() {
        let err: SwageError = ProviderError::Auth {
            provider: "openai".to_string(),
        }
        .into();
        assert!(!err.is_retriable());
    }

    #[test]
    fn swage_error_delegates_retriable_for_runtime() {
        let err: SwageError = RuntimeError::NestedRuntime.into();
        assert!(!err.is_retriable());
    }

    #[test]
    fn swage_error_delegates_retriable_for_secret() {
        let err: SwageError = SecretError::NotFound {
            key: "API_KEY".to_string(),
        }
        .into();
        assert!(!err.is_retriable());
    }

    #[test]
    fn swage_error_delegates_retriable_for_tool() {
        let err: SwageError = ToolError::Timeout {
            name: "search".to_string(),
            duration: Duration::from_secs(30),
        }
        .into();
        assert!(err.is_retriable());
    }

    #[test]
    fn swage_error_delegates_retriable_for_storage() {
        let err: SwageError = StorageError::CapacityExceeded {
            message: "full".to_string(),
        }
        .into();
        assert!(err.is_retriable());
    }

    #[test]
    fn swage_error_delegates_retriable_for_hook() {
        let err: SwageError = HookError::ExecutionFailed {
            hook_name: "pre-commit".to_string(),
            message: "failed".to_string(),
        }
        .into();
        assert!(!err.is_retriable());
    }

    #[test]
    fn swage_error_delegates_retriable_for_safety() {
        let err: SwageError = SafetyError::InvalidPattern {
            message: "bad regex".to_string(),
        }
        .into();
        assert!(!err.is_retriable());
    }

    #[test]
    fn swage_error_delegates_retriable_for_sandbox() {
        let err: SwageError = SandboxError::Unavailable {
            message: "not supported".to_string(),
        }
        .into();
        assert!(!err.is_retriable());
    }

    #[test]
    fn swage_error_delegates_retriable_for_gateway_transient() {
        let err: SwageError = GatewayError::TurnInProgress {
            session_id: Uuid::new_v4(),
        }
        .into();
        assert!(err.is_retriable());
    }

    #[test]
    fn swage_error_delegates_retriable_for_gateway_permanent() {
        let err: SwageError = GatewayError::ConversationNotFound { id: Uuid::new_v4() }.into();
        assert!(!err.is_retriable());
    }

    // -- Error display --------------------------------------------------------

    #[test]
    fn provider_rate_limited_display_includes_provider() {
        let err = ProviderError::RateLimited {
            provider: "openai".to_string(),
            retry_after: None,
        };
        let msg = format!("{err}");
        assert!(msg.contains("openai"));
        assert!(msg.contains("rate limited"));
    }

    #[test]
    fn tool_timeout_display_includes_name_and_duration() {
        let err = ToolError::Timeout {
            name: "search".to_string(),
            duration: Duration::from_secs(30),
        };
        let msg = format!("{err}");
        assert!(msg.contains("search"));
        assert!(msg.contains("30s"));
    }

    #[test]
    fn provider_http_error_display() {
        let err = ProviderError::HttpError {
            status: 429,
            body: "too many".to_string(),
        };
        assert_eq!(err.to_string(), "HTTP error 429: too many");
    }

    #[test]
    fn provider_rate_limited_display_no_retry_after() {
        let err = ProviderError::RateLimited {
            provider: "anthropic".to_string(),
            retry_after: None,
        };
        assert!(err.to_string().contains("rate limited"));
    }

    #[test]
    fn storage_not_found_display() {
        let err = StorageError::NotFound {
            entity: "conversation".to_string(),
            id: "abc".to_string(),
        };
        assert_eq!(err.to_string(), "conversation not found: abc");
    }

    // -- GatewayError conversions --------------------------------------------

    #[test]
    fn gateway_error_from_provider() {
        // GatewayError::from(ProviderError) must work via #[from].
        let provider_err = ProviderError::Auth {
            provider: "bad key".to_string(),
        };
        let gateway_err = GatewayError::from(provider_err);
        assert!(gateway_err.to_string().contains("provider error"));
    }

    #[test]
    fn gateway_error_from_storage() {
        let storage_err = StorageError::QueryFailed("syntax error".to_string());
        let gateway_err = GatewayError::from(storage_err);
        assert!(gateway_err.to_string().contains("storage error"));
    }

    // -- ProviderError serde --------------------------------------------------

    #[test]
    fn provider_error_serde_round_trip() {
        let err = ProviderError::StreamError("disconnect".to_string());
        let json = serde_json::to_string(&err).unwrap();
        let decoded: ProviderError = serde_json::from_str(&json).unwrap();
        // Compare display strings since PartialEq isn't derived for Duration-containing types.
        assert_eq!(err.to_string(), decoded.to_string());
    }

    // -- RequiresApproval ToolError variant -----------------------------------

    #[test]
    fn tool_requires_approval_is_not_retriable() {
        let err = ToolError::RequiresApproval {
            name: "shell".to_string(),
            timeout_secs: None,
            timeout_policy: None,
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn tool_requires_approval_with_timeout_is_not_retriable() {
        let err = ToolError::RequiresApproval {
            name: "shell".to_string(),
            timeout_secs: Some(30),
            timeout_policy: Some(ApprovalTimeoutPolicy::Reject),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn tool_requires_approval_display_includes_name() {
        let err = ToolError::RequiresApproval {
            name: "shell".to_string(),
            timeout_secs: None,
            timeout_policy: None,
        };
        let msg = format!("{err}");
        assert!(
            msg.contains("shell"),
            "display must include tool name; got: {msg}"
        );
    }

    #[test]
    fn tool_requires_approval_constructible_with_interrupt_policy() {
        // Verify all optional field combinations compile and are constructible.
        let err = ToolError::RequiresApproval {
            name: "write_file".to_string(),
            timeout_secs: Some(60),
            timeout_policy: Some(ApprovalTimeoutPolicy::Interrupt),
        };
        let msg = format!("{err}");
        assert!(msg.contains("write_file"));
    }

    // -- ApprovalTimeoutPolicy ------------------------------------------------

    #[test]
    fn approval_timeout_policy_default_is_reject() {
        let policy = ApprovalTimeoutPolicy::default();
        assert_eq!(policy, ApprovalTimeoutPolicy::Reject);
    }

    #[test]
    fn approval_timeout_policy_variants_are_distinct() {
        assert_ne!(
            ApprovalTimeoutPolicy::Reject,
            ApprovalTimeoutPolicy::Interrupt
        );
    }

    #[test]
    fn approval_timeout_policy_display_reject() {
        assert_eq!(ApprovalTimeoutPolicy::Reject.to_string(), "reject");
    }

    #[test]
    fn approval_timeout_policy_display_interrupt() {
        assert_eq!(ApprovalTimeoutPolicy::Interrupt.to_string(), "interrupt");
    }

    #[test]
    fn approval_timeout_policy_from_str_reject() {
        let policy: ApprovalTimeoutPolicy = "reject".parse().unwrap();
        assert_eq!(policy, ApprovalTimeoutPolicy::Reject);
    }

    #[test]
    fn approval_timeout_policy_from_str_interrupt() {
        let policy: ApprovalTimeoutPolicy = "interrupt".parse().unwrap();
        assert_eq!(policy, ApprovalTimeoutPolicy::Interrupt);
    }

    #[test]
    fn approval_timeout_policy_from_str_unknown_fails() {
        let result: Result<ApprovalTimeoutPolicy, _> = "unknown".parse();
        assert!(
            result.is_err(),
            "unknown policy string should fail to parse"
        );
    }

    #[test]
    fn approval_timeout_policy_display_from_str_round_trip() {
        for variant in [
            ApprovalTimeoutPolicy::Reject,
            ApprovalTimeoutPolicy::Interrupt,
        ] {
            let s = variant.to_string();
            let parsed: ApprovalTimeoutPolicy = s.parse().unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn approval_timeout_policy_serde_round_trip() {
        for variant in [
            ApprovalTimeoutPolicy::Reject,
            ApprovalTimeoutPolicy::Interrupt,
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            let decoded: ApprovalTimeoutPolicy = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, variant);
        }
    }

    #[test]
    fn approval_timeout_policy_clone() {
        let original = ApprovalTimeoutPolicy::Interrupt;
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }
}
