//! Agent configuration types.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Configuration for an agent instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Unique identifier for this agent — propagated to all storage records.
    pub agent_id: String,
    /// Maximum tool call rounds per turn before the agent fails the turn.
    pub max_tool_rounds: u32,
    /// When to trigger context compaction.
    pub compaction_threshold: CompactionThreshold,
    /// Arbitrary agent-level metadata.
    pub metadata: HashMap<String, Value>,
}

impl AgentConfig {
    /// Create a new config with sensible defaults.
    ///
    /// Defaults: `max_tool_rounds = 10`, `compaction_threshold = MessageCount(100)`.
    /// Call [`validate`][Self::validate] before using the config with an agent.
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            max_tool_rounds: 10,
            compaction_threshold: CompactionThreshold::MessageCount(100),
            metadata: HashMap::new(),
        }
    }

    /// Validate the configuration.
    ///
    /// Returns `Err(AgentError::InvalidConfig(...))` if:
    /// - `agent_id` is empty
    /// - `max_tool_rounds` is zero
    pub fn validate(&self) -> Result<(), crate::AgentError> {
        if self.agent_id.is_empty() {
            return Err(crate::AgentError::InvalidConfig(
                "agent_id must not be empty".into(),
            ));
        }
        if self.max_tool_rounds == 0 {
            return Err(crate::AgentError::InvalidConfig(
                "max_tool_rounds must be at least 1".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_defaults() {
        let config = AgentConfig::new("test-agent");
        assert_eq!(config.agent_id, "test-agent");
        assert_eq!(config.max_tool_rounds, 10);
        assert_eq!(config.compaction_threshold, CompactionThreshold::MessageCount(100));
        assert!(config.metadata.is_empty());
    }

    #[test]
    fn validate_accepts_valid_config() {
        let config = AgentConfig::new("valid-agent");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_agent_id() {
        let config = AgentConfig::new("");
        let err = config.validate().unwrap_err();
        assert!(matches!(err, crate::AgentError::InvalidConfig(_)));
    }

    #[test]
    fn validate_rejects_zero_max_tool_rounds() {
        let mut config = AgentConfig::new("agent");
        config.max_tool_rounds = 0;
        let err = config.validate().unwrap_err();
        assert!(matches!(err, crate::AgentError::InvalidConfig(_)));
    }
}

/// Threshold that triggers context compaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CompactionThreshold {
    /// Compact when estimated token count exceeds this value.
    TokenCount(u64),
    /// Compact when message count in the active thread exceeds this value.
    MessageCount(u32),
}

#[cfg(test)]
mod compaction_threshold_tests {
    use super::*;

    #[test]
    fn equality_same_variant_same_value() {
        assert_eq!(CompactionThreshold::TokenCount(1_000), CompactionThreshold::TokenCount(1_000));
        assert_eq!(CompactionThreshold::MessageCount(50), CompactionThreshold::MessageCount(50));
    }

    #[test]
    fn equality_same_variant_different_value() {
        assert_ne!(CompactionThreshold::TokenCount(1_000), CompactionThreshold::TokenCount(999));
        assert_ne!(CompactionThreshold::MessageCount(50), CompactionThreshold::MessageCount(51));
    }

    #[test]
    fn equality_different_variants() {
        // u64 and u32 can share the same numeric value — they are still different thresholds.
        assert_ne!(
            CompactionThreshold::TokenCount(100),
            CompactionThreshold::MessageCount(100),
        );
    }

    #[test]
    fn clone_produces_equal_value() {
        let token = CompactionThreshold::TokenCount(5_000);
        assert_eq!(token.clone(), token);

        let message = CompactionThreshold::MessageCount(200);
        assert_eq!(message.clone(), message);
    }

    #[test]
    fn serialisation_round_trip_token_count() {
        let original = CompactionThreshold::TokenCount(10_000);
        let json = serde_json::to_string(&original).expect("serialise TokenCount");
        let decoded: CompactionThreshold =
            serde_json::from_str(&json).expect("deserialise TokenCount");
        assert_eq!(original, decoded);
    }

    #[test]
    fn serialisation_round_trip_message_count() {
        let original = CompactionThreshold::MessageCount(250);
        let json = serde_json::to_string(&original).expect("serialise MessageCount");
        let decoded: CompactionThreshold =
            serde_json::from_str(&json).expect("deserialise MessageCount");
        assert_eq!(original, decoded);
    }
}
