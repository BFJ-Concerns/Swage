//! Gateway connection manager with reconnection backoff.
//!
//! Manages the lifecycle of the HTTP client and WebSocket connection to the
//! gateway. Handles initial connection, graceful disconnection, and automatic
//! reconnection with bounded exponential backoff when the connection drops.

use swage_client::error::ClientError;
use swage_client::http::GatewayClient;
use swage_client::websocket::{ClientEvent, GatewayWebSocket, ServerEvent};
use tokio::sync::watch;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// ConnectionState
// ---------------------------------------------------------------------------

/// Connection state visible to the status bar and input area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Disconnected,
    Reconnecting,
}

// ---------------------------------------------------------------------------
// ConnectionManager
// ---------------------------------------------------------------------------

/// Manages a single connection to the Swage gateway.
///
/// Owns the HTTP client for REST calls, the active WebSocket for the
/// streaming event channel, and the conversation identity. The caller drives
/// the lifecycle by calling `connect`, `disconnect`, and
/// `reconnect_with_backoff` — this type only holds state and delegates I/O.
pub struct ConnectionManager {
    url: String,
    client: GatewayClient,
    ws: Option<GatewayWebSocket>,
    conversation_id: Option<Uuid>,
    state: ConnectionState,
    /// Project context used when creating project-scoped conversations.
    project_id: Option<Uuid>,
}

impl ConnectionManager {
    /// Constructs a manager targeting the given gateway address.
    ///
    /// The connection is not established until `connect()` is called.
    pub fn new(url: String) -> Self {
        let client = GatewayClient::new(&url);
        Self {
            url,
            client,
            ws: None,
            conversation_id: None,
            state: ConnectionState::Disconnected,
            project_id: None,
        }
    }

    // --- Accessors ---

    pub fn connection_state(&self) -> ConnectionState {
        self.state
    }

