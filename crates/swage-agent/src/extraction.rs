//! JSON extraction from LLM response text.
//!
//! LLMs commonly produce JSON wrapped in markdown fences, preceded by prose,
//! or with trailing explanation. This module extracts the JSON payload from
//! common wrapping patterns before schema validation.

/// Extract a JSON value from LLM response text.
///
/// Attempts extraction in order, returning the first successful result:
///
/// 1. **Bare JSON** — if the trimmed text is valid JSON, return it directly.
///    This is the cheapest path and handles the common case where the model
///    follows instructions precisely.
///
/// 2. **Markdown fence** — if the text contains a triple-backtick fence
///    (` ```json ` or ` ``` `), extract the content between the first opening
///    and closing fence markers and attempt to parse it as JSON.
///
/// Returns `None` when neither strategy succeeds — for example, when the
/// response is plain prose, contains only a partial JSON fragment mid-sentence,
/// or the fenced block content is not valid JSON.
pub fn extract_json(text: &str) -> Option<serde_json::Value> {
    let trimmed = text.trim();

    // Strategy 1: bare JSON — cheapest, try first.
    if let Ok(v) = serde_json::from_str(trimmed) {
        return Some(v);
    }

    // Strategy 2: markdown fence extraction.
    extract_from_fence(text)
}

/// Extract JSON from the content of the first markdown code fence in `text`.
///
/// Recognises both ` ```json ` and plain ` ``` ` opening markers. Extracts
/// the content between the first opening fence and the next closing ` ``` `
/// marker, then attempts JSON parsing on the trimmed inner text.
///
/// Closing fences are matched only at the start of a line (preceded by `\n`)
/// to avoid treating backtick sequences embedded in string content as a fence.
fn extract_from_fence(text: &str) -> Option<serde_json::Value> {
    // Find the first opening fence (``` optionally followed by a language tag).
    let fence_start = text.find("```")?;
    let after_open = &text[fence_start + 3..];

    // Skip the optional language tag on the opening line (e.g. "json\n").
    let content_start = after_open.find('\n').map(|i| i + 1)?;
    let inner = &after_open[content_start..];

    // Find the closing fence — ``` at the start of a line (\n```).
    // Matching line-start only avoids false positives from backtick sequences
    // embedded inside JSON string values (e.g. `{"code": "```nested```"}`).
    let fence_end = inner.find("\n```").or_else(|| {
        // Edge case: closing fence at the very start of inner (no preceding newline).
        if inner.starts_with("```") { Some(0) } else { None }
    })?;

    let block = inner[..fence_end].trim();
    serde_json::from_str(block).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Bare JSON
    // -----------------------------------------------------------------------

    #[test]
    fn bare_json_object() {
        let result = extract_json(r#"{"x": 1}"#);
        assert_eq!(result, Some(serde_json::json!({"x": 1})));
    }

    #[test]
    fn bare_json_array() {
        let result = extract_json(r#"[1, 2, 3]"#);
        assert_eq!(result, Some(serde_json::json!([1, 2, 3])));
    }

    #[test]
    fn bare_json_with_leading_trailing_whitespace() {
        let result = extract_json("  { \"x\": 1 }  ");
        assert_eq!(result, Some(serde_json::json!({"x": 1})));
    }

    // -----------------------------------------------------------------------
    // Markdown-fenced JSON
    // -----------------------------------------------------------------------

    #[test]
    fn fenced_json_with_language_tag() {
        let input = "```json\n{\"x\": 1}\n```";
        assert_eq!(extract_json(input), Some(serde_json::json!({"x": 1})));
    }

    #[test]
    fn fenced_json_without_language_tag() {
        let input = "```\n{\"x\": 1}\n```";
        assert_eq!(extract_json(input), Some(serde_json::json!({"x": 1})));
    }

    #[test]
    fn fenced_json_with_surrounding_prose() {
        let input = "Here is the result:\n```json\n{\"name\": \"alice\"}\n```\nHope that helps!";
        assert_eq!(extract_json(input), Some(serde_json::json!({"name": "alice"})));
    }

    #[test]
    fn fenced_json_array() {
        let input = "```json\n[1, 2, 3]\n```";
        assert_eq!(extract_json(input), Some(serde_json::json!([1, 2, 3])));
    }

    // -----------------------------------------------------------------------
    // None cases
    // -----------------------------------------------------------------------

    #[test]
    fn prose_with_no_json_returns_none() {
        assert_eq!(extract_json("Hello, this is plain text."), None);
    }

    #[test]
    fn empty_string_returns_none() {
        assert_eq!(extract_json(""), None);
    }

    #[test]
    fn whitespace_only_returns_none() {
        assert_eq!(extract_json("   \n  "), None);
    }

    #[test]
    fn prose_with_embedded_fragment_returns_none() {
        // Prose embedding a JSON fragment mid-sentence is too ambiguous to extract.
        // The fragment is not at the start/end and not in a fence — return None.
        assert_eq!(extract_json("The answer is {\"x\": 1} as you can see."), None);
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn fenced_block_with_only_whitespace_inside_returns_none() {
        let input = "```json\n   \n```";
        assert_eq!(extract_json(input), None);
    }

    #[test]
    fn nested_backtick_fence_uses_first_pair() {
        // If a fence block contains another fence, extract the outermost content.
        // The inner "```" is treated as content, not a closing fence.
        let input = "```json\n{\"code\": \"```nested```\"}\n```";
        // The content inside the first fence is valid JSON, so it should parse.
        let result = extract_json(input);
        assert!(result.is_some(), "nested fence should still extract valid JSON");
    }
}
