//! Model registry for resolving model references to provider/model pairs.
//!
//! The registry maintains a central configuration of LLM providers and models,
//! enabling resolution of model references through either aliases or explicit
//! provider/model identifiers.

use std::collections::HashMap;
use thiserror::Error;

use crate::config::{ModelConfig, ProviderConfig};

/// Errors that can occur during registry operations.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// Model reference not found (neither as alias nor explicit format).
    #[error("unknown model reference '{reference}'. Available aliases: {}", format_alias_list(.available_aliases))]
    UnknownModel {
        reference: String,
        available_aliases: Vec<String>,
    },

    /// Provider not found when registering a model.
    #[error("provider '{provider_id}' not found when registering model '{model_id}'")]
    ProviderNotFound {
        provider_id: String,
        model_id: String,
    },

    /// Provider not found when resolving explicit format.
    #[error("unknown provider '{provider_id}' in reference '{reference}'")]
    UnknownProvider {
        provider_id: String,
        reference: String,
    },

    /// Duplicate alias detected during model registration.
    #[error("duplicate alias '{alias}' - already registered to model '{existing_model_id}'")]
    DuplicateAlias {
        alias: String,
        existing_model_id: String,
    },

    /// Invalid explicit format (should be "provider/model").
    #[error("invalid model reference format '{reference}' - expected 'alias' or 'provider/model'")]
    InvalidFormat { reference: String },
}

/// Formats a list of aliases for error messages.
fn format_alias_list(aliases: &[String]) -> String {
    if aliases.is_empty() {
        "(none)".to_string()
    } else {
        aliases.join(", ")
    }
}

/// Resolved model information containing provider and model identifiers.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedModel {
    /// Provider identifier.
    pub provider_id: String,
    /// Model identifier used in API requests.
    pub model_id: String,
    /// Optional full provider configuration.
    pub provider_config: Option<ProviderConfig>,
    /// Optional full model configuration.
    pub model_config: Option<ModelConfig>,
}

/// Registry for managing and resolving provider and model configurations.
///
/// The registry maintains:
/// - Provider configurations indexed by ID
/// - Model configurations indexed by ID
/// - Alias lookup table mapping aliases to model IDs
///
/// # Examples
///
/// ```
/// use swage::config::{ProviderConfig, ProviderType, ModelConfig};
/// use swage::registry::Registry;
///
/// let mut registry = Registry::new();
///
/// // Register a provider
/// let provider = ProviderConfig {
///     id: "anthropic".to_string(),
///     provider_type: ProviderType::Anthropic,
///     api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
///     base_url: None,
///     requests_per_minute: Some(50),
///     tokens_per_minute: Some(100000),
/// };
/// registry.register_provider(provider).unwrap();
///
/// // Register a model with aliases
/// let model = ModelConfig {
///     id: "claude-sonnet-4".to_string(),
///     provider_id: "anthropic".to_string(),
///     model_id: "claude-sonnet-4-20250514".to_string(),
///     aliases: vec!["sonnet".to_string(), "claude".to_string()],
///     context_window: Some(200000),
/// };
/// registry.register_model(model).unwrap();
///
/// // Resolve by alias
/// let resolved = registry.resolve("sonnet").unwrap();
/// assert_eq!(resolved.provider_id, "anthropic");
/// assert_eq!(resolved.model_id, "claude-sonnet-4-20250514");
///
/// // Resolve by explicit format
/// let resolved = registry.resolve("anthropic/claude-sonnet-4-20250514").unwrap();
/// assert_eq!(resolved.provider_id, "anthropic");
/// ```
pub struct Registry {
    /// Provider configurations indexed by provider ID.
    providers: HashMap<String, ProviderConfig>,
    /// Model configurations indexed by model ID.
    models: HashMap<String, ModelConfig>,
    /// Alias lookup table mapping alias to model ID.
    aliases: HashMap<String, String>,
}

