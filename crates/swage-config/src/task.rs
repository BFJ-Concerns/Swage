//! Task definitions — reusable work packages for multi-agent orchestration.
//!
//! A task definition specifies: instructions, output schema, model override,
//! and tool permissions. When bound to an agent via a topology node, the task
//! overlays the agent's base config using the existing cascade semantics.

use std::collections::HashMap;

use crate::merge::{PartialConfig, PartialOutputConfig, PartialProviderConfig, PartialToolConfig};
use crate::model::{RequireApprovalEntry, ResolvedConfig};
use serde::{Deserialize, Serialize};

/// A reusable work package that overlays an agent's config at execution time.
///
/// Declarable in TOML config or constructed programmatically in Python topology
/// definitions. The topology engine applies the task overlay to the agent's
/// resolved config via [`TaskDefinition::apply_to`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskDefinition {
    /// Unique identifier for this task definition.
    pub name: String,

    /// Human-readable description of what the task does.
    #[serde(default)]
    pub description: String,

    /// Task-specific instructions prepended to the agent's context.
    /// Empty string means no additional instructions.
    #[serde(default)]
    pub instructions: String,

    /// Model override — when set, the agent uses this model instead of its
    /// configured default.
    #[serde(default)]
    pub model: Option<String>,

    /// Output schema override — when set, the agent validates its response
    /// against this schema.
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,

    /// Maximum output schema validation retries.
    #[serde(default)]
    pub max_output_retries: Option<u32>,

    /// Tool permission overrides. When set, these replace the agent's base
    /// permissions entirely (replacement semantics, not merge).
    #[serde(default)]
    pub tool_allow: Option<Vec<String>>,
    #[serde(default)]
    pub tool_deny: Option<Vec<String>>,
    #[serde(default)]
    pub tool_bundles: Option<HashMap<String, Vec<String>>>,
    #[serde(default)]
    pub tool_require_approval: Option<Vec<RequireApprovalEntry>>,
}

impl TaskDefinition {
    /// Create a minimal task definition with just a name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Convert this task definition into a `PartialConfig` suitable for
    /// overlay merging via [`crate::resolve::apply_overlay`].
    ///
    /// Only fields that are explicitly set produce `Some` values in the
    /// partial — unset fields are `None` and don't override the base.
    pub fn to_partial_config(&self) -> PartialConfig {
        PartialConfig {
            provider: if self.model.is_some() {
                Some(PartialProviderConfig {
                    model: self.model.clone(),
                    ..Default::default()
                })
            } else {
                None
            },
            output: if self.output_schema.is_some() || self.max_output_retries.is_some() {
                Some(PartialOutputConfig {
                    schema: self.output_schema.clone(),
                    max_retries: self.max_output_retries,
                })
            } else {
                None
            },
            tools: if self.tool_allow.is_some()
                || self.tool_deny.is_some()
                || self.tool_bundles.is_some()
                || self.tool_require_approval.is_some()
            {
                Some(PartialToolConfig {
                    allow: self.tool_allow.clone(),
                    deny: self.tool_deny.clone(),
                    bundles: self.tool_bundles.clone(),
                    require_approval: self.tool_require_approval.clone(),
                    ..Default::default()
                })
            } else {
                None
            },
            ..Default::default()
        }
    }

    /// Apply this task definition to an agent's resolved config.
    ///
    /// Task instructions are appended after the base instructions because they
    /// are the most specific behavioural layer. Other task fields reuse the
    /// normal overlay merge semantics via [`crate::resolve::apply_overlay`].
    pub fn apply_to(&self, base: ResolvedConfig) -> ResolvedConfig {
        let base_instructions = base.instructions.clone();
        let mut resolved = crate::resolve::apply_overlay(base, self.to_partial_config());

        // Task tool bundles are an explicit tool-set surface, so a task that
        // defines bundles replaces inherited bundles rather than merging them.
        if let Some(bundles) = &self.tool_bundles {
            resolved.tools.permissions.bundles = bundles.clone();
        }

        resolved.instructions = match (
            base_instructions.trim().is_empty(),
            self.instructions.trim().is_empty(),
        ) {
            (true, true) => String::new(),
            (false, true) => base_instructions,
            (true, false) => self.rendered_instructions(),
            (false, false) => format!("{base_instructions}\n\n{}", self.rendered_instructions(),),
        };

        resolved
    }

