use std::{fmt, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use swage_core::GatewayError;
use swage_storage::{NewAuditEvent, NewChannelInboundMessage, PendingChannelOrigin};

use crate::{
    channel_gateway::{DeliveryTarget, ExternalChannelIntegration},
    onboarding::CreateChannelLinkRequest,
    state::AppState,
};

const TELEGRAM_MESSAGE_LIMIT_UTF16: usize = 4096;

pub struct TelegramClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
    bot_id: OnceCell<String>,
}

#[derive(Clone)]
pub struct TelegramClientConfig {
    pub base_url: String,
    pub token: String,
    pub request_timeout: Duration,
}

impl fmt::Debug for TelegramClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelegramClientConfig")
            .field("base_url", &self.base_url)
            .field("token", &"<redacted>")
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl TelegramClient {
    pub fn new(config: TelegramClientConfig) -> Result<Self, GatewayError> {
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .build()
            .map_err(|error| {
                GatewayError::ValidationError(format!(
                    "failed to build telegram HTTP client: {}",
                    error.without_url()
                ))
            })?;
        Ok(Self {
            http,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            token: config.token,
            bot_id: OnceCell::new(),
        })
    }

    pub async fn connect(config: TelegramClientConfig) -> Result<Self, GatewayError> {
        let client = Self::new(config)?;
        client.ensure_bot_id().await?;
        Ok(client)
    }

    pub fn bot_id(&self) -> Option<&str> {
        self.bot_id.get().map(String::as_str)
    }

    async fn ensure_bot_id(&self) -> Result<&str, GatewayError> {
        self.bot_id
            .get_or_try_init(|| async { self.get_me_bot_id().await })
            .await
            .map(String::as_str)
    }

    fn method_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base_url, self.token, method)
    }

    pub async fn send_message(&self, chat_id: &str, content: &str) -> Result<(), GatewayError> {
        for chunk in split_telegram_message(content) {
            let response = self
                .http
                .post(self.method_url("sendMessage"))
                .json(&SendMessageRequest {
                    chat_id,
                    text: &chunk,
                })
                .send()
                .await
                .map_err(|error| {
                    GatewayError::ValidationError(format!(
                        "telegram sendMessage failed: {}",
                        error.without_url()
                    ))
                })?;
            if !response.status().is_success() {
                return Err(GatewayError::ValidationError(format!(
                    "telegram sendMessage failed with status {}",
                    response.status()
                )));
            }
        }
        Ok(())
    }

    async fn get_updates(
        &self,
        offset: Option<i64>,
        timeout_secs: u64,
    ) -> Result<Vec<TelegramUpdate>, GatewayError> {
        let response = self
            .http
            .post(self.method_url("getUpdates"))
            .json(&GetUpdatesRequest {
                offset,
                timeout: timeout_secs,
                allowed_updates: &["message"],
            })
            .send()
            .await
            .map_err(|error| {
                GatewayError::ValidationError(format!(
                    "telegram getUpdates failed: {}",
                    error.without_url()
                ))
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let conflict_hint = if status.as_u16() == 409 {
                " (another poller or webhook may be holding this bot token)"
            } else {
                ""
            };
            return Err(GatewayError::ValidationError(format!(
                "telegram getUpdates failed with status {status}{conflict_hint}"
            )));
        }
        let body = response
            .json::<TelegramResponse<Vec<TelegramUpdate>>>()
            .await
            .map_err(|error| {
                GatewayError::ValidationError(format!(
                    "telegram getUpdates response was invalid: {}",
                    error.without_url()
                ))
            })?;
        if !body.ok {
            return Err(GatewayError::ValidationError(format!(
                "telegram getUpdates failed: {}",
                body.description.unwrap_or_else(|| "unknown error".into())
            )));
        }
        Ok(body.result.unwrap_or_default())
    }

    async fn get_me_bot_id(&self) -> Result<String, GatewayError> {
        let response = self
            .http
            .post(self.method_url("getMe"))
            .send()
            .await
            .map_err(|error| {
                GatewayError::ValidationError(format!(
                    "telegram getMe failed: {}",
                    error.without_url()
                ))
            })?;
        if !response.status().is_success() {
            return Err(GatewayError::ValidationError(format!(
                "telegram getMe failed with status {}",
                response.status()
            )));
        }
        let body = response
            .json::<TelegramResponse<TelegramUser>>()
            .await
            .map_err(|error| {
                GatewayError::ValidationError(format!(
                    "telegram getMe response was invalid: {}",
                    error.without_url()
                ))
            })?;
        if !body.ok {
            return Err(GatewayError::ValidationError(format!(
                "telegram getMe failed: {}",
                body.description.unwrap_or_else(|| "unknown error".into())
            )));
        }
        let user = body.result.ok_or_else(|| {
            GatewayError::ValidationError("telegram getMe returned no bot".into())
        })?;
        Ok(user.id.to_string())
    }
}

