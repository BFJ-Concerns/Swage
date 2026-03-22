//! Configuration types for LLM providers and models.
//!
//! Defines structures for configuring provider connections and model specifications,
//! supporting serialisation for loading from configuration files.

use serde::{Deserialize, Serialize};

use crate::error::ProviderError;

/// LLM provider type discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderType {
    /// OpenAI provider (GPT models).
    OpenAI,
    /// Anthropic provider (Claude models).
    Anthropic,
    /// Google provider (Gemini models).
    Google,
    /// Custom provider with custom implementation.
    Custom,
}

/// Configuration for an LLM provider.
///
/// Defines connection details, authentication, and rate limits for a provider.
/// Provider configurations are referenced by models to establish which provider
/// handles requests for specific model IDs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Unique identifier for this provider configuration.
    ///
    /// Used by `ModelConfig` to reference this provider.
    pub id: String,

    /// Type of provider (OpenAI, Anthropic, Google, Custom).
    pub provider_type: ProviderType,

    /// Name of the environment variable containing the API key.
    ///
    /// If `None`, the provider implementation will use its default environment variable
    /// (e.g., `OPENAI_API_KEY` for OpenAI).
    pub api_key_env: Option<String>,

    /// Custom base URL for API requests.
    ///
    /// If `None`, uses the provider's default endpoint. Useful for:
    /// - Proxy servers
    /// - Self-hosted model endpoints
    /// - Provider-specific regions
    ///
    /// Must be a valid URL if provided.
    pub base_url: Option<String>,

    /// Maximum requests per minute allowed for this provider.
    ///
    /// If `None`, no rate limiting is applied at the configuration level.
    /// Providers may still enforce their own limits.
    pub requests_per_minute: Option<u32>,

    /// Maximum tokens per minute allowed for this provider.
    ///
    /// If `None`, no token-based rate limiting is applied. Useful for preventing
    /// quota exhaustion on metered plans.
    pub tokens_per_minute: Option<u32>,
}

impl ProviderConfig {
    /// Creates a new provider configuration with validation.
    ///
    /// # Errors
    ///
    /// Returns `ProviderError::InvalidConfig` if:
    /// - `base_url` is provided but not a valid URL
    pub fn new(
        id: String,
        provider_type: ProviderType,
        api_key_env: Option<String>,
        base_url: Option<String>,
        requests_per_minute: Option<u32>,
        tokens_per_minute: Option<u32>,
    ) -> Result<Self, ProviderError> {
        // Validate base_url if provided
        if let Some(ref url) = base_url {
            // Basic URL validation: must start with http:// or https://
            // and contain at least a scheme and host
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(ProviderError::InvalidConfig(format!(
                    "base_url must start with http:// or https://, got: {}",
                    url
                )));
            }

            // Parse to validate structure
            url::Url::parse(url).map_err(|e| {
                ProviderError::InvalidConfig(format!("invalid base_url: {}", e))
            })?;
        }

        Ok(Self {
            id,
            provider_type,
            api_key_env,
            base_url,
            requests_per_minute,
            tokens_per_minute,
        })
    }
}

