//! JSON schema validation for tool parameters.
//!
//! Hand-rolled validation covering the subset of JSON Schema needed by tools:
//! type checking, required-field presence, and recursive property validation.
//! Full JSON Schema semantics (oneOf, allOf, $ref, etc.) are out of scope.

use serde_json::Value;
use swage_core::ToolError;

/// Validate `params` against `schema` for the named tool.
///
/// Checks:
/// - Required fields are present.
/// - Each field whose schema declares a `type` matches that type.
/// - Nested object schemas are validated recursively via `properties`.
///
/// Returns `ToolError::InvalidParameters` on the first failure found, with a
/// message that names the failing field and explains the violation.
pub fn validate_tool_schema(
    tool_name: &str,
    schema: &Value,
    params: &Value,
) -> Result<(), ToolError> {
    validate_object(tool_name, schema, params, "")
}

/// Recursive worker — `path` is the dot-separated field path for error messages.
fn validate_object(
    tool_name: &str,
    schema: &Value,
    value: &Value,
    path: &str,
) -> Result<(), ToolError> {
    // --- Required field check ---
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for req in required {
            let field_name = req.as_str().unwrap_or("");
            let field_path = join_path(path, field_name);

            let present = value
                .as_object()
                .map(|obj| obj.contains_key(field_name))
                .unwrap_or(false);

            if !present {
                return Err(invalid(
                    tool_name,
                    format!("missing required field '{field_path}'"),
                ));
            }
        }
    }

    // --- Per-property validation ---
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (field_name, field_schema) in properties {
            let field_path = join_path(path, field_name);

            // Retrieve the value for this field; absent optional fields are fine.
            let field_value = match value.get(field_name) {
                Some(v) => v,
                None => continue,
            };

            // Type check if "type" is declared.
            if let Some(declared_type) = field_schema.get("type").and_then(Value::as_str) {
                check_type(tool_name, declared_type, field_value, &field_path)?;
            }

            // Recurse into nested objects.
            if field_schema.get("properties").is_some() || field_schema.get("required").is_some() {
                validate_object(tool_name, field_schema, field_value, &field_path)?;
            }
        }
    }

    Ok(())
}

/// Check that `value` matches the declared JSON Schema `type` string.
fn check_type(
    tool_name: &str,
    declared_type: &str,
    value: &Value,
    path: &str,
) -> Result<(), ToolError> {
    let type_matches = match declared_type {
        "string" => value.is_string(),
        "number" | "integer" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        // Unknown type annotations are allowed through — extensibility.
        _ => true,
    };

    if !type_matches {
        let actual = json_type_name(value);
        return Err(invalid(
            tool_name,
            format!("expected {declared_type} for field '{path}', got {actual}"),
        ));
    }

    Ok(())
}

/// Return a human-readable JSON type name for error messages.
fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Build a dot-separated field path, handling the empty-root case.
fn join_path(parent: &str, field: &str) -> String {
    if parent.is_empty() {
        field.to_owned()
    } else {
        format!("{parent}.{field}")
    }
}

