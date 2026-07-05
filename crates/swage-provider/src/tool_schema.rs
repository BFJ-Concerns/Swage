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
            other if HOSTILE_KEYWORDS.contains(&other) => stripped.push(other.to_string()),
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

    for keyword in stripped {
        debug_assert!(HOSTILE_KEYWORDS.contains(&keyword.as_str()));
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

    use super::provider_tool_schema;

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
        assert!(projected["description"].as_str().unwrap().contains("oneOf"));

        let query = &projected["properties"]["query"];
        assert_eq!(query["type"], "string");
        assert!(query.get("minLength").is_none());
        assert!(query.get("pattern").is_none());
        assert!(query["description"].as_str().unwrap().contains("minLength"));
        assert!(query["description"].as_str().unwrap().contains("pattern"));

        let mode = &projected["properties"]["mode"];
        assert_eq!(mode["enum"], json!(["fast", "deep"]));
        assert!(mode.get("const").is_none());
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
}
