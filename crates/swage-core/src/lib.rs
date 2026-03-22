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
pub mod request;
pub mod runtime;
pub mod secret;
pub mod tool;

// ---------------------------------------------------------------------------
// Convenience re-exports
// ---------------------------------------------------------------------------

pub use error::{
    GatewayError, HookError, PermissionDeniedReason, ProviderError, RuntimeError, SafetyError,
    SandboxError, SecretError, StorageError, SwageError, ToolError,
};
pub use message::{ChatMessage, ContentPart, Role};
pub use request::{
    CompletionRequest, CompletionResponse, FinishReason, LlmRequest, LlmResponse,
    StreamChunk, TokenUsage, ToolCompletionRequest, ToolCompletionResponse, ToolRound,
};
pub use tool::{ToolCall, ToolDefinition, ToolResult, build_tool_round_messages};
