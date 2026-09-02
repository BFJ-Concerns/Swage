//! Provider-facing projection for tool parameter schemas.
//!
//! Swage validates tool arguments locally with the original tool schema. The
//! schema sent to providers only needs to describe the model-facing parameter
//! shape, and some providers reject richer JSON Schema keywords. Keep this
//! projection intentionally conservative and shared by every provider adapter.

use serde_json::{Map, Value};

const HOSTILE_KEYWORDS: &[&str] = &[
    "$schema",
    "$id",
    "$defs",
    "allOf",
    "anyOf",
    "const",
    "default",
    "dependentRequired",
    "dependentSchemas",
    "else",
    "exclusiveMaximum",
    "exclusiveMinimum",
    "format",
    "if",
    "items",
    "maxItems",
    "maxLength",
    "maximum",
    "minItems",
    "minLength",
    "minimum",
    "multipleOf",
    "not",
    "oneOf",
    "pattern",
    "patternProperties",
    "then",
    "unevaluatedProperties",
    "uniqueItems",
];

/// Keep model-facing summaries short even when a schema has many branches.
///
/// Eight alternatives leave four places of headroom beyond the nullable
/// schedule delivery target's current four branches. Further alternatives are
/// still described by the local-validation fallback rather than silently
/// omitted from the provider schema.
const MAX_DESCRIBED_SCHEMA_CHOICES: usize = 8;
const MAX_INLINE_CONSTRAINT_VALUE_CHARS: usize = 160;

pub(crate) fn provider_tool_schema(schema: &Value) -> Value {
    let mut projected = project_schema(schema);
    ensure_object_schema(&mut projected);
    projected
}

fn project_schema(schema: &Value) -> Value {
    let Some(object) = schema.as_object() else {
        return object_schema();
    };

    let mut projected = Map::new();
    let mut stripped = Vec::new();

    for (key, value) in object {
        match key.as_str() {
            "type" => {
                projected.insert(key.clone(), value.clone());
            }
            "description" if value.is_string() => {
                projected.insert(key.clone(), value.clone());
            }
            "required" if value.is_array() => {
                projected.insert(key.clone(), value.clone());
            }
            "additionalProperties" => {
                if value.is_boolean() {
                    projected.insert(key.clone(), value.clone());
                } else if value.is_object() {
                    projected.insert(key.clone(), project_schema(value));
                }
            }
            "enum" if value.is_array() => {
                projected.insert(key.clone(), value.clone());
            }
            "properties" => {
                if let Some(properties) = value.as_object() {
                    projected.insert(key.clone(), Value::Object(project_properties(properties)));
                }
            }
            other if HOSTILE_KEYWORDS.contains(&other) => {
                stripped.push(stripped_constraint_summary(other, value));
            }
            _ => {}
        }
    }

    append_stripped_keyword_note(&mut projected, &stripped);
    Value::Object(projected)
}

fn project_properties(properties: &Map<String, Value>) -> Map<String, Value> {
    properties
        .iter()
        .map(|(name, schema)| (name.clone(), project_schema(schema)))
        .collect()
}

fn append_stripped_keyword_note(projected: &mut Map<String, Value>, stripped: &[String]) {
    if stripped.is_empty() {
        return;
    }

    let note = format!(
        "Additional local validation applies for: {}.",
        stripped.join(", ")
    );
    match projected.get_mut("description") {
        Some(Value::String(description)) if !description.is_empty() => {
            description.push(' ');
            description.push_str(&note);
        }
        _ => {
            projected.insert("description".to_string(), Value::String(note));
        }
    }
}

/// Retain useful validation guidance after removing provider-hostile JSON
/// Schema keywords. Providers receive a deliberately small schema subset, but
/// models still need the constraints to form a valid first tool call.
fn stripped_constraint_summary(keyword: &str, value: &Value) -> String {
    match keyword {
        "oneOf" => format!(
            "oneOf ({})",
            describe_schema_choices(value, "exactly one of", "or")
        ),
        "anyOf" => format!(
            "anyOf ({})",
            describe_schema_choices(value, "at least one of", "or")
        ),
        "allOf" => format!(
            "allOf ({})",
            describe_schema_choices(value, "all of", "and")
        ),
        "items" => describe_item_constraint(value),
        "const" | "minLength" | "pattern" => {
            format!("{keyword} ({})", describe_constant(value))
        }
        "$schema" | "$id" | "$defs" => keyword.to_string(),
        _ => keyword.to_string(),
    }
}

