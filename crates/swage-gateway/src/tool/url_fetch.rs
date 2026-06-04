//! URL fetch tool — fetches web page content and extracts readable text.
//!
//! HTML responses are converted to plain text. JSON and plain text are
//! returned as-is. Binary content types are rejected. Output is truncated
//! to the configured byte limit.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::ToolError;
use swage_tool::context::ToolContext;
use tokio::io::AsyncWriteExt;

use crate::tool::Tool;
use crate::tool::workspace::{
    continuable_output_value, create_tool_output_artifact, tool_output_artifact_path,
    truncated_output_value,
};

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

pub struct UrlFetchTool {
    pub timeout: Duration,
    pub max_output_bytes: usize,
}

const BLOCKED_HOSTNAMES: &[&str] = &["localhost", "metadata.google.internal"];
const RAW_HTML_ARTIFACT_FILENAME: &str = "raw-body.html";

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
                },
                "offset_bytes": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Byte offset into a text or JSON response. Use next_offset_bytes from a truncated result to continue."
                }
            },
            "required": ["url"]
        })
    }

    fn requires_host_execution(&self) -> bool {
        true
    }

    async fn execute(&self, params: Value, ctx: &dyn ToolContext) -> Result<Value, ToolError> {
        let url = params["url"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "Missing required argument: url".to_owned(),
            })?
            .to_string();
        let offset_bytes = optional_offset_bytes(&params, self.name())?;

        let parsed_url = parse_fetch_url(&url, self.name())?;
        ensure_url_target_is_safe(&parsed_url, self.name()).await?;

        let client = reqwest::Client::builder()
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
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

        let mut response = client.get(parsed_url.clone()).send().await.map_err(|e| {
            let message = if e.is_timeout() {
                format!(
                    "Request timed out after {}s: {}",
                    self.timeout.as_secs(),
                    parsed_url
                )
            } else if e.is_connect() || e.is_request() {
                format!("Failed to reach {parsed_url}: {e}")
            } else {
                format!("Request failed for {parsed_url}: {e}")
            };
            ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message,
            }
        })?;
        ensure_connected_peer_is_safe(response.remote_addr(), self.name())?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("HTTP {status}: {parsed_url}\n{body}"),
            });
        }

        // Determine content type before consuming the body.
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();

        let is_html = content_type.contains("text/html");
        let is_text_response =
            content_type.contains("text/") || content_type.contains("application/json");
        if offset_bytes > 0 && is_html {
            return Err(ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message: "offset_bytes is only supported for text and JSON responses".to_owned(),
            });
        }

        // Stream the response body with a running byte counter. Store the page
        // needed for the requested offset, but keep draining under the request
        // timeout so returned metadata can say that bytes were omitted.
        let capture_limit = if is_html {
            self.max_output_bytes
        } else {
            offset_bytes.saturating_add(self.max_output_bytes)
        };
        let raw_body_artifact_path =
            is_html.then(|| tool_output_artifact_path(ctx, RAW_HTML_ARTIFACT_FILENAME));
        let mut raw_body_artifact_writer = match raw_body_artifact_path.as_ref() {
            Some(path) => Some(create_tool_output_artifact(&path.absolute).await.map_err(
                |error| ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Failed to create raw HTML artifact: {error}"),
                },
            )?),
            None => None,
        };
        let mut body_bytes: Vec<u8> = Vec::new();
        let mut total_body_bytes = 0usize;
        loop {
            let chunk = response
                .chunk()
                .await
                .map_err(|e| ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Failed to read response body: {e}"),
                })?;
            let Some(chunk) = chunk else { break };
            if let Some(writer) = raw_body_artifact_writer.as_mut() {
                writer
                    .write_all(&chunk)
                    .await
                    .map_err(|error| ToolError::ExecutionFailed {
                        name: self.name().to_owned(),
                        message: format!("Failed to write raw HTML artifact: {error}"),
                    })?;
            }
            total_body_bytes += chunk.len();
            let remaining = capture_limit.saturating_sub(body_bytes.len());
            let to_take = chunk.len().min(remaining);
            body_bytes.extend_from_slice(&chunk[..to_take]);
        }
        if let Some(writer) = raw_body_artifact_writer.as_mut() {
            writer
                .flush()
                .await
                .map_err(|error| ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Failed to write raw HTML artifact: {error}"),
                })?;
        }
        drop(raw_body_artifact_writer);
        let raw_body_artifact_relative_path = if total_body_bytes > body_bytes.len() {
            raw_body_artifact_path.map(|path| path.relative)
        } else {
            if let Some(path) = raw_body_artifact_path {
                let _ = tokio::fs::remove_file(path.absolute).await;
            }
            None
        };

        // Dispatch on content type.
        let text = if is_html {
            // Convert HTML to readable plain text.
            html_to_text(&body_bytes)
        } else if is_text_response {
            // Plain text and JSON can be returned as-is.
            String::from_utf8_lossy(&body_bytes).into_owned()
        } else {
            // Binary content types — reject with a clear message.
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Unsupported content type: {content_type}"),
            });
        };

        if is_html {
            Ok(fetch_html_output_value(
                &text,
                self.max_output_bytes,
                total_body_bytes,
                body_bytes.len(),
                raw_body_artifact_relative_path.as_deref(),
            ))
        } else {
            fetch_text_output_value(
                &text,
                &url,
                self.max_output_bytes,
                offset_bytes,
                total_body_bytes,
            )
            .map_err(|message| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message,
            })
        }
    }
}

