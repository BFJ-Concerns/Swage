//! Shared dispatch-scoped context composition inputs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Context components selected for one child dispatch.
///
/// These values are applied while composing the child turn and are not
/// persisted into the child's session-variable store.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct DispatchContextComposition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elements: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assembly: Option<String>,
}

impl DispatchContextComposition {
    /// Decode composition inputs from a larger dispatch payload.
    ///
    /// Unknown fields belong to the surrounding dispatch contract and are
    /// deliberately ignored.
    pub fn from_payload(payload: &Value) -> Result<Self, serde_json::Error> {
        let composition: Self = serde_json::from_value(payload.clone())?;
        if let Some(name) = composition.assembly.as_deref() {
            validate_context_assembly_name(name).map_err(serde::de::Error::custom)?;
        }
        Ok(composition)
    }
}

/// Validate the stable identifier used to address a context assembly.
pub fn validate_context_assembly_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(format!(
            "invalid context assembly name '{name}': use lowercase letters, digits, and hyphens"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payload_decoder_accepts_surrounding_dispatch_fields() {
        let composition = DispatchContextComposition::from_payload(&json!({
            "task": "inspect",
            "elements": ["current-work"],
            "variables": {"owner": "Ada"},
            "assembly": "focused"
        }))
        .expect("decode composition");

        assert_eq!(composition.elements, Some(vec!["current-work".into()]));
        assert_eq!(
            composition
                .variables
                .as_ref()
                .and_then(|variables| variables.get("owner")),
            Some(&"Ada".into())
        );
        assert_eq!(composition.assembly.as_deref(), Some("focused"));
    }

    #[test]
    fn payload_decoder_rejects_assembly_names_that_can_escape_the_hierarchy() {
        for name in ["../config", "/tmp/config", "Uppercase", "with/slash"] {
            let error = DispatchContextComposition::from_payload(&json!({"assembly": name}))
                .expect_err("unsafe assembly name must be rejected");
            assert!(error.to_string().contains("invalid context assembly name"));
        }
    }

    #[test]
    fn payload_decoder_rejects_each_malformed_composition_field() {
        for payload in [
            json!({"elements": "current-work"}),
            json!({"variables": {"owner": 7}}),
            json!({"assembly": ["focused"]}),
        ] {
            assert!(
                DispatchContextComposition::from_payload(&payload).is_err(),
                "malformed composition should fail: {payload}"
            );
        }
    }
}
