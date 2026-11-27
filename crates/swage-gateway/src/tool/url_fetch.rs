//! URL fetch tool — fetches web surface content and extracts readable text.
//!
//! HTML responses are converted to plain text. JSON and plain text are
//! returned as-is. Binary content types are rejected. Output is truncated
//! to the configured byte limit.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_core::{ToolError, tool_result_reference};
use swage_tool::context::ToolContext;
use tokio::io::AsyncWriteExt;
use tokio::time::{Instant, timeout_at};

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

const MAX_REDIRECTS: usize = 10;
const BLOCKED_HOSTNAMES: &[&str] = &["localhost", "metadata.google.internal"];
const RAW_HTML_ARTIFACT_FILENAME: &str = "raw-body.html";
const RAW_TEXT_ARTIFACT_FILENAME: &str = "raw-body.txt";

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

    fn display_hints(&self) -> swage_tool::tool::ToolDisplayHints {
        swage_tool::tool::ToolDisplayHints {
            summary_param: Some("url"),
        }
    }

    fn description(&self) -> &str {
        "Fetch a web address over HTTP or HTTPS and return what it serves as text, in \
         `output`. An HTML page is reduced to its readable text, plain text and JSON come \
         back as they are, and any other kind of content is refused. The page is taken as \
         served, without running its scripts, so a page that builds itself in the browser \
         comes back with little in it; the `browser_*` tools, where they are offered, read \
         such a page. An address on this machine or a private network is refused. A \
         redirect is followed, each address it leads to meeting the same checks as the one \
         given, and `final_url` in the result is the address that answered in the end. The \
         call fails, naming the address it stopped at, when a redirect leads to a refused \
         address, when redirects go on too long, or when the last response is not a success. \
         A long response is cut and marked `truncated`. Text and JSON \
         carry on from `next_offset_bytes`; an HTML page does not, but every result names, \
         in `raw_body_artifact_path`, a file holding everything that was fetched, which \
         `file_read` opens. Where there is no address to give, `web_search` finds one."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The address to fetch, complete with its `http://` or `https://` scheme."
                },
                "offset_bytes": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Where to carry on a cut text or JSON response: its `next_offset_bytes`, given with the same `url`, which is fetched again. An HTML page cannot be carried on this way."
                }
            },
            "required": ["url"]
        })
    }

    fn requires_device_execution(&self) -> bool {
        true
    }

    fn validate_call(&self, params: &Value) -> Result<(), ToolError> {
        optional_offset_bytes(params, self.name())?;
        if let Some(url) = params["url"].as_str() {
            parse_fetch_url(url, self.name())?;
        }
        Ok(())
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

        self.fetch_and_retain(client, parsed_url, url, offset_bytes, ctx)
            .await
            .map_err(ToolError::after_execution)
    }
}