#[derive(Serialize)]
struct GetUpdatesRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<i64>,
    timeout: u64,
    allowed_updates: &'a [&'a str],
}

#[derive(Serialize)]
struct SendMessageRequest<'a> {
    chat_id: &'a str,
    text: &'a str,
}

#[derive(Debug, Deserialize)]
struct TelegramResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramUser {
    id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
    from: Option<TelegramUser>,
    text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramChat {
    id: i64,
}

pub fn spawn_poll_loop_until_cancelled(
    state: AppState,
    shutdown: CancellationToken,
) -> Option<JoinHandle<()>> {
    let client = state.telegram.as_ref()?.clone();
    Some(tokio::spawn(async move {
        run_poll_loop(state, client, shutdown).await;
    }))
}

async fn run_poll_loop(state: AppState, client: Arc<TelegramClient>, shutdown: CancellationToken) {
    info!("telegram polling loop started");
    let mut failing = false;
    let mut backoff = Duration::from_secs(1);

    loop {
        if shutdown.is_cancelled() {
            break;
        }

        let bot_id = match client.ensure_bot_id().await {
            Ok(bot_id) => bot_id.to_string(),
            Err(error) => {
                warn!("{error}");
                if !failing {
                    let _ = record_telegram_audit(
                        &state,
                        "polling_failed",
                        "telegram polling failed",
                        serde_json::json!({ "error": error.to_string() }),
                    )
                    .await;
                    failing = true;
                }
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    () = tokio::time::sleep(backoff) => {}
                }
                backoff = next_poll_backoff(backoff);
                continue;
            }
        };

        let cursor = match state.storage.get_channel_cursor("telegram", &bot_id).await {
            Ok(cursor) => cursor.and_then(|value| value.parse::<i64>().ok()),
            Err(error) => {
                warn!("failed to read telegram cursor: {error}");
                None
            }
        };
        let offset = cursor.map(|value| value + 1);
        let timeout_secs = state
            .active_config()
            .channels
            .telegram
            .as_ref()
            .map(|config| config.poll_timeout_secs)
            .unwrap_or(25);

        let updates = tokio::select! {
            () = shutdown.cancelled() => break,
            result = client.get_updates(offset, timeout_secs) => result,
        };

        match updates {
            Ok(updates) => {
                if failing {
                    let _ = record_telegram_audit(
                        &state,
                        "polling_recovered",
                        "telegram polling recovered",
                        serde_json::json!({ "bot_id": bot_id }),
                    )
                    .await;
                    failing = false;
                }
                backoff = Duration::from_secs(1);
                if let Err(error) = ingest_updates(&state, &client, &bot_id, updates).await {
                    warn!("failed to ingest telegram updates: {error}");
                }
            }
            Err(error) => {
                warn!("{error}");
                if !failing {
                    let _ = record_telegram_audit(
                        &state,
                        "polling_failed",
                        "telegram polling failed",
                        serde_json::json!({
                            "bot_id": bot_id,
                            "error": error.to_string(),
                        }),
                    )
                    .await;
                    failing = true;
                }
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    () = tokio::time::sleep(backoff) => {}
                }
                backoff = next_poll_backoff(backoff);
            }
        }
    }
}

fn next_poll_backoff(current: Duration) -> Duration {
    (current * 2).min(Duration::from_secs(60))
}

