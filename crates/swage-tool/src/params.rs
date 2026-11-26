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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use swage_core::ToolError;

    use super::{require_param, require_str};

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
}
