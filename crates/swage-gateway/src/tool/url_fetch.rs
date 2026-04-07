//! URL fetch tool — fetches web page content and extracts readable text.
//!
//! HTML responses are converted to plain text. JSON and plain text are
//! returned as-is. Binary content types are rejected. Output is truncated
//! to the configured byte limit.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;

use crate::tool::Tool;
use crate::tool::workspace::truncate_output;

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct UrlFetchTool {
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

impl Default for UrlFetchTool {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_output_bytes: 100_000,
        }
    }
}

#[async_trait]
impl Tool for UrlFetchTool {
    fn name(&self) -> &str {
        "url_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL and return its content as readable text. HTML is converted to plain text. Static content only — no JavaScript rendering."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch."
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, params: Value, _ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let url = params["url"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "Missing required argument: url".to_owned(),
            })?
            .to_string();

        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .user_agent(concat!(
                "Swage/",
                env!("CARGO_PKG_VERSION"),
                " (url_fetch tool)"
            ))
            .build()
            .map_err(|e| ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Failed to build HTTP client: {e}"),
            })?;

        let mut response = client.get(&url).send().await.map_err(|e| {
            let message = if e.is_timeout() {
                format!("Request timed out after {}s: {url}", self.timeout.as_secs())
            } else if e.is_connect() || e.is_request() {
                format!("Failed to reach {url}: {e}")
            } else {
                format!("Request failed for {url}: {e}")
            };
            ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message,
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("HTTP {status}: {url}\n{body}"),
            });
        }

        // Determine content type before consuming the body.
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();

        // Stream the response body with a running byte counter, stopping when
        // max_output_bytes is reached. Avoids buffering the entire response
        // before the cap is applied — large fast responses could exhaust RAM.
        let mut body_bytes: Vec<u8> = Vec::new();
        loop {
            let chunk = response
                .chunk()
                .await
                .map_err(|e| ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Failed to read response body: {e}"),
                })?;
            let Some(chunk) = chunk else { break };
            let remaining = self.max_output_bytes.saturating_sub(body_bytes.len());
            if remaining == 0 {
                // Cap reached — stop collecting and drop the rest of the body.
                break;
            }
            let to_take = chunk.len().min(remaining);
            body_bytes.extend_from_slice(&chunk[..to_take]);
        }

        // Dispatch on content type.
        let text = if content_type.contains("text/html") {
            // Convert HTML to readable plain text.
            html_to_text(&body_bytes)
        } else if content_type.contains("text/") || content_type.contains("application/json") {
            // Plain text and JSON can be returned as-is.
            String::from_utf8_lossy(&body_bytes).into_owned()
        } else {
            // Binary content types — reject with a clear message.
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Unsupported content type: {content_type}"),
            });
        };

        let (content, _truncated, _total_bytes) = truncate_output(&text, self.max_output_bytes);

        Ok(json!({"output": content}))
    }
}

/// Convert HTML bytes to readable plain text using html2text.
///
/// We use a wide render width to avoid forced line breaks that would fragment
/// URLs and code snippets. The output is reader-mode style — navigation,
/// scripts, and style elements are stripped automatically.
fn html_to_text(bytes: &[u8]) -> String {
    // html2text::from_read expects a `Read` impl and a terminal width in chars.
    // 120 is wide enough to avoid wrapping most prose and code blocks.
    html2text::from_read(bytes, 120)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use swage_tool::context::MockToolContext;

    fn tool() -> UrlFetchTool {
        UrlFetchTool {
            timeout: Duration::from_secs(10),
            max_output_bytes: 1_000_000,
        }
    }

    fn ctx() -> MockToolContext {
        MockToolContext::new("agent-test", "sess-test")
    }

    #[tokio::test]
    async fn returns_error_for_missing_url() {
        let err = tool().execute(json!({}), &ctx()).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidParameters { .. }));
        assert!(err.to_string().contains("url"), "got: {err}");
    }

    #[tokio::test]
    async fn tool_metadata() {
        let t = tool();
        assert_eq!(t.name(), "url_fetch");
        assert!(t.description().contains("HTML"));
        let params = t.parameters();
        assert!(params["properties"]["url"].is_object());
        assert_eq!(params["required"][0].as_str(), Some("url"));
    }

    #[tokio::test]
    async fn times_out_on_unreachable_host() {
        let t = UrlFetchTool {
            // Use a very short timeout so the test doesn't hang.
            timeout: Duration::from_millis(50),
            max_output_bytes: 1_000_000,
        };
        // RFC 5737 TEST-NET — guaranteed not routable, connection will time out.
        let err = t
            .execute(json!({ "url": "http://192.0.2.1/test" }), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        // Should mention a timeout or connection failure, not panic.
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn html_to_text_strips_tags() {
        let html = b"<html><body><h1>Hello</h1><p>World</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Hello"), "got: {text}");
        assert!(text.contains("World"), "got: {text}");
        assert!(!text.contains("<h1>"), "tags should be stripped: {text}");
    }

    #[test]
    fn html_to_text_handles_empty() {
        let text = html_to_text(b"");
        // Should not panic — empty or whitespace output is fine.
        let _ = text;
    }

    #[tokio::test]
    async fn truncates_large_response() {
        // Test truncation logic directly via the truncate_output helper which
        // url_fetch delegates to — the real HTTP path can't be exercised in unit tests.
        let large = "a".repeat(200);
        let (out, truncated, total) = truncate_output(&large, 100);
        assert_eq!(out.len(), 100);
        assert!(truncated);
        assert_eq!(total, 200);
    }
}