async fn ingest_updates(
    state: &AppState,
    client: &TelegramClient,
    bot_id: &str,
    updates: Vec<TelegramUpdate>,
) -> Result<(), GatewayError> {
    if updates.is_empty() {
        return Ok(());
    }

    let mut messages = Vec::new();
    let mut controls = Vec::new();
    let mut unsupported = Vec::new();
    let mut max_update_id = i64::MIN;
    for update in updates {
        max_update_id = max_update_id.max(update.update_id);
        let Some(message) = update.message else {
            unsupported.push((update.update_id, "missing_message"));
            continue;
        };
        let Some(text) = message.text else {
            unsupported.push((update.update_id, "non_text_message"));
            continue;
        };
        if let Some(code) = link_code_from_text(&text) {
            controls.push(LinkControl {
                update_id: update.update_id.to_string(),
                chat_id: message.chat.id.to_string(),
                code,
            });
        }
        messages.push(NewChannelInboundMessage {
            integration: "telegram".into(),
            account: bot_id.to_string(),
            conversation: message.chat.id.to_string(),
            external_id: update.update_id.to_string(),
            sender: message
                .from
                .map(|from| from.id.to_string())
                .unwrap_or_else(|| message.chat.id.to_string()),
            content: text,
        });
    }

    let inserted = state
        .storage
        .ingest_channel_inbound(messages, "telegram", bot_id, &max_update_id.to_string())
        .await?;
    for (update_id, reason) in unsupported {
        if let Err(error) = record_unsupported_update(state, bot_id, update_id, reason).await {
            warn!("failed to audit unsupported telegram update: {error}");
        }
    }
    if inserted > 0 {
        for control in controls {
            if let Err(error) = redeem_link_control(state, client, bot_id, control).await {
                warn!("failed to redeem telegram link control: {error}");
            }
        }
        crate::channel_gateway::drain_channel_queue(state.clone()).await;
    }
    Ok(())
}

#[derive(Debug)]
struct LinkControl {
    update_id: String,
    chat_id: String,
    code: String,
}

fn link_code_from_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    for prefix in ["/link ", "/start "] {
        if let Some(code) = trimmed.strip_prefix(prefix) {
            let code = code.trim();
            if !code.is_empty() {
                return Some(code.to_string());
            }
        }
    }
    None
}

async fn redeem_link_control(
    state: &AppState,
    client: &TelegramClient,
    bot_id: &str,
    control: LinkControl,
) -> Result<(), GatewayError> {
    let row = state
        .storage
        .list_pending_by_origin(PendingChannelOrigin {
            integration: "telegram".into(),
            account: bot_id.to_string(),
            conversation: control.chat_id.clone(),
        })
        .await?
        .into_iter()
        .find(|row| row.external_id == control.update_id);

    let Some(row) = row else {
        return Ok(());
    };

    let record = {
        let mut codes = state.channel_link_codes.lock().await;
        codes.remove(&control.code)
    };

    let Some(record) = record else {
        state
            .storage
            .discard_channel_messages(vec![row.id], "link_attempt")
            .await?;
        client
            .send_message(
                &control.chat_id,
                "That link code is invalid or has expired.",
            )
            .await?;
        return Ok(());
    };

    if record.expires_at < chrono::Utc::now()
        || record.integration != ExternalChannelIntegration::Telegram
    {
        state
            .storage
            .discard_channel_messages(vec![row.id], "link_attempt")
            .await?;
        client
            .send_message(
                &control.chat_id,
                "That link code is invalid or has expired.",
            )
            .await?;
        return Ok(());
    }

    let response = crate::onboarding::create_channel_link(
        state,
        CreateChannelLinkRequest {
            integration: "telegram".into(),
            account: bot_id.to_string(),
            conversation: control.chat_id.clone(),
            bench_id: record.bench_id,
            agent_id: record.agent_id,
            home_target: record.home_target.or(Some(DeliveryTarget::Channel {
                integration: ExternalChannelIntegration::Telegram,
                account: bot_id.to_string(),
                conversation: control.chat_id.clone(),
            })),
        },
    )
    .await?;
    state
        .storage
        .discard_channel_messages(vec![row.id], "link_attempt")
        .await?;
    record_telegram_audit(
        state,
        "link_redeemed",
        "telegram link code redeemed",
        serde_json::json!({
            "bot_id": bot_id,
            "chat_id": control.chat_id,
            "session_id": response.session_id,
        }),
    )
    .await?;
    client
        .send_message(&response.conversation, "Telegram is linked to Swage.")
        .await?;
    Ok(())
}

async fn record_unsupported_update(
    state: &AppState,
    bot_id: &str,
    update_id: i64,
    reason: &str,
) -> Result<(), GatewayError> {
    record_telegram_audit(
        state,
        "unsupported_update",
        "telegram update ignored",
        serde_json::json!({
            "bot_id": bot_id,
            "update_id": update_id,
            "reason": reason,
        }),
    )
    .await
}

async fn record_telegram_audit(
    state: &AppState,
    event_kind: &str,
    summary: &str,
    metadata: serde_json::Value,
) -> Result<(), GatewayError> {
    state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id: None,
            entity_kind: "telegram_channel".into(),
            entity_id: "telegram".into(),
            event_kind: event_kind.into(),
            summary: summary.into(),
            metadata,
        })
        .await?;
    Ok(())
}

