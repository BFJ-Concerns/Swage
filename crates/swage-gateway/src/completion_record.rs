//! Every completion the gateway asks a provider for, as an event of the
//! local audit record.
//!
//! The gateway builds every completion provider in one place
//! (`startup::create_provider`), and wraps each model's adapter in
//! [`RecordingProvider`] beneath retry and failover, so every attempt sent
//! is recorded under the model that served it. Each call through it,
//! streamed or not, becomes one `completion` event once it ends: the model,
//! the output limit asked for, why generation stopped (a response cut off at
//! its length limit included), the tokens and cost reported, and what came
//! back — text, reasoning and the tool calls asked for — or the error that
//! ended it. Returned text is masked of every registered secret value before
//! it is written.
//!
//! A completion made inside a turn names the turn's session and the turn, read
//! from the attribution the agent sets around each turn it runs
//! (`swage_provider::attribution`); one made anywhere else is recorded with no
//! session. The record is reconstructive:
//! a failed write is logged and never fails the completion.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use async_trait::async_trait;
use futures::Stream;
use serde_json::{Value, json};
use uuid::Uuid;

use swage_core::{
    CompletionRequest, CompletionResponse, EffortCapability, FinishReason, ProviderError,
    StreamChunk, ToolCompletionRequest, ToolCompletionResponse,
};
use swage_provider::LlmProvider;
use swage_provider::attribution::{CompletionAttribution, current_attribution};
use swage_storage::model::NewAuditEvent;
use swage_storage::store::Storage;

use crate::secret::SecretManager;

/// Wraps a provider so that every completion made through it is recorded.
pub struct RecordingProvider {
    inner: Arc<dyn LlmProvider>,
    recorder: CompletionRecorder,
}

impl RecordingProvider {
    pub fn new(
        inner: Arc<dyn LlmProvider>,
        storage: Arc<dyn Storage>,
        secret_manager: SecretManager,
    ) -> Self {
        Self {
            inner,
            recorder: CompletionRecorder {
                storage,
                secret_manager,
            },
        }
    }
}

impl RecordingProvider {
    fn pending(&self, account: CompletionAccount) -> PendingCompletion {
        PendingCompletion {
            account: Some(account),
            recorder: self.recorder.clone(),
        }
    }
}

#[derive(Clone)]
struct CompletionRecorder {
    storage: Arc<dyn Storage>,
    secret_manager: SecretManager,
}

/// What one completion was asked for and what it returned, gathered as it
/// runs.
struct CompletionAccount {
    attribution: Option<CompletionAttribution>,
    model: String,
    streamed: bool,
    max_tokens: Option<u32>,
    started: Instant,
    content: String,
    reasoning: String,
    tool_calls: Vec<Value>,
    finish_reason: Option<FinishReason>,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    cost: Option<f64>,
    error: Option<String>,
    interrupted: bool,
    /// The provider's own account of the response: its raw stop reason,
    /// usage and the size of each kind of content block. It names a
    /// response that ran out of tokens while reasoning even where the
    /// adapter reports it as empty rather than as cut off.
    provider_witness: Option<swage_core::ProviderResponseWitness>,
}

impl CompletionAccount {
    fn new(
        provider: &dyn LlmProvider,
        model: Option<&str>,
        max_tokens: Option<u32>,
        streamed: bool,
    ) -> Self {
        Self {
            attribution: current_attribution(),
            model: model.unwrap_or_else(|| provider.model_id()).to_owned(),
            streamed,
            max_tokens,
            started: Instant::now(),
            content: String::new(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            input_tokens: None,
            output_tokens: None,
            cost: None,
            error: None,
            interrupted: false,
            provider_witness: None,
        }
    }

    fn outcome(&self) -> &'static str {
        if self.error.is_some() {
            return "failed";
        }
        if self.interrupted {
            return "interrupted";
        }
        match self.finish_reason {
            Some(FinishReason::Length) => "cut off at its length limit",
            Some(FinishReason::ToolUse) => "asked for tools",
            Some(FinishReason::Refusal) => "refused",
            Some(FinishReason::ContentFilter) => "filtered",
            Some(FinishReason::Stop) => "finished",
            Some(FinishReason::Unknown) | None => "ended",
        }
    }

