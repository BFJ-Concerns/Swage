//! LLM provider abstraction with streaming support.
//!
//! Provides a unified `LlmProvider` trait with concrete implementations for
//! OpenAI-compatible, Anthropic, and Google APIs, wrapped in composable
//! reliability decorators (retry, circuit breaking, failover).

// Concrete providers
pub mod anthropic;
pub mod google;
pub mod http;
pub mod openai;
pub mod provider;
pub mod voice;

// Reliability decorators
pub mod builder;
pub mod circuit_breaker;
pub mod failover;
pub mod retry;

// Model resolution
pub mod registry;

mod tool_schema;

pub use provider::{LlmProvider, ProviderConfig, UnsupportedLlmProvider};
pub use registry::{Registry, RegistryError, ResolvedModel};
pub use voice::{
    OpenAiVoiceProvider, SpeechRequest, SpeechResponse, TranscriptionRequest,
    TranscriptionResponse, UnsupportedVoiceProvider, VoiceCapabilities, VoiceProvider,
};