/// Construct `ToolError::InvalidParameters` with the given message.
fn invalid(tool_name: &str, message: impl Into<String>) -> ToolError {
    ToolError::InvalidParameters {
        name: tool_name.to_owned(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TOOL: &str = "test_tool";

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

    fn ok(schema: Value, params: Value) {
        validate_tool_schema(TOOL, &schema, &params).expect("expected validation to pass");
    }

    fn err(schema: Value, params: Value) -> String {
        validate_tool_schema(TOOL, &schema, &params)
            .expect_err("expected validation to fail")
            .to_string()
    }

    // -------------------------------------------------------------------------
    // Required field presence
    // -------------------------------------------------------------------------

    #[test]
    fn required_field_present_passes() {
        ok(
            json!({"type": "object", "required": ["cmd"], "properties": {"cmd": {"type": "string"}}}),
            json!({"cmd": "ls"}),
        );
    }

    #[test]
    fn missing_required_field_fails_with_field_name() {
        let msg = err(
            json!({"type": "object", "required": ["cmd"], "properties": {"cmd": {"type": "string"}}}),
            json!({}),
        );
        assert!(
            msg.contains("cmd"),
            "error should name the missing field: {msg}"
        );
        assert!(msg.contains("missing"), "error should say 'missing': {msg}");
    }

    #[test]
    fn multiple_required_fields_first_missing_reported() {
        let msg = err(
            json!({"required": ["a", "b"], "properties": {
                "a": {"type": "string"},
                "b": {"type": "string"}
            }}),
            json!({}),
        );
        // The first required field missing is "a".
        assert!(
            msg.contains("'a'") || msg.contains("'b'"),
            "error should name a missing field: {msg}"
        );
    }

    // -------------------------------------------------------------------------
    // Type checking
    // -------------------------------------------------------------------------

    #[test]
    fn string_type_passes_for_string_value() {
        ok(
            json!({"properties": {"x": {"type": "string"}}}),
            json!({"x": "hello"}),
        );
    }

    #[test]
    fn string_type_fails_for_number_value() {
        let msg = err(
            json!({"properties": {"x": {"type": "string"}}}),
            json!({"x": 42}),
        );
        assert!(
            msg.contains("string"),
            "should mention expected type: {msg}"
        );
        assert!(msg.contains("'x'"), "should name the field: {msg}");
    }

    #[test]
    fn number_type_passes_for_number_value() {
        ok(
            json!({"properties": {"n": {"type": "number"}}}),
            json!({"n": 2.5}),
        );
    }

    #[test]
    fn number_type_fails_for_string_value() {
        let msg = err(
            json!({"properties": {"n": {"type": "number"}}}),
            json!({"n": "not-a-number"}),
        );
        assert!(msg.contains("number"), "{msg}");
    }

    #[test]
    fn boolean_type_passes() {
        ok(
            json!({"properties": {"flag": {"type": "boolean"}}}),
            json!({"flag": true}),
        );
    }

    #[test]
    fn boolean_type_fails_for_string() {
        let msg = err(
            json!({"properties": {"flag": {"type": "boolean"}}}),
            json!({"flag": "yes"}),
        );
        assert!(msg.contains("boolean"), "{msg}");
    }

    #[test]
    fn array_type_passes() {
        ok(
            json!({"properties": {"items": {"type": "array"}}}),
            json!({"items": [1, 2, 3]}),
        );
    }

    #[test]
    fn array_type_fails_for_object() {
        let msg = err(
            json!({"properties": {"items": {"type": "array"}}}),
            json!({"items": {"a": 1}}),
        );
        assert!(msg.contains("array"), "{msg}");
    }

    #[test]
    fn object_type_passes() {
        ok(
            json!({"properties": {"meta": {"type": "object"}}}),
            json!({"meta": {"key": "value"}}),
        );
    }

    #[test]
    fn object_type_fails_for_array() {
        let msg = err(
            json!({"properties": {"meta": {"type": "object"}}}),
            json!({"meta": [1, 2]}),
        );
        assert!(msg.contains("object"), "{msg}");
    }

    // -------------------------------------------------------------------------
    // Nested object recursion
    // -------------------------------------------------------------------------

    #[test]
    fn nested_object_required_field_enforced() {
        let schema = json!({
            "properties": {
                "options": {
                    "type": "object",
                    "required": ["timeout"],
                    "properties": {
                        "timeout": {"type": "number"}
                    }
                }
            }
        });

        // Passes when nested required field is present
        ok(schema.clone(), json!({"options": {"timeout": 30}}));

        // Fails when nested required field is missing
        let msg = err(schema, json!({"options": {}}));
        assert!(
            msg.contains("timeout"),
            "should name the nested missing field: {msg}"
        );
    }

    #[test]
    fn nested_object_type_mismatch_caught() {
        let schema = json!({
            "properties": {
                "config": {
                    "type": "object",
                    "properties": {
                        "retries": {"type": "number"}
                    }
                }
            }
        });

        let msg = err(schema, json!({"config": {"retries": "many"}}));
        assert!(msg.contains("number"), "{msg}");
        assert!(
            msg.contains("retries"),
            "should name the nested field: {msg}"
        );
    }

    // -------------------------------------------------------------------------
    // Optional fields
    // -------------------------------------------------------------------------

    #[test]
    fn optional_field_absent_passes() {
        // Declared in properties but not in required — absence is fine.
        ok(
            json!({"properties": {"optional_field": {"type": "string"}}}),
            json!({}),
        );
    }

    #[test]
    fn optional_field_present_with_wrong_type_fails() {
        let msg = err(
            json!({"properties": {"opt": {"type": "boolean"}}}),
            json!({"opt": "true"}),
        );
        assert!(msg.contains("boolean"), "{msg}");
    }

    // -------------------------------------------------------------------------
    // Empty / no-constraint schemas
    // -------------------------------------------------------------------------

    #[test]
    fn schema_with_no_constraints_passes_any_params() {
        ok(
            json!({"type": "object", "properties": {}}),
            json!({"anything": 42}),
        );
        ok(json!({}), json!({"anything": "goes"}));
    }

    // -------------------------------------------------------------------------
    // Error includes tool name
    // -------------------------------------------------------------------------

    #[test]
    fn error_message_includes_tool_name() {
        let err = validate_tool_schema(
            "my_custom_tool",
            &json!({"required": ["x"], "properties": {"x": {"type": "string"}}}),
            &json!({}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("my_custom_tool"), "{err}");
    }
}
