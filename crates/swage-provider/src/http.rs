use bytes::Bytes;
use futures::channel::mpsc;
use futures::{Stream, StreamExt};
use reqwest::header::HeaderMap;
use serde::{de::DeserializeOwned, Serialize};
use std::time::Duration;
use swage_core::ProviderError;

// ---------------------------------------------------------------------------
// SseEvent
// ---------------------------------------------------------------------------

/// A parsed Server-Sent Event.
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event_type: Option<String>,
    pub data: String,
}

// ---------------------------------------------------------------------------
// HttpClient
// ---------------------------------------------------------------------------

/// Thin wrapper around `reqwest::Client` for LLM API calls.
pub struct HttpClient {
    client: reqwest::Client,
    base_url: String,
}

impl HttpClient {
    /// Build a client with the given base URL and default request headers.
    pub fn new(base_url: String, default_headers: HeaderMap) -> Self {
        let client = reqwest::Client::builder()
            .default_headers(default_headers)
            .build()
            .expect("failed to build reqwest client");

        Self { client, base_url }
    }

    /// POST JSON body to `path` and deserialise the response.
    pub async fn post_json<T, R>(&self, path: &str, body: &T) -> Result<R, ProviderError>
    where
        T: Serialize,
        R: DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url, path);

        let response = self
            .client
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| ProviderError::StreamError(e.to_string()))?;

        let status = response.status().as_u16();

        if !response.status().is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(map_http_error(status, body_text, None));
        }

        response
            .json::<R>()
            .await
            .map_err(|e| ProviderError::InvalidResponse(e.to_string()))
    }

    /// POST JSON body to `path` and return a stream of parsed SSE events.
    ///
    /// `extra_headers` are merged with the client's defaults for this request.
    pub async fn post_stream(
        &self,
        path: &str,
        body: &impl Serialize,
        extra_headers: HeaderMap,
    ) -> Result<impl Stream<Item = Result<SseEvent, ProviderError>>, ProviderError> {
        let url = format!("{}{}", self.base_url, path);

        let response = self
            .client
            .post(&url)
            .headers(extra_headers)
            .json(body)
            .send()
            .await
            .map_err(|e| ProviderError::StreamError(e.to_string()))?;

        let status = response.status().as_u16();

        if !response.status().is_success() {
            // Try to parse Retry-After before consuming the body.
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            let body_text = response.text().await.unwrap_or_default();
            return Err(map_http_error(status, body_text, retry_after));
        }

        Ok(sse_stream_from_bytes(response.bytes_stream()))
    }
}

// ---------------------------------------------------------------------------
// SSE stream adapter
// ---------------------------------------------------------------------------

/// Convert a raw byte stream into a stream of parsed SSE events.
///
/// Runs a background task that buffers bytes, splits on `\n\n`, and sends
/// complete events to an unbounded channel. The channel receiver is returned
/// as the stream.
fn sse_stream_from_bytes(
    byte_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
) -> impl Stream<Item = Result<SseEvent, ProviderError>> + Send + 'static {
    let (tx, rx) = mpsc::unbounded();

    tokio::spawn(async move {
        let mut buffer = String::new();
        tokio::pin!(byte_stream);

        'outer: while let Some(chunk) = byte_stream.next().await {
            let bytes = match chunk {
                Err(e) => {
                    let _ = tx.unbounded_send(Err(ProviderError::StreamError(e.to_string())));
                    break;
                }
                Ok(b) => b,
            };

            let text = match std::str::from_utf8(&bytes) {
                Ok(s) => s.to_owned(),
                Err(e) => {
                    let _ = tx.unbounded_send(Err(ProviderError::StreamError(
                        format!("invalid UTF-8 in stream: {e}"),
                    )));
                    break;
                }
            };

            buffer.push_str(&text);

            // Emit all complete events in the buffer (delimited by \n\n).
            while let Some(idx) = buffer.find("\n\n") {
                let block = buffer[..idx].to_string();
                buffer = buffer[idx + 2..].to_string();

                if let Some(event) = parse_sse_block(&block) {
                    // [DONE] is the standard LLM stream termination sentinel.
                    if event.data == "[DONE]" {
                        break 'outer;
                    }
                    if tx.unbounded_send(Ok(event)).is_err() {
                        break 'outer; // Receiver dropped — stop processing.
                    }
                }
            }
        }
    });

    rx
}

// ---------------------------------------------------------------------------
// SSE block parser
// ---------------------------------------------------------------------------

/// Parse a single SSE block (the text between two `\n\n` boundaries) into an
/// `SseEvent`. Returns `None` for blocks that carry no data (e.g. only comments).
pub(crate) fn parse_sse_block(block: &str) -> Option<SseEvent> {
    let mut event_type: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();

    for line in block.lines() {
        if line.starts_with(':') {
            // Comment — ignore per SSE spec.
            continue;
        }

        if let Some(val) = line.strip_prefix("event:") {
            event_type = Some(val.trim_start_matches(' ').to_owned());
        } else if let Some(val) = line.strip_prefix("data:") {
            // Leading single space is stripped per spec; additional spaces are kept.
            let content = val.strip_prefix(' ').unwrap_or(val);
            data_lines.push(content.to_owned());
        }
    }

    if data_lines.is_empty() {
        return None;
    }

    Some(SseEvent {
        event_type,
        data: data_lines.join("\n"),
    })
}