    fn take_chunk(&mut self, chunk: &StreamChunk) {
        match chunk {
            StreamChunk::Delta { content } => self.content.push_str(content),
            StreamChunk::Reasoning { content, .. } => self.reasoning.push_str(content),
            StreamChunk::Usage(usage) => {
                *self.input_tokens.get_or_insert(0) += usage.input_tokens;
                *self.output_tokens.get_or_insert(0) += usage.output_tokens;
            }
            StreamChunk::Done { finish_reason } => {
                self.finish_reason = Some(finish_reason.clone().unwrap_or(FinishReason::Unknown));
            }
            StreamChunk::Error { message } => self.error = Some(message.clone()),
            StreamChunk::ProviderAnomaly { message } => {
                self.error = Some(format!("provider response anomaly: {message}"));
            }
            StreamChunk::ToolUse {
                call_id,
                name,
                arguments,
            } => self.tool_calls.push(json!({
                "call_id": call_id,
                "name": name,
                "arguments": arguments,
            })),
            StreamChunk::ProviderResponseWitness(witness) => {
                self.provider_witness = Some(witness.clone());
            }
            StreamChunk::ProviderAttempt(_)
            | StreamChunk::ProviderRetryStarted { .. }
            | StreamChunk::ProviderRetrySettled { .. } => {}
        }
    }

    fn take_response(&mut self, response: &CompletionResponse) {
        self.content = response.content.clone();
        self.finish_reason = Some(response.finish_reason.clone());
        self.input_tokens = Some(response.input_tokens);
        self.output_tokens = Some(response.output_tokens);
        self.cost = response.cost;
    }

    fn take_tool_response(&mut self, response: &ToolCompletionResponse) {
        self.content = response.content.clone();
        self.finish_reason = Some(response.finish_reason.clone());
        self.input_tokens = Some(response.input_tokens);
        self.output_tokens = Some(response.output_tokens);
        self.cost = response.cost;
        self.tool_calls = response
            .tool_calls
            .iter()
            .map(|call| {
                json!({
                    "call_id": call.call_id,
                    "name": call.name,
                    "arguments": call.arguments,
                })
            })
            .collect();
    }

    fn event(self, secret_values: Option<&[String]>) -> NewAuditEvent {
        let outcome = self.outcome();
        let id = Uuid::new_v4();
        let mut returned = json!({
            "content": self.content,
            "reasoning": self.reasoning,
            "tool_calls": self.tool_calls,
            "error": self.error,
        });
        let content_omitted = match secret_values {
            Some(values) => {
                mask_registered_values(&mut returned, values);
                false
            }
            None => {
                returned = json!({});
                true
            }
        };
        let mut metadata = json!({
            "model": self.model,
            "provider_witness": self.provider_witness,
            "turn_id": self.attribution.and_then(|attribution| attribution.turn_id),
            "streamed": self.streamed,
            "max_tokens": self.max_tokens,
            "finish_reason": self.finish_reason,
            "outcome": outcome,
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "cost_usd": self.cost,
            "duration_ms": u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        });
        if content_omitted {
            metadata["content_omitted"] = json!(
                "what the completion returned is not recorded: secret redaction was unavailable"
            );
        } else if let (Value::Object(metadata), Value::Object(returned)) = (&mut metadata, returned)
        {
            metadata.extend(returned);
        }
        NewAuditEvent {
            id: id.to_string(),
            session_id: self.attribution.map(|attribution| attribution.session_id),
            entity_kind: "completion".into(),
            entity_id: id.to_string(),
            event_kind: "completion".into(),
            summary: format!("completion {} {outcome}", self.model),
            metadata,
        }
    }
}

