//! Async newline-delimited JSON transport over Unix domain sockets.
//!
//! Each JSON message is written as a single line followed by `\n`.
//! The transport handles framing, serialisation, and deserialisation.

use std::collections::HashSet;
use std::time::Duration;

use swage_core::SandboxError;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::ipc::message::IpcMessage;

const MAX_IPC_MESSAGE_BYTES: usize = 1024 * 1024;

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
    /// Bytes consumed from the socket for the frame not yet returned.
    incoming_frame: Vec<u8>,
    /// Tool calls the host stopped waiting for. Their results may still
    /// arrive, and are recognised by request ID rather than mistaken for the
    /// answer to a later call.
    abandoned_tool_calls: HashSet<String>,
}

impl IpcTransport {
    /// Wrap a connected `UnixStream` in the NDJSON transport.
    pub fn new(stream: UnixStream) -> Self {
        let (read_half, write_half) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer: BufWriter::new(write_half),
            incoming_frame: Vec::new(),
            abandoned_tool_calls: HashSet::new(),
        }
    }

    /// Record that the host no longer waits for the result of `request_id`.
    pub fn abandon_tool_call(&mut self, request_id: String) {
        self.abandoned_tool_calls.insert(request_id);
    }

    /// Whether `request_id` names an abandoned call, forgetting it if so —
    /// each abandoned call produces at most one late result.
    pub fn take_abandoned_tool_call(&mut self, request_id: &str) -> bool {
        self.abandoned_tool_calls.remove(request_id)
    }

    /// Serialise `msg` to JSON, write it as a single line, and flush.
    pub async fn send(&mut self, msg: &IpcMessage) -> Result<(), SandboxError> {
        let mut line = serde_json::to_string(msg).map_err(|e| SandboxError::IpcError {
            message: format!("serialise failed: {e}"),
        })?;
        if line.len() > MAX_IPC_MESSAGE_BYTES {
            return Err(SandboxError::IpcError {
                message: format!("IPC message exceeds {MAX_IPC_MESSAGE_BYTES} byte limit"),
            });
        }
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
        let remaining = (MAX_IPC_MESSAGE_BYTES + 1).saturating_sub(self.incoming_frame.len());
        let bytes = (&mut self.reader)
            .take(remaining as u64)
            .read_until(b'\n', &mut self.incoming_frame)
            .await
            .map_err(|e| SandboxError::IpcError {
                message: format!("read failed: {e}"),
            })?;

        if bytes == 0 && self.incoming_frame.is_empty() {
            // EOF — remote end closed the connection.
            return Err(SandboxError::IpcError {
                message: "connection closed".into(),
            });
        }

        let ended_with_newline = self.incoming_frame.last() == Some(&b'\n');
        if self.incoming_frame.len() > MAX_IPC_MESSAGE_BYTES || !ended_with_newline {
            return Err(SandboxError::IpcError {
                message: format!("IPC message exceeds {MAX_IPC_MESSAGE_BYTES} byte limit"),
            });
        }

        let line = String::from_utf8(std::mem::take(&mut self.incoming_frame)).map_err(|e| {
            SandboxError::IpcError {
                message: format!("invalid UTF-8 in IPC message: {e}"),
            }
        })?;

        serde_json::from_str(line.trim_end()).map_err(|e| SandboxError::IpcError {
            message: format!("deserialise failed: {e}"),
        })
    }

    /// Receive a message, returning `SandboxError::IpcError` if `timeout`
    /// elapses before a complete line arrives.
    ///
    /// Partial bytes live on the transport, so dropping this receive leaves
    /// the next receive able to complete the same frame.
    pub async fn receive_timeout(&mut self, timeout: Duration) -> Result<IpcMessage, SandboxError> {
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
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    use super::{IpcTransport, MAX_IPC_MESSAGE_BYTES};
    use crate::ipc::message::IpcMessage;

    #[tokio::test]
    async fn cancelled_receive_retains_partial_frame_and_its_size_limit() {
        for prefix in [1, 8, 16] {
            let (socket, mut peer) = UnixStream::pair().unwrap();
            let mut transport = IpcTransport::new(socket);
            let frame = b"{\"kind\":\"ready\"}\n";
            peer.write_all(&frame[..prefix]).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(20), transport.receive())
                    .await
                    .is_err()
            );
            peer.write_all(&frame[prefix..]).await.unwrap();
            peer.write_all(b"{\"kind\":\"shutdown\"}\n").await.unwrap();
            assert!(
                matches!(transport.receive().await, Ok(IpcMessage::Ready)),
                "the next receive must complete the interrupted frame at byte {prefix}"
            );
            assert!(matches!(
                transport.receive().await,
                Ok(IpcMessage::Shutdown)
            ));
        }

        let (socket, mut peer) = UnixStream::pair().unwrap();
        let mut transport = IpcTransport::new(socket);
        peer.write_all(b"{").await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), transport.receive())
                .await
                .is_err()
        );
        let writer = tokio::spawn(async move {
            peer.write_all(&vec![b' '; MAX_IPC_MESSAGE_BYTES - 1])
                .await
                .unwrap();
            peer.write_all(b"\n").await.unwrap();
        });
        let error = transport.receive().await.unwrap_err();
        assert!(error.to_string().contains("byte limit"), "{error}");
        writer.await.unwrap();
    }

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
            IpcMessage::ToolCallRequest {
                request_id,
                tool_name,
                parameters,
            } => {
                assert_eq!(request_id, "req-99");
                assert_eq!(tool_name, "fs_read");
                assert_eq!(parameters["path"], "/tmp/test");
            }
            other => panic!("expected ToolCallRequest, got {other:?}"),
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

        assert!(matches!(
            receiver.receive().await.unwrap(),
            IpcMessage::Ready
        ));
        assert!(matches!(
            receiver.receive().await.unwrap(),
            IpcMessage::Shutdown
        ));
        assert!(matches!(
            receiver.receive().await.unwrap(),
            IpcMessage::ShutdownAck
        ));
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
            other => panic!("expected SandboxError::IpcError, got {other:?}"),
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

    #[tokio::test]
    async fn send_rejects_oversized_message() {
        let (a, _b) = UnixStream::pair().unwrap();
        let mut sender = IpcTransport::new(a);
        let oversized = "x".repeat(MAX_IPC_MESSAGE_BYTES + 1);

        let err = sender
            .send(&IpcMessage::ErrorMessage {
                request_id: None,
                category: crate::ipc::message::ErrorCategory::Protocol,
                description: oversized,
            })
            .await
            .unwrap_err();

        match err {
            SandboxError::IpcError { message } => {
                assert!(message.contains("byte limit"), "got: {message}");
            }
            other => panic!("expected SandboxError::IpcError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn receive_rejects_oversized_message() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut receiver = IpcTransport::new(a);
        let mut sender = IpcTransport::new(b);
        let oversized = "x".repeat(MAX_IPC_MESSAGE_BYTES + 1);

        // Write concurrently with receive so the Unix socket buffer cannot
        // deadlock before the receiver has a chance to enforce the frame limit.
        let writer = tokio::spawn(async move {
            sender.writer.write_all(oversized.as_bytes()).await.unwrap();
            sender.writer.flush().await.unwrap();
        });

        let err = receiver.receive().await.unwrap_err();
        writer.await.unwrap();
        match err {
            SandboxError::IpcError { message } => {
                assert!(message.contains("byte limit"), "got: {message}");
            }
            other => panic!("expected SandboxError::IpcError, got {other:?}"),
        }
    }
}
