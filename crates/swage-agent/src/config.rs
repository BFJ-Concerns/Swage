//! Agent configuration types.

use std::collections::HashMap;
use std::path::PathBuf;

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
    /// JSON Schema to validate agent output against. When present, the agent
    /// validates its final response and retries on schema violations.
    pub output_schema: Option<Value>,
    /// Maximum number of validation retry attempts when output doesn't match
    /// the schema. 0 means no retries — first failure is final.
    pub max_output_retries: u32,
    /// Whether `working_directory` came from a caller-resolved execution scope.
    pub has_resolved_workspace: bool,
    /// Working directory exposed to tools executed within this agent.
    pub working_directory: PathBuf,
    /// Gateway-governed allowed path roots for tool execution.
    pub allowed_paths: Vec<PathBuf>,
    /// Optional generation controls loaded from the selected global preset.
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub stop_sequences: Option<Vec<String>>,
    pub generation_metadata: Option<Value>,
    /// When true, the turn loop pre-approves every gate so tool, location,
    /// and hook approval prompts are bypassed. Mirrors the gateway-level
    /// `dangerously_skip_permissions` setting; intended for callers that
    /// have explicitly opted out of the approval system.
    #[serde(default)]
    pub skip_permissions: bool,
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
            output_schema: None,
            max_output_retries: 3,
            has_resolved_workspace: false,
            working_directory: PathBuf::new(),
            allowed_paths: Vec::new(),
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            generation_metadata: None,
            skip_permissions: false,
        }
    }

    /// Set the output schema and maximum retry count.
    ///
    /// When an output schema is configured, the agent validates its final
    /// response text against it and retries up to `max_retries` times on
    /// schema violations.
    pub fn with_output_schema(mut self, schema: Value, max_retries: u32) -> Self {
        self.output_schema = Some(schema);
        self.max_output_retries = max_retries;
        self
    }

    pub fn with_workspace(
        mut self,
        working_directory: impl Into<PathBuf>,
        allowed_paths: Vec<PathBuf>,
    ) -> Self {
        self.working_directory = working_directory.into();
        self.allowed_paths = allowed_paths;
        self.has_resolved_workspace = true;
        self
    }

    /// Validate the configuration.
    ///
    /// Returns `Err(AgentError::InvalidConfig(...))` if:
    /// - `agent_id` is empty
    /// - `max_tool_rounds` is zero
    /// - `output_schema` is present but not a valid JSON Schema
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
        if !self.has_resolved_workspace {
            return Err(crate::AgentError::InvalidConfig(
                "working_directory must be resolved with AgentConfig::with_workspace".into(),
            ));
        }
        if !self.working_directory.is_absolute() {
            return Err(crate::AgentError::InvalidConfig(
                "working_directory must be absolute".into(),
            ));
        }
        // Compile the output schema eagerly to catch malformed schemas at
        // construction time rather than at the first validation attempt.
        if let Some(schema) = &self.output_schema {
            jsonschema::validator_for(schema).map_err(|e| {
                crate::AgentError::InvalidConfig(format!("output schema compilation failed: {e}"))
            })?;
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
        assert_eq!(
            config.compaction_threshold,
            CompactionThreshold::MessageCount(100)
        );
        assert!(config.metadata.is_empty());
        assert!(!config.has_resolved_workspace);
        assert_eq!(config.working_directory, PathBuf::new());
        assert!(config.allowed_paths.is_empty());
    }

    #[test]
    fn validate_accepts_valid_config() {
        let config = AgentConfig::new("valid-agent").with_workspace("/tmp/swage-test", vec![]);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_agent_id() {
        let config = AgentConfig::new("");
        let err = config.validate().unwrap_err();
        assert!(matches!(err, crate::AgentError::InvalidConfig(_)));
    }

    #[test]
    fn validate_rejects_unresolved_workspace() {
        let config = AgentConfig::new("agent");
        let err = config.validate().unwrap_err();
        assert!(matches!(err, crate::AgentError::InvalidConfig(_)));
        assert!(err.to_string().contains("working_directory"));
    }

    #[test]
    fn validate_rejects_relative_workspace() {
        let config = AgentConfig::new("agent").with_workspace("relative", vec![]);
        let err = config.validate().unwrap_err();
        assert!(matches!(err, crate::AgentError::InvalidConfig(_)));
        assert!(err.to_string().contains("absolute"));
    }

    #[test]
    fn validate_rejects_zero_max_tool_rounds() {
        let mut config = AgentConfig::new("agent");
        config.max_tool_rounds = 0;
        let err = config.validate().unwrap_err();
        assert!(matches!(err, crate::AgentError::InvalidConfig(_)));
    }

    #[test]
    fn default_has_no_output_schema() {
        let config = AgentConfig::new("test");
        assert!(config.output_schema.is_none());
        assert_eq!(config.max_output_retries, 3);
    }

    #[test]
    fn with_output_schema_sets_both_fields() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        let config = AgentConfig::new("test").with_output_schema(schema.clone(), 2);
        assert_eq!(config.output_schema, Some(schema));
        assert_eq!(config.max_output_retries, 2);
    }

    #[test]
    fn validate_accepts_valid_output_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "result": { "type": "string" },
                "score": { "type": "number" }
            },
            "required": ["result"]
        });
        let config = AgentConfig::new("test")
            .with_workspace("/tmp/swage-test", vec![])
            .with_output_schema(schema, 2);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_malformed_output_schema() {
        // A schema with an invalid type keyword should fail compilation.
        let schema = serde_json::json!({
            "type": "not-a-real-type"
        });
        let config = AgentConfig::new("test")
            .with_workspace("/tmp/swage-test", vec![])
            .with_output_schema(schema, 1);
        let err = config.validate().unwrap_err();
        match &err {
            crate::AgentError::InvalidConfig(msg) => {
                assert!(
                    msg.contains("output schema"),
                    "error should mention output schema, got: {msg}"
                );
            }
            other => panic!("expected InvalidConfig, got: {other:?}"),
        }
    }

    #[test]
    fn validate_accepts_config_without_output_schema() {
        let config = AgentConfig::new("test").with_workspace("/tmp/swage-test", vec![]);
        assert!(config.validate().is_ok());
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
        assert_eq!(
            CompactionThreshold::TokenCount(1_000),
            CompactionThreshold::TokenCount(1_000)
        );
        assert_eq!(
            CompactionThreshold::MessageCount(50),
            CompactionThreshold::MessageCount(50)
        );
    }

    #[test]
    fn equality_same_variant_different_value() {
        assert_ne!(
            CompactionThreshold::TokenCount(1_000),
            CompactionThreshold::TokenCount(999)
        );
        assert_ne!(
            CompactionThreshold::MessageCount(50),
            CompactionThreshold::MessageCount(51)
        );
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
