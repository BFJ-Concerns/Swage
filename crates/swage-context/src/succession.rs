//! Succession — the follow-up context when a session's history outgrows its
//! room.
//!
//! A successor is assembled from the same elements as any other turn:
//! orientation, norms, prompts, briefings and skills are rendered afresh from
//! their sources, so anything the agent changed through session variables or
//! named elements reaches its successor by the ordinary route. Only the
//! conversation history is replaced — its older part by a summary written
//! *against* the carried elements (the summariser is shown them and told
//! they will be in place again, so it records what the conversation
//! established rather than restating what the context already says), its
//! newer part kept as it was. The agent shapes the summary through the
//! turn's compaction focus.
//!
//! The pass runs at the turn's assembly step, at the same threshold the
//! agent runtime compacts at; once it has run, the runtime finds the
//! threshold met and its own compactor stays idle. When the pass cannot run
//! (the summarising provider fails) the turn proceeds on the unsucceeded
//! context, the runtime's compactor handles it as before, and the report
//! records the attempt.

use swage_core::{ChatMessage, CompletionRequest, ProviderError, Role};
use swage_provider::LlmProvider;

use crate::element::ContextElement;
use crate::report::CompactionRecord;
use crate::tokeniser::Tokeniser;

/// The element name the successor's summary is carried under.
pub const SUCCESSION_ELEMENT: &str = "succession";

/// The strategy a threshold-triggered pass records.
pub const THRESHOLD_STRATEGY: &str = "succession";

/// The strategy a degradation-triggered pass records: the summary is a
/// diagnosis of where the session went wrong, written by readers outside it.
pub const DEGRADATION_STRATEGY: &str = "succession-degradation";

/// Directly above history (0) and below everything rendered from sources:
/// the summary reads as the conversation's own opening.
pub const SUCCESSION_PRIORITY: i32 = 1;

/// What the succession pass is given.
pub struct SuccessionRequest<'a> {
    /// Every carried element's rendered content — the context that will be
    /// in place again for the successor, shown to the summariser so the
    /// summary skims past it.
    pub carried: &'a [ChatMessage],
    /// The thread's history, oldest first.
    pub history: &'a [ChatMessage],
    /// The agent's shaping instructions for the summary, when it gave any.
    pub focus: Option<&'a str>,
    /// The most history messages the successor may keep verbatim, when the
    /// caller's threshold counts messages: the successor is built to fit,
    /// so the runtime's own compactor never runs over it. `None` keeps the
    /// midpoint rule.
    pub retain_at_most: Option<usize>,
}

/// What the succession pass produced: the summary element, the history the
/// successor keeps as it was, and the record the assembly report carries.
#[derive(Debug)]
pub struct Succession {
    pub element: SuccessionElement,
    pub retained_history: Vec<ChatMessage>,
    pub record: CompactionRecord,
}

#[derive(Debug, thiserror::Error)]
pub enum SuccessionError {
    #[error("history holds {0} message(s); succession needs at least two")]
    TooShort(usize),
    #[error("the summarising provider failed: {0}")]
    Provider(#[from] ProviderError),
    #[error("the summarising provider returned an empty summary")]
    EmptySummary,
    #[error("the diagnosis could not be written: {0}")]
    Diagnosis(String),
}

/// The summary of the conversation's earlier part, as an element.
#[derive(Debug, Clone)]
pub struct SuccessionElement {
    summary: String,
    summarised: usize,
    retained: usize,
}

impl SuccessionElement {
    pub fn new(summary: impl Into<String>, summarised: usize, retained: usize) -> Self {
        Self {
            summary: summary.into(),
            summarised,
            retained,
        }
    }

    fn render(&self) -> String {
        format!(
            "## Conversation so far\n\n{}\n\nThe {} earlier messages are represented by this summary; the {} most recent follow as they were.",
            self.summary.trim(),
            self.summarised,
            self.retained
        )
    }
}

impl ContextElement for SuccessionElement {
    fn name(&self) -> &str {
        SUCCESSION_ELEMENT
    }

    fn priority(&self) -> i32 {
        SUCCESSION_PRIORITY
    }

    fn content(&self) -> Vec<ChatMessage> {
        vec![ChatMessage::new(Role::System, self.render())]
    }

    fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
        tokeniser.count_messages(&self.content())
    }

    fn source(&self) -> String {
        format!(
            "succession summary of {} earlier messages ({} retained)",
            self.summarised, self.retained
        )
    }
}

