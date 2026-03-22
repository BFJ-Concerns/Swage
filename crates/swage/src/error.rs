//! Error types for the Swage framework.
//!
//! Distinguishes between transient (retriable) and permanent failures to enable
//! intelligent retry logic in agent orchestration.

use thiserror::Error;

// RuntimeError and SecretError migrated to swage-core — re-export for compatibility
pub use swage_core::error::{RuntimeError, SecretError};

/// Top-level error type for the Swage framework.
#[derive(Debug, Error)]
pub enum SwageError {
    /// Error from an LLM provider.
    #[error("provider error: {0}")]
    Provider(#[from] ProviderError),

    /// Error from storage operations.
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    /// Error from tool execution.
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),

    /// Error from runtime operations.
    #[error("runtime error: {0}")]
    Runtime(#[from] RuntimeError),

    /// Error from secret retrieval.
    #[error("secret error: {0}")]
    Secret(#[from] SecretError),
}

impl SwageError {
    /// Returns true if the error represents a transient failure that may succeed on retry.
    pub fn is_retriable(&self) -> bool {
        match self {
            SwageError::Provider(e) => e.is_retriable(),
            SwageError::Storage(e) => e.is_retriable(),
            SwageError::Tool(e) => e.is_retriable(),
            SwageError::Runtime(e) => e.is_retriable(),
            SwageError::Secret(e) => e.is_retriable(),
        }
    }
}

/// Errors that can occur when interacting with LLM providers.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// Rate limit exceeded (transient).
    #[error("rate limit exceeded: {0}")]
    RateLimit(String),

    /// Request timeout (transient).
    #[error("request timeout: {0}")]
    Timeout(String),

    /// Network error (transient).
    #[error("network error: {0}")]
    Network(String),

    /// Authentication failure (permanent).
    #[error("authentication failed: {0}")]
    Auth(String),

    /// Invalid configuration (permanent).
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// Invalid request format or parameters (permanent).
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Model not found or unavailable (permanent).
    #[error("model error: {0}")]
    ModelError(String),
}

impl ProviderError {
    /// Returns true if the error represents a transient failure that may succeed on retry.
    pub fn is_retriable(&self) -> bool {
        match self {
            // Transient errors
            ProviderError::RateLimit(_) | ProviderError::Timeout(_) | ProviderError::Network(_) => {
                true
            }
            // Permanent errors
            ProviderError::Auth(_)
            | ProviderError::InvalidConfig(_)
            | ProviderError::InvalidRequest(_)
            | ProviderError::ModelError(_) => false,
        }
    }
}

/// Errors that can occur during storage operations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// I/O error (may be transient depending on cause).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialisation or deserialisation error (permanent).
    #[error("serialisation error: {0}")]
    Serialisation(String),

    /// Item not found (permanent).
    #[error("not found: {0}")]
    NotFound(String),

    /// Storage corruption detected (permanent).
    #[error("corruption detected: {0}")]
    Corruption(String),

    /// Storage capacity exceeded (potentially transient if temporary).
    #[error("capacity exceeded: {0}")]
    CapacityExceeded(String),
}

impl StorageError {
    /// Returns true if the error represents a transient failure that may succeed on retry.
    pub fn is_retriable(&self) -> bool {
        match self {
            // I/O errors can be transient (e.g., temporary lock contention)
            // but we conservatively treat them as non-retriable by default
            StorageError::Io(_) => false,
            // Serialisation errors are permanent
            StorageError::Serialisation(_) => false,
            // Not found is permanent
            StorageError::NotFound(_) => false,
            // Corruption is permanent
            StorageError::Corruption(_) => false,
            // Capacity might be temporary if space is being freed
            StorageError::CapacityExceeded(_) => true,
        }
    }
}

/// Errors that can occur during tool execution.
#[derive(Debug, Error)]
pub enum ToolError {
    /// Tool not found (permanent).
    #[error("tool not found: {0}")]
    NotFound(String),

    /// Tool execution failed (potentially transient).
    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    /// Invalid tool parameters (permanent).
    #[error("invalid parameters: {0}")]
    InvalidParameters(String),

    /// Tool timeout (transient).
    #[error("tool timeout: {0}")]
    Timeout(String),

    /// Missing required capability or permission (permanent).
    #[error("permission denied: {0}")]
    PermissionDenied(String),
}

impl ToolError {
    /// Returns true if the error represents a transient failure that may succeed on retry.
    pub fn is_retriable(&self) -> bool {
        match self {
            // Not found is permanent
            ToolError::NotFound(_) => false,
            // Execution failures might be transient (e.g., external service unavailable)
            ToolError::ExecutionFailed(_) => true,
            // Invalid parameters are permanent
            ToolError::InvalidParameters(_) => false,
            // Timeouts are transient
            ToolError::Timeout(_) => true,
            // Permission issues are permanent
            ToolError::PermissionDenied(_) => false,
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_retriability() {
        // Transient errors
        assert!(ProviderError::RateLimit("test".into()).is_retriable());
        assert!(ProviderError::Timeout("test".into()).is_retriable());
        assert!(ProviderError::Network("test".into()).is_retriable());

        // Permanent errors
        assert!(!ProviderError::Auth("test".into()).is_retriable());
        assert!(!ProviderError::InvalidConfig("test".into()).is_retriable());
        assert!(!ProviderError::InvalidRequest("test".into()).is_retriable());
        assert!(!ProviderError::ModelError("test".into()).is_retriable());
    }

    #[test]
    fn storage_error_retriability() {
        // I/O errors are conservatively treated as non-retriable
        assert!(!StorageError::Io(std::io::Error::new(std::io::ErrorKind::Other, "test")).is_retriable());
        assert!(StorageError::CapacityExceeded("test".into()).is_retriable());
        assert!(!StorageError::Serialisation("test".into()).is_retriable());
        assert!(!StorageError::NotFound("test".into()).is_retriable());
        assert!(!StorageError::Corruption("test".into()).is_retriable());
    }

    #[test]
    fn tool_error_retriability() {
        // Transient errors
        assert!(ToolError::ExecutionFailed("test".into()).is_retriable());
        assert!(ToolError::Timeout("test".into()).is_retriable());

        // Permanent errors
        assert!(!ToolError::NotFound("test".into()).is_retriable());
        assert!(!ToolError::InvalidParameters("test".into()).is_retriable());
        assert!(!ToolError::PermissionDenied("test".into()).is_retriable());
    }

    #[test]
    fn secret_error_retriability() {
        assert!(!SecretError::NotFound {
            key: "test".into()
        }
        .is_retriable());
        assert!(!SecretError::AccessDenied {
            key: "test".into()
        }
        .is_retriable());
        assert!(!SecretError::InvalidEncoding {
            key: "test".into()
        }
        .is_retriable());
    }

    #[test]
    fn swage_error_propagates_retriability() {
        let provider_err = SwageError::Provider(ProviderError::RateLimit("test".into()));
        assert!(provider_err.is_retriable());

        let tool_err = SwageError::Tool(ToolError::Timeout("test".into()));
        assert!(tool_err.is_retriable());

        let storage_err = SwageError::Storage(StorageError::NotFound("test".into()));
        assert!(!storage_err.is_retriable());

        let secret_err = SwageError::Secret(SecretError::NotFound {
            key: "test".into(),
        });
        assert!(!secret_err.is_retriable());

        let runtime_err = SwageError::Runtime(RuntimeError::NestedRuntime);
        assert!(!runtime_err.is_retriable());

        let runtime_creation_err = SwageError::Runtime(RuntimeError::CreationFailed("test".into()));
        assert!(!runtime_creation_err.is_retriable());
    }
}
