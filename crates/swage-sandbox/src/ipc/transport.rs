//! Async newline-delimited JSON transport over Unix domain sockets.
//!
//! Each JSON message is written as a single line followed by `\n`.
//! The transport handles framing, serialisation, and deserialisation.

use std::time::Duration;

use swage_core::SandboxError;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

use crate::ipc::message::IpcMessage;

// ---------------------------------------------------------------------------
// IpcTransport
// ---------------------------------------------------------------------------

/// Async framing layer for the host-agent IPC protocol.
///
/// Each `IpcMessage` is serialised as a single JSON line terminated by `\n`.
/// The socket is split into owned read/write halves so they can be wrapped
/// independently in buffered I/O adapters.
pub struct IpcTransport {
    reader: BufReader<OwnedReadHalf>,
    writer: BufWriter<OwnedWriteHalf>,
}

impl IpcTransport {
    /// Wrap a connected `UnixStream` in the NDJSON transport.
    pub fn new(stream: UnixStream) -> Self {
        let (read_half, write_half) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer: BufWriter::new(write_half),
        }
    }

    /// Serialise `msg` to JSON, write it as a single line, and flush.
    pub async fn send(&mut self, msg: &IpcMessage) -> Result<(), SandboxError> {
        let mut line = serde_json::to_string(msg).map_err(|e| SandboxError::IpcError {
            message: format!("serialise failed: {e}"),
        })?;
        // Newline delimiter — NDJSON protocol.
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| SandboxError::IpcError {
                message: format!("write failed: {e}"),
            })?;
        self.writer
            .flush()
            .await
            .map_err(|e| SandboxError::IpcError {
                message: format!("flush failed: {e}"),
            })
    }

    /// Read one line from the socket and deserialise it as an `IpcMessage`.
    ///
    /// Returns `SandboxError::IpcError` if the connection closes before a
    /// complete line is received.
    pub async fn receive(&mut self) -> Result<IpcMessage, SandboxError> {
        let mut line = String::new();
        let bytes = self
            .reader
            .read_line(&mut line)
            .await
            .map_err(|e| SandboxError::IpcError {
                message: format!("read failed: {e}"),
            })?;

        if bytes == 0 {
            // EOF — remote end closed the connection.
            return Err(SandboxError::IpcError {
                message: "connection closed".into(),
            });
        }

        serde_json::from_str(line.trim_end()).map_err(|e| SandboxError::IpcError {
            message: format!("deserialise failed: {e}"),
        })
    }

    /// Receive a message, returning `SandboxError::IpcError` if `timeout`
    /// elapses before a complete line arrives.
    pub async fn receive_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<IpcMessage, SandboxError> {
        tokio::time::timeout(timeout, self.receive())
            .await
            .map_err(|_| SandboxError::IpcError {
                message: "timeout waiting for IPC message".into(),
            })?
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use swage_core::SandboxError;
    use tokio::net::UnixStream;

    use super::IpcTransport;
    use crate::ipc::message::IpcMessage;

    // --- Roundtrip: single message across connected socket pair ---

    #[tokio::test]
    async fn send_receive_roundtrip_ready() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut sender = IpcTransport::new(a);
        let mut receiver = IpcTransport::new(b);

        sender.send(&IpcMessage::Ready).await.unwrap();
        let received = receiver.receive().await.unwrap();
        assert!(matches!(received, IpcMessage::Ready));
    }

    #[tokio::test]
    async fn send_receive_roundtrip_tool_call_request() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut sender = IpcTransport::new(a);
        let mut receiver = IpcTransport::new(b);

        let msg = IpcMessage::ToolCallRequest {
            request_id: "req-99".into(),
            tool_name: "fs_read".into(),
            parameters: serde_json::json!({"path": "/tmp/test"}),
        };
        sender.send(&msg).await.unwrap();

        let received = receiver.receive().await.unwrap();
        match received {
            IpcMessage::ToolCallRequest { request_id, tool_name, parameters } => {
                assert_eq!(request_id, "req-99");
                assert_eq!(tool_name, "fs_read");
                assert_eq!(parameters["path"], "/tmp/test");
            }
            other => panic!("expected ToolCallRequest, got {:?}", other),
        }
    }

    // --- Multiple messages in sequence ---

    #[tokio::test]
    async fn multiple_messages_in_sequence() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut sender = IpcTransport::new(a);
        let mut receiver = IpcTransport::new(b);

        // Send three messages and receive them all in order.
        sender.send(&IpcMessage::Ready).await.unwrap();
        sender.send(&IpcMessage::Shutdown).await.unwrap();
        sender.send(&IpcMessage::ShutdownAck).await.unwrap();

        assert!(matches!(receiver.receive().await.unwrap(), IpcMessage::Ready));
        assert!(matches!(receiver.receive().await.unwrap(), IpcMessage::Shutdown));
        assert!(matches!(receiver.receive().await.unwrap(), IpcMessage::ShutdownAck));
    }

    // --- Timeout: no data arrives before deadline ---

    #[tokio::test]
    async fn receive_timeout_returns_error_on_idle_socket() {
        // Keep _b alive so the connection stays open — we want a timeout, not a close.
        let (a, _b) = UnixStream::pair().unwrap();
        let mut transport = IpcTransport::new(a);

        let result = transport.receive_timeout(Duration::from_millis(50)).await;
        assert!(result.is_err(), "expected error, got Ok");
        match result.unwrap_err() {
            SandboxError::IpcError { message } => {
                assert!(
                    message.contains("timeout"),
                    "error message should mention 'timeout', got: {message}"
                );
            }
            other => panic!("expected SandboxError::IpcError, got {:?}", other),
        }
    }

    // --- Timeout: message arrives in time ---

    #[tokio::test]
    async fn receive_timeout_succeeds_when_data_arrives() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut sender = IpcTransport::new(a);
        let mut receiver = IpcTransport::new(b);

        sender.send(&IpcMessage::Ready).await.unwrap();

        // Generous deadline; the message is already buffered.
        let result = receiver.receive_timeout(Duration::from_secs(1)).await;
        assert!(matches!(result.unwrap(), IpcMessage::Ready));
    }

    // --- Connection closure is reported as an error ---

    #[tokio::test]
    async fn receive_on_closed_connection_returns_error() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut transport = IpcTransport::new(a);
        drop(b); // close the remote end

        let result = transport.receive().await;
        assert!(result.is_err(), "closed connection should return an error");
    }
}
