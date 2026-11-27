//! Masking of registered secret values.
//!
//! A value registered as a secret is replaced by [`REDACTED_SECRET`] wherever
//! the gateway records or forwards text that could carry it. The set of values
//! is held in one shared [`RegisteredSecretValues`]: the secret store refreshes
//! it whenever it reads the values from their backends, and every writer of a
//! durable record masks through it, so a value reaches no record however it
//! came in.

use std::collections::HashSet;
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
/// A JSON object or array stays a JSON document: its decoded strings and
/// keys are masked, a number or a part of the document holding a value
/// becomes the marker string, and a document still holding a value across
/// its own syntax becomes the marker string whole. It is written out again
/// only when something was replaced, so no record is left holding the value
/// and none is left unparsable. Any other text is masked as text, in the
/// value's plain form and in the escaped form it takes when JSON is quoted
/// inside it.
pub fn mask_record_text(text: &mut String, values: &[String]) -> bool {
    if values.is_empty() {
        return false;
    }
    let trimmed = text.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(mut document) = serde_json::from_str::<serde_json::Value>(text)
    {
        let documents = registered_documents(values);
        let changed = mask_document(&mut document, values, &documents);
        if holds_any_form(&document.to_string(), values) {
            *text = serde_json::Value::String(REDACTED_SECRET.to_owned()).to_string();
            return true;
        }
        if changed {
            *text = document.to_string();
        }
        return changed;
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

/// Whether a name is one the product gives its own keys: an identifier
/// (`id`, `*_id`) or a slug (`slug`, `*_slug`). The name alone exempts
/// nothing. The store leaves its own columns of such a name unmasked, and an
/// answer keeps such a field only when the store confirms the value is one of
/// its keys ([`mask_answer`]), so a field of that name in what a tool, a
/// model or a person wrote is masked like any other.
pub fn is_key_field(name: &str) -> bool {
    name == "id" || name.ends_with("_id") || name == "slug" || name.ends_with("_slug")
}

/// The string values of an answer's key-named fields that hold a registered
/// value: the candidates [`mask_answer`] keeps when the store confirms they
/// are its keys. Strings that hold a document of their own are content, so
/// their fields are never candidates.
pub fn answer_key_candidates(answer: &serde_json::Value, values: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    collect_key_candidates(answer, values, &mut found);
    found.sort_unstable();
    found.dedup();
    found
}

fn collect_key_candidates(value: &serde_json::Value, values: &[String], found: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                match item {
                    serde_json::Value::String(text)
                        if is_key_field(key) && holds_any_form(text, values) =>
                    {
                        found.push(text.clone());
                    }
                    item => collect_key_candidates(item, values, found),
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_key_candidates(item, values, found);
            }
        }
        _ => {}
    }
}

/// Mask every value in a JSON answer the product gives, as
/// [`mask_record_text`] masks a record, except that a key-named field whose
/// string value is in `keys` — the product's own keys, as its store confirms
/// them — is left as it is, so the answer still names what it points at.
pub fn mask_answer(
    answer: &mut serde_json::Value,
    values: &[String],
    keys: &HashSet<String>,
) -> bool {
    if values.is_empty() {
        return false;
    }
    let documents = registered_documents(values);
    let changed = mask_answer_part(answer, values, &documents, keys);
    if holds_any_form(&without_keys(answer, keys).to_string(), values) {
        *answer = serde_json::Value::String(REDACTED_SECRET.to_owned());
        return true;
    }
    changed
}

fn mask_answer_part(
    value: &mut serde_json::Value,
    values: &[String],
    documents: &[serde_json::Value],
    keys: &HashSet<String>,
) -> bool {
    if documents.contains(value) {
        return mask_document(value, values, documents);
    }
    let mut replaced = false;
    match value {
        serde_json::Value::Object(map) => {
            let entries = std::mem::take(map);
            for (mut key, mut item) in entries {
                if !is_kept_key(&key, &item, keys) {
                    replaced |= mask_record_text(&mut key, values);
                    replaced |= mask_answer_part(&mut item, values, documents, keys);
                }
                map.insert(key, item);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                replaced |= mask_answer_part(item, values, documents, keys);
            }
        }
        other => replaced = mask_document(other, values, documents),
    }
    replaced
}

/// Whether an answer's field is one of the product's keys that
/// [`mask_answer`] leaves as it is.
fn is_kept_key(name: &str, item: &serde_json::Value, keys: &HashSet<String>) -> bool {
    is_key_field(name) && item.as_str().is_some_and(|text| keys.contains(text))
}

/// The answer without the key fields [`mask_answer`] kept.
fn without_keys(value: &serde_json::Value, keys: &HashSet<String>) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .filter(|(key, item)| !is_kept_key(key, item, keys))
                .map(|(key, item)| (key.clone(), without_keys(item, keys)))
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(|item| without_keys(item, keys)).collect())
        }
        other => other.clone(),
    }
}

