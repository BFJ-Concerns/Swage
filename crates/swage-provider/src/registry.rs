//! Model registry for provider and model resolution.
//!
//! Registers provider configurations (with `Protocol` for dispatch) and model
//! configurations (with aliases), then resolves model references at runtime.
//!
//! Resolution supports two forms:
//! - **By alias**: `"claude"` → find model with alias → look up its provider
//! - **By explicit format**: `"anthropic/claude-sonnet"` → provider_id `"anthropic"`,
//!   model substring `"claude-sonnet"`

use std::collections::HashMap;

use swage_core::config::{ModelConfig, ProviderConfig};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors arising from registry operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum RegistryError {
    /// A provider with the given ID is already registered.
    #[error("duplicate provider id: {0}")]
    DuplicateProvider(String),

    /// A model with the given ID is already registered.
    #[error("duplicate model id: {0}")]
    DuplicateModel(String),

    /// A model alias clashes with one already claimed by another model.
    #[error("duplicate alias '{alias}' (already used by model '{existing_model}')")]
    DuplicateAlias {
        alias: String,
        existing_model: String,
    },

    /// A model references a provider that hasn't been registered.
    #[error("model '{model_id}' references unknown provider '{provider_id}'")]
    UnknownProvider {
        model_id: String,
        provider_id: String,
    },

    /// Resolution found no model matching the given name.
    #[error("no model found for '{0}'")]
    NotFound(String),
}

// ---------------------------------------------------------------------------
// ResolvedModel
// ---------------------------------------------------------------------------

/// The result of resolving a model name — carries everything needed to
/// construct a provider and issue an API call.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// The provider that serves this model.
    pub provider: ProviderConfig,
    /// The model configuration (including the provider-specific model_id).
    pub model: ModelConfig,
}

impl ResolvedModel {
    /// Return a reference to the resolved provider configuration.
    pub fn provider_config(&self) -> &ProviderConfig {
        &self.provider
    }

