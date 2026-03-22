//! HTTP client infrastructure for LLM provider communication.
//!
//! Provides a configured HTTP client with appropriate timeouts and error mapping
//! from HTTP status codes to ProviderError variants.

use crate::error::ProviderError;
use reqwest::{Client, StatusCode};
use std::time::Duration;

/// HTTP client wrapper with reasonable defaults for LLM provider communication.
///
/// Configured with:
/// - Connect timeout: 10 seconds
/// - Request timeout: 60 seconds
/// - TLS enabled by default
/// - Connection pooling enabled
#[derive(Debug)]
pub struct HttpClient {
    client: Client,
}

impl HttpClient {
    /// Creates a new HTTP client with default configuration.
    pub fn new() -> Result<Self, ProviderError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| ProviderError::InvalidConfig(format!("Failed to build HTTP client: {}", e)))?;

        Ok(Self { client })
    }

    /// Returns a reference to the underlying reqwest client.
    ///
    /// Allows direct access for advanced use cases not covered by the wrapper.
    pub fn client(&self) -> &Client {
        &self.client
    }
}

impl Default for HttpClient {
    /// Creates a new HTTP client with default configuration.
    ///
    /// # Panics
    ///
    /// Panics if the HTTP client cannot be constructed (e.g., TLS initialisation
    /// failure). For fallible construction, use [`HttpClient::new()`] instead.
    fn default() -> Self {
        Self::new().expect("Failed to create default HTTP client")
    }
}

/// Maps a reqwest error to a ProviderError variant.
///
/// Categorises errors into appropriate error types for retry logic:
/// - Timeout errors → ProviderError::Timeout (transient)
/// - Connection errors → ProviderError::Network (transient)
/// - 401, 403 → ProviderError::Auth (permanent)
/// - 404 → ProviderError::ModelError (permanent)
/// - 429 → ProviderError::RateLimit (transient)
/// - 500+ → ProviderError::Network (transient, server-side)
/// - 400-499 (other) → ProviderError::InvalidRequest (permanent)
pub fn map_reqwest_error(error: reqwest::Error) -> ProviderError {
    // Check for timeout
    if error.is_timeout() {
        return ProviderError::Timeout(error.to_string());
    }

    // Check for connection errors
    if error.is_connect() {
        return ProviderError::Network(format!("Connection failed: {}", error));
    }

    // Check for status code errors
    if let Some(status) = error.status() {
        return map_status_code(status, error.to_string());
    }

    // Generic network error for anything else (DNS, protocol, etc.)
    ProviderError::Network(error.to_string())
}

/// Maps HTTP status codes to ProviderError variants.
///
/// Used to classify responses from LLM providers for retry logic.
pub fn map_status_code(status: StatusCode, message: String) -> ProviderError {
    match status.as_u16() {
        // Authentication errors
        401 | 403 => ProviderError::Auth(format!("{}: {}", status, message)),

        // Not found — typically indicates unknown model
        404 => ProviderError::ModelError(format!("Resource not found: {}", message)),

        // Rate limiting
        429 => ProviderError::RateLimit(format!("Rate limit exceeded: {}", message)),

        // Client errors (other than auth, not found, rate limit)
        400..=499 => ProviderError::InvalidRequest(format!("{}: {}", status, message)),

        // Server errors — treat as transient network issues
        500..=599 => ProviderError::Network(format!("Server error {}: {}", status, message)),

        // Unexpected status codes
        _ => ProviderError::Network(format!("Unexpected status {}: {}", status, message)),
    }
}

/// Parses Server-Sent Events (SSE) from a streaming response.
///
/// Extracts "data:" lines from SSE format, commonly used by streaming LLM APIs.
///
/// # Format
///
/// SSE streams contain lines like:
/// ```text
/// data: {"chunk": "content"}
/// data: [DONE]
///
/// ```
///
/// This function extracts the content after "data: " prefix.
pub fn parse_sse_line(line: &str) -> Option<&str> {
    line.strip_prefix("data: ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_client_creation() {
        let client = HttpClient::new();
        assert!(client.is_ok());
    }

    #[test]
    fn http_client_default() {
        let _client = HttpClient::default();
        // Should not panic
    }

    #[test]
    fn map_timeout_error() {
        // Create a timeout error by building a client with instant timeout
        let client = Client::builder()
            .timeout(Duration::from_nanos(1))
            .build()
            .unwrap();

        // This will timeout when attempted
        let error = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async {
                client.get("https://httpbin.org/delay/10").send().await
            })
            .unwrap_err();

        let mapped = map_reqwest_error(error);
        assert!(matches!(mapped, ProviderError::Timeout(_)));
        assert!(mapped.is_retriable());
    }

    #[test]
    fn map_status_code_auth_errors() {
        let err = map_status_code(StatusCode::UNAUTHORIZED, "Unauthorized".to_string());
        assert!(matches!(err, ProviderError::Auth(_)));
        assert!(!err.is_retriable());

        let err = map_status_code(StatusCode::FORBIDDEN, "Forbidden".to_string());
        assert!(matches!(err, ProviderError::Auth(_)));
        assert!(!err.is_retriable());
    }

    #[test]
    fn map_status_code_not_found() {
        let err = map_status_code(StatusCode::NOT_FOUND, "Not found".to_string());
        assert!(matches!(err, ProviderError::ModelError(_)));
        assert!(!err.is_retriable());
    }

    #[test]
    fn map_status_code_rate_limit() {
        let err = map_status_code(StatusCode::TOO_MANY_REQUESTS, "Rate limit".to_string());
        assert!(matches!(err, ProviderError::RateLimit(_)));
        assert!(err.is_retriable());
    }

    #[test]
    fn map_status_code_client_errors() {
        let err = map_status_code(StatusCode::BAD_REQUEST, "Bad request".to_string());
        assert!(matches!(err, ProviderError::InvalidRequest(_)));
        assert!(!err.is_retriable());

        let err = map_status_code(StatusCode::UNPROCESSABLE_ENTITY, "Invalid data".to_string());
        assert!(matches!(err, ProviderError::InvalidRequest(_)));
        assert!(!err.is_retriable());
    }

    #[test]
    fn map_status_code_server_errors() {
        let err = map_status_code(StatusCode::INTERNAL_SERVER_ERROR, "Server error".to_string());
        assert!(matches!(err, ProviderError::Network(_)));
        assert!(err.is_retriable());

        let err = map_status_code(StatusCode::BAD_GATEWAY, "Bad gateway".to_string());
        assert!(matches!(err, ProviderError::Network(_)));
        assert!(err.is_retriable());

        let err = map_status_code(StatusCode::SERVICE_UNAVAILABLE, "Unavailable".to_string());
        assert!(matches!(err, ProviderError::Network(_)));
        assert!(err.is_retriable());
    }

    #[test]
    fn parse_sse_data_line() {
        assert_eq!(
            parse_sse_line("data: {\"content\": \"hello\"}"),
            Some("{\"content\": \"hello\"}")
        );
        assert_eq!(parse_sse_line("data: [DONE]"), Some("[DONE]"));
    }

    #[test]
    fn parse_sse_non_data_line() {
        assert_eq!(parse_sse_line("event: message"), None);
        assert_eq!(parse_sse_line("id: 123"), None);
        assert_eq!(parse_sse_line(""), None);
        assert_eq!(parse_sse_line(": comment"), None);
    }

    #[test]
    fn parse_sse_empty_data() {
        assert_eq!(parse_sse_line("data: "), Some(""));
    }
}