    pub fn conversation_id(&self) -> Option<Uuid> {
        self.conversation_id
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    // --- Connection lifecycle ---

    /// Establishes a connection to the gateway.
    ///
    /// 1. Health-checks the gateway.
    /// 2. Creates a new conversation, or resumes the existing one if
    ///    `conversation_id` is already set.
    /// 3. Opens the WebSocket for the conversation.
    /// 4. Subscribes to `"context"` and `"tools"` observability channels.
    ///
    /// On success the state transitions to `Connected`.
    pub async fn connect(&mut self) -> Result<(), ClientError> {
        self.client.health().await?;

        // Resume an existing conversation or create a fresh one.
        let conv_id = if let Some(id) = self.conversation_id {
            id
        } else {
            let conv = self.client.create_conversation().await?;
            self.conversation_id = Some(conv.id);
            conv.id
        };

        let mut ws = GatewayWebSocket::connect(&self.url, conv_id).await?;
        ws.subscribe(vec!["context".to_string(), "tools".to_string()]).await?;

        self.ws = Some(ws);
        self.state = ConnectionState::Connected;
        Ok(())
    }

    /// Like `connect()`, but creates the conversation inside a specific project.
    ///
    /// Always creates a new conversation in the given project — the existing
    /// `conversation_id` is replaced. Use when the user selects a project from
    /// the project selector overlay.
    pub async fn connect_for_project(&mut self, project_id: Uuid) -> Result<(), ClientError> {
        self.client.health().await?;
        self.project_id = Some(project_id);

        let conv = self.client.create_project_conversation(project_id).await?;
        self.conversation_id = Some(conv.id);

        let mut ws = GatewayWebSocket::connect(&self.url, conv.id).await?;
        ws.subscribe(vec!["context".to_string(), "tools".to_string()]).await?;

        self.ws = Some(ws);
        self.state = ConnectionState::Connected;
        Ok(())
    }

    /// Closes the WebSocket and transitions to `Disconnected`.
    ///
    /// Safe to call when already disconnected — it is a no-op in that case.
    pub async fn disconnect(&mut self) {
        self.ws = None;
        self.state = ConnectionState::Disconnected;
    }

    /// Attempts to reconnect with bounded exponential backoff.
    ///
    /// Transitions to `Reconnecting` and loops calling `connect()` until it
    /// succeeds or the `cancel` receiver becomes `true`. Backoff delays:
    /// 1 s, 2 s, 4 s, 8 s, then capped at 15 s for all subsequent attempts.
    ///
    /// Returns `Ok(())` on successful connection or clean cancellation.
    /// Returns the last `Err` if the cancel signal fires during an error
    /// handling path.
    pub async fn reconnect_with_backoff(
        &mut self,
        cancel: watch::Receiver<bool>,
    ) -> Result<(), ClientError> {
        self.state = ConnectionState::Reconnecting;
        let mut attempt = 0u32;

        loop {
            // Check for cancellation before each attempt.
            if *cancel.borrow() {
                self.state = ConnectionState::Disconnected;
                return Ok(());
            }

            match self.connect().await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    // Check for cancellation before sleeping.
                    if *cancel.borrow() {
                        self.state = ConnectionState::Disconnected;
                        return Err(e);
                    }
                    let delay = backoff_delay(attempt);
                    attempt += 1;
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    // --- Event I/O ---

    /// Reads the next typed event from the WebSocket stream.
    ///
    /// Returns `Ok(None)` on a clean close. Returns
    /// `ClientError::WebSocketFailed` if the WebSocket is not open.
    pub async fn next_event(&mut self) -> Result<Option<ServerEvent>, ClientError> {
        match &mut self.ws {
            Some(ws) => ws.next_event().await,
            None => Err(ClientError::WebSocketFailed {
                address: self.url.clone(),
                message: "WebSocket is not connected".to_string(),
            }),
        }
    }

    /// Sends a user message to the active conversation turn.
    ///
    /// Returns `ClientError::WebSocketFailed` if the WebSocket is not open.
    pub async fn send_message(&mut self, content: String) -> Result<(), ClientError> {
        match &mut self.ws {
            Some(ws) => ws.send(ClientEvent::SendMessage { content }).await,
            None => Err(ClientError::WebSocketFailed {
                address: self.url.clone(),
                message: "WebSocket is not connected".to_string(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Backoff
// ---------------------------------------------------------------------------

/// Computes the sleep duration for a reconnection attempt.
///
/// Doubles on each attempt starting at 1 s, capped at 15 s:
/// attempt 0 → 1 s, 1 → 2 s, 2 → 4 s, 3 → 8 s, 4+ → 15 s.
pub(crate) fn backoff_delay(attempt: u32) -> std::time::Duration {
    // 2^attempt seconds, capped at 15 s. saturating_pow avoids overflow on
    // large attempt counts (e.g., a long-running reconnect loop).
    let secs = 2u64.saturating_pow(attempt).min(15);
    std::time::Duration::from_secs(secs)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Construction ---

    #[test]
    fn new_creates_disconnected_manager() {
        let mgr = ConnectionManager::new("http://127.0.0.1:3000".to_string());
        assert_eq!(mgr.connection_state(), ConnectionState::Disconnected);
    }

    #[test]
    fn url_returns_configured_address() {
        let mgr = ConnectionManager::new("http://127.0.0.1:3000".to_string());
        assert_eq!(mgr.url(), "http://127.0.0.1:3000");
    }

    #[test]
    fn conversation_id_is_none_after_new() {
        let mgr = ConnectionManager::new("http://127.0.0.1:3000".to_string());
        assert!(mgr.conversation_id().is_none());
    }

    #[test]
    fn connection_state_variants_are_distinct() {
        assert_ne!(ConnectionState::Connected, ConnectionState::Disconnected);
        assert_ne!(ConnectionState::Disconnected, ConnectionState::Reconnecting);
        assert_ne!(ConnectionState::Connected, ConnectionState::Reconnecting);
    }

    // --- Backoff ---

    #[test]
    fn backoff_first_attempt_is_one_second() {
        assert_eq!(backoff_delay(0), std::time::Duration::from_secs(1));
    }

    #[test]
    fn backoff_doubles_on_each_attempt() {
        assert_eq!(backoff_delay(1), std::time::Duration::from_secs(2));
        assert_eq!(backoff_delay(2), std::time::Duration::from_secs(4));
        assert_eq!(backoff_delay(3), std::time::Duration::from_secs(8));
    }

    #[test]
    fn backoff_caps_at_fifteen_seconds() {
        // attempt 4 = 16s → capped at 15s
        assert_eq!(backoff_delay(4), std::time::Duration::from_secs(15));
        assert_eq!(backoff_delay(5), std::time::Duration::from_secs(15));
        assert_eq!(backoff_delay(100), std::time::Duration::from_secs(15));
    }

    // --- disconnect ---

    #[tokio::test]
    async fn disconnect_on_disconnected_manager_is_noop() {
        let mut mgr = ConnectionManager::new("http://127.0.0.1:3000".to_string());
        mgr.disconnect().await;
        assert_eq!(mgr.connection_state(), ConnectionState::Disconnected);
    }

    // --- reconnect_with_backoff cancellation ---

    #[tokio::test]
    async fn reconnect_cancels_immediately_when_cancel_is_already_true() {
        // A pre-cancelled token means the loop exits before any connect() attempt.
        let (_tx, rx) = watch::channel(true);
        let mut mgr = ConnectionManager::new("http://127.0.0.1:3000".to_string());
        let result = mgr.reconnect_with_backoff(rx).await;
        // Returns Ok — cancellation is a clean exit, not an error.
        assert!(result.is_ok());
        // State is Disconnected after a cancelled reconnect.
        assert_eq!(mgr.connection_state(), ConnectionState::Disconnected);
    }

    // --- next_event / send_message without WebSocket ---

    #[tokio::test]
    async fn next_event_returns_error_when_disconnected() {
        let mut mgr = ConnectionManager::new("http://127.0.0.1:3000".to_string());
        let result = mgr.next_event().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn send_message_returns_error_when_disconnected() {
        let mut mgr = ConnectionManager::new("http://127.0.0.1:3000".to_string());
        let result = mgr.send_message("hello".to_string()).await;
        assert!(result.is_err());
    }
}