    /// Return a reference to the resolved model configuration.
    pub fn model_config(&self) -> &ModelConfig {
        &self.model
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// In-memory registry of provider and model configurations.
///
/// Thread-safe for reads once populated — designed to be built at startup and
/// then shared immutably. No interior mutability required because the gateway
/// builds the registry during initialisation and then passes `Arc<Registry>`
/// to request handlers.
#[derive(Debug, Default)]
pub struct Registry {
    /// Providers keyed by their `id`.
    providers: HashMap<String, ProviderConfig>,
    /// Models keyed by their `id`.
    models: HashMap<String, ModelConfig>,
    /// Alias → model id mapping. Every alias must be globally unique.
    aliases: HashMap<String, String>,
}

impl Registry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a provider configuration.
    ///
    /// Rejects duplicates — a provider with the same `id` cannot be
    /// registered twice.
    pub fn register_provider(
        &mut self,
        config: ProviderConfig,
    ) -> Result<(), RegistryError> {
        if self.providers.contains_key(&config.id) {
            return Err(RegistryError::DuplicateProvider(config.id.clone()));
        }
        self.providers.insert(config.id.clone(), config);
        Ok(())
    }

    /// Registers a model configuration.
    ///
    /// Validates that:
    /// 1. No model with the same `id` is already registered.
    /// 2. The referenced `provider_id` exists.
    /// 3. None of the model's aliases clash with existing aliases.
    pub fn register_model(
        &mut self,
        config: ModelConfig,
    ) -> Result<(), RegistryError> {
        // Reject duplicate model id.
        if self.models.contains_key(&config.id) {
            return Err(RegistryError::DuplicateModel(config.id.clone()));
        }

        // The provider must already be registered.
        if !self.providers.contains_key(&config.provider_id) {
            return Err(RegistryError::UnknownProvider {
                model_id: config.id.clone(),
                provider_id: config.provider_id.clone(),
            });
        }

        // Check all aliases for uniqueness before committing any.
        for alias in &config.aliases {
            if let Some(existing_model_id) = self.aliases.get(alias) {
                return Err(RegistryError::DuplicateAlias {
                    alias: alias.clone(),
                    existing_model: existing_model_id.clone(),
                });
            }
        }

        // Commit: register aliases, then store the model.
        for alias in &config.aliases {
            self.aliases.insert(alias.clone(), config.id.clone());
        }
        self.models.insert(config.id.clone(), config);

        Ok(())
    }

    /// Resolves a model name to its provider and model configuration.
    ///
    /// Resolution order:
    /// 1. **Exact model id** — if `name` matches a registered model id directly.
    /// 2. **Alias** — if `name` matches a registered alias.
    /// 3. **Explicit format** — `"provider_id/model_substring"` searches models
    ///    belonging to the given provider for one whose `model_id` contains the
    ///    substring.
    pub fn resolve(&self, name: &str) -> Result<ResolvedModel, RegistryError> {
        // 1. Exact model id match.
        if let Some(model) = self.models.get(name) {
            return self.build_resolved(model);
        }

        // 2. Alias match.
        if let Some(model_id) = self.aliases.get(name) {
            if let Some(model) = self.models.get(model_id) {
                return self.build_resolved(model);
            }
        }

        // 3. Explicit "provider_id/model_substring" format.
        if let Some((provider_id, model_substring)) = name.split_once('/') {
            // The provider must exist for this format to make sense.
            if !self.providers.contains_key(provider_id) {
                return Err(RegistryError::NotFound(name.to_string()));
            }

            // Search models belonging to this provider whose model_id contains
            // the substring.
            let matching: Vec<&ModelConfig> = self
                .models
                .values()
                .filter(|m| {
                    m.provider_id == provider_id
                        && m.model_id.contains(model_substring)
                })
                .collect();

            if matching.len() == 1 {
                return self.build_resolved(matching[0]);
            }
            // Ambiguous or no match — fall through to NotFound.
        }

        Err(RegistryError::NotFound(name.to_string()))
    }

    // -- Internal helpers ---------------------------------------------------

    /// Pairs a model with its provider to build a `ResolvedModel`.
    fn build_resolved(
        &self,
        model: &ModelConfig,
    ) -> Result<ResolvedModel, RegistryError> {
        let provider = self
            .providers
            .get(&model.provider_id)
            // Shouldn't happen — register_model validates provider existence —
            // but guard against inconsistency defensively.
            .ok_or_else(|| RegistryError::UnknownProvider {
                model_id: model.id.clone(),
                provider_id: model.provider_id.clone(),
            })?;

        Ok(ResolvedModel {
            provider: provider.clone(),
            model: model.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::config::Protocol;

    // -- Helpers ------------------------------------------------------------

    /// Convenience: build a provider config without URL validation overhead.
    fn provider(id: &str, protocol: Protocol) -> ProviderConfig {
        ProviderConfig::new(
            id.to_string(),
            protocol,
            Some(format!("{}_API_KEY", id.to_uppercase())),
            None,
            None,
            None,
        )
        .unwrap()
    }

    fn model(
        id: &str,
        provider_id: &str,
        model_id: &str,
        aliases: &[&str],
    ) -> ModelConfig {
        ModelConfig {
            id: id.to_string(),
            provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            context_window: None,
        }
    }

    // -- Registry::new ------------------------------------------------------

    #[test]
    fn new_registry_is_empty() {
        let registry = Registry::new();
        assert!(registry.providers.is_empty());
        assert!(registry.models.is_empty());
        assert!(registry.aliases.is_empty());
    }

    // -- register_provider --------------------------------------------------

    #[test]
    fn register_provider_succeeds() {
        let mut registry = Registry::new();
        let result =
            registry.register_provider(provider("openai", Protocol::OpenAiCompatible));
        assert!(result.is_ok());
        assert!(registry.providers.contains_key("openai"));
    }

    #[test]
    fn register_multiple_providers_with_different_protocols() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("openai", Protocol::OpenAiCompatible))
            .unwrap();
        registry
            .register_provider(provider("anthropic", Protocol::Anthropic))
            .unwrap();
        registry
            .register_provider(provider("google", Protocol::Google))
            .unwrap();

        assert_eq!(registry.providers.len(), 3);
    }

    #[test]
    fn register_duplicate_provider_rejected() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("openai", Protocol::OpenAiCompatible))
            .unwrap();

        let result =
            registry.register_provider(provider("openai", Protocol::OpenAiCompatible));
        assert!(matches!(result, Err(RegistryError::DuplicateProvider(id)) if id == "openai"));
    }

    // -- register_model -----------------------------------------------------

