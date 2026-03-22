//! URL fetch tool — fetches web page content and extracts readable text.
//!
//! HTML responses are converted to plain text. JSON and plain text are
//! returned as-is. Binary content types are rejected. Output is truncated
//! to the configured byte limit.

use std::time::Duration;
use async_trait::async_trait;
use serde_json::json;
use crate::tool::{Tool, ToolExecutionResult};
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

    fn parameters(&self) -> serde_json::Value {
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

    async fn execute(&self, arguments: serde_json::Value) -> ToolExecutionResult {
        let url = match arguments["url"].as_str() {
            Some(u) => u.to_string(),
            None => {
                return ToolExecutionResult {
                    content: "Missing required argument: url".into(),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                }
            }
        };

        let client = match reqwest::Client::builder()
            .timeout(self.timeout)
            .user_agent(concat!(
                "Swage/",
                env!("CARGO_PKG_VERSION"),
                " (url_fetch tool)"
            ))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                return ToolExecutionResult {
                    content: format!("Failed to build HTTP client: {e}"),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                }
            }
        };

        let response = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                let msg = if e.is_timeout() {
                    format!(
                        "Request timed out after {}s: {url}",
                        self.timeout.as_secs()
                    )
                } else if e.is_connect() || e.is_request() {
                    format!("Failed to reach {url}: {e}")
                } else {
                    format!("Request failed for {url}: {e}")
                };
                return ToolExecutionResult {
                    content: msg,
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                };
            }
        };

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_default();
            return ToolExecutionResult {
                content: format!("HTTP {status}: {url}\n{body}"),
                is_error: true,
                truncated: false,
                total_bytes: None,
            };
        }

        // Determine content type before consuming the body.
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();

        let body_bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                return ToolExecutionResult {
                    content: format!("Failed to read response body: {e}"),
                    is_error: true,
                    truncated: false,
                    total_bytes: None,
                }
            }
        };

        // Dispatch on content type.
        let text = if content_type.contains("text/html") {
            // Convert HTML to readable plain text.
            html_to_text(&body_bytes)
        } else if content_type.contains("text/") || content_type.contains("application/json") {
            // Plain text and JSON can be returned as-is.
            String::from_utf8_lossy(&body_bytes).into_owned()
        } else {
            // Binary content types — reject with a clear message.
            return ToolExecutionResult {
                content: format!("Unsupported content type: {content_type}"),
                is_error: true,
                truncated: false,
                total_bytes: None,
            };
        };

        let (content, truncated, total_bytes) = truncate_output(&text, self.max_output_bytes);

        ToolExecutionResult {
            content,
            is_error: false,
            truncated,
            total_bytes: if truncated { Some(total_bytes) } else { None },
        }
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

    fn tool() -> UrlFetchTool {
        UrlFetchTool {
            timeout: Duration::from_secs(10),
            max_output_bytes: 1_000_000,
        }
    }

    fn tool_with_limit(limit: usize) -> UrlFetchTool {
        UrlFetchTool {
            timeout: Duration::from_secs(10),
            max_output_bytes: limit,
        }
    }

    #[tokio::test]
    async fn returns_error_for_missing_url() {
        let result = tool().execute(json!({})).await;
        assert!(result.is_error);
        assert!(result.content.contains("url"), "got: {}", result.content);
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
        let result = t.execute(json!({ "url": "http://192.0.2.1/test" })).await;
        assert!(result.is_error);
        // Should mention a timeout or connection failure, not panic.
        assert!(!result.content.is_empty());
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
