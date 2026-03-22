//! Parameter extraction and redaction utilities.

use serde_json::Value;
use swage_core::ToolError;

// ---------------------------------------------------------------------------
// Extraction helpers
// ---------------------------------------------------------------------------

/// Extract a required string parameter from `params` by `key`.
///
/// Returns the string slice on success. Returns [`ToolError::InvalidParameters`]
/// if the key is absent or the value is not a JSON string.
pub fn require_str<'a>(
    tool_name: &str,
    params: &'a Value,
    key: &str,
) -> Result<&'a str, ToolError> {
    params
        .get(key)
        .ok_or_else(|| ToolError::InvalidParameters {
            name: tool_name.to_string(),
            message: format!("required parameter '{key}' is missing"),
        })?
        .as_str()
        .ok_or_else(|| ToolError::InvalidParameters {
            name: tool_name.to_string(),
            message: format!("parameter '{key}' must be a string"),
        })
}

/// Extract a required parameter of any JSON type from `params` by `key`.
///
/// Returns a reference to the value on success. Returns
/// [`ToolError::InvalidParameters`] if the key is absent.
pub fn require_param<'a>(
    tool_name: &str,
    params: &'a Value,
    key: &str,
) -> Result<&'a Value, ToolError> {
    params.get(key).ok_or_else(|| ToolError::InvalidParameters {
        name: tool_name.to_string(),
        message: format!("required parameter '{key}' is missing"),
    })
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

/// Produce a copy of `params` with each key in `sensitive_keys` replaced by
/// the literal string `"[REDACTED]"`.
///
/// Only top-level keys are redacted. Nested objects with the same key names
/// are left untouched. Absent sensitive keys are silently skipped.
///
/// The redacted copy is used for logging and observability; the original
/// `params` are passed through to the tool's `execute()` unchanged.
pub fn redact_params(params: &Value, sensitive_keys: &[&str]) -> Value {
    // If params is not an object there are no keys to redact — return as-is.
    let Some(map) = params.as_object() else {
        return params.clone();
    };

    let mut redacted = map.clone();
    for key in sensitive_keys {
        // Only redact keys that are actually present — absent keys are a no-op.
        if redacted.contains_key(*key) {
            redacted.insert((*key).to_string(), Value::String("[REDACTED]".into()));
        }
    }
    Value::Object(redacted)
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};
    use swage_core::ToolError;

    use super::{redact_params, require_param, require_str};

    // -- require_str ----------------------------------------------------------

    #[test]
    fn require_str_returns_value_for_present_string() {
        let params = json!({"path": "/tmp/file.txt"});
        let result = require_str("file_read", &params, "path").unwrap();
        assert_eq!(result, "/tmp/file.txt");
    }

    #[test]
    fn require_str_errors_on_missing_key() {
        let params = json!({});
        let err = require_str("file_read", &params, "path").unwrap_err();
        match err {
            ToolError::InvalidParameters { name, message } => {
                assert_eq!(name, "file_read");
                assert!(
                    message.contains("path"),
                    "error message should include field name; got: {message}"
                );
            }
            other => panic!("expected InvalidParameters, got: {other:?}"),
        }
    }

    #[test]
    fn require_str_errors_on_non_string_value() {
        let params = json!({"count": 42});
        let err = require_str("counter", &params, "count").unwrap_err();
        match err {
            ToolError::InvalidParameters { name, message } => {
                assert_eq!(name, "counter");
                assert!(
                    message.contains("count"),
                    "error message should include field name; got: {message}"
                );
            }
            other => panic!("expected InvalidParameters, got: {other:?}"),
        }
    }

    // -- require_param --------------------------------------------------------

    #[test]
    fn require_param_returns_value_for_present_key() {
        let params = json!({"items": [1, 2, 3]});
        let result = require_param("list_tool", &params, "items").unwrap();
        assert_eq!(*result, json!([1, 2, 3]));
    }

    #[test]
    fn require_param_errors_on_missing_key() {
        let params = json!({});
        let err = require_param("list_tool", &params, "items").unwrap_err();
        match err {
            ToolError::InvalidParameters { name, message } => {
                assert_eq!(name, "list_tool");
                assert!(
                    message.contains("items"),
                    "error message should include field name; got: {message}"
                );
            }
            other => panic!("expected InvalidParameters, got: {other:?}"),
        }
    }

    // -- redact_params --------------------------------------------------------

    #[test]
    fn redact_params_replaces_sensitive_keys_with_placeholder() {
        let params = json!({"api_key": "sk-secret", "query": "hello"});
        let redacted = redact_params(&params, &["api_key"]);
        assert_eq!(redacted["api_key"], Value::String("[REDACTED]".into()));
        assert_eq!(redacted["query"], json!("hello"));
    }

    #[test]
    fn redact_params_absent_key_produces_no_error() {
        let params = json!({"query": "hello"});
        // api_key is not present — redact should silently skip it.
        let redacted = redact_params(&params, &["api_key"]);
        assert_eq!(redacted["query"], json!("hello"));
        assert!(redacted.get("api_key").is_none());
    }

    #[test]
    fn redact_params_only_top_level_keys_redacted() {
        // Nested keys with the same name must not be touched.
        let params = json!({
            "token": "top-level-secret",
            "nested": {"token": "inner-value"}
        });
        let redacted = redact_params(&params, &["token"]);
        assert_eq!(redacted["token"], Value::String("[REDACTED]".into()));
        // Inner "token" must be untouched.
        assert_eq!(redacted["nested"]["token"], json!("inner-value"));
    }

    #[test]
    fn redact_params_non_sensitive_keys_intact() {
        let params = json!({"a": 1, "b": "two", "c": [3]});
        let redacted = redact_params(&params, &[]);
        assert_eq!(redacted, params);
    }
}
