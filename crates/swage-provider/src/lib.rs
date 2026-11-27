//! LLM provider abstraction with streaming support.
//!
//! Provides a unified `LlmProvider` trait with concrete implementations for
//! OpenAI-compatible, Anthropic, and Google APIs, wrapped in composable
//! reliability decorators (retry, circuit breaking, failover).

// Concrete providers
pub mod anthropic;
pub mod attribution;
pub mod credential_transport;
pub mod google;
pub mod http;
pub mod openai;
pub mod provider;
pub mod responses;
pub mod voice;

// Reliability decorators
pub mod builder;
pub mod circuit_breaker;
pub mod failover;
pub mod retry;

// Model resolution
pub mod registry;

mod tool_schema;

#[cfg(test)]
pub(crate) mod test_support;

pub use credential_transport::{PlaintextCredentialVerdict, PlaintextExposure};
pub use provider::{LlmProvider, ProviderConfig, UnsupportedLlmProvider};
pub use registry::{Registry, RegistryError, ResolvedModel};
pub use voice::{
    OpenAiVoiceProvider, SpeechRequest, SpeechResponse, TranscriptionRequest,
    TranscriptionResponse, UnsupportedVoiceProvider, VoiceCapabilities, VoiceModelRoutes,
    VoiceProvider,
};

/// Effort control documented by the completion adapter for this model.
/// Codex declares its capabilities through its own runtime.
pub fn documented_effort_capability(
    protocol: &swage_config::Protocol,
    model: &str,
) -> Option<swage_core::EffortCapability> {
    use swage_config::Protocol;

    match protocol {
        Protocol::Anthropic => anthropic::model_effort_capability(model),
        Protocol::OpenAi | Protocol::OpenAiResponses | Protocol::OpenRouter => {
            openai::model_effort_capability(model)
        }
        Protocol::Google => google::google_effort_capability(model),
        Protocol::Codex => None,
    }
}