    #[test]
    fn register_model_succeeds() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("openai", Protocol::OpenAiCompatible))
            .unwrap();

        let result = registry.register_model(model(
            "gpt4o",
            "openai",
            "gpt-4o",
            &["4o", "gpt4-omni"],
        ));
        assert!(result.is_ok());
        assert!(registry.models.contains_key("gpt4o"));
        assert_eq!(registry.aliases.get("4o"), Some(&"gpt4o".to_string()));
        assert_eq!(
            registry.aliases.get("gpt4-omni"),
            Some(&"gpt4o".to_string())
        );
    }

    #[test]
    fn register_model_with_unknown_provider_rejected() {
        let mut registry = Registry::new();

        let result = registry.register_model(model(
            "gpt4",
            "nonexistent",
            "gpt-4",
            &[],
        ));
        assert!(matches!(
            result,
            Err(RegistryError::UnknownProvider { model_id, provider_id })
            if model_id == "gpt4" && provider_id == "nonexistent"
        ));
    }

    #[test]
    fn register_duplicate_model_id_rejected() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("openai", Protocol::OpenAiCompatible))
            .unwrap();
        registry
            .register_model(model("gpt4", "openai", "gpt-4", &[]))
            .unwrap();

        let result =
            registry.register_model(model("gpt4", "openai", "gpt-4-turbo", &[]));
        assert!(matches!(result, Err(RegistryError::DuplicateModel(id)) if id == "gpt4"));
    }

    #[test]
    fn register_model_with_duplicate_alias_rejected() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("openai", Protocol::OpenAiCompatible))
            .unwrap();
        registry
            .register_model(model("gpt4", "openai", "gpt-4", &["smart"]))
            .unwrap();

        // Another model tries to claim the same alias.
        let result = registry.register_model(model(
            "gpt4turbo",
            "openai",
            "gpt-4-turbo",
            &["smart"],
        ));
        assert!(matches!(
            result,
            Err(RegistryError::DuplicateAlias { alias, existing_model })
            if alias == "smart" && existing_model == "gpt4"
        ));
    }

    #[test]
    fn duplicate_alias_rejection_is_atomic() {
        // If a model has aliases ["ok", "clash"], and "clash" collides,
        // then "ok" should NOT have been registered either.
        let mut registry = Registry::new();
        registry
            .register_provider(provider("openai", Protocol::OpenAiCompatible))
            .unwrap();
        registry
            .register_model(model("existing", "openai", "gpt-4", &["clash"]))
            .unwrap();

        let result = registry.register_model(model(
            "newcomer",
            "openai",
            "gpt-4-turbo",
            &["ok", "clash"],
        ));
        assert!(result.is_err());

        // "ok" must not have leaked into the alias map.
        assert!(!registry.aliases.contains_key("ok"));
        // "newcomer" must not be in models.
        assert!(!registry.models.contains_key("newcomer"));
    }

    // -- resolve by exact model id ------------------------------------------

    #[test]
    fn resolve_by_exact_model_id() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("anthropic", Protocol::Anthropic))
            .unwrap();
        registry
            .register_model(model(
                "sonnet",
                "anthropic",
                "claude-sonnet-4-20250514",
                &["claude"],
            ))
            .unwrap();

        let resolved = registry.resolve("sonnet").unwrap();
        assert_eq!(resolved.model.id, "sonnet");
        assert_eq!(resolved.model.model_id, "claude-sonnet-4-20250514");
        assert_eq!(resolved.provider.id, "anthropic");
        assert_eq!(resolved.provider.protocol, Protocol::Anthropic);
    }

    // -- resolve by alias ---------------------------------------------------

    #[test]
    fn resolve_by_alias() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("anthropic", Protocol::Anthropic))
            .unwrap();
        registry
            .register_model(model(
                "sonnet",
                "anthropic",
                "claude-sonnet-4-20250514",
                &["claude", "sonnet4"],
            ))
            .unwrap();

        let resolved = registry.resolve("claude").unwrap();
        assert_eq!(resolved.model.id, "sonnet");

        let resolved2 = registry.resolve("sonnet4").unwrap();
        assert_eq!(resolved2.model.id, "sonnet");
    }

    // -- resolve by explicit provider/model format --------------------------

    #[test]
    fn resolve_by_explicit_format() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("openai", Protocol::OpenAiCompatible))
            .unwrap();
        registry
            .register_model(model("gpt4o", "openai", "gpt-4o", &[]))
            .unwrap();

        let resolved = registry.resolve("openai/gpt-4o").unwrap();
        assert_eq!(resolved.model.id, "gpt4o");
        assert_eq!(resolved.model.model_id, "gpt-4o");
        assert_eq!(resolved.provider.id, "openai");
    }

    #[test]
    fn resolve_by_explicit_format_with_substring() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("google", Protocol::Google))
            .unwrap();
        registry
            .register_model(model(
                "gemini-pro",
                "google",
                "gemini-1.5-pro-latest",
                &[],
            ))
            .unwrap();

        // Substring match: "pro" matches "gemini-1.5-pro-latest".
        let resolved = registry.resolve("google/pro").unwrap();
        assert_eq!(resolved.model.id, "gemini-pro");
    }

    #[test]
    fn resolve_explicit_format_ambiguous_returns_not_found() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("openai", Protocol::OpenAiCompatible))
            .unwrap();
        registry
            .register_model(model("gpt4", "openai", "gpt-4", &[]))
            .unwrap();
        registry
            .register_model(model("gpt4o", "openai", "gpt-4o", &[]))
            .unwrap();

        // "gpt-4" matches both "gpt-4" and "gpt-4o" — ambiguous.
        let result = registry.resolve("openai/gpt-4");
        assert!(matches!(result, Err(RegistryError::NotFound(_))));
    }

    #[test]
    fn resolve_explicit_format_unknown_provider_returns_not_found() {
        let registry = Registry::new();
        let result = registry.resolve("unknown/model");
        assert!(matches!(result, Err(RegistryError::NotFound(_))));
    }

    // -- resolve not found --------------------------------------------------

    #[test]
    fn resolve_unknown_name_returns_not_found() {
        let registry = Registry::new();
        let result = registry.resolve("nonexistent");
        assert!(matches!(result, Err(RegistryError::NotFound(name)) if name == "nonexistent"));
    }

    // -- ResolvedModel carries correct provider config ----------------------

    #[test]
    fn resolved_model_carries_provider_api_key_env() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("anthropic", Protocol::Anthropic))
            .unwrap();
        registry
            .register_model(model(
                "claude",
                "anthropic",
                "claude-sonnet-4-20250514",
                &[],
            ))
            .unwrap();

        let resolved = registry.resolve("claude").unwrap();
        assert_eq!(
            resolved.provider.api_key_env,
            Some("ANTHROPIC_API_KEY".to_string())
        );
    }

    // -- Multi-provider scenario --------------------------------------------

    #[test]
    fn multi_provider_registration_and_resolution() {
        let mut registry = Registry::new();

        // Three providers with different protocols.
        registry
            .register_provider(provider("openai", Protocol::OpenAiCompatible))
            .unwrap();
        registry
            .register_provider(provider("anthropic", Protocol::Anthropic))
            .unwrap();
        registry
            .register_provider(provider("google", Protocol::Google))
            .unwrap();

        // Models across providers.
        registry
            .register_model(model("gpt4o", "openai", "gpt-4o", &["4o"]))
            .unwrap();
        registry
            .register_model(model(
                "sonnet",
                "anthropic",
                "claude-sonnet-4-20250514",
                &["claude"],
            ))
            .unwrap();
        registry
            .register_model(model(
                "gemini-flash",
                "google",
                "gemini-2.0-flash",
                &["flash"],
            ))
            .unwrap();

        // Resolve each by alias.
        let r1 = registry.resolve("4o").unwrap();
        assert_eq!(r1.provider.protocol, Protocol::OpenAiCompatible);
        assert_eq!(r1.model.model_id, "gpt-4o");

        let r2 = registry.resolve("claude").unwrap();
        assert_eq!(r2.provider.protocol, Protocol::Anthropic);
        assert_eq!(r2.model.model_id, "claude-sonnet-4-20250514");

        let r3 = registry.resolve("flash").unwrap();
        assert_eq!(r3.provider.protocol, Protocol::Google);
        assert_eq!(r3.model.model_id, "gemini-2.0-flash");
    }

    // -- Model with context_window ------------------------------------------

    #[test]
    fn resolved_model_preserves_context_window() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("openai", Protocol::OpenAiCompatible))
            .unwrap();

        let mut m = model("gpt4o", "openai", "gpt-4o", &[]);
        m.context_window = Some(128_000);
        registry.register_model(m).unwrap();

        let resolved = registry.resolve("gpt4o").unwrap();
        assert_eq!(resolved.model.context_window, Some(128_000));
    }

    // -- Model with no aliases resolves only by id or explicit format -------

    #[test]
    fn model_without_aliases_resolves_by_id_and_explicit_format() {
        let mut registry = Registry::new();
        registry
            .register_provider(provider("openai", Protocol::OpenAiCompatible))
            .unwrap();
        registry
            .register_model(model("gpt4", "openai", "gpt-4", &[]))
            .unwrap();

        // By id.
        assert!(registry.resolve("gpt4").is_ok());
        // By explicit format.
        assert!(registry.resolve("openai/gpt-4").is_ok());
        // By non-existent alias.
        assert!(registry.resolve("smart").is_err());
    }
}