/// How many of `history_len` messages the summary replaces: the older half,
/// the newer half staying verbatim — the midpoint the agent runtime's own
/// compactor also uses — lowered to keep at most `retain_at_most` when the
/// newer half alone would not fit the caller's threshold, and never below
/// one retained message (the newest is always kept as it was).
pub fn summarised_count(history_len: usize, retain_at_most: Option<usize>) -> usize {
    let midpoint_retained = history_len - history_len / 2;
    let retained = match retain_at_most {
        Some(at_most) => midpoint_retained.min(at_most.max(1)),
        None => midpoint_retained,
    };
    history_len - retained.min(history_len)
}

/// Run the pass: summarise the older half of `history` against `carried`
/// and return the successor's parts.
pub async fn succeed(
    request: SuccessionRequest<'_>,
    provider: &dyn LlmProvider,
    tokeniser: &dyn Tokeniser,
) -> Result<Succession, SuccessionError> {
    let summarised = split_point(&request)?;
    let response = provider
        .complete(summary_request(
            request.carried,
            &request.history[..summarised],
            request.focus,
        ))
        .await?;
    successor(&request, response.content, THRESHOLD_STRATEGY, tokeniser)
}

/// Where `request`'s history splits into the summarised part and the part
/// kept as it was; history too short to split refuses the pass.
pub fn split_point(request: &SuccessionRequest<'_>) -> Result<usize, SuccessionError> {
    let history_len = request.history.len();
    if history_len < 2 {
        return Err(SuccessionError::TooShort(history_len));
    }
    Ok(summarised_count(history_len, request.retain_at_most))
}

/// The successor's parts from a summary of `request`'s older history,
/// whoever wrote it, recorded under `strategy`.
pub fn successor(
    request: &SuccessionRequest<'_>,
    summary: String,
    strategy: &str,
    tokeniser: &dyn Tokeniser,
) -> Result<Succession, SuccessionError> {
    let summarised = split_point(request)?;
    if summary.trim().is_empty() {
        return Err(SuccessionError::EmptySummary);
    }
    let newer = &request.history[summarised..];
    let tokens_before = tokeniser.count_messages(request.history);
    let element = SuccessionElement::new(summary, summarised, newer.len());
    let retained_history = newer.to_vec();
    let tokens_after = element.token_count(tokeniser) + tokeniser.count_messages(&retained_history);
    let record = CompactionRecord {
        element: "history".to_string(),
        requested_strategy: strategy.to_string(),
        actual_strategy: THRESHOLD_STRATEGY.to_string(),
        messages_removed: summarised,
        tokens_before,
        tokens_after,
        source_message_range: Some((0, summarised)),
        budget: None,
        failure_reason: None,
        focus: request.focus.map(str::to_owned),
    };
    Ok(Succession {
        element,
        retained_history,
        record,
    })
}

/// The record for a pass that could not run, so the report shows the
/// attempt beside whatever the agent runtime's compactor then did.
pub fn failure_record(
    history: &[ChatMessage],
    tokeniser: &dyn Tokeniser,
    focus: Option<&str>,
    strategy: &str,
    error: &SuccessionError,
) -> CompactionRecord {
    let tokens = tokeniser.count_messages(history);
    CompactionRecord {
        element: "history".to_string(),
        requested_strategy: strategy.to_string(),
        actual_strategy: "none".to_string(),
        messages_removed: 0,
        tokens_before: tokens,
        tokens_after: tokens,
        source_message_range: None,
        budget: None,
        failure_reason: Some(error.to_string()),
        focus: focus.map(str::to_owned),
    }
}

const SUMMARY_INSTRUCTIONS: &str = "You write the hand-over notes an assistant continues a conversation from. \
The assistant's standing context is shown first and will be in place again, word for word, when it continues — \
never restate, paraphrase or summarise it. Summarise the conversation alone: what was asked, what was decided, \
what was done and how it turned out, what is still open, and any fact the assistant learned that the standing \
context does not hold. Write in the past tense, as notes to the assistant, in plain prose without headings.";

