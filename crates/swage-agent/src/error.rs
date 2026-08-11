//! Agent error types.

use swage_core::{ProviderError, StorageError, ToolError};
use thiserror::Error;

/// Errors produced by the agent runtime.
#[derive(Debug, Error)]
pub enum AgentError {
    #[error("LLM provider failed: {0}")]
    ProviderFailed(#[from] ProviderError),

    #[error("storage operation failed: {0}")]
    StorageFailed(#[from] StorageError),

    #[error("tool dispatch failed: {0}")]
    ToolFailed(#[from] ToolError),

    #[error("hook '{hook}' blocked: {reason}")]
    HookBlocked { hook: String, reason: String },

    #[error("compaction failed: {0}")]
    CompactionFailed(String),

    #[error("session not found: {0}")]
    SessionNotFound(uuid::Uuid),

    #[error("access denied to session {session_id} for agent {agent_id}")]
    SessionAccessDenied {
        session_id: uuid::Uuid,
        agent_id: String,
    },

    #[error("thread not found: {0}")]
    ThreadNotFound(uuid::Uuid),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("context exceeds capacity even after compaction")]
    ContextTooLong,

    /// Output schema validation failed after all retry attempts were exhausted.
    #[error("output schema validation failed after {attempts} attempt(s)")]
    OutputSchemaFailed { attempts: u32 },

    /// The turn was cancelled by the caller via the cancellation token.
    #[error("turn cancelled")]
    Cancelled,
}

impl AgentError {
    /// Returns true if the error represents a transient failure that may
    /// succeed on retry.
    ///
    /// Delegates to the inner error's `is_retriable()` for wrapped types
    /// (ProviderError, StorageError, ToolError). All other variants represent
    /// permanent failures — hook rejections, configuration errors, resource
    /// limits, and access control are not transient.
    pub fn is_retriable(&self) -> bool {
        match self {
            AgentError::ProviderFailed(e) => e.is_retriable(),
            AgentError::StorageFailed(e) => e.is_retriable(),
            AgentError::ToolFailed(e) => e.is_retriable(),
            AgentError::HookBlocked { .. } => false,
            AgentError::CompactionFailed(_) => false,
            AgentError::SessionNotFound(_) => false,
            AgentError::SessionAccessDenied { .. } => false,
            AgentError::ThreadNotFound(_) => false,
            AgentError::InvalidConfig(_) => false,
            AgentError::ContextTooLong => false,
            // Schema validation exhausted all retries — not transient.
            AgentError::OutputSchemaFailed { .. } => false,
            // Cancellation is not a transient failure — the caller chose to stop.
            AgentError::Cancelled => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    // -----------------------------------------------------------------------
    // is_retriable — delegated variants
    // -----------------------------------------------------------------------

    #[test]
    fn provider_failed_delegates_retriable() {
        // RateLimited is transient in ProviderError
        let err = AgentError::ProviderFailed(ProviderError::RateLimited {
            provider: "openai".into(),
            retry_after: None,
        });
        assert!(err.is_retriable());
    }

    #[test]
    fn provider_failed_delegates_non_retriable() {
        // Auth failure is permanent in ProviderError
        let err = AgentError::ProviderFailed(ProviderError::Auth {
            provider: "openai".into(),
        });
        assert!(!err.is_retriable());
    }

    #[test]
    fn storage_failed_delegates_retriable() {
        // ConnectionFailed is transient in StorageError
        let err = AgentError::StorageFailed(StorageError::ConnectionFailed(
            "database unreachable".into(),
        ));
        assert!(err.is_retriable());
    }

    #[test]
    fn storage_capacity_exceeded_is_retriable() {
        let err = AgentError::StorageFailed(StorageError::CapacityExceeded {
            message: "WAL too large".into(),
        });
        assert!(err.is_retriable());
    }

    #[test]
    fn storage_failed_delegates_non_retriable() {
        // Corruption is permanent in StorageError
        let err = AgentError::StorageFailed(StorageError::Corruption {
            message: "checksum mismatch".into(),
        });
        assert!(!err.is_retriable());
    }

    #[test]
    fn tool_failed_delegates_retriable() {
        // Timeout is transient in ToolError
        let err = AgentError::ToolFailed(ToolError::Timeout {
            name: "web_search".into(),
            duration: std::time::Duration::from_secs(30),
        });
        assert!(err.is_retriable());
    }

    #[test]
    fn tool_failed_delegates_non_retriable() {
        let err = AgentError::ToolFailed(ToolError::NotFound {
            name: "nonexistent".into(),
        });
        assert!(!err.is_retriable());
    }

    // -----------------------------------------------------------------------
    // is_retriable — permanently non-retriable variants
    // -----------------------------------------------------------------------

    #[test]
    fn hook_blocked_not_retriable() {
        let err = AgentError::HookBlocked {
            hook: "safety_check".into(),
            reason: "content policy violation".into(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn compaction_failed_not_retriable() {
        let err = AgentError::CompactionFailed("all tiers exhausted".into());
        assert!(!err.is_retriable());
    }

    #[test]
    fn session_not_found_not_retriable() {
        let err = AgentError::SessionNotFound(Uuid::nil());
        assert!(!err.is_retriable());
    }

    #[test]
    fn session_access_denied_not_retriable() {
        let err = AgentError::SessionAccessDenied {
            session_id: Uuid::nil(),
            agent_id: "agent-1".into(),
        };
        assert!(!err.is_retriable());
    }

    #[test]
    fn thread_not_found_not_retriable() {
        let err = AgentError::ThreadNotFound(Uuid::nil());
        assert!(!err.is_retriable());
    }

    #[test]
    fn invalid_config_not_retriable() {
        let err = AgentError::InvalidConfig("missing model".into());
        assert!(!err.is_retriable());
    }

    #[test]
    fn context_too_long_not_retriable() {
        let err = AgentError::ContextTooLong;
        assert!(!err.is_retriable());
    }

    // -----------------------------------------------------------------------
    // Display formatting
    // -----------------------------------------------------------------------

    #[test]
    fn display_provider_failed() {
        let err = AgentError::ProviderFailed(ProviderError::Auth {
            provider: "anthropic".into(),
        });
        assert_eq!(
            err.to_string(),
            "LLM provider failed: authentication failed for provider anthropic"
        );
    }

    #[test]
    fn display_storage_failed() {
        let err = AgentError::StorageFailed(StorageError::ConnectionFailed("timeout".into()));
        assert_eq!(
            err.to_string(),
            "storage operation failed: connection failed: timeout"
        );
    }

    #[test]
    fn display_tool_failed() {
        let err = AgentError::ToolFailed(ToolError::NotFound {
            name: "calculator".into(),
        });
        assert_eq!(
            err.to_string(),
            "tool dispatch failed: tool not found: calculator"
        );
    }

    #[test]
    fn display_hook_blocked() {
        let err = AgentError::HookBlocked {
            hook: "safety".into(),
            reason: "blocked by policy".into(),
        };
        assert_eq!(err.to_string(), "hook 'safety' blocked: blocked by policy");
    }

    #[test]
    fn display_compaction_failed() {
        let err = AgentError::CompactionFailed("tier 3 failed".into());
        assert_eq!(err.to_string(), "compaction failed: tier 3 failed");
    }

    #[test]
    fn display_session_not_found() {
        let id = Uuid::nil();
        let err = AgentError::SessionNotFound(id);
        assert_eq!(err.to_string(), format!("session not found: {id}"));
    }

    #[test]
    fn display_session_access_denied() {
        let id = Uuid::nil();
        let err = AgentError::SessionAccessDenied {
            session_id: id,
            agent_id: "bot-x".into(),
        };
        assert_eq!(
            err.to_string(),
            format!("access denied to session {id} for agent bot-x")
        );
    }

    #[test]
    fn display_thread_not_found() {
        let id = Uuid::nil();
        let err = AgentError::ThreadNotFound(id);
        assert_eq!(err.to_string(), format!("thread not found: {id}"));
    }

    #[test]
    fn display_invalid_config() {
        let err = AgentError::InvalidConfig("model required".into());
        assert_eq!(err.to_string(), "invalid configuration: model required");
    }

    #[test]
    fn display_context_too_long() {
        let err = AgentError::ContextTooLong;
        assert_eq!(
            err.to_string(),
            "context exceeds capacity even after compaction"
        );
    }

    #[test]
    fn output_schema_failed_not_retriable() {
        let err = AgentError::OutputSchemaFailed { attempts: 3 };
        assert!(!err.is_retriable());
    }

    #[test]
    fn display_output_schema_failed() {
        let err = AgentError::OutputSchemaFailed { attempts: 3 };
        assert_eq!(
            err.to_string(),
            "output schema validation failed after 3 attempt(s)"
        );
    }
}
