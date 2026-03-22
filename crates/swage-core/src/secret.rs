//! Secret retrieval abstraction.
//!
//! Defines the `SecretProvider` trait for retrieving sensitive configuration
//! values such as API keys, tokens, and credentials. Implementations abstract
//! the source of secrets (environment variables, key vaults, configuration
//! files) from application code.
//!
//! # Provider Responsibilities
//!
//! - **Retrieval**: Fetch secret values by key identifier
//! - **Error handling**: Distinguish between missing secrets and access failures
//! - **Security**: Avoid logging or exposing secret values in error messages
//!
//! # Examples
//!
//! ```rust,ignore
//! use swage_core::secret::{SecretProvider, EnvSecretProvider};
//!
//! async fn get_api_key<S: SecretProvider>(provider: &S) -> Result<String, SecretError> {
//!     provider.get_secret("OPENAI_API_KEY").await
//! }
//!
//! // Using environment variable provider
//! let provider = EnvSecretProvider::new();
//! let key = provider.get_secret("MY_SECRET").await?;
//! ```

use async_trait::async_trait;

use crate::error::SecretError;

/// Async trait for secret provider implementations.
///
/// Abstracts retrieval of sensitive configuration values from various sources.
/// Implementations must be `Send + Sync` to support concurrent access across
/// async tasks.
#[async_trait]
pub trait SecretProvider: Send + Sync {
    /// Retrieves the value of a secret by key.
    ///
    /// # Arguments
    ///
    /// * `key` - The identifier for the secret (e.g., environment variable
    ///   name, key vault path)
    ///
    /// # Errors
    ///
    /// Returns `SecretError` if:
    /// - The secret does not exist (`SecretError::NotFound`)
    /// - Access to the secret is denied (`SecretError::AccessDenied`)
    async fn get_secret(&self, key: &str) -> Result<String, SecretError>;
}

/// Secret provider that reads from environment variables.
///
/// Retrieves secrets using `std::env::var`. This is the simplest provider
/// implementation, suitable for development and deployment environments where
/// configuration is injected via environment variables.
///
/// # Security Considerations
///
/// Environment variables may be visible to other processes on the system.
/// For production deployments requiring higher security, consider using
/// a dedicated secret management service.
#[derive(Debug, Clone, Default)]
pub struct EnvSecretProvider;

impl EnvSecretProvider {
    /// Creates a new environment variable secret provider.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SecretProvider for EnvSecretProvider {
    async fn get_secret(&self, key: &str) -> Result<String, SecretError> {
        use std::env::VarError;

        // Read from environment variable.
        // This is synchronous but wrapped in async for trait consistency.
        std::env::var(key).map_err(|e| match e {
            VarError::NotPresent => SecretError::NotFound {
                key: key.to_string(),
            },
            VarError::NotUnicode(_) => SecretError::InvalidEncoding {
                key: key.to_string(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn env_provider_retrieves_existing_variable() {
        // SAFETY: Test runs sequentially; no other code reads this variable concurrently.
        unsafe { std::env::set_var("SWAGE_TEST_SECRET_EXISTS", "secret_value") };

        let provider = EnvSecretProvider::new();
        let result = provider.get_secret("SWAGE_TEST_SECRET_EXISTS").await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "secret_value");

        unsafe { std::env::remove_var("SWAGE_TEST_SECRET_EXISTS") };
    }

    #[tokio::test]
    async fn env_provider_returns_not_found_for_missing_variable() {
        // SAFETY: Test runs sequentially; no other code reads this variable concurrently.
        unsafe { std::env::remove_var("SWAGE_TEST_SECRET_MISSING") };

        let provider = EnvSecretProvider::new();
        let result = provider.get_secret("SWAGE_TEST_SECRET_MISSING").await;

        assert!(result.is_err());
        match result.unwrap_err() {
            SecretError::NotFound { key } => assert_eq!(key, "SWAGE_TEST_SECRET_MISSING"),
            _ => panic!("Expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn env_provider_handles_empty_value() {
        // SAFETY: Test runs sequentially; no other code reads this variable concurrently.
        unsafe { std::env::set_var("SWAGE_TEST_SECRET_EMPTY", "") };

        let provider = EnvSecretProvider::new();
        let result = provider.get_secret("SWAGE_TEST_SECRET_EMPTY").await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "");

        unsafe { std::env::remove_var("SWAGE_TEST_SECRET_EMPTY") };
    }
}
