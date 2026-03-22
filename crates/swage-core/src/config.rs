//! Configuration types for LLM providers and models.
//!
//! Defines `ProviderConfig`, `ModelConfig`, and `Protocol` for configuring
//! provider connections with URL validation and serialisation support.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::ProviderError;

/// Wire protocol used to communicate with an LLM provider.
///
/// No "Custom" variant — custom endpoints use `OpenAiCompatible` with a
/// `base_url` override in `ProviderConfig`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    OpenAiCompatible,
    Anthropic,
    Google,
}

/// Configuration for connecting to an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Unique identifier for this provider instance.
    pub id: String,
    /// Wire protocol to use.
    pub protocol: Protocol,
    /// Environment variable name containing the API key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Custom base URL (must be HTTP or HTTPS).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Rate limit: maximum requests per minute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requests_per_minute: Option<u32>,
    /// Rate limit: maximum tokens per minute.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_per_minute: Option<u32>,
}

impl ProviderConfig {
    /// Creates a new provider configuration, validating the base URL if provided.
    ///
    /// Rejects URLs that are not HTTP or HTTPS. `http://localhost` and loopback
    /// addresses are permitted for local development.
    pub fn new(
        id: String,
        protocol: Protocol,
        api_key_env: Option<String>,
        base_url: Option<String>,
        requests_per_minute: Option<u32>,
        tokens_per_minute: Option<u32>,
    ) -> Result<Self, ProviderError> {
        // Validate the URL scheme if one is provided.
        if let Some(ref raw_url) = base_url {
            let parsed = Url::parse(raw_url).map_err(|err| ProviderError::InvalidConfig {
                message: format!("invalid base URL '{raw_url}': {err}"),
            })?;

            let scheme = parsed.scheme();
            if scheme != "http" && scheme != "https" {
                return Err(ProviderError::InvalidConfig {
                    message: format!(
                        "base URL must use http or https scheme, got '{scheme}'"
                    ),
                });
            }
        }

        Ok(Self {
            id,
            protocol,
            api_key_env,
            base_url,
            requests_per_minute,
            tokens_per_minute,
        })
    }
}

/// Configuration for a specific model exposed by a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Unique identifier for this model configuration.
    pub id: String,
    /// Which provider serves this model (references `ProviderConfig::id`).
    pub provider_id: String,
    /// Provider-specific model identifier (e.g. "gpt-4", "claude-sonnet-4-20250514").
    pub model_id: String,
    /// Alternative names that resolve to this model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Maximum context window size in tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- Protocol -------------------------------------------------------------

    #[test]
    fn protocol_serialisation_roundtrip() {
        let variants = [Protocol::OpenAiCompatible, Protocol::Anthropic, Protocol::Google];
        for protocol in &variants {
            let json = serde_json::to_string(protocol).unwrap();
            let restored: Protocol = serde_json::from_str(&json).unwrap();
            assert_eq!(*protocol, restored);
        }
    }

    // -- ProviderConfig construction ------------------------------------------

    #[test]
    fn new_without_base_url_succeeds() {
        let config = ProviderConfig::new(
            "openai".to_string(),
            Protocol::OpenAiCompatible,
            Some("OPENAI_API_KEY".to_string()),
            None,
            Some(60),
            None,
        );
        assert!(config.is_ok());
        let config = config.unwrap();
        assert_eq!(config.id, "openai");
        assert!(config.base_url.is_none());
    }

    #[test]
    fn new_with_https_url_succeeds() {
        let config = ProviderConfig::new(
            "custom".to_string(),
            Protocol::OpenAiCompatible,
            None,
            Some("https://api.example.com/v1".to_string()),
            None,
            None,
        );
        assert!(config.is_ok());
    }

    #[test]
    fn new_with_http_localhost_succeeds() {
        let config = ProviderConfig::new(
            "local".to_string(),
            Protocol::OpenAiCompatible,
            None,
            Some("http://localhost:8080".to_string()),
            None,
            None,
        );
        assert!(config.is_ok());
    }

    #[test]
    fn new_rejects_missing_scheme() {
        let result = ProviderConfig::new(
            "bad".to_string(),
            Protocol::Anthropic,
            None,
            Some("example.com/v1".to_string()),
            None,
            None,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ProviderError::InvalidConfig { .. }));
    }

    #[test]
    fn new_rejects_non_http_scheme() {
        let result = ProviderConfig::new(
            "bad".to_string(),
            Protocol::Google,
            None,
            Some("ftp://files.example.com".to_string()),
            None,
            None,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        let ProviderError::InvalidConfig { message } = err else {
            panic!("expected InvalidConfig");
        };
        assert!(message.contains("http or https"));
    }

    #[test]
    fn new_rejects_malformed_url() {
        let result = ProviderConfig::new(
            "bad".to_string(),
            Protocol::OpenAiCompatible,
            None,
            Some("://not-a-url".to_string()),
            None,
            None,
        );
        assert!(result.is_err());
    }

    // -- ProviderConfig serialisation -----------------------------------------

    #[test]
    fn provider_config_roundtrips_through_json() {
        let original = ProviderConfig::new(
            "anthropic".to_string(),
            Protocol::Anthropic,
            Some("ANTHROPIC_API_KEY".to_string()),
            Some("https://api.anthropic.com".to_string()),
            Some(100),
            Some(100_000),
        )
        .unwrap();

        let json = serde_json::to_string(&original).unwrap();
        let restored: ProviderConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.id, "anthropic");
        assert_eq!(restored.protocol, Protocol::Anthropic);
        assert_eq!(restored.api_key_env, Some("ANTHROPIC_API_KEY".to_string()));
        assert_eq!(
            restored.base_url,
            Some("https://api.anthropic.com".to_string())
        );
        assert_eq!(restored.requests_per_minute, Some(100));
        assert_eq!(restored.tokens_per_minute, Some(100_000));
    }

    #[test]
    fn provider_config_omits_none_fields_in_json() {
        let config = ProviderConfig::new(
            "minimal".to_string(),
            Protocol::Google,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("api_key_env"));
        assert!(!json.contains("base_url"));
        assert!(!json.contains("requests_per_minute"));
        assert!(!json.contains("tokens_per_minute"));
    }

    // -- ModelConfig ----------------------------------------------------------

    #[test]
    fn model_config_roundtrips_through_json() {
        let original = ModelConfig {
            id: "gpt4o".to_string(),
            provider_id: "openai".to_string(),
            model_id: "gpt-4o".to_string(),
            aliases: vec!["4o".to_string(), "gpt4-omni".to_string()],
            context_window: Some(128_000),
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored: ModelConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.id, "gpt4o");
        assert_eq!(restored.provider_id, "openai");
        assert_eq!(restored.model_id, "gpt-4o");
        assert_eq!(restored.aliases, vec!["4o", "gpt4-omni"]);
        assert_eq!(restored.context_window, Some(128_000));
    }

    #[test]
    fn model_config_with_empty_aliases_omits_field() {
        let config = ModelConfig {
            id: "claude".to_string(),
            provider_id: "anthropic".to_string(),
            model_id: "claude-sonnet-4-20250514".to_string(),
            aliases: vec![],
            context_window: None,
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("aliases"));
        assert!(!json.contains("context_window"));
    }
}