    fn rendered_instructions(&self) -> String {
        if self.name.trim().is_empty() {
            self.instructions.clone()
        } else {
            format!("<!-- task: {} -->\n{}", self.name, self.instructions)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_definition_new_has_defaults() {
        let task = TaskDefinition::new("review");
        assert_eq!(task.name, "review");
        assert!(task.instructions.is_empty());
        assert!(task.model.is_none());
        assert!(task.output_schema.is_none());
        assert!(task.tool_allow.is_none());
        assert!(task.tool_bundles.is_none());
        assert!(task.tool_require_approval.is_none());
    }

    #[test]
    fn to_partial_config_with_model_override() {
        let task = TaskDefinition {
            name: "fast-task".into(),
            model: Some("claude-haiku".into()),
            ..Default::default()
        };
        let partial = task.to_partial_config();
        assert!(partial.provider.is_some());
        assert_eq!(
            partial.provider.unwrap().model.as_deref(),
            Some("claude-haiku")
        );
        // Other sections should be None.
        assert!(partial.output.is_none());
        assert!(partial.tools.is_none());
    }

    #[test]
    fn to_partial_config_with_output_schema() {
        let task = TaskDefinition {
            name: "typed-task".into(),
            output_schema: Some(serde_json::json!({"type": "object"})),
            max_output_retries: Some(5),
            ..Default::default()
        };
        let partial = task.to_partial_config();
        assert!(partial.output.is_some());
        let output = partial.output.unwrap();
        assert!(output.schema.is_some());
        assert_eq!(output.max_retries, Some(5));
    }

    #[test]
    fn to_partial_config_with_tool_permissions() {
        let task = TaskDefinition {
            name: "restricted-task".into(),
            tool_allow: Some(vec!["file_read".into()]),
            tool_deny: Some(vec!["shell".into()]),
            ..Default::default()
        };
        let partial = task.to_partial_config();
        assert!(partial.tools.is_some());
        let tools = partial.tools.unwrap();
        assert_eq!(tools.allow, Some(vec!["file_read".to_string()]));
        assert_eq!(tools.deny, Some(vec!["shell".to_string()]));
    }

    #[test]
    fn to_partial_config_with_tool_bundles_and_approval_rules() {
        let task = TaskDefinition {
            name: "approval-task".into(),
            tool_bundles: Some(HashMap::from([(
                "safe-read".into(),
                vec!["file_read".into(), "web_search".into()],
            )])),
            tool_require_approval: Some(vec![RequireApprovalEntry::Simple("shell".into())]),
            ..Default::default()
        };

        let partial = task.to_partial_config();
        let tools = partial.tools.expect("tools overlay");
        assert_eq!(
            tools.bundles,
            Some(HashMap::from([(
                "safe-read".to_string(),
                vec!["file_read".to_string(), "web_search".to_string()],
            )]))
        );
        assert_eq!(
            tools.require_approval,
            Some(vec![RequireApprovalEntry::Simple("shell".to_string())])
        );
    }

    #[test]
    fn to_partial_config_empty_task_produces_empty_partial() {
        let task = TaskDefinition::new("bare");
        let partial = task.to_partial_config();
        assert!(partial.provider.is_none());
        assert!(partial.output.is_none());
        assert!(partial.tools.is_none());
    }

    #[test]
    fn apply_task_overlay_overrides_model() {
        let base = ResolvedConfig::default();
        let task = TaskDefinition {
            name: "custom-model-task".into(),
            model: Some("custom-model".into()),
            ..Default::default()
        };

        let result = task.apply_to(base);
        assert_eq!(result.provider.model, "custom-model");
    }

    #[test]
    fn apply_task_overlay_preserves_unset_fields() {
        let mut base = ResolvedConfig::default();
        base.server.port = 9999;
        base.provider.model = "base-model".into();

        let task = TaskDefinition {
            name: "instructions-only".into(),
            instructions: "Do a code review.".into(),
            ..Default::default()
        };

        let result = task.apply_to(base);
        // No model override → base model preserved.
        assert_eq!(result.provider.model, "base-model");
        assert_eq!(result.server.port, 9999);
    }

    #[test]
    fn apply_task_overlay_appends_task_instructions_after_base_instructions() {
        let mut base = ResolvedConfig::default();
        base.instructions = "Base persona guidance.".into();

        let task = TaskDefinition {
            name: "review".into(),
            instructions: "Review for correctness and edge cases.".into(),
            ..Default::default()
        };

        let result = task.apply_to(base);
        let base_pos = result
            .instructions
            .find("Base persona guidance.")
            .expect("base instructions");
        let task_pos = result
            .instructions
            .find("Review for correctness and edge cases.")
            .expect("task instructions");

        assert!(
            base_pos < task_pos,
            "task instructions should follow base instructions"
        );
        assert!(result.instructions.contains("<!-- task: review -->"));
    }

    #[test]
    fn apply_task_overlay_replaces_tool_permission_sets() {
        let mut base = ResolvedConfig::default();
        base.tools.permissions.allow = vec!["*".into()];
        base.tools.permissions.deny = vec!["shell".into()];
        base.tools.permissions.bundles =
            HashMap::from([("default".into(), vec!["file_read".into()])]);
        base.tools.permissions.require_approval =
            vec![RequireApprovalEntry::Simple("web_search".into())];

        let task = TaskDefinition {
            name: "locked-down".into(),
            tool_allow: Some(vec!["file_read".into()]),
            tool_deny: Some(vec!["web_search".into()]),
            tool_bundles: Some(HashMap::from([(
                "read-only".into(),
                vec!["file_read".into()],
            )])),
            tool_require_approval: Some(vec![RequireApprovalEntry::Simple("file_read".into())]),
            ..Default::default()
        };

        let result = task.apply_to(base);
        assert_eq!(result.tools.permissions.allow, vec!["file_read"]);
        assert_eq!(result.tools.permissions.deny, vec!["web_search"]);
        assert_eq!(
            result.tools.permissions.bundles,
            HashMap::from([("read-only".to_string(), vec!["file_read".to_string()])])
        );
        assert_eq!(
            result.tools.permissions.require_approval,
            vec![RequireApprovalEntry::Simple("file_read".to_string())]
        );
    }

    #[test]
    fn task_definition_toml_round_trip() {
        let task = TaskDefinition {
            name: "review".into(),
            description: "Review code for correctness".into(),
            instructions: "Focus on edge cases and error handling.".into(),
            model: Some("claude-opus".into()),
            output_schema: None,
            max_output_retries: None,
            tool_allow: Some(vec!["file_read".into(), "web_search".into()]),
            tool_deny: None,
            tool_bundles: None,
            tool_require_approval: None,
        };
        let serialised = toml::to_string(&task).expect("serialise");
        let restored: TaskDefinition = toml::from_str(&serialised).expect("deserialise");
        assert_eq!(restored.name, "review");
        assert_eq!(restored.model.as_deref(), Some("claude-opus"));
        assert_eq!(
            restored.tool_allow.unwrap(),
            vec!["file_read", "web_search"]
        );
    }

    #[test]
    fn task_definition_json_round_trip() {
        let task = TaskDefinition {
            name: "analysis".into(),
            output_schema: Some(
                serde_json::json!({"type": "object", "properties": {"score": {"type": "number"}}}),
            ),
            max_output_retries: Some(3),
            ..Default::default()
        };
        let json = serde_json::to_string(&task).expect("serialise");
        let restored: TaskDefinition = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(restored.name, "analysis");
        assert!(restored.output_schema.is_some());
        assert_eq!(restored.max_output_retries, Some(3));
    }
}