impl UrlFetchTool {
    async fn fetch_response(
        &self,
        client: &reqwest::Client,
        mut url: reqwest::Url,
    ) -> Result<reqwest::Response, ToolError> {
        let deadline = Instant::now() + self.timeout;
        let mut redirects = 0;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let response = client
                .get(url.clone())
                .timeout(remaining)
                .send()
                .await
                .map_err(|error| {
                    let message = if error.is_timeout() {
                        self.timeout_message(&url)
                    } else if error.is_connect() || error.is_request() {
                        format!("Failed to reach {url}: {error}")
                    } else {
                        format!("Request failed for {url}: {error}")
                    };
                    self.fetch_error(message)
                })?;
            ensure_connected_peer_is_safe(response.remote_addr(), self.name()).map_err(
                |error| self.fetch_error(format!("Connection to {url} refused: {error}")),
            )?;

            let Some(target) = redirect_target(&response, self.name())? else {
                return Ok(response);
            };
            if redirects == MAX_REDIRECTS {
                return Err(self.fetch_error(format!(
                    "Too many redirects at {url} (limit {MAX_REDIRECTS}); next address: {target}"
                )));
            }
            timeout_at(
                deadline,
                ensure_redirect_target_is_safe(&url, &target, self.name()),
            )
            .await
            .map_err(|_| self.fetch_error(self.timeout_message(&target)))??;
            url = target;
            redirects += 1;
        }
    }

    fn fetch_error(&self, message: String) -> ToolError {
        ToolError::ExecutionFailed {
            name: self.name().to_owned(),
            message,
        }
    }

    fn timeout_message(&self, url: &reqwest::Url) -> String {
        format!("Request timed out after {}s: {url}", self.timeout.as_secs())
    }

    async fn fetch_and_retain(
        &self,
        client: reqwest::Client,
        parsed_url: reqwest::Url,
        url: String,
        offset_bytes: usize,
        ctx: &dyn ToolContext,
    ) -> Result<Value, ToolError> {
        let mut response = self.fetch_response(&client, parsed_url).await?;
        let final_url = response.url().to_string();
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("HTTP {status}: {final_url}\n{body}"),
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

        // Stream the response body with a running byte counter. Store the surface
        // needed for the requested offset, but keep draining under the request
        // timeout so returned metadata can say that bytes were omitted.
        let capture_limit = if is_html {
            self.max_output_bytes
        } else {
            offset_bytes.saturating_add(self.max_output_bytes)
        };
        // Every body is retained as fetched, whatever its size: the file is
        // what a later knowledge_record captures, so the knowledge base holds
        // the bytes the platform fetched and never the model's retelling.
        let raw_body_artifact_path = tool_output_artifact_path(
            ctx,
            if is_html {
                RAW_HTML_ARTIFACT_FILENAME
            } else {
                RAW_TEXT_ARTIFACT_FILENAME
            },
        );
        let mut raw_body_artifact_writer = Some(
            create_tool_output_artifact(ctx, &raw_body_artifact_path.absolute)
                .await
                .map_err(|error| ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Failed to create raw body artifact: {error}"),
                })?,
        );
        let mut body_bytes: Vec<u8> = Vec::new();
        let mut total_body_bytes = 0usize;
        loop {
            let chunk = response
                .chunk()
                .await
                .map_err(|e| ToolError::ExecutionFailed {
                    name: self.name().to_owned(),
                    message: format!("Failed to read response body at {final_url}: {e}"),
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

        // Dispatch on content type.
        let text = if is_html {
            // Convert HTML to readable plain text.
            html_to_text(&body_bytes)
        } else if is_text_response {
            // Plain text and JSON can be returned as-is.
            String::from_utf8_lossy(&body_bytes).into_owned()
        } else {
            // Binary content types — reject with a clear message, and keep
            // nothing: an unsupported body is not a capture candidate.
            let _ = tokio::fs::remove_file(&raw_body_artifact_path.absolute).await;
            return Err(ToolError::ExecutionFailed {
                name: self.name().to_owned(),
                message: format!("Unsupported content type: {content_type}"),
            });
        };

        let retained = RetainedBody {
            artifact_path: &raw_body_artifact_path.reported,
            media_type: &content_type,
            capture: capture_hint(ctx),
        };
        let mut value = if is_html {
            fetch_html_output_value(
                &text,
                self.max_output_bytes,
                total_body_bytes,
                body_bytes.len(),
                &retained,
            )
        } else {
            fetch_text_output_value(
                &text,
                &url,
                self.max_output_bytes,
                offset_bytes,
                total_body_bytes,
                &retained,
            )
            .map_err(|message| ToolError::InvalidParameters {
                name: self.name().to_owned(),
                message,
            })?
        };
        value["final_url"] = json!(final_url);
        Ok(value)
    }
}

/// Resolve a redirect reference against the address that returned it.
fn redirect_target(
    response: &reqwest::Response,
    tool_name: &str,
) -> Result<Option<reqwest::Url>, ToolError> {
    use reqwest::StatusCode;

    let status = response.status();
    if !matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    ) {
        return Ok(None);
    }
    let from = response.url();
    let failure = |detail| ToolError::ExecutionFailed {
        name: tool_name.to_owned(),
        message: format!("HTTP {status} at {from}: {detail}"),
    };
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .ok_or_else(|| failure("redirect has no Location header".to_owned()))?;
    let location = location
        .to_str()
        .map_err(|_| failure(format!("invalid redirect Location: {location:?}")))?;
    let target = from
        .join(location)
        .map_err(|error| failure(format!("invalid redirect Location {location:?}: {error}")))?;
    ensure_fetch_scheme(&target, tool_name).map_err(|error| {
        let reason = redirect_refusal_reason(error);
        failure(format!("redirect to {target} refused: {reason}"))
    })?;
    Ok(Some(target))
}