// ---------------------------------------------------------------------------
// HTTP error mapping
// ---------------------------------------------------------------------------

/// Map an HTTP error status code to the appropriate `ProviderError` variant.
pub(crate) fn map_http_error(
    status: u16,
    body: String,
    retry_after: Option<u64>,
) -> ProviderError {
    match status {
        401 => ProviderError::Auth { provider: body },
        // 429 (Too Many Requests) and 529 (Anthropic overloaded) are both rate limits.
        429 | 529 => ProviderError::RateLimited {
            provider: "anthropic".to_string(),
            // Convert seconds (u64) to Duration; None stays None.
            retry_after: retry_after.map(Duration::from_secs),
        },
        _ => ProviderError::HttpError { status, body },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- SSE parsing ---

    #[test]
    fn parse_single_event() {
        let block = "data: hello world";
        let event = parse_sse_block(block).expect("should parse");
        assert_eq!(event.data, "hello world");
        assert!(event.event_type.is_none());
    }

    #[test]
    fn parse_event_with_type() {
        let block = "event: message\ndata: {\"text\":\"hi\"}";
        let event = parse_sse_block(block).expect("should parse");
        assert_eq!(event.event_type.as_deref(), Some("message"));
        assert_eq!(event.data, "{\"text\":\"hi\"}");
    }

    #[test]
    fn parse_multi_line_data() {
        // Multiple data: lines are concatenated with \n per SSE spec.
        let block = "data: line one\ndata: line two\ndata: line three";
        let event = parse_sse_block(block).expect("should parse");
        assert_eq!(event.data, "line one\nline two\nline three");
    }

    #[test]
    fn parse_ignores_comments() {
        let block = ": this is a comment\ndata: actual data\n: another comment";
        let event = parse_sse_block(block).expect("should parse");
        assert_eq!(event.data, "actual data");
    }

    #[test]
    fn parse_comment_only_block_returns_none() {
        let block = ": just a comment\n: another comment";
        assert!(parse_sse_block(block).is_none());
    }

    #[test]
    fn parse_empty_block_returns_none() {
        assert!(parse_sse_block("").is_none());
        assert!(parse_sse_block("   ").is_none());
    }

    #[test]
    fn parse_done_sentinel() {
        let block = "data: [DONE]";
        let event = parse_sse_block(block).expect("should parse");
        assert_eq!(event.data, "[DONE]");
    }

    #[test]
    fn parse_strips_single_leading_space_from_data() {
        // SSE spec: "data: value" → "value" (one leading space stripped).
        let block = "data: value with leading space";
        let event = parse_sse_block(block).expect("should parse");
        assert_eq!(event.data, "value with leading space");
    }

    #[test]
    fn parse_multiple_events_from_raw_text() {
        // Simulate splitting a raw SSE response on \n\n.
        let raw = "data: first\n\ndata: second\n\n: comment\n\ndata: third\n\n";
        let events: Vec<SseEvent> = raw
            .split("\n\n")
            .filter_map(|block| parse_sse_block(block))
            .collect();

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].data, "first");
        assert_eq!(events[1].data, "second");
        assert_eq!(events[2].data, "third");
    }

    // --- HTTP error mapping ---

    #[test]
    fn map_401_to_auth_error() {
        let err = map_http_error(401, "Unauthorized".into(), None);
        assert!(matches!(err, ProviderError::Auth { .. }));
    }

    #[test]
    fn map_429_to_rate_limited_with_retry_after() {
        let err = map_http_error(429, "Too Many Requests".into(), Some(30));
        assert!(matches!(err, ProviderError::RateLimited { retry_after: Some(_), .. }));
    }

    #[test]
    fn map_429_to_rate_limited_without_retry_after() {
        let err = map_http_error(429, "rate limited".into(), None);
        assert!(matches!(err, ProviderError::RateLimited { retry_after: None, .. }));
    }

    #[test]
    fn map_529_to_rate_limited() {
        // 529 is Anthropic's overloaded status — treated as rate limit.
        let err = map_http_error(529, "overloaded".into(), None);
        assert!(matches!(err, ProviderError::RateLimited { .. }));
    }

    #[test]
    fn map_500_to_http_error() {
        let err = map_http_error(500, "Internal Server Error".into(), None);
        assert!(matches!(err, ProviderError::HttpError { status: 500, .. }));
    }

    #[test]
    fn map_422_to_http_error() {
        let err = map_http_error(422, "Unprocessable Entity".into(), None);
        assert!(matches!(err, ProviderError::HttpError { status: 422, .. }));
    }
}
