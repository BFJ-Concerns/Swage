//! Output schema contract override for inter-agent delegation.
//!
//! When a Delegate message includes an output schema, it overrides the target
//! agent's own `[output]` config for that delegation. The delegator defines the
//! contract, not the delegate.
//!
//! The Phase 02 validation infrastructure handles the actual validation and
//! retry loop — this module only resolves which schema to use.

use serde_json::Value;
use swage_config::OutputConfig;

/// Resolved output schema and retry settings for a delegation.
///
/// Used when constructing the target Agent via AgentBuilder.
#[derive(Debug, Clone)]
pub struct DelegationOutputConfig {
    /// JSON Schema for validating the target's output. `None` means no validation.
    pub schema: Option<Value>,
    /// Maximum number of retry attempts for schema validation.
    pub max_retries: u32,
}

/// Resolve the output schema for a delegation.
///
/// Priority order:
/// 1. Schema from the Delegate message payload (`output_schema` field).
/// 2. Target agent's own `[output]` config.
/// 3. No schema — no validation occurs.
///
/// `max_retries` follows a narrower priority: the delegate's value is only
/// used when the delegate also provides an `output_schema` (case 1). When
/// falling back to the target's own schema (case 2) or no schema (case 3),
/// `max_retries` always comes from the target config — any `max_retries`
/// in the payload without an accompanying schema is intentionally ignored.
pub fn resolve_delegation_schema(
    delegate_payload: &Value,
    target_output_config: &OutputConfig,
) -> DelegationOutputConfig {
    let delegate_schema = delegate_payload.get("output_schema").cloned();
    let delegate_retries = delegate_payload
        .get("max_retries")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    if let Some(schema) = delegate_schema {
        // Delegate-specified schema takes precedence.
        DelegationOutputConfig {
            schema: Some(schema),
            max_retries: delegate_retries.unwrap_or(target_output_config.max_retries),
        }
    } else if let Some(ref target_schema) = target_output_config.schema {
        // Fall back to target's own config.
        DelegationOutputConfig {
            schema: Some(target_schema.clone()),
            max_retries: target_output_config.max_retries,
        }
    } else {
        // Neither specifies a schema — no validation.
        DelegationOutputConfig {
            schema: None,
            max_retries: target_output_config.max_retries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn target_config_with_schema() -> OutputConfig {
        OutputConfig {
            schema: Some(json!({"type": "object", "properties": {"x": {"type": "number"}}})),
            max_retries: 3,
        }
    }

    fn target_config_no_schema() -> OutputConfig {
        OutputConfig {
            schema: None,
            max_retries: 3,
        }
    }

    #[test]
    fn delegate_schema_overrides_target() {
        let delegate_schema = json!({"type": "object", "required": ["answer"]});
        let payload = json!({"output_schema": delegate_schema, "task": "work"});

        let result = resolve_delegation_schema(&payload, &target_config_with_schema());
        assert_eq!(result.schema.unwrap(), delegate_schema);
        assert_eq!(result.max_retries, 3); // Falls back to target's retries.
    }

    #[test]
    fn delegate_retries_override_target() {
        let payload = json!({
            "output_schema": {"type": "string"},
            "max_retries": 5,
        });

        let result = resolve_delegation_schema(&payload, &target_config_with_schema());
        assert_eq!(result.max_retries, 5);
    }

    #[test]
    fn no_delegate_schema_falls_back_to_target() {
        let payload = json!({"task": "work"});
        let result = resolve_delegation_schema(&payload, &target_config_with_schema());
        assert!(result.schema.is_some());
        assert_eq!(result.max_retries, 3);
    }

    #[test]
    fn neither_has_schema_returns_none() {
        let payload = json!({"task": "work"});
        let result = resolve_delegation_schema(&payload, &target_config_no_schema());
        assert!(result.schema.is_none());
    }
}