async fn ensure_redirect_target_is_safe(
    from: &reqwest::Url,
    target: &reqwest::Url,
    tool_name: &str,
) -> Result<(), ToolError> {
    ensure_url_target_is_safe(target, tool_name)
        .await
        .map_err(|error| {
            let reason = redirect_refusal_reason(error);
            ToolError::ExecutionFailed {
                name: tool_name.to_owned(),
                message: format!("Redirect from {from} to {target} refused: {reason}"),
            }
        })
}

fn redirect_refusal_reason(error: ToolError) -> String {
    match error {
        ToolError::ExecutionFailed { message, .. }
        | ToolError::InvalidParameters { message, .. } => message,
        other => other.to_string(),
    }
}

/// What a fetch result says about the body it kept: where the bytes are,
/// what they are, and the one call that captures them.
struct RetainedBody<'a> {
    artifact_path: &'a str,
    media_type: &'a str,
    capture: Option<Value>,
}

/// The knowledge_record call that captures this fetch, when the fetch runs
/// inside a recorded turn (a stored call is what the reference resolves to).
fn capture_hint(ctx: &dyn ToolContext) -> Option<Value> {
    let turn_id = ctx.turn_id()?;
    let call_id = ctx.call_id()?;
    Some(json!({
        "tool": "knowledge_record",
        "arguments": {
            "fetch_call": tool_result_reference(&turn_id.to_string(), call_id),
        },
    }))
}

fn describe_retained_body(value: &mut Value, retained: &RetainedBody<'_>) {
    value["raw_body_artifact_path"] = json!(retained.artifact_path);
    value["media_type"] = json!(retained.media_type);
    if let Some(capture) = &retained.capture {
        value["capture"] = capture.clone();
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
    retained: &RetainedBody<'_>,
) -> Value {
    let mut value = truncated_output_value(text, max_output_bytes);
    describe_retained_body(&mut value, retained);
    if total_body_bytes > stored_body_bytes {
        value["truncated"] = json!(true);
        value["total_bytes"] = json!(total_body_bytes);
        value["raw_body_follow_up"] = file_read_follow_up(retained.artifact_path);
    }
    value
}

fn fetch_text_output_value(
    text: &str,
    url: &str,
    max_output_bytes: usize,
    offset_bytes: usize,
    total_body_bytes: usize,
    retained: &RetainedBody<'_>,
) -> Result<Value, String> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("url".to_string(), json!(url));
    let mut value =
        continuable_output_value(text, max_output_bytes, offset_bytes, "url_fetch", arguments)?;
    describe_retained_body(&mut value, retained);

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

pub(crate) fn parse_fetch_url(url: &str, tool_name: &str) -> Result<reqwest::Url, ToolError> {
    let parsed = reqwest::Url::parse(url).map_err(|error| ToolError::InvalidParameters {
        name: tool_name.to_owned(),
        message: format!("Invalid URL: {error}"),
    })?;

    ensure_fetch_scheme(&parsed, tool_name)?;
    Ok(parsed)
}

fn ensure_fetch_scheme(url: &reqwest::Url, tool_name: &str) -> Result<(), ToolError> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        scheme => Err(ToolError::InvalidParameters {
            name: tool_name.to_owned(),
            message: format!("Unsupported URL scheme: {scheme}"),
        }),
    }
}

