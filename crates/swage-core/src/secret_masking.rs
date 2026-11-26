//! Masking of registered secret values.
//!
//! A value registered as a secret is replaced by [`REDACTED_SECRET`] wherever
//! the gateway records or forwards text that could carry it. The set of values
//! is held in one shared [`RegisteredSecretValues`]: the secret store refreshes
//! it whenever it reads the values from their backends, and every writer of a
//! durable record masks through it, so a value reaches no record however it
//! came in.

use std::sync::{Arc, RwLock};

/// The text a registered value is replaced by.
pub const REDACTED_SECRET: &str = "[REDACTED_SECRET]";

/// The current registered values, shared between the secret store that knows
/// them and every record writer that masks them.
#[derive(Debug, Clone, Default)]
pub struct RegisteredSecretValues {
    values: Arc<RwLock<Arc<Vec<String>>>>,
}

impl RegisteredSecretValues {
    /// Replace the set. Empty values are dropped, and the rest are held
    /// longest first so that a value containing another is masked whole.
    pub fn replace(&self, mut values: Vec<String>) {
        values.retain(|value| !value.is_empty());
        values.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();
        *self
            .values
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = Arc::new(values);
    }

    /// The values as they stand, longest first.
    pub fn current(&self) -> Arc<Vec<String>> {
        Arc::clone(
            &self
                .values
                .read()
                .unwrap_or_else(|poison| poison.into_inner()),
        )
    }

    /// Mask every registered value in `text`, in its plain form and in the
    /// escaped form it takes inside serialised JSON. Returns whether anything
    /// was replaced.
    pub fn mask(&self, text: &mut String) -> bool {
        mask_serialised_text(text, &self.current())
    }

    /// [`Self::mask`] over an optional text.
    pub fn mask_option(&self, text: &mut Option<String>) -> bool {
        text.as_mut().is_some_and(|text| self.mask(text))
    }
}

/// Replace each of `values` in `text`, which must be ordered longest first.
pub fn mask_text(text: &mut String, values: &[impl AsRef<str>]) -> bool {
    let mut replaced = false;
    for value in values {
        let value = value.as_ref();
        if !value.is_empty() && text.contains(value) {
            *text = text.replace(value, REDACTED_SECRET);
            replaced = true;
        }
    }
    replaced
}

/// Mask every value in text that may hold them inside serialised JSON, where a
/// value with quotes, backslashes or control characters appears escaped: both
/// the escaped and the plain form are replaced. The marker has no character
/// JSON escapes, so a masked document stays the document it was.
pub fn mask_serialised_text(text: &mut String, values: &[String]) -> bool {
    let mut changed = false;
    for value in values {
        let encoded = serde_json::to_string(value).expect("secret text serialises");
        let escaped = &encoded[1..encoded.len() - 1];
        if escaped != value.as_str() {
            changed |= mask_text(text, &[escaped]);
        }
        changed |= mask_text(text, &[value.as_str()]);
    }
    changed
}

/// Mask every value in the strings of a JSON document, keys included.
pub fn mask_json_strings(value: &mut serde_json::Value, values: &[String]) -> bool {
    let mut replaced = false;
    match value {
        serde_json::Value::String(text) => return mask_text(text, values),
        serde_json::Value::Array(items) => {
            for item in items {
                replaced |= mask_json_strings(item, values);
            }
        }
        serde_json::Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                let mut masked_key = key.clone();
                if mask_text(&mut masked_key, values) {
                    let item = map.remove(&key).expect("key was listed");
                    map.insert(masked_key, item);
                    replaced = true;
                }
            }
            for item in map.values_mut() {
                replaced |= mask_json_strings(item, values);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
    replaced
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_containing_another_is_masked_whole() {
        let set = RegisteredSecretValues::default();
        set.replace(vec!["abc".into(), "xxabcxx".into(), String::new()]);
        let mut text = "see xxabcxx and abc".to_string();
        assert!(set.mask(&mut text));
        assert_eq!(text, "see [REDACTED_SECRET] and [REDACTED_SECRET]");
    }

    #[test]
    fn the_escaped_form_inside_serialised_json_is_masked() {
        let set = RegisteredSecretValues::default();
        let secret = "line\"one\nline two";
        set.replace(vec![secret.into()]);
        let mut text = serde_json::json!({ "output": format!("key {secret} end") }).to_string();
        assert!(set.mask(&mut text));
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["output"], "key [REDACTED_SECRET] end");
    }

    #[test]
    fn json_keys_are_masked_as_well_as_values() {
        let values = vec!["hunter2".to_string()];
        let mut value = serde_json::json!({ "hunter2": ["a hunter2 b"] });
        assert!(mask_json_strings(&mut value, &values));
        assert_eq!(
            value,
            serde_json::json!({ "[REDACTED_SECRET]": ["a [REDACTED_SECRET] b"] })
        );
    }
}