impl Registry {
    /// Creates a new empty registry.
    pub fn new() -> Self {
        Self {
            providers: HashMap::new(),
            models: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    /// Registers a provider configuration.
    ///
    /// If a provider with the same ID already exists, it will be replaced.
    ///
    /// # Examples
    ///
    /// ```
    /// use swage::config::{ProviderConfig, ProviderType};
    /// use swage::registry::Registry;
    ///
    /// let mut registry = Registry::new();
    /// let provider = ProviderConfig {
    ///     id: "openai".to_string(),
    ///     provider_type: ProviderType::OpenAI,
    ///     api_key_env: Some("OPENAI_API_KEY".to_string()),
    ///     base_url: None,
    ///     requests_per_minute: Some(60),
    ///     tokens_per_minute: Some(90000),
    /// };
    ///
    /// registry.register_provider(provider).unwrap();
    /// ```
    pub fn register_provider(&mut self, config: ProviderConfig) -> Result<(), RegistryError> {
        self.providers.insert(config.id.clone(), config);
        Ok(())
    }

    /// Registers a model configuration and builds alias index.
    ///
    /// # Validation
    ///
    /// - Ensures the referenced provider exists
    /// - Detects duplicate aliases across models
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::ProviderNotFound` if the provider doesn't exist.
    /// Returns `RegistryError::DuplicateAlias` if any alias is already registered.
    ///
    /// # Examples
    ///
    /// ```
    /// use swage::config::{ProviderConfig, ProviderType, ModelConfig};
    /// use swage::registry::Registry;
    ///
    /// let mut registry = Registry::new();
    ///
    /// // Must register provider first
    /// let provider = ProviderConfig {
    ///     id: "openai".to_string(),
    ///     provider_type: ProviderType::OpenAI,
    ///     api_key_env: None,
    ///     base_url: None,
    ///     requests_per_minute: None,
    ///     tokens_per_minute: None,
    /// };
    /// registry.register_provider(provider).unwrap();
    ///
    /// let model = ModelConfig {
    ///     id: "gpt-4".to_string(),
    ///     provider_id: "openai".to_string(),
    ///     model_id: "gpt-4-turbo-preview".to_string(),
    ///     aliases: vec!["gpt4".to_string()],
    ///     context_window: Some(128000),
    /// };
    /// registry.register_model(model).unwrap();
    /// ```
    pub fn register_model(&mut self, config: ModelConfig) -> Result<(), RegistryError> {
        // Validate provider exists
        if !self.providers.contains_key(&config.provider_id) {
            return Err(RegistryError::ProviderNotFound {
                provider_id: config.provider_id.clone(),
                model_id: config.id.clone(),
            });
        }

        // Check for duplicate aliases
        for alias in &config.aliases {
            if let Some(existing_model_id) = self.aliases.get(alias) {
                return Err(RegistryError::DuplicateAlias {
                    alias: alias.clone(),
                    existing_model_id: existing_model_id.clone(),
                });
            }
        }

        // Register aliases
        for alias in &config.aliases {
            self.aliases.insert(alias.clone(), config.id.clone());
        }

        // Register model
        self.models.insert(config.id.clone(), config);

        Ok(())
    }

    /// Resolves a model reference to provider and model identifiers.
    ///
    /// Supports two resolution formats:
    /// - **Alias**: `"sonnet"` → looks up alias in registry
    /// - **Explicit**: `"anthropic/claude-sonnet-4"` → parses provider/model directly
    ///
    /// # Errors
    ///
    /// - `RegistryError::UnknownModel` if alias not found (includes available aliases)
    /// - `RegistryError::UnknownProvider` if provider in explicit format not found
    /// - `RegistryError::InvalidFormat` if reference is malformed
    ///
    /// # Examples
    ///
    /// ```
    /// use swage::config::{ProviderConfig, ProviderType, ModelConfig};
    /// use swage::registry::Registry;
    ///
    /// let mut registry = Registry::new();
    ///
    /// let provider = ProviderConfig {
    ///     id: "anthropic".to_string(),
    ///     provider_type: ProviderType::Anthropic,
    ///     api_key_env: None,
    ///     base_url: None,
    ///     requests_per_minute: None,
    ///     tokens_per_minute: None,
    /// };
    /// registry.register_provider(provider).unwrap();
    ///
    /// let model = ModelConfig {
    ///     id: "sonnet-4".to_string(),
    ///     provider_id: "anthropic".to_string(),
    ///     model_id: "claude-sonnet-4-20250514".to_string(),
    ///     aliases: vec!["sonnet".to_string()],
    ///     context_window: Some(200000),
    /// };
    /// registry.register_model(model).unwrap();
    ///
    /// // Alias resolution
    /// let resolved = registry.resolve("sonnet").unwrap();
    /// assert_eq!(resolved.provider_id, "anthropic");
    /// assert_eq!(resolved.model_id, "claude-sonnet-4-20250514");
    ///
    /// // Explicit resolution
    /// let resolved = registry.resolve("anthropic/claude-sonnet-4-20250514").unwrap();
    /// assert_eq!(resolved.provider_id, "anthropic");
    /// assert_eq!(resolved.model_id, "claude-sonnet-4-20250514");
    /// ```
    pub fn resolve(&self, model_ref: &str) -> Result<ResolvedModel, RegistryError> {
        // Try explicit format first (contains '/')
        if model_ref.contains('/') {
            return self.resolve_explicit(model_ref);
        }

        // Try alias lookup
        self.resolve_alias(model_ref)
    }

    /// Resolves a model reference by alias lookup.
    fn resolve_alias(&self, alias: &str) -> Result<ResolvedModel, RegistryError> {
        let model_id = self.aliases.get(alias).ok_or_else(|| {
            let mut available: Vec<String> = self.aliases.keys().cloned().collect();
            available.sort();
            RegistryError::UnknownModel {
                reference: alias.to_string(),
                available_aliases: available,
            }
        })?;

        let model_config = self
            .models
            .get(model_id)
            .expect("alias points to non-existent model");

        let provider_config = self
            .providers
            .get(&model_config.provider_id)
            .expect("model references non-existent provider");

        Ok(ResolvedModel {
            provider_id: provider_config.id.clone(),
            model_id: model_config.model_id.clone(),
            provider_config: Some(provider_config.clone()),
            model_config: Some(model_config.clone()),
        })
    }

    /// Resolves a model reference in explicit "provider/model" format.
    fn resolve_explicit(&self, reference: &str) -> Result<ResolvedModel, RegistryError> {
        let parts: Vec<&str> = reference.splitn(2, '/').collect();

        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(RegistryError::InvalidFormat {
                reference: reference.to_string(),
            });
        }

        let provider_id = parts[0];
        let model_id = parts[1];

        // Validate provider exists
        let provider_config = self.providers.get(provider_id).ok_or_else(|| {
            RegistryError::UnknownProvider {
                provider_id: provider_id.to_string(),
                reference: reference.to_string(),
            }
        })?;

        Ok(ResolvedModel {
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
            provider_config: Some(provider_config.clone()),
            model_config: None, // Explicit format doesn't map to a registered ModelConfig
        })
    }

    /// Returns a sorted list of all registered aliases.
    ///
    /// Useful for displaying available models to users.
    pub fn list_aliases(&self) -> Vec<String> {
        let mut aliases: Vec<String> = self.aliases.keys().cloned().collect();
        aliases.sort();
        aliases
    }

    /// Returns a list of all registered provider IDs.
    pub fn list_providers(&self) -> Vec<String> {
        let mut providers: Vec<String> = self.providers.keys().cloned().collect();
        providers.sort();
        providers
    }

    /// Returns a list of all registered model IDs.
    pub fn list_models(&self) -> Vec<String> {
        let mut models: Vec<String> = self.models.keys().cloned().collect();
        models.sort();
        models
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderType;

    fn create_test_provider(id: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.to_string(),
            provider_type: ProviderType::Anthropic,
            api_key_env: None,
            base_url: None,
            requests_per_minute: None,
            tokens_per_minute: None,
        }
    }

    fn create_test_model(id: &str, provider_id: &str, model_id: &str) -> ModelConfig {
        ModelConfig {
            id: id.to_string(),
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
            aliases: vec![],
            context_window: None,
        }
    }

    #[test]
    fn register_provider_succeeds() {
        let mut registry = Registry::new();
        let provider = create_test_provider("anthropic");

        assert!(registry.register_provider(provider).is_ok());
        assert_eq!(registry.list_providers(), vec!["anthropic"]);
    }

    #[test]
    fn register_model_validates_provider_exists() {
        let mut registry = Registry::new();
        let model = create_test_model("sonnet", "anthropic", "claude-sonnet-4");

        // Should fail without provider
        let result = registry.register_model(model.clone());
        assert!(matches!(
            result,
            Err(RegistryError::ProviderNotFound { .. })
        ));

        // Should succeed after provider registration
        registry
            .register_provider(create_test_provider("anthropic"))
            .unwrap();
        assert!(registry.register_model(model).is_ok());
    }

    #[test]
    fn register_model_builds_alias_index() {
        let mut registry = Registry::new();
        registry
            .register_provider(create_test_provider("anthropic"))
            .unwrap();

        let mut model = create_test_model("sonnet-4", "anthropic", "claude-sonnet-4-20250514");
        model.aliases = vec!["sonnet".to_string(), "claude".to_string()];

        registry.register_model(model).unwrap();

        let aliases = registry.list_aliases();
        assert_eq!(aliases, vec!["claude", "sonnet"]);
    }

    #[test]
    fn register_model_detects_duplicate_aliases() {
        let mut registry = Registry::new();
        registry
            .register_provider(create_test_provider("anthropic"))
            .unwrap();

        let mut model1 = create_test_model("model-1", "anthropic", "claude-1");
        model1.aliases = vec!["sonnet".to_string()];
        registry.register_model(model1).unwrap();

        let mut model2 = create_test_model("model-2", "anthropic", "claude-2");
        model2.aliases = vec!["sonnet".to_string()]; // Duplicate!

        let result = registry.register_model(model2);
        assert!(matches!(result, Err(RegistryError::DuplicateAlias { .. })));
    }

    #[test]
    fn resolve_alias_succeeds() {
        let mut registry = Registry::new();
        registry
            .register_provider(create_test_provider("anthropic"))
            .unwrap();

        let mut model = create_test_model("sonnet-4", "anthropic", "claude-sonnet-4-20250514");
        model.aliases = vec!["sonnet".to_string()];
        registry.register_model(model).unwrap();

        let resolved = registry.resolve("sonnet").unwrap();
        assert_eq!(resolved.provider_id, "anthropic");
        assert_eq!(resolved.model_id, "claude-sonnet-4-20250514");
        assert!(resolved.provider_config.is_some());
        assert!(resolved.model_config.is_some());
    }

    #[test]
    fn resolve_alias_returns_error_with_suggestions() {
        let mut registry = Registry::new();
        registry
            .register_provider(create_test_provider("anthropic"))
            .unwrap();

        let mut model = create_test_model("sonnet-4", "anthropic", "claude-sonnet-4");
        model.aliases = vec!["sonnet".to_string(), "claude".to_string()];
        registry.register_model(model).unwrap();

        let result = registry.resolve("unknown");
        match result {
            Err(RegistryError::UnknownModel {
                reference,
                available_aliases,
            }) => {
                assert_eq!(reference, "unknown");
                assert_eq!(available_aliases, vec!["claude", "sonnet"]);
            }
            _ => panic!("Expected UnknownModel error"),
        }
    }

    #[test]
    fn resolve_explicit_format_succeeds() {
        let mut registry = Registry::new();
        registry
            .register_provider(create_test_provider("anthropic"))
            .unwrap();

        let resolved = registry.resolve("anthropic/claude-sonnet-4-20250514").unwrap();
        assert_eq!(resolved.provider_id, "anthropic");
        assert_eq!(resolved.model_id, "claude-sonnet-4-20250514");
        assert!(resolved.provider_config.is_some());
        assert!(resolved.model_config.is_none()); // Explicit format doesn't map to ModelConfig
    }

    #[test]
    fn resolve_explicit_format_validates_provider() {
        let registry = Registry::new();

        let result = registry.resolve("unknown-provider/model-id");
        assert!(matches!(
            result,
            Err(RegistryError::UnknownProvider { .. })
        ));
    }

    #[test]
    fn resolve_explicit_format_rejects_invalid_formats() {
        let mut registry = Registry::new();
        registry
            .register_provider(create_test_provider("anthropic"))
            .unwrap();

        // Missing model part
        let result = registry.resolve("anthropic/");
        assert!(matches!(result, Err(RegistryError::InvalidFormat { .. })));

        // Missing provider part
        let result = registry.resolve("/model");
        assert!(matches!(result, Err(RegistryError::InvalidFormat { .. })));

        // Multiple slashes (only first split matters)
        let result = registry.resolve("anthropic/nested/path");
        assert!(result.is_ok()); // "nested/path" is treated as the model ID
    }

    #[test]
    fn list_methods_return_sorted_results() {
        let mut registry = Registry::new();

        registry
            .register_provider(create_test_provider("zeta"))
            .unwrap();
        registry
            .register_provider(create_test_provider("alpha"))
            .unwrap();
        registry
            .register_provider(create_test_provider("beta"))
            .unwrap();

        let mut model1 = create_test_model("model-z", "alpha", "model-z-id");
        model1.aliases = vec!["zebra".to_string()];
        registry.register_model(model1).unwrap();

        let mut model2 = create_test_model("model-a", "beta", "model-a-id");
        model2.aliases = vec!["apple".to_string()];
        registry.register_model(model2).unwrap();

        assert_eq!(registry.list_providers(), vec!["alpha", "beta", "zeta"]);
        assert_eq!(registry.list_models(), vec!["model-a", "model-z"]);
        assert_eq!(registry.list_aliases(), vec!["apple", "zebra"]);
    }

    #[test]
    fn empty_registry_lists_return_empty() {
        let registry = Registry::new();

        assert!(registry.list_providers().is_empty());
        assert!(registry.list_models().is_empty());
        assert!(registry.list_aliases().is_empty());
    }

    #[test]
    fn resolve_preserves_full_configs() {
        let mut registry = Registry::new();

        let provider = ProviderConfig {
            id: "anthropic".to_string(),
            provider_type: ProviderType::Anthropic,
            api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
            base_url: Some("https://api.anthropic.com".to_string()),
            requests_per_minute: Some(50),
            tokens_per_minute: Some(100000),
        };
        registry.register_provider(provider.clone()).unwrap();

        let model = ModelConfig {
            id: "sonnet-4".to_string(),
            provider_id: "anthropic".to_string(),
            model_id: "claude-sonnet-4-20250514".to_string(),
            aliases: vec!["sonnet".to_string()],
            context_window: Some(200000),
        };
        registry.register_model(model.clone()).unwrap();

        let resolved = registry.resolve("sonnet").unwrap();

        // Verify full configs are included
        assert_eq!(
            resolved.provider_config.unwrap().api_key_env,
            Some("ANTHROPIC_API_KEY".to_string())
        );
        assert_eq!(resolved.model_config.unwrap().context_window, Some(200000));
    }
}