pub(crate) async fn ensure_url_target_is_safe(
    parsed_url: &reqwest::Url,
    tool_name: &str,
) -> Result<(), ToolError> {
    let host = parsed_url
        .host()
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

    // An address literal is checked as written; only a name is looked up.
    let host = match host {
        url::Host::Ipv4(ip) => return ensure_public_ip(IpAddr::V4(ip), tool_name),
        url::Host::Ipv6(ip) => return ensure_public_ip(IpAddr::V6(ip), tool_name),
        url::Host::Domain(host) => host,
    };

    if is_blocked_hostname(host) {
        return Err(ToolError::ExecutionFailed {
            name: tool_name.to_owned(),
            message: format!("Blocked private or metadata destination: {host}"),
        });
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
        // An IPv4-mapped address (`::ffff:a.b.c.d`) reaches the IPv4 host it
        // carries, so it is judged as that host.
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(ip) => is_private_or_metadata_ipv4(ip),
            None => is_private_or_metadata_ipv6(ip),
        },
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
pub(crate) fn html_to_text(bytes: &[u8]) -> String {
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

    #[test]
    fn resolves_relative_redirects_and_names_refused_destinations() {
        use axum::http::Response;
        use reqwest::ResponseBuilderExt;

        let redirect = |location| {
            Response::builder()
                .status(302)
                .url("http://example.com/old/page".parse().unwrap())
                .header("location", location)
                .body("")
                .unwrap()
                .into()
        };
        let target = redirect_target(&redirect("../new/page"), "url_fetch")
            .unwrap()
            .unwrap();
        assert_eq!(target.as_str(), "http://example.com/new/page");

        let error = redirect_target(&redirect("file:///index.html"), "url_fetch").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("file:///index.html"), "{message}");
        assert_eq!(message.matches("url_fetch").count(), 1, "{message}");
    }

    #[tokio::test]
    async fn refused_redirect_to_private_address_names_the_tool_once() {
        let from = "https://example.com/old/page".parse().unwrap();
        let target = "http://127.0.0.1/".parse().unwrap();
        let error = ensure_redirect_target_is_safe(&from, &target, "url_fetch")
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains(from.as_str()), "{message}");
        assert!(message.contains(target.as_str()), "{message}");
        assert!(
            message.contains("Blocked private or metadata destination"),
            "{message}"
        );
        assert_eq!(message.matches("url_fetch").count(), 1, "{message}");
    }

    #[tokio::test]
    async fn ipv6_literals_are_judged_by_the_address_they_reach() {
        let public = "http://[2606:4700:4700::1111]/".parse().unwrap();
        ensure_url_target_is_safe(&public, "url_fetch")
            .await
            .expect("a public IPv6 literal is fetched");

        let from: reqwest::Url = "https://example.com/old/page".parse().unwrap();
        for mapped in [
            "http://[::ffff:127.0.0.1]:8080/",
            "http://[::ffff:10.0.0.1]/",
            "http://[::ffff:169.254.169.254]/latest/meta-data/",
        ] {
            let target: reqwest::Url = mapped.parse().unwrap();
            for error in [
                ensure_url_target_is_safe(&target, "url_fetch").await,
                ensure_redirect_target_is_safe(&from, &target, "url_fetch").await,
            ]
            .map(Result::unwrap_err)
            {
                assert!(
                    error
                        .to_string()
                        .contains("Blocked private or metadata destination"),
                    "{mapped}: {error}"
                );
            }
        }

        let mapped_loopback = SocketAddr::from((Ipv4Addr::LOCALHOST.to_ipv6_mapped(), 80));
        assert!(ensure_connected_peer_is_safe(Some(mapped_loopback), "url_fetch").is_err());
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
        let value = fetch_html_output_value(
            &capped_text,
            100,
            200,
            100,
            &retained(".swage/tool-output/turn/call/raw-body.html", "text/html"),
        );

        assert_eq!(value["output"].as_str().unwrap().len(), 100);
        assert_eq!(value["truncated"], json!(true));
        assert_eq!(value["total_bytes"], json!(200));
    }

    fn retained(path: &'static str, media_type: &'static str) -> RetainedBody<'static> {
        RetainedBody {
            artifact_path: path,
            media_type,
            capture: Some(json!({
                "tool": "knowledge_record",
                "arguments": { "fetch_call": "tool_call:turn:call:result" },
            })),
        }
    }

    #[test]
    fn html_fetch_output_returns_raw_body_follow_up() {
        let value = fetch_html_output_value(
            "Rendered text",
            100,
            200,
            100,
            &retained(".swage/tool-output/turn/call/raw-body.html", "text/html"),
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
    fn every_fetch_names_its_retained_body_and_the_call_that_captures_it() {
        let value = fetch_html_output_value(
            "Rendered text",
            100,
            13,
            13,
            &retained(".swage/tool-output/turn/call/raw-body.html", "text/html"),
        );
        assert_eq!(
            value["raw_body_artifact_path"],
            json!(".swage/tool-output/turn/call/raw-body.html")
        );
        assert_eq!(value["media_type"], json!("text/html"));
        assert_eq!(value["capture"]["tool"], json!("knowledge_record"));
        assert_eq!(
            value["capture"]["arguments"]["fetch_call"],
            json!("tool_call:turn:call:result")
        );
        assert!(
            value.get("raw_body_follow_up").is_none(),
            "a body that fit needs no follow-up read; it is still retained"
        );

        let text = fetch_text_output_value(
            "abc",
            "https://example.com/data.txt",
            100,
            0,
            3,
            &retained(".swage/tool-output/turn/call/raw-body.txt", "text/plain"),
        )
        .expect("small text output");
        assert_eq!(text["output"], json!("abc"));
        assert_eq!(
            text["raw_body_artifact_path"],
            json!(".swage/tool-output/turn/call/raw-body.txt")
        );
        assert_eq!(
            text["capture"]["arguments"]["fetch_call"],
            json!("tool_call:turn:call:result")
        );
    }

    #[test]
    fn text_fetch_output_returns_follow_up_offset() {
        let value = fetch_text_output_value(
            "abcdefghij",
            "https://example.com/data.txt",
            4,
            0,
            10,
            &retained(".swage/tool-output/turn/call/raw-body.txt", "text/plain"),
        )
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
        let value = fetch_text_output_value(
            "abcdefghij",
            "https://example.com/data.txt",
            4,
            4,
            10,
            &retained(".swage/tool-output/turn/call/raw-body.txt", "text/plain"),
        )
        .expect("continuable text output");

        assert_eq!(value["output"], json!("efgh"));
        assert_eq!(value["truncated"], json!(true));
        assert_eq!(value["total_bytes"], json!(10));
        assert_eq!(value["offset_bytes"], json!(4));
        assert_eq!(value["next_offset_bytes"], json!(8));
    }

    #[test]
    fn small_text_fetch_output_keeps_simple_shape() {
        let value = fetch_text_output_value(
            "abc",
            "https://example.com/data.txt",
            100,
            0,
            3,
            &RetainedBody {
                artifact_path: ".swage/tool-output/adhoc/call/raw-body.txt",
                media_type: "text/plain",
                capture: None,
            },
        )
        .expect("small text output");

        assert_eq!(
            value,
            json!({
                "output": "abc",
                "raw_body_artifact_path": ".swage/tool-output/adhoc/call/raw-body.txt",
                "media_type": "text/plain",
            }),
            "outside a recorded turn there is no call to reference, so no capture hint"
        );
    }
}