fn summary_request(
    carried: &[ChatMessage],
    older: &[ChatMessage],
    focus: Option<&str>,
) -> CompletionRequest {
    let focus_block = focus
        .map(|focus| {
            format!("The assistant asked that the notes attend to the following:\n{focus}\n\n")
        })
        .unwrap_or_default();
    let carried_text = transcript(carried);
    let older_text = transcript(older);
    CompletionRequest {
        effort: None,
        messages: vec![
            ChatMessage::new(Role::System, SUMMARY_INSTRUCTIONS),
            ChatMessage::new(
                Role::User,
                format!(
                    "{focus_block}## Standing context (already in place; do not repeat)\n\n{carried_text}\n\n## Conversation to summarise\n\n{older_text}"
                ),
            ),
        ],
        model: None,
        max_tokens: Some(1024),
        temperature: Some(0.3),
        stop_sequences: None,
        metadata: None,
        tool_definitions: vec![],
    }
}

/// Messages as labelled prose for a reader outside the conversation;
/// non-text parts are named rather than dropped silently.
pub fn transcript(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .map(|message| {
            let label = match message.role() {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            let text = message.text_content();
            if text.trim().is_empty() {
                format!("[{label}] (non-text content)")
            } else {
                format!("[{label}] {text}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assembly::default_assembly_carries;
    use async_trait::async_trait;
    use futures::Stream;
    use std::pin::Pin;
    use std::sync::Mutex;
    use swage_core::{
        CompletionResponse, FinishReason, StreamChunk, ToolCompletionRequest,
        ToolCompletionResponse,
    };

    /// A provider that answers with a fixed summary and keeps the request it
    /// was asked, so a test can read what the summariser was shown.
    struct RecordingProvider {
        summary: &'static str,
        requests: Mutex<Vec<CompletionRequest>>,
        fail: bool,
    }

    impl RecordingProvider {
        fn answering(summary: &'static str) -> Self {
            Self {
                summary,
                requests: Mutex::new(Vec::new()),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                summary: "",
                requests: Mutex::new(Vec::new()),
                fail: true,
            }
        }

        fn only_request(&self) -> CompletionRequest {
            let requests = self.requests.lock().expect("requests");
            assert_eq!(requests.len(), 1, "one summary request per pass");
            requests[0].clone()
        }
    }

    #[async_trait]
    impl LlmProvider for RecordingProvider {
        fn model_id(&self) -> &str {
            "recording"
        }

        async fn complete(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse, ProviderError> {
            self.requests.lock().expect("requests").push(request);
            if self.fail {
                return Err(ProviderError::Network {
                    provider: "recording".to_string(),
                    message: "summariser down".to_string(),
                });
            }
            Ok(CompletionResponse {
                content: self.summary.to_string(),
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: FinishReason::Stop,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _request: ToolCompletionRequest,
        ) -> Result<ToolCompletionResponse, ProviderError> {
            unreachable!("succession never calls tools")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            Box::pin(futures::stream::empty())
        }
    }

    struct CharTokeniser;

    impl Tokeniser for CharTokeniser {
        fn count_text(&self, text: &str) -> usize {
            text.len()
        }

        fn count_messages(&self, messages: &[ChatMessage]) -> usize {
            messages.iter().map(|m| m.text_content().len()).sum()
        }
    }

    fn history(len: usize) -> Vec<ChatMessage> {
        (0..len)
            .map(|index| {
                let role = if index % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                };
                ChatMessage::new(role, format!("message {index}"))
            })
            .collect()
    }

    #[tokio::test]
    async fn the_summariser_sees_the_carried_context_the_focus_and_only_the_older_half() {
        let provider = RecordingProvider::answering("They agreed the plan.");
        let carried = vec![ChatMessage::new(
            Role::System,
            "You are Swage. Topic: alpha.",
        )];
        let history = history(6);

        let succession = succeed(
            SuccessionRequest {
                carried: &carried,
                history: &history,
                focus: Some("keep the open questions"),
                retain_at_most: None,
            },
            &provider,
            &CharTokeniser,
        )
        .await
        .expect("succession runs");

        let request = provider.only_request();
        assert_eq!(request.messages[0].role(), Role::System);
        let shown = request.messages[1].text_content();
        assert!(shown.contains("already in place; do not repeat"), "{shown}");
        assert!(shown.contains("You are Swage. Topic: alpha."), "{shown}");
        assert!(shown.contains("keep the open questions"), "{shown}");
        assert!(shown.contains("[user] message 0") && shown.contains("[assistant] message 1"));
        assert!(
            !shown.contains("message 3"),
            "the newer half is retained, not summarised: {shown}"
        );

        assert_eq!(
            succession
                .retained_history
                .iter()
                .map(|m| m.text_content())
                .collect::<Vec<_>>(),
            ["message 3", "message 4", "message 5"]
        );
        let rendered = succession.element.content()[0].text_content();
        assert!(rendered.starts_with("## Conversation so far\n\nThey agreed the plan."));
        assert!(rendered.contains("3 earlier messages") && rendered.contains("3 most recent"));
        assert_eq!(succession.record.actual_strategy, "succession");
        assert_eq!(succession.record.messages_removed, 3);
        assert_eq!(succession.record.source_message_range, Some((0, 3)));
        assert_eq!(
            succession.record.focus.as_deref(),
            Some("keep the open questions")
        );
    }

    #[test]
    fn a_diagnosis_becomes_the_successor_s_summary_under_its_trigger_s_strategy() {
        let history = history(6);
        let succession = successor(
            &SuccessionRequest {
                carried: &[],
                history: &history,
                focus: None,
                retain_at_most: Some(1),
            },
            "The build kept failing on a missing file.".to_string(),
            DEGRADATION_STRATEGY,
            &CharTokeniser,
        )
        .expect("a diagnosis makes a successor");
        assert_eq!(
            succession.record.requested_strategy,
            "succession-degradation"
        );
        assert_eq!(succession.record.actual_strategy, "succession");
        assert_eq!(succession.record.messages_removed, 5);
        assert_eq!(
            succession
                .retained_history
                .iter()
                .map(|m| m.text_content())
                .collect::<Vec<_>>(),
            ["message 5"]
        );
        assert!(
            succession.element.content()[0]
                .text_content()
                .contains("The build kept failing on a missing file.")
        );
    }

    #[tokio::test]
    async fn a_failed_summariser_is_an_error_the_caller_records() {
        let provider = RecordingProvider::failing();
        let history = history(4);
        let error = succeed(
            SuccessionRequest {
                carried: &[],
                history: &history,
                focus: None,
                retain_at_most: None,
            },
            &provider,
            &CharTokeniser,
        )
        .await
        .expect_err("the provider failed");
        assert!(matches!(error, SuccessionError::Provider(_)));

        let record = failure_record(&history, &CharTokeniser, None, THRESHOLD_STRATEGY, &error);
        assert_eq!(record.actual_strategy, "none");
        assert_eq!(record.messages_removed, 0);
        assert_eq!(record.tokens_before, record.tokens_after);
        assert!(
            record
                .failure_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("summariser down"))
        );
    }

    #[tokio::test]
    async fn an_empty_summary_is_a_failure_not_a_successor() {
        let provider = RecordingProvider::answering("   \n");
        let history = history(4);
        let error = succeed(
            SuccessionRequest {
                carried: &[],
                history: &history,
                focus: None,
                retain_at_most: None,
            },
            &provider,
            &CharTokeniser,
        )
        .await
        .expect_err("a blank summary would discard the older half for nothing");
        assert!(matches!(error, SuccessionError::EmptySummary));
    }

    #[tokio::test]
    async fn one_message_is_too_short_to_succeed() {
        let provider = RecordingProvider::answering("unused");
        let history = history(1);
        let error = succeed(
            SuccessionRequest {
                carried: &[],
                history: &history,
                focus: None,
                retain_at_most: None,
            },
            &provider,
            &CharTokeniser,
        )
        .await
        .expect_err("nothing to summarise");
        assert!(matches!(error, SuccessionError::TooShort(1)));
        assert!(provider.requests.lock().expect("requests").is_empty());
    }

    #[test]
    fn the_split_is_the_midpoint_unless_the_threshold_cannot_hold_the_newer_half() {
        assert_eq!(summarised_count(102, None), 51);
        assert_eq!(summarised_count(2, None), 1);
        assert_eq!(summarised_count(300, Some(95)), 205, "keeps 95, not 150");
        assert_eq!(
            summarised_count(102, Some(95)),
            51,
            "the midpoint already fits"
        );
        assert_eq!(
            summarised_count(10, Some(0)),
            9,
            "the newest message is always kept"
        );
    }

    #[test]
    fn the_succession_element_is_baseline_content_a_named_assembly_cannot_drop() {
        assert!(default_assembly_carries(SUCCESSION_ELEMENT));
        let element = SuccessionElement::new("notes", 4, 2);
        assert_eq!(element.priority(), SUCCESSION_PRIORITY);
        assert!(element.priority() > 0, "sits directly above history");
        assert_eq!(
            element.source(),
            "succession summary of 4 earlier messages (2 retained)"
        );
    }
}
