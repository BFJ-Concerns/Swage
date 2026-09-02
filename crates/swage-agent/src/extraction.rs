//! JSON extraction from LLM response text.
//!
//! LLMs commonly produce JSON wrapped in markdown fences, preceded by prose,
//! or with trailing explanation. This module finds every JSON value in that
//! text so schema validation can choose the final value that fulfils its
//! contract.

/// Return every JSON value that starts in the response, in source order.
///
///
/// The scan deliberately does not treat Markdown fences as syntax. JSON's
/// grammar is sufficient to recognise values in bare text and fenced blocks
/// alike, while preserving their order in the complete reply.
pub fn extract_json_values(text: &str) -> Vec<serde_json::Value> {
    text.char_indices()
        .filter_map(|(offset, character)| {
            matches!(
                character,
                '{' | '[' | '"' | '-' | '0'..='9' | 't' | 'f' | 'n'
            )
            .then(|| {
                serde_json::Deserializer::from_str(&text[offset..])
                    .into_iter::<serde_json::Value>()
                    .next()
                    .and_then(Result::ok)
            })
            .flatten()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Bare JSON
    // -----------------------------------------------------------------------

    #[test]
    fn bare_json_object() {
        let result = extract_json_values(r#"{"x": 1}"#);
        assert_eq!(
            result,
            vec![serde_json::json!({"x": 1}), serde_json::json!(1)]
        );
    }

    #[test]
    fn bare_json_array() {
        let result = extract_json_values(r#"[1, 2, 3]"#);
        assert_eq!(
            result,
            vec![
                serde_json::json!([1, 2, 3]),
                serde_json::json!(1),
                serde_json::json!(2),
                serde_json::json!(3),
            ]
        );
    }

    #[test]
    fn bare_json_with_leading_trailing_whitespace() {
        let result = extract_json_values("  { \"x\": 1 }  ");
        assert_eq!(result[0], serde_json::json!({"x": 1}));
    }

    // -----------------------------------------------------------------------
    // Prose and Markdown-fenced JSON
    // -----------------------------------------------------------------------

    #[test]
    fn fenced_json_with_language_tag() {
        let input = "```json\n{\"x\": 1}\n```";
        assert!(extract_json_values(input).contains(&serde_json::json!({"x": 1})));
    }

    #[test]
    fn fenced_json_without_language_tag() {
        let input = "```\n{\"x\": 1}\n```";
        assert!(extract_json_values(input).contains(&serde_json::json!({"x": 1})));
    }

    #[test]
    fn fenced_json_with_surrounding_prose() {
        let input = "Here is the result:\n```json\n{\"name\": \"alice\"}\n```\nHope that helps!";
        assert_eq!(
            extract_json_values(input),
            vec![
                serde_json::json!({"name": "alice"}),
                serde_json::json!("alice"),
            ]
        );
    }

    #[test]
    fn fenced_json_array() {
        let input = "```json\n[1, 2, 3]\n```";
        assert!(extract_json_values(input).contains(&serde_json::json!([1, 2, 3])));
    }

    // -----------------------------------------------------------------------
    // None cases
    // -----------------------------------------------------------------------

    #[test]
    fn prose_with_no_json_returns_none() {
        assert!(extract_json_values("Hello, this is plain text.").is_empty());
    }

    #[test]
    fn empty_string_returns_none() {
        assert!(extract_json_values("").is_empty());
    }

    #[test]
    fn whitespace_only_returns_none() {
        assert!(extract_json_values("   \n  ").is_empty());
    }

    #[test]
    fn prose_with_embedded_fragment_returns_none() {
        assert_eq!(
            extract_json_values("The answer is {\"x\": 1} as you can see."),
            vec![serde_json::json!({"x": 1}), serde_json::json!(1)]
        );
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn fenced_block_with_only_whitespace_inside_returns_none() {
        let input = "```json\n   \n```";
        assert!(extract_json_values(input).is_empty());
    }

    #[test]
    fn nested_backtick_fence_uses_first_pair() {
        let input = "```json\n{\"code\": \"```nested```\"}\n```";
        let result = extract_json_values(input);
        assert!(
            result.contains(&serde_json::json!({"code": "```nested```"})),
            "nested fence should still extract valid JSON"
        );
    }

    #[test]
    fn scans_every_value_in_source_order() {
        let values = extract_json_values(
            "preface {\"phase\": \"draft\"} then ```json\n{\"phase\": \"final\"}\n```",
        );

        assert_eq!(
            values
                .into_iter()
                .filter(|value| value.get("phase").is_some())
                .collect::<Vec<_>>(),
            vec![
                serde_json::json!({"phase": "draft"}),
                serde_json::json!({"phase": "final"}),
            ]
        );
    }
}
