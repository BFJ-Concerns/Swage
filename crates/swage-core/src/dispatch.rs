//! Shared dispatch-scoped context composition inputs.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

/// Context components selected for one child dispatch.
///
/// These values are applied while composing the child turn and are not
/// persisted into the child's session-variable store.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct DispatchContextComposition {
    #[serde(default)]
    pub elements: Option<Vec<String>>,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    #[serde(default)]
    pub assembly: Option<String>,
}

impl DispatchContextComposition {
    /// Decode composition inputs from a larger dispatch payload.
    ///
    /// Unknown fields belong to the surrounding dispatch contract and are
    /// deliberately ignored.
    pub fn from_payload(payload: &Value) -> Result<Self, serde_json::Error> {
        serde_json::from_value(payload.clone())
    }
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
        assert_eq!(composition.variables.get("owner"), Some(&"Ada".into()));
        assert_eq!(composition.assembly.as_deref(), Some("focused"));
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
