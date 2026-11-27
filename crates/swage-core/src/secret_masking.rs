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

    /// Mask every registered value in `text` ([`mask_record_text`]).
    /// Returns whether anything was replaced.
    pub fn mask(&self, text: &mut String) -> bool {
        mask_record_text(text, &self.current())
    }

    /// Whether `text` holds any registered value, by the same reading as
    /// [`Self::mask`].
    pub fn holds_any(&self, text: &str) -> bool {
        let mut copy = text.to_owned();
        self.mask(&mut copy)
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

/// Mask every value in `text`, reading it as the record it is.
///
/// A JSON object or array is masked in its decoded strings and keys and
/// written out again only when something was replaced, so a value that
/// happens to match a number or the document's own syntax never turns the
/// record into text that no longer parses. Numbers are left as they are.
/// Any other text is masked as text, in the value's plain form and in the
/// escaped form it takes when JSON is quoted inside it.
pub fn mask_record_text(text: &mut String, values: &[String]) -> bool {
    if values.is_empty() {
        return false;
    }
    let trimmed = text.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(mut document) = serde_json::from_str::<serde_json::Value>(text)
    {
        let documents = registered_documents(values);
        if documents.contains(&document) {
            *text = REDACTED_SECRET.to_owned();
            return true;
        }
        if mask_document(&mut document, values, &documents) {
            *text = document.to_string();
            return true;
        }
        return false;
    }
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

/// Mask every value in the strings of a JSON document, keys included. A
/// string that is itself a serialised document is masked as one, and a part
/// of the document equal to a registered value that is itself a JSON object
/// or array becomes the marker.
pub fn mask_json_strings(value: &mut serde_json::Value, values: &[String]) -> bool {
    mask_document(value, values, &registered_documents(values))
}

/// The registered values that are JSON objects or arrays, decoded, so a
/// record holding one as structure rather than as text is still found.
fn registered_documents(values: &[String]) -> Vec<serde_json::Value> {
    values
        .iter()
        .filter(|value| {
            let trimmed = value.trim_start();
            trimmed.starts_with('{') || trimmed.starts_with('[')
        })
        .filter_map(|value| serde_json::from_str(value).ok())
        .collect()
}

fn mask_document(
    value: &mut serde_json::Value,
    values: &[String],
    documents: &[serde_json::Value],
) -> bool {
    if (value.is_object() || value.is_array()) && documents.contains(value) {
        *value = serde_json::Value::String(REDACTED_SECRET.to_owned());
        return true;
    }
    let mut replaced = false;
    match value {
        serde_json::Value::String(text) => return mask_record_text(text, values),
        serde_json::Value::Array(items) => {
            for item in items {
                replaced |= mask_document(item, values, documents);
            }
        }
        serde_json::Value::Object(map) => {
            let entries = std::mem::take(map);
            for (mut key, mut item) in entries {
                replaced |= mask_record_text(&mut key, values);
                replaced |= mask_document(&mut item, values, documents);
                map.insert(key, item);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
    replaced
}

/// Holds back the end of a stream of text that could be the start of a
/// registered value, so a value split across two pieces is masked whole.
/// Each piece pushed returns the text that is safe to pass on now; `finish`
/// returns what is left when the stream ends or pauses.
#[derive(Debug, Default)]
pub struct StreamMasking {
    pending: String,
}

impl StreamMasking {
    /// Take the next piece of the stream and return what can be passed on.
    pub fn push(&mut self, piece: &str, values: &[String]) -> String {
        self.pending.push_str(piece);
        mask_text(&mut self.pending, values);
        let held = self.held_back_from(values);
        let rest = self.pending.split_off(held);
        std::mem::replace(&mut self.pending, rest)
    }

    /// Everything still held back, masked.
    pub fn finish(&mut self, values: &[String]) -> String {
        let mut rest = std::mem::take(&mut self.pending);
        mask_text(&mut rest, values);
        rest
    }

    /// The byte index from which `pending` could still begin a value: the
    /// start of the longest ending that is a proper prefix of one.
    fn held_back_from(&self, values: &[String]) -> usize {
        let text = self.pending.as_str();
        for (start, _) in text.char_indices() {
            let ending = &text[start..];
            if values
                .iter()
                .any(|value| value.len() > ending.len() && value.starts_with(ending))
            {
                return start;
            }
        }
        text.len()
    }
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
    fn a_value_matching_a_number_or_syntax_leaves_the_document_parsable() {
        let set = RegisteredSecretValues::default();
        set.replace(vec!["123456".into()]);
        let mut text = r#"{"output":"PIN 123456","total_bytes":123456}"#.to_string();
        assert!(set.mask(&mut text));
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["output"], "PIN [REDACTED_SECRET]");
        assert_eq!(parsed["total_bytes"], 123_456);

        set.replace(vec![r#"","#.into()]);
        let mut text = r#"{"a":"x","b":"y"}"#.to_string();
        assert!(!set.mask(&mut text));
        assert_eq!(text, r#"{"a":"x","b":"y"}"#);
    }

    #[test]
    fn a_value_that_is_itself_a_document_is_masked_whole_or_within_one() {
        let set = RegisteredSecretValues::default();
        let value = r#"{"token":"verifier-json-84c2"}"#;
        set.replace(vec![value.into()]);

        let mut text = value.to_string();
        assert!(set.mask(&mut text));
        assert_eq!(text, REDACTED_SECRET);

        let mut text = r#"{"meta": {"token": "verifier-json-84c2"}, "n": 1}"#.to_string();
        assert!(set.mask(&mut text));
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, serde_json::json!({"meta": REDACTED_SECRET, "n": 1}));

        let mut text = format!("key {value} end");
        assert!(set.mask(&mut text));
        assert_eq!(text, "key [REDACTED_SECRET] end");
    }

    #[test]
    fn a_value_split_across_stream_pieces_is_masked_whole() {
        let values = vec!["verifier-canary-7F29".to_string()];
        let mut stream = StreamMasking::default();
        let mut passed = stream.push("key: verifier-canary-", &values);
        passed.push_str(&stream.push("7F29 done, verif", &values));
        passed.push_str(&stream.finish(&values));
        assert_eq!(passed, "key: [REDACTED_SECRET] done, verif");
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