fn optional_offset_bytes(params: &Value, tool_name: &str) -> Result<usize, ToolError> {
    match params.get("offset_bytes") {
        Some(Value::Number(number)) => {
            let offset = number
                .as_u64()
                .ok_or_else(|| ToolError::InvalidParameters {
                    name: tool_name.to_owned(),
                    message: "offset_bytes must be a non-negative integer".to_owned(),
                })?;
            usize::try_from(offset).map_err(|_| ToolError::InvalidParameters {
                name: tool_name.to_owned(),
                message: "offset_bytes is too large for this platform".to_owned(),
            })
        }
        Some(Value::Null) | None => Ok(0),
        Some(_) => Err(ToolError::InvalidParameters {
            name: tool_name.to_owned(),
            message: "offset_bytes must be a non-negative integer".to_owned(),
        }),
    }
}

fn fetch_html_output_value(
    text: &str,
    max_output_bytes: usize,
    total_body_bytes: usize,
    stored_body_bytes: usize,
    raw_body_artifact_path: Option<&str>,
) -> Value {
    let mut value = truncated_output_value(text, max_output_bytes);
    if total_body_bytes > stored_body_bytes {
        value["truncated"] = json!(true);
        value["total_bytes"] = json!(total_body_bytes);
    }
    if let Some(path) = raw_body_artifact_path {
        value["raw_body_artifact_path"] = json!(path);
        value["raw_body_follow_up"] = file_read_follow_up(path);
    }
    value
}

fn fetch_text_output_value(
    text: &str,
    url: &str,
    max_output_bytes: usize,
    offset_bytes: usize,
    total_body_bytes: usize,
) -> Result<Value, String> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("url".to_string(), json!(url));
    let mut value =
        continuable_output_value(text, max_output_bytes, offset_bytes, "url_fetch", arguments)?;

    let output_len = value
        .get("output")
        .and_then(Value::as_str)
        .map(str::len)
        .unwrap_or(0);
    let next_offset = offset_bytes + output_len;
    if total_body_bytes > next_offset {
        let object = value
            .as_object_mut()
            .expect("continuable_output_value always returns an object");
        object.insert("truncated".to_string(), json!(true));
        object.insert("offset_bytes".to_string(), json!(offset_bytes));
        object.insert("total_bytes".to_string(), json!(total_body_bytes));
        object.insert("next_offset_bytes".to_string(), json!(next_offset));
        object.insert(
            "follow_up".to_string(),
            json!({
                "tool": "url_fetch",
                "arguments": {
                    "url": url,
                    "offset_bytes": next_offset,
                },
            }),
        );
    }

    Ok(value)
}

fn file_read_follow_up(path: &str) -> Value {
    json!({
        "tool": "file_read",
        "arguments": {
            "path": path
        }
    })
}

