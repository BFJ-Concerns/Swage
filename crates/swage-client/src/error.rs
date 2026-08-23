//! Typed error types for the gateway client library.
//!
//! Every error includes the address that was attempted and the underlying cause,
//! so callers can produce actionable diagnostics without re-deriving context.

/// Errors produced by gateway client operations.
///
/// Network variants carry the attempted `address` so callers can log or display
/// useful diagnostics without having to rediscover where the request was sent.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// TCP connection refused or timed out before a response was received.
    #[error("Failed to connect to gateway at {address}: {source}")]
    ConnectionFailed {
        address: String,
        #[source]
        source: reqwest::Error,
    },

    /// HTTP request completed but resulted in an error (4xx/5xx or transport failure).
    #[error("Request to gateway at {address} failed: {source}")]
    RequestFailed {
        address: String,
        #[source]
        source: reqwest::Error,
    },

    /// WebSocket handshake or framing failed after the TCP connection was established.
    #[error("WebSocket connection to {address} failed: {message}")]
    WebSocketFailed { address: String, message: String },

    /// The gateway returned a non-success HTTP status with a body that can be shown
    /// to the user for context.
    #[error("Unexpected response from gateway at {address}: HTTP {status}: {body}")]
    UnexpectedResponse {
        address: String,
        status: u16,
        body: String,
    },

    /// JSON response from the gateway could not be deserialised into the expected type.
    #[error("Failed to parse response from {address}: {source}")]
    Deserialisation {
        address: String,
        #[source]
        source: serde_json::Error,
    },

    /// Reading or writing the runtime metadata file failed.
    #[error("Failed to read gateway metadata: {source}")]
    MetadataIo {
        #[source]
        source: std::io::Error,
    },

    /// No metadata file was found, or the recorded PID is no longer alive.
    #[error("Gateway is not running: {reason}")]
    GatewayNotRunning { reason: String },

    /// A start was requested while a live gateway already owned the runtime.
    #[error("Gateway is already running: {reason}")]
    GatewayAlreadyRunning { reason: String },

    /// A restart was refused before stopping the live gateway.
    #[error("Gateway restart was not attempted: {reason}")]
    GatewayRestartRefused { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_json_error() -> serde_json::Error {
        serde_json::from_str::<serde_json::Value>("not json").unwrap_err()
    }

    fn dummy_io_error() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no such file")
    }

    /// Returns a real `reqwest::Error` by attempting a connection to an invalid
    /// address. This avoids the `blocking` feature and keeps tests async-native.
    async fn dummy_reqwest_error() -> reqwest::Error {
        reqwest::get("http://0.0.0.0:1").await.unwrap_err()
    }

    #[tokio::test]
    async fn connection_failed_display() {
        let err = ClientError::ConnectionFailed {
            address: "http://127.0.0.1:3000".to_string(),
            source: dummy_reqwest_error().await,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("http://127.0.0.1:3000"),
            "display should include address, got: {msg}"
        );
        assert!(
            msg.contains("Failed to connect to gateway"),
            "display should match the error prefix, got: {msg}"
        );
    }

    #[tokio::test]
    async fn request_failed_display() {
        let err = ClientError::RequestFailed {
            address: "http://127.0.0.1:3000".to_string(),
            source: dummy_reqwest_error().await,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("http://127.0.0.1:3000"),
            "display should include address, got: {msg}"
        );
        assert!(
            msg.contains("Request to gateway"),
            "display should match the error prefix, got: {msg}"
        );
    }

    #[test]
    fn websocket_failed_display() {
        let err = ClientError::WebSocketFailed {
            address: "ws://127.0.0.1:3000/ws".to_string(),
            message: "protocol error".to_string(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("ws://127.0.0.1:3000/ws"),
            "display should include address, got: {msg}"
        );
        assert!(
            msg.contains("protocol error"),
            "display should include message, got: {msg}"
        );
    }

    #[test]
    fn unexpected_response_display() {
        let err = ClientError::UnexpectedResponse {
            address: "http://127.0.0.1:3000".to_string(),
            status: 500,
            body: "internal server error".to_string(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("http://127.0.0.1:3000"),
            "display should include address, got: {msg}"
        );
        assert!(
            msg.contains("500"),
            "display should include status code, got: {msg}"
        );
        assert!(
            msg.contains("internal server error"),
            "display should include body, got: {msg}"
        );
    }

    #[test]
    fn deserialisation_display() {
        let err = ClientError::Deserialisation {
            address: "http://127.0.0.1:3000".to_string(),
            source: dummy_json_error(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("http://127.0.0.1:3000"),
            "display should include address, got: {msg}"
        );
        assert!(
            msg.contains("Failed to parse response"),
            "display should match the error prefix, got: {msg}"
        );
    }

    #[test]
    fn metadata_io_display() {
        let err = ClientError::MetadataIo {
            source: dummy_io_error(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Failed to read gateway metadata"),
            "display should mention metadata, got: {msg}"
        );
    }

    #[test]
    fn gateway_not_running_display() {
        let err = ClientError::GatewayNotRunning {
            reason: "no metadata file found".to_string(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Gateway is not running"),
            "display should mention gateway not running, got: {msg}"
        );
        assert!(
            msg.contains("no metadata file found"),
            "display should include reason, got: {msg}"
        );
    }

    #[test]
    fn gateway_already_running_display() {
        let err = ClientError::GatewayAlreadyRunning {
            reason: "PID 42 is serving http://127.0.0.1:3000".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "Gateway is already running: PID 42 is serving http://127.0.0.1:3000"
        );
    }

    #[test]
    fn gateway_restart_refused_display() {
        let err = ClientError::GatewayRestartRefused {
            reason: "the running gateway's data root is unavailable".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "Gateway restart was not attempted: the running gateway's data root is unavailable"
        );
    }

    #[test]
    fn errors_implement_std_error() {
        // Verify the type erases to dyn std::error::Error correctly.
        let err: Box<dyn std::error::Error> = Box::new(ClientError::GatewayNotRunning {
            reason: "stale PID".to_string(),
        });
        assert!(err.to_string().contains("stale PID"));
    }

    #[test]
    fn metadata_io_exposes_source() {
        use std::error::Error;
        let err = ClientError::MetadataIo {
            source: dummy_io_error(),
        };
        // thiserror wires #[source] to std::error::Error::source().
        assert!(
            err.source().is_some(),
            "MetadataIo should expose its source"
        );
    }

    #[test]
    fn deserialisation_exposes_source() {
        use std::error::Error;
        let err = ClientError::Deserialisation {
            address: "http://127.0.0.1:3000".to_string(),
            source: dummy_json_error(),
        };
        assert!(
            err.source().is_some(),
            "Deserialisation should expose its source"
        );
    }
}
