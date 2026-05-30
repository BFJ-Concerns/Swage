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
    /// Bench context used when creating bench-scoped conversations.
    bench_id: Option<Uuid>,
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
            bench_id: None,
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
        let (conv_id, should_store_conversation_id) = if let Some(id) = self.conversation_id {
            (id, false)
        } else {
            let conv = self.client.create_conversation().await?;
            (conv.id, true)
        };

        let ws = GatewayWebSocket::connect(&self.url, conv_id).await?;
        self.finish_connect(ws).await?;
        if should_store_conversation_id {
            self.conversation_id = Some(conv_id);
        }
        Ok(())
    }

    /// Like `connect()`, but creates the conversation inside a specific bench.
    ///
    /// Always creates a new conversation in the given bench — the existing
    /// `conversation_id` is replaced. Use when the user selects a bench from
    /// the bench selector overlay.
    pub async fn connect_for_bench(&mut self, bench_id: Uuid) -> Result<(), ClientError> {
        self.client.health().await?;

        let conv = self.client.create_bench_conversation(bench_id).await?;
        let ws = GatewayWebSocket::connect(&self.url, conv.id).await?;
        self.finish_connect(ws).await?;
        self.bench_id = Some(bench_id);
        self.conversation_id = Some(conv.id);
        Ok(())
    }

    /// Starts a fresh conversation while preserving the current bench scope.
    ///
    /// This replaces the active WebSocket only after the new conversation is
    /// successfully created and subscribed, so a failure does not discard the
    /// existing live session.
    pub async fn start_new_conversation(&mut self) -> Result<(), ClientError> {
        self.client.health().await?;

        let conv = if let Some(bench_id) = self.bench_id {
            self.client.create_bench_conversation(bench_id).await?
        } else {
            self.client.create_conversation().await?
        };

        let ws = GatewayWebSocket::connect(&self.url, conv.id).await?;
        self.finish_connect(ws).await?;
        self.conversation_id = Some(conv.id);
        Ok(())
    }

    /// Subscribes the WebSocket to observability channels, stores it, and marks
    /// the connection as `Connected`.
    ///
    /// Shared tail of `connect()` and `connect_for_bench()` — both obtain a
    /// fresh `GatewayWebSocket` and then need identical subscription + state
    /// bookkeeping.
    async fn finish_connect(&mut self, mut ws: GatewayWebSocket) -> Result<(), ClientError> {
        ws.subscribe(vec!["context".into(), "tools".into()]).await?;
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

    pub fn begin_reconnect(&mut self) {
        self.ws = None;
        self.state = ConnectionState::Reconnecting;
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
        mut cancel: watch::Receiver<bool>,
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
                    // Race the backoff delay against the cancel signal so the
                    // application can exit promptly even during a long wait.
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {},
                        _ = cancel.changed() => {
                            self.state = ConnectionState::Disconnected;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    // --- Event I/O ---

    /// Returns a mutable reference to the active WebSocket, or a
    /// `WebSocketFailed` error if the connection is not open.
    ///
    /// Used by `next_event()` and `send_message()` to avoid duplicating the
    /// identical `None` arm in each match.
    fn ws_required(&mut self) -> Result<&mut GatewayWebSocket, ClientError> {
        self.ws
            .as_mut()
            .ok_or_else(|| ClientError::WebSocketFailed {
                address: self.url.clone(),
                message: "WebSocket is not connected".to_string(),
            })
    }

    /// Reads the next typed event from the WebSocket stream.
    ///
    /// Returns `Ok(None)` on a clean close. Returns
    /// `ClientError::WebSocketFailed` if the WebSocket is not open.
    pub async fn next_event(&mut self) -> Result<Option<ServerEvent>, ClientError> {
        self.ws_required()?.next_event().await
    }

    /// Sends a user message to the active conversation turn.
    ///
    /// Returns `ClientError::WebSocketFailed` if the WebSocket is not open.
    pub async fn send_message(&mut self, content: String) -> Result<(), ClientError> {
        self.ws_required()?
            .send(ClientEvent::SendMessage {
                content,
                thread_id: None,
                selected_model_id: None,
            })
            .await
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
    // Test servers are short-lived Axum tasks owned by the individual test; abort
    // is the cleanup path once assertions have observed the client behaviour.
    #![allow(clippy::disallowed_methods)]

    use super::*;
    use axum::{
        Json, Router,
        extract::{
            Path, State, WebSocketUpgrade,
            ws::{CloseFrame, Message, close_code},
        },
        http::StatusCode,
        response::IntoResponse,
        routing::{get, post},
    };
    use chrono::Utc;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct TestGatewayState {
        health_status: StatusCode,
        bench_session_status: StatusCode,
        bench_requests: Arc<Mutex<Vec<Uuid>>>,
        created_conversations: Arc<Mutex<Vec<Uuid>>>,
        websocket_conversations: Arc<Mutex<Vec<Uuid>>>,
    }

    impl TestGatewayState {
        fn ok() -> Self {
            Self {
                health_status: StatusCode::OK,
                bench_session_status: StatusCode::CREATED,
                bench_requests: Arc::new(Mutex::new(Vec::new())),
                created_conversations: Arc::new(Mutex::new(Vec::new())),
                websocket_conversations: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    async fn health(State(state): State<TestGatewayState>) -> impl IntoResponse {
        if state.health_status == StatusCode::OK {
            (StatusCode::OK, Json(serde_json::json!({ "status": "ok" }))).into_response()
        } else {
            (
                state.health_status,
                Json(serde_json::json!({ "error": "unavailable" })),
            )
                .into_response()
        }
    }

    async fn create_bench_conversation(
        State(state): State<TestGatewayState>,
        Path(bench_id): Path<Uuid>,
    ) -> impl IntoResponse {
        if state.bench_session_status != StatusCode::CREATED {
            return (
                state.bench_session_status,
                Json(serde_json::json!({ "error": "cannot create conversation" })),
            )
                .into_response();
        }

        let conversation_id = Uuid::new_v4();
        state.bench_requests.lock().unwrap().push(bench_id);
        state
            .created_conversations
            .lock()
            .unwrap()
            .push(conversation_id);

        (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "id": conversation_id,
                "bench_id": bench_id,
                "created_at": Utc::now(),
                "last_activity": Utc::now(),
            })),
        )
            .into_response()
    }

    async fn websocket_route(
        State(state): State<TestGatewayState>,
        Path(conversation_id): Path<Uuid>,
        ws: WebSocketUpgrade,
    ) -> impl IntoResponse {
        ws.on_upgrade(move |mut socket| async move {
            state
                .websocket_conversations
                .lock()
                .unwrap()
                .push(conversation_id);
            let _ = socket.recv().await;
            let _ = socket
                .send(Message::Close(Some(CloseFrame {
                    code: close_code::NORMAL,
                    reason: "test close".into(),
                })))
                .await;
        })
    }

    async fn spawn_test_gateway(
        state: TestGatewayState,
    ) -> (String, tokio::task::JoinHandle<()>, TestGatewayState) {
        let router = Router::new()
            .route("/api/health", get(health))
            .route(
                "/api/benches/{bench_id}/sessions",
                post(create_bench_conversation),
            )
            .route(
                "/api/conversations/{conversation_id}/ws",
                get(websocket_route),
            )
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test gateway listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve test gateway");
        });

        (format!("http://{address}"), server, state)
    }

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

    #[test]
    fn begin_reconnect_sets_reconnecting_state() {
        let mut mgr = ConnectionManager::new("http://127.0.0.1:3000".to_string());
        mgr.begin_reconnect();
        assert_eq!(mgr.connection_state(), ConnectionState::Reconnecting);
    }

    #[tokio::test]
    async fn connect_for_bench_creates_bench_scoped_conversation() {
        let (base_url, server, state) = spawn_test_gateway(TestGatewayState::ok()).await;
        let bench_id = Uuid::new_v4();
        let mut mgr = ConnectionManager::new(base_url);

        mgr.connect_for_bench(bench_id)
            .await
            .expect("connect_for_bench should succeed");

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert_eq!(mgr.connection_state(), ConnectionState::Connected);
        assert_eq!(state.bench_requests.lock().unwrap().as_slice(), &[bench_id]);
        let created = state.created_conversations.lock().unwrap().clone();
        assert_eq!(created.len(), 1);
        assert_eq!(mgr.conversation_id(), Some(created[0]));
        assert_eq!(
            state.websocket_conversations.lock().unwrap().as_slice(),
            &[created[0]]
        );

        server.abort();
    }

    #[tokio::test]
    async fn connect_for_bench_propagates_health_check_failure() {
        let mut state = TestGatewayState::ok();
        state.health_status = StatusCode::SERVICE_UNAVAILABLE;
        let (base_url, server, _) = spawn_test_gateway(state).await;
        let bench_id = Uuid::new_v4();
        let mut mgr = ConnectionManager::new(base_url);

        let result = mgr.connect_for_bench(bench_id).await;

        assert!(result.is_err());
        assert_eq!(mgr.connection_state(), ConnectionState::Disconnected);
        assert!(mgr.conversation_id().is_none());

        server.abort();
    }

    #[tokio::test]
    async fn connect_for_bench_propagates_conversation_creation_failure() {
        let mut state = TestGatewayState::ok();
        state.bench_session_status = StatusCode::INTERNAL_SERVER_ERROR;
        let (base_url, server, state) = spawn_test_gateway(state).await;
        let bench_id = Uuid::new_v4();
        let mut mgr = ConnectionManager::new(base_url);

        let result = mgr.connect_for_bench(bench_id).await;

        assert!(result.is_err());
        assert_eq!(mgr.connection_state(), ConnectionState::Disconnected);
        assert!(mgr.conversation_id().is_none());
        assert!(state.websocket_conversations.lock().unwrap().is_empty());

        server.abort();
    }

    #[tokio::test]
    async fn start_new_conversation_replaces_existing_bench_conversation() {
        let (base_url, server, state) = spawn_test_gateway(TestGatewayState::ok()).await;
        let bench_id = Uuid::new_v4();
        let mut mgr = ConnectionManager::new(base_url);

        mgr.connect_for_bench(bench_id)
            .await
            .expect("initial bench connection should succeed");
        let first_conversation_id = mgr.conversation_id().expect("first conversation id");

        mgr.start_new_conversation()
            .await
            .expect("starting a new conversation should succeed");
        let second_conversation_id = mgr.conversation_id().expect("second conversation id");

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert_ne!(first_conversation_id, second_conversation_id);
        let created = state.created_conversations.lock().unwrap().clone();
        assert_eq!(created.len(), 2);
        assert_eq!(created[0], first_conversation_id);
        assert_eq!(created[1], second_conversation_id);

        server.abort();
    }

    #[tokio::test]
    async fn clean_websocket_close_is_reported_for_reconnect_path() {
        let (base_url, server, _state) = spawn_test_gateway(TestGatewayState::ok()).await;
        let mut mgr = ConnectionManager::new(base_url);
        let bench_id = Uuid::new_v4();

        mgr.connect_for_bench(bench_id)
            .await
            .expect("initial connection should succeed");
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), mgr.next_event())
            .await
            .expect("clean close should be observed")
            .expect("clean close should not be an error");

        assert!(event.is_none(), "clean close should return Ok(None)");

        server.abort();
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