fn parse_fetch_url(url: &str, tool_name: &str) -> Result<reqwest::Url, ToolError> {
    let parsed = reqwest::Url::parse(url).map_err(|error| ToolError::InvalidParameters {
        name: tool_name.to_owned(),
        message: format!("Invalid URL: {error}"),
    })?;

    match parsed.scheme() {
        "http" | "https" => Ok(parsed),
        scheme => Err(ToolError::InvalidParameters {
            name: tool_name.to_owned(),
            message: format!("Unsupported URL scheme: {scheme}"),
        }),
    }
}

async fn ensure_url_target_is_safe(
    parsed_url: &reqwest::Url,
    tool_name: &str,
) -> Result<(), ToolError> {
    let host = parsed_url
        .host_str()
        .ok_or_else(|| ToolError::InvalidParameters {
            name: tool_name.to_owned(),
            message: "URL must include a host".to_owned(),
        })?;

    let port = parsed_url
        .port_or_known_default()
        .ok_or_else(|| ToolError::InvalidParameters {
            name: tool_name.to_owned(),
            message: "URL must include a recognised port".to_owned(),
        })?;

    if is_blocked_hostname(host) {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_owned(),
            message: format!("Blocked private or metadata destination: {host}"),
        });
    }

    if let Ok(ip) = host.parse::<IpAddr>() {
        return ensure_public_ip(ip, tool_name);
    }

    let resolved = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| ToolError::ExecutionFailed {
            name: tool_name.to_owned(),
            message: format!("Failed to resolve {host}: {error}"),
        })?;

    let mut saw_address = false;
    for addr in resolved {
        saw_address = true;
        ensure_public_ip(addr.ip(), tool_name)?;
    }

    if !saw_address {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_owned(),
            message: format!("Failed to resolve {host}: no addresses returned"),
        });
    }

    Ok(())
}

fn is_blocked_hostname(host: &str) -> bool {
    BLOCKED_HOSTNAMES
        .iter()
        .any(|blocked| host.eq_ignore_ascii_case(blocked))
}

fn ensure_public_ip(ip: IpAddr, tool_name: &str) -> Result<(), ToolError> {
    if is_private_or_metadata_ip(ip) {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_owned(),
            message: format!("Blocked private or metadata destination: {ip}"),
        });
    }
    Ok(())
}

fn ensure_connected_peer_is_safe(
    remote_addr: Option<SocketAddr>,
    tool_name: &str,
) -> Result<(), ToolError> {
    let Some(remote_addr) = remote_addr else {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_owned(),
            message: "Could not verify the connected peer address".to_owned(),
        });
    };

    ensure_public_ip(remote_addr.ip(), tool_name)
}

fn is_private_or_metadata_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_private_or_metadata_ipv4(ip),
        IpAddr::V6(ip) => is_private_or_metadata_ipv6(ip),
    }
}

fn is_private_or_metadata_ipv4(ip: Ipv4Addr) -> bool {
    if ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
    {
        return true;
    }

    let octets = ip.octets();
    matches!(octets, [100, second, ..] if (64..=127).contains(&second))
        || matches!(octets, [198, second, ..] if second == 18 || second == 19)
        || matches!(octets, [192, 0, 0, ..])
}

fn is_private_or_metadata_ipv6(ip: Ipv6Addr) -> bool {
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
}