/// Whether `text` holds any value, plain or escaped, outside the markers
/// already in it.
fn holds_any_form(text: &str, values: &[String]) -> bool {
    let text = text.replace(REDACTED_SECRET, "");
    values.iter().any(|value| {
        let encoded = serde_json::to_string(value).expect("secret text serialises");
        text.contains(value.as_str()) || text.contains(&encoded[1..encoded.len() - 1])
    })
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
        serde_json::Value::Number(number) => {
            let digits = number.to_string();
            if values.iter().any(|value| digits.contains(value.as_str())) {
                *value = serde_json::Value::String(REDACTED_SECRET.to_owned());
                return true;
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) => {}
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

    /// Masking a document never leaves the value in it and never leaves it
    /// unparsable: a number holding the value, a part equal to it, the whole
    /// document equal to it, and the value across the document's own syntax
    /// each end as JSON holding only the marker where the value was.
    #[test]
    fn a_masked_document_holds_no_value_and_still_parses() {
        let set = RegisteredSecretValues::default();
        let cases: [(&str, &str, serde_json::Value); 4] = [
            (
                "73918264",
                r#"{"pin":73918264,"total_bytes":12}"#,
                serde_json::json!({"pin": REDACTED_SECRET, "total_bytes": 12}),
            ),
            (
                "73918264",
                r#"{"output":"PIN 73918264"}"#,
                serde_json::json!({"output": "PIN [REDACTED_SECRET]"}),
            ),
            (
                r#"{"kind":"created","id":"a"}"#,
                r#"{"kind": "created", "id": "a"}"#,
                serde_json::json!(REDACTED_SECRET),
            ),
            (
                r#"","#,
                r#"{"a":"x","b":"y"}"#,
                serde_json::json!(REDACTED_SECRET),
            ),
        ];
        for (value, record, expected) in cases {
            set.replace(vec![value.into()]);
            let mut text = record.to_string();
            assert!(set.mask(&mut text), "{record} holds {value}");
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(parsed, expected, "{record}");
            assert!(!holds_any_form(&text, &[value.to_string()]));
        }
    }

    /// A string whose text looks like a document is masked as one, number
    /// included, and the outer record stays a document.
    #[test]
    fn a_string_holding_a_document_with_the_value_as_a_number_is_masked() {
        let set = RegisteredSecretValues::default();
        set.replace(vec!["73918264".into()]);
        let mut text = r#"{"description":"{\"pin\":73918264}"}"#.to_string();
        assert!(set.mask(&mut text));
        assert!(!text.contains("73918264"));
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(parsed["description"].is_string());
    }

    #[test]
    fn a_value_that_is_itself_a_document_is_masked_whole_or_within_one() {
        let set = RegisteredSecretValues::default();
        let value = r#"{"token":"verifier-json-84c2"}"#;
        set.replace(vec![value.into()]);

        let mut text = value.to_string();
        assert!(set.mask(&mut text));
        assert_eq!(text, format!("\"{REDACTED_SECRET}\""));

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

    /// Content is masked whatever its fields are called: a value under an
    /// identifier- or slug-named field, or inside an object under one, is
    /// masked like any other, and the record keeps its other fields.
    #[test]
    fn a_key_named_field_of_a_record_is_masked() {
        let set = RegisteredSecretValues::default();
        set.replace(vec!["vault-7Q2".into()]);
        let mut text = r#"{"output":"{\"secret_id\":\"vault-7Q2\",\"note\":\"ok\"}","session_id":"vault-7Q2","id":{"token":"vault-7Q2"},"slug":"x-vault-7Q2"}"#.to_string();
        assert!(set.mask(&mut text));
        let mut parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let output: serde_json::Value =
            serde_json::from_str(parsed["output"].take().as_str().unwrap()).unwrap();
        assert_eq!(
            output,
            serde_json::json!({ "secret_id": REDACTED_SECRET, "note": "ok" })
        );
        assert_eq!(
            parsed,
            serde_json::json!({
                "output": null,
                "session_id": REDACTED_SECRET,
                "id": { "token": REDACTED_SECRET },
                "slug": "x-[REDACTED_SECRET]",
            })
        );
    }

    /// An answer keeps a key-named field only when its value is one of the
    /// keys given — the store's own — and masks the same name anywhere else,
    /// including in a tool's result inside the answer.
    #[test]
    fn an_answer_keeps_only_the_keys_it_is_given() {
        let values = vec!["7391".to_string()];
        let mut answer = serde_json::json!({
            "topics": [{ "id": "a-7391", "slug": "topic-7391", "name": "Topic 7391" }],
            "result": { "secret_id": "7391", "id": "b-7391" },
            "output": r#"{"slug":"topic-7391"}"#,
        });
        assert_eq!(
            answer_key_candidates(&answer, &values),
            vec![
                "7391".to_string(),
                "a-7391".into(),
                "b-7391".into(),
                "topic-7391".into()
            ]
        );
        let keys = HashSet::from(["a-7391".to_string(), "topic-7391".to_string()]);
        assert!(mask_answer(&mut answer, &values, &keys));
        assert_eq!(
            answer,
            serde_json::json!({
                "topics": [{ "id": "a-7391", "slug": "topic-7391", "name": "Topic [REDACTED_SECRET]" }],
                "result": { "secret_id": REDACTED_SECRET, "id": "b-[REDACTED_SECRET]" },
                "output": r#"{"slug":"topic-[REDACTED_SECRET]"}"#,
            })
        );
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