pub fn split_telegram_message(content: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_units = 0usize;

    for character in content.chars() {
        let units = character.len_utf16();
        if current_units + units > TELEGRAM_MESSAGE_LIMIT_UTF16 && !current.is_empty() {
            chunks.push(current);
            current = String::new();
            current_units = 0;
        }
        current.push(character);
        current_units += units;
    }

    if current.is_empty() {
        chunks.push(String::new());
    } else {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::Path;
    use std::pin::Pin;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use async_trait::async_trait;
    use axum::{
        Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::post,
    };
    use futures::StreamExt as _;
    use serde::Deserialize;
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use swage_core::{
        CompletionRequest, CompletionResponse, ProviderError, StreamChunk, ToolCompletionRequest,
        ToolCompletionResponse,
    };
    use swage_protocol::client::channel::CreateChannelLinkCodeRequest;
    use swage_provider::LlmProvider;
    use swage_storage::{
        AuditStore, ChannelLinkStore, ChannelQueueStore, DeliveryStore, SessionStore,
        SqliteStorage, ThreadStore, TurnStore, model::TurnState,
    };

    #[test]
    fn split_message_respects_utf16_limit() {
        let content = "a".repeat(4097);
        let chunks = split_telegram_message(&content);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].encode_utf16().count(), 4096);
        assert_eq!(chunks[1].encode_utf16().count(), 1);
    }

    #[test]
    fn split_message_does_not_split_surrogate_pair() {
        let content = format!("{}🙂", "a".repeat(4095));
        let chunks = split_telegram_message(&content);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].encode_utf16().count(), 4095);
        assert_eq!(chunks[1], "🙂");
    }

    #[test]
    fn telegram_client_config_debug_redacts_token() {
        let config = TelegramClientConfig {
            base_url: "https://api.telegram.org".into(),
            token: "123456:secret-token".into(),
            request_timeout: Duration::from_secs(30),
        };

        let rendered = format!("{config:?}");

        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("123456:secret-token"));
    }

    #[test]
    fn telegram_client_new_does_not_call_get_me() {
        let client = TelegramClient::new(TelegramClientConfig {
            base_url: "http://127.0.0.1:9".into(),
            token: "test-token".into(),
            request_timeout: Duration::from_millis(1),
        })
        .expect("client construction should not depend on Telegram availability");

        assert!(client.bot_id().is_none());
    }

    #[test]
    fn poll_backoff_progresses_and_caps() {
        let mut backoff = Duration::from_secs(1);
        let mut observed = Vec::new();

        for _ in 0..8 {
            backoff = next_poll_backoff(backoff);
            observed.push(backoff);
        }

        assert_eq!(
            observed,
            vec![
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(32),
                Duration::from_secs(60),
                Duration::from_secs(60),
                Duration::from_secs(60),
            ]
        );
    }

    struct MockProvider {
        rounds: Mutex<VecDeque<Vec<StreamChunk>>>,
    }

    impl MockProvider {
        fn new(rounds: Vec<Vec<StreamChunk>>) -> Self {
            Self {
                rounds: Mutex::new(rounds.into()),
            }
        }

        fn text_round(content: &str) -> Vec<StreamChunk> {
            vec![
                StreamChunk::Delta {
                    content: content.into(),
                },
                StreamChunk::Done {
                    finish_reason: None,
                },
            ]
        }
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn model_id(&self) -> &str {
            "telegram-test"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unreachable!("telegram tests use streaming only")
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unreachable!("telegram tests use streaming only")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            let chunks = self.rounds.lock().unwrap().pop_front().unwrap_or_else(|| {
                vec![StreamChunk::Done {
                    finish_reason: None,
                }]
            });
            Box::pin(futures::stream::iter(chunks))
        }
    }

    struct GatedProvider {
        rounds: Mutex<VecDeque<GatedRound>>,
        requests: Arc<Mutex<Vec<CompletionRequest>>>,
    }

    struct GatedRound {
        started: Option<oneshot::Sender<()>>,
        release: oneshot::Receiver<()>,
        content: String,
    }

    impl GatedProvider {
        fn new(rounds: Vec<(oneshot::Sender<()>, oneshot::Receiver<()>, &str)>) -> Self {
            Self {
                rounds: Mutex::new(
                    rounds
                        .into_iter()
                        .map(|(started, release, content)| GatedRound {
                            started: Some(started),
                            release,
                            content: content.into(),
                        })
                        .collect(),
                ),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn requests(&self) -> Arc<Mutex<Vec<CompletionRequest>>> {
            self.requests.clone()
        }
    }

    #[async_trait]
    impl LlmProvider for GatedProvider {
        fn model_id(&self) -> &str {
            "telegram-gated-test"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            unreachable!("telegram tests use streaming only")
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unreachable!("telegram tests use streaming only")
        }

        fn stream(
            &self,
            request: CompletionRequest,
        ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
            self.requests.lock().unwrap().push(request);
            let mut round = self
                .rounds
                .lock()
                .unwrap()
                .pop_front()
                .expect("no more gated rounds configured");
            let content = round.content.clone();
            if let Some(started) = round.started.take() {
                let _ = started.send(());
            }
            Box::pin(
                futures::stream::once(async move {
                    let _ = round.release.await;
                    StreamChunk::Delta { content }
                })
                .chain(futures::stream::once(async {
                    StreamChunk::Done {
                        finish_reason: None,
                    }
                })),
            )
        }
    }

    #[derive(Clone, Default)]
    struct MockTelegramState {
        updates: Arc<Mutex<Vec<TelegramUpdate>>>,
        sent: Arc<Mutex<Vec<SentTelegramMessage>>>,
        offsets: Arc<Mutex<Vec<Option<i64>>>>,
        fail_send: Arc<AtomicBool>,
    }

    impl MockTelegramState {
        fn push_update(&self, update_id: i64, chat_id: i64, text: &str) {
            self.updates.lock().unwrap().push(TelegramUpdate {
                update_id,
                message: Some(TelegramMessage {
                    chat: TelegramChat { id: chat_id },
                    from: Some(TelegramUser { id: chat_id }),
                    text: Some(text.into()),
                }),
            });
        }

        fn push_non_text_update(&self, update_id: i64, chat_id: i64) {
            self.updates.lock().unwrap().push(TelegramUpdate {
                update_id,
                message: Some(TelegramMessage {
                    chat: TelegramChat { id: chat_id },
                    from: Some(TelegramUser { id: chat_id }),
                    text: None,
                }),
            });
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    struct SentTelegramMessage {
        chat_id: String,
        text: String,
    }

    #[derive(Debug, Deserialize)]
    struct GetUpdatesBody {
        offset: Option<i64>,
    }

    async fn start_mock_telegram() -> (String, MockTelegramState) {
        async fn get_me() -> Json<serde_json::Value> {
            Json(json!({
                "ok": true,
                "result": { "id": 12345 }
            }))
        }

        async fn send_message(
            State(state): State<MockTelegramState>,
            Json(body): Json<SentTelegramMessage>,
        ) -> axum::response::Response {
            if state.fail_send.load(Ordering::SeqCst) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "ok": false,
                        "description": "forced failure"
                    })),
                )
                    .into_response();
            }
            state.sent.lock().unwrap().push(body);
            Json(json!({
                "ok": true,
                "result": { "message_id": 1 }
            }))
            .into_response()
        }

        async fn get_updates(
            State(state): State<MockTelegramState>,
            Json(body): Json<GetUpdatesBody>,
        ) -> Json<serde_json::Value> {
            state.offsets.lock().unwrap().push(body.offset);
            let updates = state
                .updates
                .lock()
                .unwrap()
                .iter()
                .filter(|update| {
                    body.offset
                        .map(|offset| update.update_id >= offset)
                        .unwrap_or(true)
                })
                .cloned()
                .collect::<Vec<_>>();
            Json(json!({
                "ok": true,
                "result": updates,
            }))
        }

        let state = MockTelegramState::default();
        let app = Router::new()
            .route("/bot{token}/getMe", post(get_me))
            .route("/bot{token}/getUpdates", post(get_updates))
            .route("/bot{token}/sendMessage", post(send_message))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), state)
    }

    async fn connect_mock_client(base_url: String) -> Arc<TelegramClient> {
        Arc::new(
            TelegramClient::connect(TelegramClientConfig {
                base_url,
                token: "test-token".into(),
                request_timeout: Duration::from_secs(5),
            })
            .await
            .unwrap(),
        )
    }

    fn test_state(
        storage: Arc<SqliteStorage>,
        provider: impl LlmProvider + 'static,
        client: Arc<TelegramClient>,
    ) -> AppState {
        let mut state = AppState::new(
            Arc::clone(&storage) as Arc<dyn swage_storage::store::Storage>,
            Arc::clone(&storage) as Arc<dyn swage_storage::store::BenchStore>,
            Arc::new(provider),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        );
        state.telegram = Some(client);
        state
    }

    fn file_state(
        db_path: &Path,
        provider: impl LlmProvider + 'static,
        client: Arc<TelegramClient>,
    ) -> (AppState, Arc<SqliteStorage>) {
        let storage = Arc::new(SqliteStorage::open(db_path).unwrap());
        let state = test_state(storage.clone(), provider, client);
        (state, storage)
    }

    async fn issue_link_code(state: &AppState) -> String {
        crate::onboarding::create_channel_link_code(
            state,
            CreateChannelLinkCodeRequest {
                integration: ExternalChannelIntegration::Telegram,
                bench_id: None,
                agent_id: None,
                home_target: None,
            },
        )
        .await
        .unwrap()
        .link_code
    }

    async fn poll_once(state: &AppState, client: &TelegramClient) -> Result<(), GatewayError> {
        let bot_id = client.ensure_bot_id().await?.to_string();
        let cursor = state
            .storage
            .get_channel_cursor("telegram", &bot_id)
            .await?
            .and_then(|value| value.parse::<i64>().ok());
        let updates = client.get_updates(cursor.map(|value| value + 1), 0).await?;
        ingest_updates(state, client, &bot_id, updates).await
    }

    async fn link_chat(
        state: &AppState,
        client: &TelegramClient,
        telegram: &MockTelegramState,
        update_id: i64,
        chat_id: i64,
    ) {
        let code = issue_link_code(state).await;
        telegram.push_update(update_id, chat_id, &format!("/link {code}"));
        poll_once(state, client).await.unwrap();
    }

    async fn startup_recover_channel_queue(state: &AppState) {
        state
            .storage
            .requeue_presented_for_inflight_turns()
            .await
            .unwrap();
        crate::shutdown::recover_orphaned_turns(state.storage.as_ref(), state.bench_store.as_ref())
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn link_and_reply_journey_uses_same_telegram_chat() {
        let (base_url, telegram_state) = start_mock_telegram().await;
        let client = connect_mock_client(base_url).await;
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let state = test_state(
            storage.clone(),
            MockProvider::new(vec![MockProvider::text_round("telegram reply")]),
            client.clone(),
        );

        link_chat(&state, &client, &telegram_state, 1, 777).await;
        telegram_state.push_update(2, 777, "hello");
        poll_once(&state, &client).await.unwrap();

        let sent = telegram_state.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].chat_id, "777");
        assert_eq!(sent[0].text, "Telegram is linked to Swage.");
        assert_eq!(sent[1].chat_id, "777");
        assert_eq!(sent[1].text, "telegram reply");

        let deliveries = storage.list_delivery_records().await.unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].status, "sent");
        assert_eq!(deliveries[0].content, "telegram reply");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn link_code_is_single_use() {
        let (base_url, telegram) = start_mock_telegram().await;
        let client = connect_mock_client(base_url).await;
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let state = test_state(
            storage.clone(),
            MockProvider::new(vec![MockProvider::text_round("unused")]),
            client.clone(),
        );
        let code = issue_link_code(&state).await;

        telegram.push_update(1, 777, &format!("/link {code}"));
        poll_once(&state, &client).await.unwrap();
        telegram.push_update(2, 888, &format!("/link {code}"));
        poll_once(&state, &client).await.unwrap();

        let links = storage.list_channel_links().await.unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].conversation, "777");
        let sent = telegram.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].chat_id, "888");
        assert_eq!(sent[1].text, "That link code is invalid or has expired.");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn expired_link_code_is_rejected_without_creating_link() {
        let (base_url, telegram) = start_mock_telegram().await;
        let client = connect_mock_client(base_url).await;
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let state = test_state(
            storage.clone(),
            MockProvider::new(vec![MockProvider::text_round("unused")]),
            client.clone(),
        );
        let code = issue_link_code(&state).await;
        {
            let mut codes = state.channel_link_codes.lock().await;
            codes.get_mut(&code).unwrap().expires_at =
                chrono::Utc::now() - chrono::Duration::seconds(1);
        }

        telegram.push_update(1, 777, &format!("/link {code}"));
        poll_once(&state, &client).await.unwrap();

        assert!(storage.list_channel_links().await.unwrap().is_empty());
        let sent = telegram.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].text, "That link code is invalid or has expired.");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn restart_polling_resumes_offset_without_replaying_update() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("telegram-restart.db");
        let (base_url, telegram) = start_mock_telegram().await;
        let client = connect_mock_client(base_url).await;

        let (state, storage) = file_state(
            &db_path,
            MockProvider::new(vec![MockProvider::text_round("before restart")]),
            client.clone(),
        );
        link_chat(&state, &client, &telegram, 1, 777).await;
        telegram.push_update(2, 777, "before");
        poll_once(&state, &client).await.unwrap();
        let first_session = storage
            .find_session_by_channel_origin("telegram", client.bot_id().unwrap(), "777")
            .await
            .unwrap()
            .unwrap()
            .id;

        let (restarted, restarted_storage) = file_state(
            &db_path,
            MockProvider::new(vec![MockProvider::text_round("after restart")]),
            client.clone(),
        );
        telegram.push_update(3, 777, "after");
        poll_once(&restarted, &client).await.unwrap();

        assert!(
            telegram
                .offsets
                .lock()
                .unwrap()
                .iter()
                .any(|offset| *offset == Some(3)),
            "restarted poll should request offset after the last persisted update"
        );
        let session_after_restart = restarted_storage
            .find_session_by_channel_origin("telegram", client.bot_id().unwrap(), "777")
            .await
            .unwrap()
            .unwrap()
            .id;
        assert_eq!(first_session, session_after_restart);
        let turns = restarted_storage
            .list_turns_by_thread(
                restarted_storage
                    .list_by_session(first_session)
                    .await
                    .unwrap()[0]
                    .id,
            )
            .await
            .unwrap();
        let user_inputs: Vec<_> = turns
            .iter()
            .filter_map(|turn| turn.user_input.as_deref())
            .collect();
        assert_eq!(user_inputs, vec!["before", "after"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn queued_mid_turn_messages_are_presented_together_next_turn() {
        let (base_url, telegram) = start_mock_telegram().await;
        let client = connect_mock_client(base_url).await;
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let (first_started_tx, first_started_rx) = oneshot::channel();
        let (first_release_tx, first_release_rx) = oneshot::channel();
        let (second_started_tx, second_started_rx) = oneshot::channel();
        let (second_release_tx, second_release_rx) = oneshot::channel();
        let provider = GatedProvider::new(vec![
            (first_started_tx, first_release_rx, "first reply"),
            (second_started_tx, second_release_rx, "batched reply"),
        ]);
        let requests = provider.requests();
        let state = test_state(storage.clone(), provider, client.clone());

        link_chat(&state, &client, &telegram, 1, 777).await;
        telegram.push_update(2, 777, "A");
        let first_state = state.clone();
        let first_client = client.clone();
        let first_poll = tokio::spawn(async move {
            poll_once(&first_state, &first_client).await.unwrap();
        });
        first_started_rx.await.unwrap();

        telegram.push_update(3, 777, "B");
        telegram.push_update(4, 777, "C");
        poll_once(&state, &client).await.unwrap();
        assert_eq!(
            storage.list_pending_channel_origins().await.unwrap().len(),
            1
        );

        first_release_tx.send(()).unwrap();
        first_poll.await.unwrap();

        let drain_state = state.clone();
        let drain = tokio::spawn(async move {
            crate::channel_gateway::drain_channel_queue(drain_state).await;
        });
        second_started_rx.await.unwrap();
        second_release_tx.send(()).unwrap();
        drain.await.unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        let second_prompt = requests[1]
            .messages
            .iter()
            .map(|message| message.text_content())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            second_prompt.contains("B\n\nC"),
            "second turn should present queued Telegram messages together; prompt was {second_prompt:?}"
        );
        let sent = telegram.sent.lock().unwrap().clone();
        assert_eq!(sent.last().unwrap().text, "batched reply");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn restart_drains_messages_received_while_prior_turn_was_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("telegram-requeue.db");
        let (base_url, telegram) = start_mock_telegram().await;
        let client = connect_mock_client(base_url).await;

        let (state, storage) = file_state(&db_path, MockProvider::new(vec![]), client.clone());
        link_chat(&state, &client, &telegram, 1, 777).await;
        let session = storage
            .find_session_by_channel_origin("telegram", client.bot_id().unwrap(), "777")
            .await
            .unwrap()
            .unwrap();
        let turn_id =
            crate::automated_turn::create_placeholder_turn(&state, session.id, "gateway", "A")
                .await
                .unwrap();
        state
            .active_turns
            .lock()
            .await
            .insert(session.id, CancellationToken::new());

        telegram.push_update(2, 777, "B");
        telegram.push_update(3, 777, "C");
        poll_once(&state, &client).await.unwrap();
        assert!(
            !storage
                .list_pending_channel_origins()
                .await
                .unwrap()
                .is_empty(),
            "messages received while A is in flight should be pending before restart"
        );

        let (restarted, restarted_storage) = file_state(
            &db_path,
            MockProvider::new(vec![MockProvider::text_round("after crash")]),
            client.clone(),
        );
        startup_recover_channel_queue(&restarted).await;
        crate::channel_gateway::drain_channel_queue_checked(&restarted)
            .await
            .unwrap();

        assert!(
            telegram
                .offsets
                .lock()
                .unwrap()
                .iter()
                .any(|offset| *offset == Some(2)),
            "second poll should have consumed B/C through getUpdates"
        );
        assert_eq!(
            restarted_storage
                .get_channel_cursor("telegram", client.bot_id().unwrap())
                .await
                .unwrap()
                .as_deref(),
            Some("3")
        );
        let session = restarted_storage
            .find_session_by_channel_origin("telegram", client.bot_id().unwrap(), "777")
            .await
            .unwrap()
            .unwrap();
        let thread = restarted_storage.list_by_session(session.id).await.unwrap()[0].clone();
        let turns = restarted_storage
            .list_turns_by_thread(thread.id)
            .await
            .unwrap();
        assert!(
            turns.iter().any(|turn| turn.state == TurnState::Interrupted
                && turn.id == turn_id
                && turn.user_input.as_deref() == Some("A")),
            "orphaned claimed turn should be interrupted"
        );
        assert!(
            turns
                .iter()
                .any(|turn| turn.user_input.as_deref() == Some("B\n\nC")
                    && turn.assistant_response.as_deref() == Some("after crash")),
            "pending messages should be presented together after restart; turns were {turns:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unlinked_chat_is_discarded_without_session_or_reply() {
        let (base_url, telegram) = start_mock_telegram().await;
        let client = connect_mock_client(base_url).await;
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let state = test_state(
            storage.clone(),
            MockProvider::new(vec![MockProvider::text_round("should not run")]),
            client.clone(),
        );

        telegram.push_update(7, 999, "stranger");
        telegram.push_non_text_update(8, 999);
        poll_once(&state, &client).await.unwrap();

        assert!(
            storage
                .find_session_by_channel_origin("telegram", client.bot_id().unwrap(), "999")
                .await
                .unwrap()
                .is_none()
        );
        assert!(telegram.sent.lock().unwrap().is_empty());
        assert_eq!(
            storage
                .get_channel_cursor("telegram", client.bot_id().unwrap())
                .await
                .unwrap()
                .as_deref(),
            Some("8")
        );
        let audits = storage.list_audit_events().await.unwrap();
        assert!(
            audits
                .iter()
                .any(|event| event.event_kind == "unsupported_update")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn telegram_token_is_not_persisted_after_send_or_poll_failure() {
        let (base_url, telegram) = start_mock_telegram().await;
        let client = connect_mock_client(base_url).await;
        let token = client.token.clone();
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let state = test_state(
            storage.clone(),
            MockProvider::new(vec![MockProvider::text_round("will fail delivery")]),
            client.clone(),
        );

        link_chat(&state, &client, &telegram, 1, 777).await;
        telegram.fail_send.store(true, Ordering::SeqCst);
        telegram.push_update(2, 777, "fail send");
        let _ = poll_once(&state, &client).await;

        let bad_client = Arc::new(TelegramClient {
            http: reqwest::Client::builder()
                .timeout(Duration::from_millis(50))
                .build()
                .unwrap(),
            base_url: "http://127.0.0.1:9".into(),
            token: token.clone(),
            bot_id: {
                let bot_id = OnceCell::new();
                bot_id
                    .set(client.bot_id().unwrap().to_string())
                    .expect("test bot id should set once");
                bot_id
            },
        });
        let shutdown = CancellationToken::new();
        let poll_state = state.clone();
        let poll_client = bad_client.clone();
        let handle = tokio::spawn(async move {
            run_poll_loop(poll_state, poll_client, shutdown.clone()).await;
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        handle.abort();

        let deliveries = storage.list_delivery_records().await.unwrap();
        let audits = storage.list_audit_events().await.unwrap();
        let session = storage
            .find_session_by_channel_origin("telegram", client.bot_id().unwrap(), "777")
            .await
            .unwrap()
            .unwrap();
        let thread = storage.list_by_session(session.id).await.unwrap()[0].clone();
        let turns = storage.list_turns_by_thread(thread.id).await.unwrap();
        let durable = serde_json::to_string(&json!({
            "deliveries": deliveries,
            "audits": audits,
            "turns": turns,
        }))
        .unwrap();
        assert!(
            !durable.contains(&token),
            "durable records must not contain telegram token: {durable}"
        );
    }
}