/// Convert HTML bytes to readable plain text using html2text.
///
/// We use a wide render width to avoid forced line breaks that would fragment
/// URLs and code snippets. The output is reader-mode style — navigation,
/// scripts, and style elements are stripped automatically.
fn html_to_text(bytes: &[u8]) -> String {
    // html2text::from_read expects a `Read` impl and a terminal width in chars.
    // 120 is wide enough to avoid wrapping most prose and code blocks.
    html2text::from_read(bytes, 120).unwrap_or_else(|_| String::from_utf8_lossy(bytes).into_owned())
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
    async fn rejects_private_ipv4_targets() {
        let err = tool()
            .execute(json!({ "url": "http://127.0.0.1/test" }), &ctx())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(
            err.to_string()
                .contains("Blocked private or metadata destination")
        );
    }

    #[tokio::test]
    async fn rejects_metadata_hostname_targets() {
        let err = tool()
            .execute(
                json!({ "url": "http://metadata.google.internal/computeMetadata/v1" }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ExecutionFailed { .. }));
        assert!(
            err.to_string()
                .contains("Blocked private or metadata destination")
        );
    }

    #[tokio::test]
    async fn tool_metadata() {
        let t = tool();
        assert_eq!(t.name(), "url_fetch");
        assert!(t.description().contains("HTML"));
        let params = t.parameters();
        assert!(params["properties"]["url"].is_object());
        assert!(params["properties"]["offset_bytes"].is_object());
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

    #[test]
    fn private_or_metadata_ip_detection_covers_reserved_ranges() {
        assert!(is_private_or_metadata_ip(IpAddr::V4(Ipv4Addr::new(
            10, 0, 0, 1
        ))));
        assert!(is_private_or_metadata_ip(IpAddr::V4(Ipv4Addr::new(
            169, 254, 169, 254
        ))));
        assert!(is_private_or_metadata_ip(IpAddr::V4(Ipv4Addr::new(
            198, 19, 0, 1
        ))));
        assert!(!is_private_or_metadata_ip(IpAddr::V4(Ipv4Addr::new(
            93, 184, 216, 34
        ))));
    }

    #[test]
    fn connected_peer_validation_rejects_private_address() {
        let err = ensure_connected_peer_is_safe(
            Some(SocketAddr::from(([127, 0, 0, 1], 80))),
            "url_fetch",
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("Blocked private or metadata destination")
        );
    }

    #[test]
    fn connected_peer_validation_rejects_missing_remote_address() {
        let err = ensure_connected_peer_is_safe(None, "url_fetch").unwrap_err();
        assert!(
            err.to_string()
                .contains("Could not verify the connected peer address")
        );
    }

    #[tokio::test]
    async fn truncates_large_response() {
        let capped_text = "a".repeat(100);
        let value = fetch_html_output_value(&capped_text, 100, 200, 100, None);

        assert_eq!(value["output"].as_str().unwrap().len(), 100);
        assert_eq!(value["truncated"], json!(true));
        assert_eq!(value["total_bytes"], json!(200));
    }

    #[test]
    fn html_fetch_output_returns_raw_body_follow_up() {
        let value = fetch_html_output_value(
            "Rendered text",
            100,
            200,
            100,
            Some(".swage/tool-output/turn/call/raw-body.html"),
        );

        assert_eq!(
            value["raw_body_artifact_path"],
            json!(".swage/tool-output/turn/call/raw-body.html")
        );
        assert_eq!(value["raw_body_follow_up"]["tool"], json!("file_read"));
        assert_eq!(
            value["raw_body_follow_up"]["arguments"]["path"],
            json!(".swage/tool-output/turn/call/raw-body.html")
        );
    }

    #[test]
    fn text_fetch_output_returns_follow_up_offset() {
        let value = fetch_text_output_value("abcdefghij", "https://example.com/data.txt", 4, 0, 10)
            .expect("continuable text output");

        assert_eq!(value["output"], json!("abcd"));
        assert_eq!(value["truncated"], json!(true));
        assert_eq!(value["total_bytes"], json!(10));
        assert_eq!(value["offset_bytes"], json!(0));
        assert_eq!(value["next_offset_bytes"], json!(4));
        assert_eq!(value["follow_up"]["tool"], json!("url_fetch"));
        assert_eq!(
            value["follow_up"]["arguments"]["url"],
            json!("https://example.com/data.txt")
        );
        assert_eq!(value["follow_up"]["arguments"]["offset_bytes"], json!(4));
    }

    #[test]
    fn offset_text_fetch_output_continues_from_prior_truncation() {
        let value = fetch_text_output_value("abcdefghij", "https://example.com/data.txt", 4, 4, 10)
            .expect("continuable text output");

        assert_eq!(value["output"], json!("efgh"));
        assert_eq!(value["truncated"], json!(true));
        assert_eq!(value["total_bytes"], json!(10));
        assert_eq!(value["offset_bytes"], json!(4));
        assert_eq!(value["next_offset_bytes"], json!(8));
    }

    #[test]
    fn small_text_fetch_output_keeps_simple_shape() {
        let value = fetch_text_output_value("abc", "https://example.com/data.txt", 100, 0, 3)
            .expect("small text output");

        assert_eq!(value, json!({ "output": "abc" }));
    }
}
