use swage_core::CompletionRequest;

use crate::ProviderConfig;

/// The unadorned completion request used by provider projection tests.
pub(crate) fn empty_completion_request() -> CompletionRequest {
    CompletionRequest {
        effort: None,
        messages: vec![],
        model: None,
        max_tokens: None,
        temperature: None,
        stop_sequences: None,
        metadata: None,
        tool_definitions: vec![],
    }
}

/// A local provider configuration for request-shape tests.
pub(crate) fn local_provider_config() -> ProviderConfig {
    ProviderConfig::new("key", "model", "http://localhost")
}