fn describe_schema_choices(value: &Value, quantifier: &str, connector: &str) -> String {
    let Some(choices) = value.as_array() else {
        return "an additional locally validated choice".to_string();
    };

    let summaries: Vec<_> = choices
        .iter()
        .take(MAX_DESCRIBED_SCHEMA_CHOICES)
        .map(describe_schema_choice)
        .collect();
    let description = format!(
        "{quantifier}: {}",
        summaries.join(&format!("; {connector} "))
    );

    if choices.len() > MAX_DESCRIBED_SCHEMA_CHOICES {
        let remaining = if connector == "and" {
            "remaining required conditions are validated locally"
        } else {
            "remaining alternatives are validated locally"
        };
        format!("{description}; {remaining}")
    } else {
        description
    }
}

fn describe_schema_choice(schema: &Value) -> String {
    let Some(object) = schema.as_object() else {
        return "an additional locally validated alternative".to_string();
    };
    let mut details = Vec::new();

    if object.get("type").is_some_and(is_null_schema_type) {
        details.push("is `null`".to_string());
    }

    if let Some(required) = object.get("required").and_then(Value::as_array) {
        let fields: Vec<_> = required
            .iter()
            .filter_map(Value::as_str)
            .map(quoted)
            .collect();
        if !fields.is_empty() {
            details.push(format!("requires {}", fields.join(", ")));
        }
    }

    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        for (name, property) in properties {
            if let Some(value) = property.get("const") {
                details.push(format!(
                    "{} must be {}",
                    quoted(name),
                    describe_constant(value)
                ));
            }
        }
    }

    if details.is_empty() {
        "an additional locally validated alternative".to_string()
    } else {
        details.join("; ")
    }
}

fn describe_item_constraint(value: &Value) -> String {
    let Some(object) = value.as_object() else {
        return "items".to_string();
    };

    match object.get("type") {
        Some(Value::String(kind)) => format!("items must be {}", indefinite_noun(kind)),
        Some(Value::Array(kinds)) => {
            let kinds: Vec<_> = kinds
                .iter()
                .filter_map(Value::as_str)
                .map(indefinite_noun)
                .collect();
            if kinds.is_empty() {
                "items".to_string()
            } else {
                format!("items must be {}", kinds.join(" or "))
            }
        }
        _ => "items".to_string(),
    }
}

fn is_null_schema_type(value: &Value) -> bool {
    matches!(value, Value::String(kind) if kind == "null")
        || value
            .as_array()
            .is_some_and(|kinds| kinds.iter().any(|kind| kind == "null"))
}

fn indefinite_noun(kind: &str) -> String {
    match kind {
        "array" => "an array".to_string(),
        "object" => "an object".to_string(),
        "integer" => "an integer".to_string(),
        "null" => "`null`".to_string(),
        _ => format!("a {kind}"),
    }
}

fn describe_constant(value: &Value) -> String {
    match value {
        Value::String(value) if value.chars().count() <= MAX_INLINE_CONSTRAINT_VALUE_CHARS => {
            format!("\"{value}\"")
        }
        Value::String(_) => "a fixed value".to_string(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Null => "`null`".to_string(),
        Value::Array(_) | Value::Object(_) => "a fixed value".to_string(),
    }
}

fn quoted(value: &str) -> String {
    format!("`{value}`")
}

fn ensure_object_schema(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        *schema = object_schema();
        return;
    };

    object
        .entry("type")
        .or_insert_with(|| Value::String("object".to_string()));
    object
        .entry("properties")
        .or_insert_with(|| Value::Object(Map::new()));
}