/// Configuration for a specific model.
///
/// Defines model-specific metadata including provider association, identifiers,
/// and constraints like context window size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Unique identifier for this model configuration.
    ///
    /// Used to reference this model in application code or configuration.
    pub id: String,

    /// ID of the provider that serves this model.
    ///
    /// Must correspond to a `ProviderConfig.id` in the configuration.
    pub provider_id: String,

    /// Provider's model identifier.
    ///
    /// This is the actual model ID used in API requests (e.g., "gpt-4",
    /// "claude-3-opus-20240229").
    pub model_id: String,

    /// Alternative names for looking up this model.
    ///
    /// Allows referencing the same model by multiple names (e.g., "gpt4" as
    /// an alias for "gpt-4").
    pub aliases: Vec<String>,

    /// Maximum context window size in tokens.
    ///
    /// If `None`, the model's context window is unknown or unlimited.
    pub context_window: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_config_serialisation_roundtrip() {
        let config = ProviderConfig {
            id: "openai-main".to_string(),
            provider_type: ProviderType::OpenAI,
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            base_url: Some("https://api.openai.com/v1".to_string()),
            requests_per_minute: Some(60),
            tokens_per_minute: Some(100000),
        };

        let json = serde_json::to_string(&config).expect("serialisation failed");
        let deserialised: ProviderConfig =
            serde_json::from_str(&json).expect("deserialisation failed");

        assert_eq!(config.id, deserialised.id);
        assert_eq!(config.provider_type, deserialised.provider_type);
        assert_eq!(config.api_key_env, deserialised.api_key_env);
        assert_eq!(config.base_url, deserialised.base_url);
        assert_eq!(config.requests_per_minute, deserialised.requests_per_minute);
        assert_eq!(config.tokens_per_minute, deserialised.tokens_per_minute);
    }

    #[test]
    fn model_config_serialisation_roundtrip() {
        let config = ModelConfig {
            id: "gpt-4-turbo".to_string(),
            provider_id: "openai-main".to_string(),
            model_id: "gpt-4-turbo-preview".to_string(),
            aliases: vec!["gpt4".to_string(), "gpt-4".to_string()],
            context_window: Some(128000),
        };

        let json = serde_json::to_string(&config).expect("serialisation failed");
        let deserialised: ModelConfig =
            serde_json::from_str(&json).expect("deserialisation failed");

        assert_eq!(config.id, deserialised.id);
        assert_eq!(config.provider_id, deserialised.provider_id);
        assert_eq!(config.model_id, deserialised.model_id);
        assert_eq!(config.aliases, deserialised.aliases);
        assert_eq!(config.context_window, deserialised.context_window);
    }

    #[test]
    fn provider_config_validates_url() {
        // Valid HTTPS URL
        let result = ProviderConfig::new(
            "test".to_string(),
            ProviderType::OpenAI,
            None,
            Some("https://api.example.com".to_string()),
            None,
            None,
        );
        assert!(result.is_ok());

        // Valid HTTP URL (allowed for local development)
        let result = ProviderConfig::new(
            "test".to_string(),
            ProviderType::Custom,
            None,
            Some("http://localhost:8080".to_string()),
            None,
            None,
        );
        assert!(result.is_ok());

        // Invalid URL: missing scheme
        let result = ProviderConfig::new(
            "test".to_string(),
            ProviderType::OpenAI,
            None,
            Some("api.example.com".to_string()),
            None,
            None,
        );
        assert!(result.is_err());

        // Invalid URL: malformed
        let result = ProviderConfig::new(
            "test".to_string(),
            ProviderType::OpenAI,
            None,
            Some("https://".to_string()),
            None,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn provider_config_accepts_none_url() {
        let result = ProviderConfig::new(
            "test".to_string(),
            ProviderType::Anthropic,
            Some("ANTHROPIC_API_KEY".to_string()),
            None, // No custom URL
            Some(50),
            Some(80000),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn provider_type_serialisation() {
        let types = vec![
            ProviderType::OpenAI,
            ProviderType::Anthropic,
            ProviderType::Google,
            ProviderType::Custom,
        ];

        for provider_type in types {
            let json = serde_json::to_string(&provider_type).expect("serialisation failed");
            let deserialised: ProviderType =
                serde_json::from_str(&json).expect("deserialisation failed");
            assert_eq!(provider_type, deserialised);
        }
    }

    #[test]
    fn minimal_configs_serialise() {
        // ProviderConfig with minimal fields
        let provider = ProviderConfig {
            id: "minimal".to_string(),
            provider_type: ProviderType::Custom,
            api_key_env: None,
            base_url: None,
            requests_per_minute: None,
            tokens_per_minute: None,
        };

        let json = serde_json::to_string(&provider).expect("serialisation failed");
        let _: ProviderConfig = serde_json::from_str(&json).expect("deserialisation failed");

        // ModelConfig with minimal fields
        let model = ModelConfig {
            id: "minimal".to_string(),
            provider_id: "test".to_string(),
            model_id: "test-model".to_string(),
            aliases: vec![],
            context_window: None,
        };

        let json = serde_json::to_string(&model).expect("serialisation failed");
        let _: ModelConfig = serde_json::from_str(&json).expect("deserialisation failed");
    }
}
