#![allow(clippy::unwrap_used)]

use std::{
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
};

use swage_agent::{
    CompactionThreshold,
    compaction::{compact, exceeds_threshold},
};
use swage_core::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart, FinishReason, ProviderError,
    Role, StreamChunk, ToolCompletionRequest, ToolCompletionResponse, ToolReplayPart,
};
use swage_provider::LlmProvider;
use swage_storage::SqliteStorage;
use uuid::Uuid;

struct CountingProvider {
    calls: AtomicUsize,
    counts: Vec<Result<Option<u64>, ProviderError>>,
}

#[async_trait::async_trait]
impl LlmProvider for CountingProvider {
    fn model_id(&self) -> &str {
        "native-counter"
    }
    async fn count_tokens(&self, _: &CompletionRequest) -> Result<Option<u64>, ProviderError> {
        self.counts[self.calls.fetch_add(1, Ordering::SeqCst)].clone()
    }
    async fn complete(&self, _: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        Ok(CompletionResponse {
            content: "summary".into(),
            input_tokens: 1,
            output_tokens: 1,
            finish_reason: FinishReason::Stop,
            cost: None,
        })
    }
    async fn complete_with_tools(
        &self,
        _: ToolCompletionRequest,
    ) -> Result<ToolCompletionResponse, ProviderError> {
        unreachable!()
    }
    fn stream(
        &self,
        _: CompletionRequest,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamChunk> + Send>> {
        unreachable!()
    }
}

#[tokio::test]
async fn non_text_and_tool_payloads_use_native_counting_while_exact_limit_and_errors_do_not_compact()
 {
    let storage = SqliteStorage::in_memory().unwrap();
    let non_text = ChatMessage::new_parts(
        Role::User,
        vec![ContentPart::ImageUrl {
            url: "https://example.test/image".into(),
        }],
    );
    let tool_replay = ChatMessage::with_tool_replay(
        Role::Assistant,
        vec![ToolReplayPart::ToolCall {
            call_id: "call".into(),
            name: "lookup".into(),
            arguments: serde_json::json!({}),
        }],
    );
    let threshold = CompactionThreshold::TokenCount(10);
    let provider = CountingProvider {
        calls: AtomicUsize::new(0),
        counts: vec![
            Ok(Some(11)),
            Ok(Some(10)),
            Err(ProviderError::Network {
                provider: "native".into(),
                message: "unavailable".into(),
            }),
        ],
    };
    assert!(
        exceeds_threshold(&[non_text.clone()], &threshold, &provider).await,
        "non-text payload bypasses the byte screen and uses native count"
    );
    assert!(
        !exceeds_threshold(&[tool_replay.clone()], &threshold, &provider).await,
        "native count at the exact limit must not compact"
    );
    assert!(
        !exceeds_threshold(
            &[ChatMessage::new(Role::User, "tiny")],
            &threshold,
            &provider
        )
        .await,
        "a provider count failure keeps the non-exceeding estimate"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    let provider = CountingProvider {
        calls: AtomicUsize::new(0),
        counts: vec![Ok(Some(100)), Ok(Some(1))],
    };
    let (_messages, record) = compact(
        vec![non_text, tool_replay],
        &threshold,
        &provider,
        &storage,
        Uuid::new_v4(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        record
            .expect("compact consumes the native-count breach")
            .actual_strategy,
        "summarisation"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "compact re-counts after its provider summary"
    );
}
