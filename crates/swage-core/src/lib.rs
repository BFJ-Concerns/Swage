//! Core types and abstractions for the Swage framework.
//!
//! Provides shared types used across all Swage crates: conversation messages,
//! structured errors with retriability semantics, provider/model configuration,
//! completion request/response models, tool definitions, and utility functions.
//!
//! This crate has no internal dependencies on other Swage crates. It serves
//! as the foundation that all other crates build upon.

pub mod config;
pub mod dispatch;
pub mod effort;
pub mod error;
pub mod message;
pub mod message_rendering;
pub mod messaging;
pub mod request;
pub mod runtime;
pub mod scope;
pub mod secret;
pub mod tool;

/// Whether a value is a non-empty lowercase ASCII slug.
pub fn is_lowercase_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Compile a JSON Schema for the validation surfaces that share Swage's
/// contract grammar. Callers retain ownership of their user-facing errors.
pub fn json_schema_validator(schema: &serde_json::Value) -> Result<jsonschema::Validator, String> {
    jsonschema::validator_for(schema).map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Convenience re-exports
// ---------------------------------------------------------------------------

pub use dispatch::{DispatchContextComposition, validate_context_assembly_name};
pub use effort::{EffortControl, ResolvedEffort, resolve_effort};
pub use error::{
    ApprovalTimeoutPolicy, DelegateCompletionMode, DelegationRequest, GatewayError, HookError,
    PermissionDeniedReason, ProviderError, RuntimeError, SafetyError, SandboxError, SecretError,
    StorageError, SwageError, ToolError,
};
pub use message::{ChatMessage, ContentPart, Role, ToolReplayPart};
pub use message_rendering::{render_message_for_model_context, role_label_for_model_context};
pub use messaging::{ContextTransferMode, MessageKind, MessageStatus};
pub use request::{
    CompletionRequest, CompletionResponse, FinishReason, LlmRequest, LlmResponse,
    ProviderContentBlockWitness, ProviderResponseWitness, StreamChunk, TokenUsage,
    ToolCompletionRequest, ToolCompletionResponse,
};
pub use scope::ScopeLocation;
pub use swage_protocol::client::settings::{Effort, EffortCapability};
pub use tool::{
    CompactToolReplay, ToolCall, ToolDefinition, ToolReplayReasoning, ToolResult,
    build_tool_round_messages, build_tool_round_messages_with_reasoning,
    compact_tool_arguments_for_model, compact_tool_result_for_model,
    compact_tool_result_for_model_with_reference, extract_tool_result_metadata,
    parse_tool_result_reference, tool_result_reference,
};