fn object_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {}
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{provider_tool_schema, MAX_DESCRIBED_SCHEMA_CHOICES};

    #[test]
    fn provider_tool_schema_strips_hostile_keywords_and_notes_local_validation() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query.",
                    "minLength": 3,
                    "pattern": "^[a-z]+$"
                },
                "mode": {
                    "type": "string",
                    "enum": ["fast", "deep"],
                    "const": "fast"
                }
            },
            "required": ["query"],
            "additionalProperties": false,
            "oneOf": [{"required": ["mode"]}]
        });

        let projected = provider_tool_schema(&schema);

        assert_eq!(projected["type"], "object");
        assert_eq!(projected["required"], json!(["query"]));
        assert_eq!(projected["additionalProperties"], false);
        assert!(projected.get("$schema").is_none());
        assert!(projected.get("oneOf").is_none());
        let description = projected["description"].as_str().unwrap();
        assert!(description.contains("oneOf"));
        assert!(description.contains("requires `mode`"));

        let query = &projected["properties"]["query"];
        assert_eq!(query["type"], "string");
        assert!(query.get("minLength").is_none());
        assert!(query.get("pattern").is_none());
        let query_description = query["description"].as_str().unwrap();
        assert!(query_description.contains("minLength (3)"));
        assert!(query_description.contains("pattern (\"^[a-z]+$\")"));

        let mode = &projected["properties"]["mode"];
        assert_eq!(mode["enum"], json!(["fast", "deep"]));
        assert!(mode.get("const").is_none());
        assert!(
            mode["description"]
                .as_str()
                .unwrap()
                .contains("const (\"fast\")")
        );
    }

    #[test]
    fn provider_tool_schema_preserves_property_names_that_match_keywords() {
        let schema = json!({
            "type": "object",
            "properties": {
                "oneOf": {
                    "type": "string",
                    "description": "A normal parameter name."
                },
                "nested": {
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "maxLength": 10
                        }
                    }
                }
            }
        });

        let projected = provider_tool_schema(&schema);

        assert_eq!(projected["properties"]["oneOf"]["type"], "string");
        assert_eq!(
            projected["properties"]["nested"]["properties"]["pattern"]["type"],
            "string"
        );
        assert!(
            projected["properties"]["nested"]["properties"]["pattern"]
                .get("maxLength")
                .is_none()
        );
    }

    #[test]
    fn provider_tool_schema_explains_discriminated_union_requirements() {
        let schema = json!({
            "type": "object",
            "oneOf": [
                {
                    "properties": {"kind": {"const": "channel"}},
                    "required": ["kind", "integration", "account", "conversation"]
                },
                {
                    "properties": {"kind": {"const": "local_storage"}},
                    "required": ["kind"]
                },
                {
                    "properties": {"kind": {"const": "webhook"}},
                    "required": ["kind", "url"]
                }
            ]
        });

        let projected = provider_tool_schema(&schema);
        let description = projected["description"].as_str().unwrap();

        assert!(description.contains("exactly one of"));
        assert!(description.contains("`kind` must be \"channel\""));
        assert!(description.contains("requires `kind`, `integration`, `account`, `conversation`"));
        assert!(description.contains("`kind` must be \"local_storage\""));
        assert!(description.contains("requires `kind`, `url`"));
    }

    #[test]
    fn provider_tool_schema_explains_all_of_requirements_conjunctively() {
        let schema = json!({
            "allOf": [
                {"required": ["kind"]},
                {"required": ["payload"]}
            ]
        });

        let projected = provider_tool_schema(&schema);
        let description = projected["description"].as_str().unwrap();

        assert!(description.contains("all of"));
        assert!(description.contains("requires `kind`; and requires `payload`"));
        assert!(!description.contains("requires `kind`; or requires `payload`"));
    }

    #[test]
    fn provider_tool_schema_calls_overflowed_all_of_branches_required_conditions() {
        let schema = json!({
            "allOf": (0..=MAX_DESCRIBED_SCHEMA_CHOICES)
                .map(|index| json!({"required": [format!("field_{index}")]}))
                .collect::<Vec<_>>()
        });

        let projected = provider_tool_schema(&schema);
        let description = projected["description"].as_str().unwrap();

        assert!(description.contains("remaining required conditions are validated locally"));
        assert!(!description.contains("remaining alternatives are validated locally"));
    }

    #[test]
    fn provider_tool_schema_describes_eight_choices_and_uses_plain_item_guidance() {
        let schema = json!({
            "type": "object",
            "properties": {
                "changes": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "secret": {"type": "string", "description": "Do not expose this schema."}
                        }
                    }
                }
            },
            "oneOf": [
                {"type": "null"},
                {"required": ["first"]},
                {"required": ["second"]},
                {"required": ["third"]},
                {"required": ["fourth"]},
                {"required": ["fifth"]},
                {"required": ["sixth"]},
                {"required": ["seventh"]},
                {"required": ["eighth"]},
                {"required": ["ninth"]}
            ]
        });

        let projected = provider_tool_schema(&schema);
        let description = projected["description"].as_str().unwrap();
        let changes_description = projected["properties"]["changes"]["description"]
            .as_str()
            .unwrap();

        assert!(description.contains("is `null`"));
        assert!(description.contains("requires `seventh`"));
        assert!(!description.contains("requires `eighth`"));
        assert!(description.contains("remaining alternatives are validated locally"));
        assert_eq!(
            changes_description,
            "Additional local validation applies for: items must be an object."
        );
        assert!(!changes_description.contains("secret"));
    }

    #[test]
    fn provider_tool_schema_bounds_inline_constraint_values() {
        let long_pattern = "a".repeat(161);
        let schema = json!({
            "type": "object",
            "properties": {"value": {"type": "string", "pattern": long_pattern}}
        });

        let projected = provider_tool_schema(&schema);
        let description = projected["properties"]["value"]["description"]
            .as_str()
            .unwrap();

        assert!(description.contains("pattern (a fixed value)"));
        assert!(!description.contains(&long_pattern));
    }
}