/// Mask every registered value in each string and each object key, in its
/// plain form and in the escaped form it takes inside JSON text, which a
/// completion may return.
fn mask_registered_values(value: &mut Value, secret_values: &[String]) {
    match value {
        Value::String(text) => {
            crate::turn::redact_serialised_text(text, secret_values);
        }
        Value::Array(values) => {
            for value in values {
                mask_registered_values(value, secret_values);
            }
        }
        Value::Object(values) => {
            *values = std::mem::take(values)
                .into_iter()
                .map(|(mut key, mut value)| {
                    crate::turn::redact_serialised_text(&mut key, secret_values);
                    mask_registered_values(&mut value, secret_values);
                    (key, value)
                })
                .collect();
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

impl CompletionRecorder {
    async fn record(&self, account: CompletionAccount) {
        let secret_values = crate::turn::registered_secret_values(&self.secret_manager)
            .await
            .inspect_err(|error| {
                tracing::warn!(%error, "completion recorded without its content: secret redaction unavailable");
            })
            .ok();
        let event = account.event(secret_values.as_deref());
        if let Err(error) = self.storage.create_audit_event(event).await {
            tracing::warn!(%error, "failed to record a completion in the audit record");
        }
    }

    /// Record on a task of its own, so that neither a caller that stops
    /// waiting nor a stream dropped mid-read loses the record.
    fn record_detached(&self, account: CompletionAccount) {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let recorder = self.clone();
                handle.spawn(async move { recorder.record(account).await });
            }
            Err(_) => tracing::warn!(
                model = %account.model,
                "completion dropped outside a runtime was not recorded"
            ),
        }
    }
}

/// A completion in flight. It is recorded once, when it is finished or,
/// failing that, when it is dropped: a caller that stops waiting, or a
/// reader that stops reading a stream before its end, leaves a completion
/// recorded as interrupted with whatever had arrived.
struct PendingCompletion {
    account: Option<CompletionAccount>,
    recorder: CompletionRecorder,
}

impl PendingCompletion {
    fn account(&mut self) -> Option<&mut CompletionAccount> {
        self.account.as_mut()
    }

    fn finish(&mut self) {
        if let Some(account) = self.account.take() {
            self.recorder.record_detached(account);
        }
    }
}

impl Drop for PendingCompletion {
    fn drop(&mut self) {
        if let Some(account) = self.account.as_mut() {
            // A stream its reader stops reading after `Done` has still ended;
            // one dropped before then was cut short.
            account.interrupted = account.finish_reason.is_none() && account.error.is_none();
        }
        self.finish();
    }
}

/// A provider stream that records its completion when it ends.
struct RecordingStream {
    inner: Pin<Box<dyn Stream<Item = StreamChunk> + Send>>,
    pending: PendingCompletion,
}

impl Stream for RecordingStream {
    type Item = StreamChunk;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<StreamChunk>> {
        let this = &mut *self;
        let polled = this.inner.as_mut().poll_next(cx);
        match &polled {
            Poll::Ready(Some(chunk)) => {
                if let Some(account) = this.pending.account() {
                    account.take_chunk(chunk);
                }
            }
            Poll::Ready(None) => this.pending.finish(),
            Poll::Pending => {}
        }
        polled
    }
}

#[async_trait]
impl LlmProvider for RecordingProvider {
    fn model_id(&self) -> &str {
        self.inner.model_id()
    }

    fn effort_capability(&self, model: &str) -> Option<EffortCapability> {
        self.inner.effort_capability(model)
    }

    fn output_limit(&self, model: &str) -> Option<u32> {
        self.inner.output_limit(model)
    }

    fn provider_request_snapshot(&self, request: &CompletionRequest, stream: bool) -> Value {
        self.inner.provider_request_snapshot(request, stream)
    }

    async fn count_tokens(
        &self,
        request: &CompletionRequest,
    ) -> Result<Option<u64>, ProviderError> {
        self.inner.count_tokens(request).await
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        let mut pending = self.pending(CompletionAccount::new(
            self.inner.as_ref(),
            request.model.as_deref(),
            request.max_tokens,
            false,
        ));
        let result = self.inner.complete(request).await;
        if let Some(account) = pending.account() {
            match &result {
                Ok(response) => account.take_response(response),
                Err(error) => account.error = Some(error.to_string()),
            }
        }
        pending.finish();
        result
    }

    async fn complete_with_tools(
        &self,
        request: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        let mut pending = self.pending(CompletionAccount::new(
            self.inner.as_ref(),
            request.model.as_deref(),
            request.max_tokens,
            false,
        ));
        let result = self.inner.complete_with_tools(request).await;
        if let Some(account) = pending.account() {
            match &result {
                Ok(response) => account.take_tool_response(response),
                Err(error) => account.error = Some(error.to_string()),
            }
        }
        pending.finish();
        result
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
        let pending = self.pending(CompletionAccount::new(
            self.inner.as_ref(),
            request.model.as_deref(),
            request.max_tokens,
            true,
        ));
        Box::pin(RecordingStream {
            inner: self.inner.stream(request),
            pending,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use swage_provider::attribution::COMPLETION_ATTRIBUTION;
    use swage_storage::{
        AuditEventFilter, AuditStore, NewTopic, SessionStore, SqliteStorage, TopicStore,
    };

    /// A provider whose every answer is a reply cut off at its length limit,
    /// the non-streamed one after `delay`.
    struct CutOffProvider {
        content: String,
        delay: std::time::Duration,
    }

    impl Default for CutOffProvider {
        fn default() -> Self {
            Self {
                content: "a summary that stops mid-".into(),
                delay: std::time::Duration::ZERO,
            }
        }
    }

    #[async_trait]
    impl LlmProvider for CutOffProvider {
        fn model_id(&self) -> &str {
            "fixture-model"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            tokio::time::sleep(self.delay).await;
            Ok(CompletionResponse {
                content: self.content.clone(),
                input_tokens: 40,
                output_tokens: 8,
                finish_reason: FinishReason::Length,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unreachable!("not asked for in these tests")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::iter([
                StreamChunk::Reasoning {
                    content: "thinking".into(),
                    details: None,
                },
                StreamChunk::Delta {
                    content: "half an ".into(),
                },
                StreamChunk::Delta {
                    content: "answer".into(),
                },
                StreamChunk::Usage(swage_core::TokenUsage {
                    input_tokens: 12,
                    output_tokens: 4,
                }),
                StreamChunk::ProviderResponseWitness(swage_core::ProviderResponseWitness {
                    stop_reason: Some("max_tokens".into()),
                    ..Default::default()
                }),
                StreamChunk::Done {
                    finish_reason: Some(FinishReason::Length),
                },
            ]))
        }
    }

    fn request(max_tokens: Option<u32>) -> CompletionRequest {
        CompletionRequest {
            messages: vec![],
            model: None,
            effort: None,
            max_tokens,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: vec![],
        }
    }

    fn recording(storage: &Arc<SqliteStorage>) -> RecordingProvider {
        recording_with(storage, CutOffProvider::default())
    }

    fn recording_with(storage: &Arc<SqliteStorage>, provider: CutOffProvider) -> RecordingProvider {
        let storage = Arc::clone(storage) as Arc<dyn Storage>;
        RecordingProvider::new(
            Arc::new(provider),
            Arc::clone(&storage),
            SecretManager::new(storage),
        )
    }

    /// Detached records land on a spawned task; wait until `count` exist.
    async fn completions(storage: &SqliteStorage, count: usize) -> Vec<swage_storage::AuditEvent> {
        for _ in 0..200 {
            let events = storage
                .list_audit_events_filtered(AuditEventFilter {
                    event_kind: Some("completion".into()),
                    ..Default::default()
                })
                .await
                .unwrap();
            if events.len() >= count {
                return events;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("{count} completion events were never recorded");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_completion_cut_off_at_its_length_limit_is_recorded_with_what_it_returned() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let response = recording(&storage)
            .complete(request(Some(8)))
            .await
            .unwrap();
        assert_eq!(response.finish_reason, FinishReason::Length);

        let events = completions(&storage, 1).await;
        let event = &events[0];
        assert_eq!(event.session_id, None, "made outside any turn");
        assert_eq!(
            event.summary,
            "completion fixture-model cut off at its length limit"
        );
        assert_eq!(event.metadata["finish_reason"], "length");
        assert_eq!(event.metadata["max_tokens"], 8);
        assert_eq!(event.metadata["content"], "a summary that stops mid-");
        assert_eq!(event.metadata["output_tokens"], 8);
        assert_eq!(event.metadata["streamed"], false);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_streamed_completion_in_a_turn_is_recorded_against_its_session_and_turn() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let topic = TopicStore::create(
            storage.as_ref(),
            NewTopic {
                name: "t".into(),
                slug: "t".into(),
            },
        )
        .await
        .unwrap();
        let session = storage
            .create_session(swage_storage::fixtures::new_session(topic.id))
            .await
            .unwrap();
        let turn_id = Uuid::new_v4();
        let provider = recording(&storage);
        let chunks: Vec<StreamChunk> = COMPLETION_ATTRIBUTION
            .scope(
                CompletionAttribution {
                    session_id: session.id,
                    turn_id: Some(turn_id),
                },
                async { provider.stream(request(None)).collect().await },
            )
            .await;
        assert_eq!(chunks.len(), 6, "the stream reaches its reader unchanged");

        let events = completions(&storage, 1).await;
        let event = &events[0];
        assert_eq!(event.session_id, Some(session.id));
        assert_eq!(event.metadata["turn_id"], turn_id.to_string());
        assert_eq!(event.metadata["content"], "half an answer");
        assert_eq!(event.metadata["reasoning"], "thinking");
        assert_eq!(event.metadata["outcome"], "cut off at its length limit");
        assert_eq!(event.metadata["input_tokens"], 12);
        assert_eq!(event.metadata["streamed"], true);
        assert_eq!(
            event.metadata["provider_witness"]["stop_reason"],
            "max_tokens"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stream_dropped_before_it_ends_is_recorded_as_interrupted() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let mut stream = recording(&storage).stream(request(None));
        stream.next().await;
        stream.next().await;
        drop(stream);

        let events = completions(&storage, 1).await;
        assert_eq!(events[0].metadata["outcome"], "interrupted");
        assert_eq!(events[0].metadata["content"], "half an ");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_registered_value_returned_inside_json_text_is_masked() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let secret = r#"audit"secret-123"#;
        unsafe { std::env::set_var("SWAGE_COMPLETION_RECORD_SECRET_TEST", secret) };
        SecretManager::new(Arc::clone(&storage) as Arc<dyn Storage>)
            .create_secret(crate::secret::CreateSecretRequest {
                name: "Completion record key".into(),
                material_kind: "provider_key".into(),
                owner_kind: "service".into(),
                backend_kind: "env".into(),
                scope_kind: "gateway".into(),
                scope_id: None,
                metadata: None,
                value: None,
                external_reference: Some("SWAGE_COMPLETION_RECORD_SECRET_TEST".into()),
            })
            .await
            .unwrap();
        let content = json!({ "token": secret }).to_string();
        assert!(!content.contains(secret), "the reply carries it escaped");
        recording_with(
            &storage,
            CutOffProvider {
                content,
                ..Default::default()
            },
        )
        .complete(request(None))
        .await
        .unwrap();

        let recorded = completions(&storage, 1).await[0].metadata["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            recorded,
            json!({ "token": "[REDACTED_SECRET]" }).to_string()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_completion_its_caller_stops_waiting_for_is_recorded_as_interrupted() {
        let storage = Arc::new(SqliteStorage::in_memory().unwrap());
        let provider = recording_with(
            &storage,
            CutOffProvider {
                delay: std::time::Duration::from_secs(30),
                ..Default::default()
            },
        );
        let waited = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            provider.complete(request(Some(8))),
        )
        .await;
        assert!(waited.is_err(), "the caller stopped waiting");

        let events = completions(&storage, 1).await;
        assert_eq!(events[0].metadata["outcome"], "interrupted");
        assert_eq!(events[0].metadata["max_tokens"], 8);
    }

    #[test]
    fn a_registered_value_is_masked_in_object_keys_as_well_as_values() {
        let secret = r#"audit"secret-123"#.to_owned();
        let mut returned = json!({
            "tool_calls": [{ "arguments": { "command": "printf probe", secret.clone(): secret.clone() } }],
        });
        mask_registered_values(&mut returned, std::slice::from_ref(&secret));
        assert_eq!(
            returned,
            json!({
                "tool_calls": [{ "arguments": {
                    "command": "printf probe",
                    "[REDACTED_SECRET]": "[REDACTED_SECRET]",
                } }],
            })
        );
    }
}
