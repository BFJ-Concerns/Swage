//! Core types and abstractions for the Swage framework.
//!
//! Provides shared types used across all Swage crates: conversation messages,
//! structured errors with retriability semantics, provider/model configuration,
//! completion request/response models, tool definitions, and utility functions.
//!
//! This crate has no internal dependencies on other Swage crates. It serves
//! as the foundation that all other crates build upon.

pub mod config;
pub mod error;
pub mod message;
pub mod message_rendering;
pub mod messaging;
pub mod request;
pub mod runtime;
pub mod secret;
pub mod tool;

// ---------------------------------------------------------------------------
// Convenience re-exports
// ---------------------------------------------------------------------------

pub use error::{
    ApprovalTimeoutPolicy, GatewayError, HookError, PermissionDeniedReason, ProviderError,
    RuntimeError, SafetyError, SandboxError, SecretError, StorageError, SwageError, ToolError,
};
pub use message::{ChatMessage, ContentPart, Role, ToolReplayPart};
pub use message_rendering::{render_message_for_model_context, role_label_for_model_context};
pub use messaging::{ContextTransferMode, MessageKind, MessageStatus};
pub use request::{
    CompletionRequest, CompletionResponse, FinishReason, LlmRequest, LlmResponse, StreamChunk,
    TokenUsage, ToolCompletionRequest, ToolCompletionResponse,
};
pub use tool::{
    CompactToolReplay, ToolCall, ToolDefinition, ToolReplayReasoning, ToolResult,
    build_tool_round_messages, build_tool_round_messages_with_reasoning,
    compact_tool_arguments_for_model, compact_tool_result_for_model,
    compact_tool_result_for_model_with_reference, extract_tool_result_metadata,
    parse_tool_result_reference, tool_result_reference,
};
