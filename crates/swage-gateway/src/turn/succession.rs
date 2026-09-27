//! Succession at the turn's assembly step.
//!
//! When the assembled context is over the compaction threshold, the turn
//! runs on a successor: every carried element rendered afresh, the older
//! history summarised against them, the newer history kept as it was
//! (`swage_context::succession`). Assembly runs again over the successor's
//! elements — the same named or built-in assembly as the first pass — so the
//! report shows the successor exactly as the model received it, with the
//! pass's record under `compaction_applied`. When the pass cannot run, the
//! turn proceeds on the first assembly and the agent runtime's own compactor
//! takes over as it always did; the report carries the failed attempt beside
//! whatever that compactor then records.
//!
//! A session that keeps hitting walls succeeds too, whatever its size: the
//! agent's own recorded failures across the thread's recent turns
//! (`recent_walls`, `detect_walls`) trigger a pass whose summary is a
//! diagnosis written from outside the session — blind readers each locate
//! where things went wrong in a slice of the history, and a synthesis over
//! their findings names the problems. The successor keeps only the newest
//! message verbatim, so the failing attempts do not follow it in.

use std::collections::{BTreeMap, HashMap};

use swage_core::{CompletionRequest, Role};
use swage_protocol::client::workflow::WorkflowProviderRoundOutcome;
use swage_storage::{
    ContextSnapshotStore, ProviderRoundRecord, ProviderRoundStore, ToolCallStore, TurnState,
    TurnStore,
};
use uuid::Uuid;

use swage_agent::compaction::exceeds_threshold_with_model;
use swage_agent::config::CompactionThreshold;
use swage_context::assembly::default_assembly_carries;
use swage_context::element::ContextElement;
use swage_context::history::HistoryElement;
use swage_context::js_assembly::rendered_content;
use swage_context::report::AssemblyOutput;
use swage_context::succession::{
    self, DEGRADATION_STRATEGY, SuccessionError, SuccessionRequest, THRESHOLD_STRATEGY,
};
use swage_context::tokeniser::TiktokenTokeniser;
use swage_core::ChatMessage;
use swage_provider::LlmProvider;

/// The name `HistoryElement` reports itself under; the element the pass
/// replaces.
const HISTORY_ELEMENT: &str = "history";

/// What the pass needs from the turn beyond the elements themselves.
pub(super) struct SuccessionSite<'a> {
    pub provider: &'a dyn LlmProvider,
    pub threshold: &'a CompactionThreshold,
    pub model: Option<&'a str>,
    /// The agent's shaping instructions for the summary, from the turn
    /// request.
    pub focus: Option<&'a str>,
    /// The session values the assembly renders into opted-in elements; the
    /// summariser is shown elements as the successor will carry them.
    pub variables: &'a BTreeMap<String, String>,
    pub tokeniser: &'a TiktokenTokeniser,
    /// The wall the thread's recent turns ran into, when they ran into one:
    /// the pass runs on a diagnosis, whatever the context's size.
    pub walls: Option<WallSignal>,
}

/// Return `output` unchanged when the thread ran into no wall and the
/// context is within the threshold; otherwise the successor's assembly,
/// produced by `assemble` over the successor's elements, with the pass
/// recorded in its report. A wall takes precedence; when its diagnosis
/// cannot be written the failed attempt is recorded and the threshold
/// decides as it would have.
pub(super) async fn succeed_over_threshold(
    site: SuccessionSite<'_>,
    elements: Vec<Box<dyn ContextElement>>,
    output: AssemblyOutput,
    assemble: impl Fn(&[Box<dyn ContextElement>]) -> AssemblyOutput,
) -> AssemblyOutput {
    let mut output = output;
    if let Some(walls) = &site.walls
        && let Some(history) = history_of(&elements)
    {
        match succeed_on_degradation(&site, walls, &history).await {
            Ok(succession) => return reassemble(elements, succession, &assemble),
            Err(error) => {
                tracing::warn!(%error, "the degradation diagnosis did not run");
                output
                    .report
                    .compaction_applied
                    .push(succession::failure_record(
                        &history,
                        site.tokeniser,
                        site.focus,
                        DEGRADATION_STRATEGY,
                        &error,
                    ));
            }
        }
    }
    let evaluation = exceeds_threshold_with_model(
        &output.messages,
        site.threshold,
        site.provider,
        site.model,
        false,
    )
    .await;
    if !evaluation.exceeds {
        return output;
    }
    let Some(history) = history_of(&elements) else {
        return output;
    };
    // What the first assembly actually carried, by the report's own element
    // list — so an element a named assembly declared omitted is never shown
    // to the summariser as "already in place".
    let carried_names: std::collections::BTreeSet<&str> = output
        .report
        .elements
        .iter()
        .map(|report| report.name.as_str())
        .collect();
    let carried: Vec<ChatMessage> = elements
        .iter()
        .filter(|element| {
            element.name() != HISTORY_ELEMENT
                && default_assembly_carries(element.name())
                && carried_names.contains(element.name())
        })
        .flat_map(|element| rendered_content(element.as_ref(), site.variables))
        .collect();

    // Under a message-count threshold the successor is built to fit: what
    // the first assembly carried besides history, plus the summary, plus the
    // retained history must not exceed the count — otherwise the runtime's
    // compactor would run its summarise-everything pass over the successor.
    let retain_at_most = match site.threshold {
        CompactionThreshold::MessageCount(max) => {
            let carried_messages = output.messages.len().saturating_sub(history.len());
            Some((*max as usize).saturating_sub(carried_messages + 1))
        }
        CompactionThreshold::TokenCount(_) => None,
    };
    let request = SuccessionRequest {
        carried: &carried,
        history: &history,
        focus: site.focus,
        retain_at_most,
    };
    match succession::succeed(request, site.provider, site.tokeniser).await {
        Ok(succession) => reassemble(elements, succession, &assemble),
        Err(error) => {
            tracing::warn!(
                %error,
                "succession did not run; the agent runtime compacts this turn instead"
            );
            let mut output = output;
            output
                .report
                .compaction_applied
                .push(succession::failure_record(
                    &history,
                    site.tokeniser,
                    site.focus,
                    THRESHOLD_STRATEGY,
                    &error,
                ));
            output
        }
    }
}

/// The successor's assembly: every element but history, the succession
/// element, and the retained history, with the pass's record.
fn reassemble(
    elements: Vec<Box<dyn ContextElement>>,
    succession: succession::Succession,
    assemble: &impl Fn(&[Box<dyn ContextElement>]) -> AssemblyOutput,
) -> AssemblyOutput {
    let mut successor: Vec<Box<dyn ContextElement>> = elements
        .into_iter()
        .filter(|element| element.name() != HISTORY_ELEMENT)
        .collect();
    successor.push(Box::new(succession.element));
    successor.push(Box::new(HistoryElement::new(succession.retained_history)));
    let mut output = assemble(&successor);
    output.report.compaction_applied.push(succession.record);
    output
}

fn history_of(elements: &[Box<dyn ContextElement>]) -> Option<Vec<ChatMessage>> {
    elements
        .iter()
        .find(|element| element.name() == HISTORY_ELEMENT)
        .map(|element| element.content())
}

/// How many of the thread's most recent finished turns the wall signal
/// reads.
pub(super) const DEGRADATION_WINDOW_TURNS: usize = 6;

/// The same tool called with the same arguments and failing this many times
/// within the window is a wall on its own.
pub(super) const REPEATED_FAILING_CALL_THRESHOLD: usize = 3;

/// This many walls of either kind within the window — failed tool calls and
/// failed turns together — are a wall.
pub(super) const WALL_THRESHOLD: usize = 5;

/// The tool-call status counted as a failure: the tool ran and its
/// execution failed. A call the user rejected at approval (`rejected`) is
/// the user's decision, and a call cut off by the gateway stopping
/// (`interrupted`) is the host's; neither is the agent stuck, and neither
/// counts.
pub(super) const FAILED_TOOL_CALL_STATUS: &str = "error";

/// What one finished turn contributes to the wall signal, read from its
/// records alone — never from what the conversation said.
#[derive(Debug, Clone, Default)]
pub(super) struct TurnWalls {
    /// The turn ended failed for a reason other than its provider, as
    /// `ended_on_its_own` reads its last provider round.
    pub agent_failed: bool,
    /// The turn's tool calls whose recorded status is
    /// `FAILED_TOOL_CALL_STATUS`, as tool name and arguments.
    pub failed_calls: Vec<(String, serde_json::Value)>,
}

/// Which wall the recent turns ran into.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WallSignal {
    /// One tool, called with the same arguments, failed again and again.
    RepeatedFailingCall { tool: String, failures: usize },
    /// Failed tool calls and failed turns together reached the threshold.
    RepeatedFailures {
        failed_calls: usize,
        failed_turns: usize,
    },
}

impl WallSignal {
    /// The recorded facts, as the synthesis is told them.
    fn describe(&self) -> String {
        match self {
            Self::RepeatedFailingCall { tool, failures } => format!(
                "The `{tool}` tool was called {failures} times with identical arguments and failed every time."
            ),
            Self::RepeatedFailures {
                failed_calls,
                failed_turns,
            } => format!(
                "Across the recent turns {failed_calls} tool calls failed and {failed_turns} turns ended failed."
            ),
        }
    }
}

/// The wall the given turns — the thread's recent finished turns, oldest
/// first — ran into, if any. Only tool calls recorded with status `error`
/// (`FAILED_TOOL_CALL_STATUS`) count as failed calls; `rejected` (the user's
/// decision at approval) and `interrupted` (cut off by the gateway stopping)
/// never do. An identical failing call repeated
/// `REPEATED_FAILING_CALL_THRESHOLD` times is reported in preference to the
/// total reaching `WALL_THRESHOLD`.
pub(super) fn detect_walls(turns: &[TurnWalls]) -> Option<WallSignal> {
    let mut repeats: HashMap<(&str, String), usize> = HashMap::new();
    for (tool, arguments) in turns.iter().flat_map(|turn| &turn.failed_calls) {
        // serde_json's map is ordered, so equal arguments print equally.
        *repeats
            .entry((tool.as_str(), arguments.to_string()))
            .or_default() += 1;
    }
    if let Some(((tool, _), failures)) = repeats
        .into_iter()
        .filter(|(_, failures)| *failures >= REPEATED_FAILING_CALL_THRESHOLD)
        .max_by(|(a_key, a), (b_key, b)| a.cmp(b).then_with(|| b_key.cmp(a_key)))
    {
        return Some(WallSignal::RepeatedFailingCall {
            tool: tool.to_string(),
            failures,
        });
    }
    let failed_calls: usize = turns.iter().map(|turn| turn.failed_calls.len()).sum();
    let failed_turns = turns.iter().filter(|turn| turn.agent_failed).count();
    (failed_calls + failed_turns >= WALL_THRESHOLD).then_some(WallSignal::RepeatedFailures {
        failed_calls,
        failed_turns,
    })
}

/// The wall signal over `thread_id`'s recent finished turns, read from the
/// store: turn states, provider-round outcomes, tool-call statuses. Turns up
/// to and including the last one that already ran on a degradation
/// successor are not read again, so one streak of walls triggers one pass.
/// A read that fails leaves the signal absent; the threshold still applies.
pub(super) async fn recent_walls<S>(storage: &S, thread_id: Uuid) -> Option<WallSignal>
where
    S: TurnStore + ToolCallStore + ProviderRoundStore + ContextSnapshotStore + ?Sized,
{
    let turns = storage
        .list_turns_by_thread(thread_id)
        .await
        .inspect_err(|error| tracing::warn!(%error, "wall signal: turns unreadable"))
        .ok()?;
    let finished: Vec<_> = turns
        .into_iter()
        .filter(|turn| {
            matches!(
                turn.state,
                TurnState::Completed | TurnState::Failed | TurnState::Interrupted
            )
        })
        .collect();
    let window = &finished[finished.len().saturating_sub(DEGRADATION_WINDOW_TURNS)..];
    let mut walls = Vec::with_capacity(window.len());
    for turn in window.iter().rev() {
        if ran_on_degradation_successor(storage, turn.id).await {
            break;
        }
        walls.push(turn_walls(storage, turn.id, turn.state == TurnState::Failed).await?);
    }
    walls.reverse();
    detect_walls(&walls)
}

async fn turn_walls<S>(storage: &S, turn_id: Uuid, failed: bool) -> Option<TurnWalls>
where
    S: ToolCallStore + ProviderRoundStore + ?Sized,
{
    let agent_failed = failed && {
        let rounds = storage
            .list_provider_rounds_by_turn(turn_id)
            .await
            .inspect_err(|error| tracing::warn!(%error, "wall signal: rounds unreadable"))
            .ok()?;
        ended_on_its_own(&rounds)
    };
    let failed_calls = storage
        .list_tool_calls_by_turn(turn_id)
        .await
        .inspect_err(|error| tracing::warn!(%error, "wall signal: tool calls unreadable"))
        .ok()?
        .into_iter()
        .filter(|call| call.status.as_deref() == Some(FAILED_TOOL_CALL_STATUS))
        .map(|call| {
            let arguments = serde_json::from_str(&call.arguments)
                .unwrap_or(serde_json::Value::String(call.arguments));
            (call.tool, arguments)
        })
        .collect();
    Some(TurnWalls {
        agent_failed,
        failed_calls,
    })
}

/// Words in a failed round's recorded provider error that mark transport
/// trouble. They match `ProviderError`'s own renderings: "rate limited by
/// provider", "HTTP error 429", "request … timed out", "network error …",
/// and the stream's connection and retry failures.
const TRANSPORT_FAILURE_MARKERS: &[&str] = &[
    "rate limit",
    "429",
    "timed out",
    "timeout",
    "network error",
    "connection",
    "retry",
    "retries",
];

/// Whether a failed turn, by its provider rounds, failed on its own rather
/// than on its provider. Only the last round decides: it is the turn's
/// ending. An ending that is a transport failure (`TRANSPORT_FAILURE_MARKERS`)
/// or an interruption is the provider's or the host's; an ending that is
/// missing, still outstanding or unreadable cannot be told apart, so it does
/// not count either. Any other ending — a response, or a provider refusal
/// of what the agent sent — is the agent's.
pub(super) fn ended_on_its_own(rounds: &[ProviderRoundRecord]) -> bool {
    let Some(last) = rounds.iter().max_by_key(|round| round.round) else {
        return false;
    };
    match serde_json::from_value::<WorkflowProviderRoundOutcome>(last.outcome.clone()) {
        Ok(WorkflowProviderRoundOutcome::Responded { .. }) => true,
        Ok(WorkflowProviderRoundOutcome::Failed { message }) => {
            let message = message.to_lowercase();
            !TRANSPORT_FAILURE_MARKERS
                .iter()
                .any(|marker| message.contains(marker))
        }
        Ok(
            WorkflowProviderRoundOutcome::Interrupted | WorkflowProviderRoundOutcome::Outstanding,
        )
        | Err(_) => false,
    }
}

async fn ran_on_degradation_successor<S>(storage: &S, turn_id: Uuid) -> bool
where
    S: ContextSnapshotStore + ?Sized,
{
    let Ok(Some(report)) = storage.get_snapshot(turn_id).await else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&report)
        .ok()
        .and_then(|report| report["compaction_applied"].as_array().cloned())
        .is_some_and(|records| {
            records.iter().any(|record| {
                record["requested_strategy"] == DEGRADATION_STRATEGY
                    && record["actual_strategy"] == THRESHOLD_STRATEGY
            })
        })
}

/// How many blind readers share the history between them.
const LOCATOR_COUNT: usize = 3;

const LOCATOR_INSTRUCTIONS: &str = "You are reading one part of a conversation between a user and an \
assistant that has stalled: the assistant keeps failing at its work. Locate where things went wrong in \
this part — failing steps, repeated attempts at the same thing, assumptions that turned out false, \
instructions that were missed. Report each place briefly: what was being attempted, what happened, and \
why it seems to have failed. If nothing in this part went wrong, say so in one line.";

const SYNTHESIS_INSTRUCTIONS: &str = "You write the notes an assistant starts afresh from after its \
conversation stalled. You are given what the record shows went wrong and reports from readers who each \
examined part of the conversation. Name the problems plainly — what kept failing, the likely cause, and \
what to avoid repeating — then state what the conversation was trying to achieve and what is still open. \
Write as notes to the assistant, in plain prose without headings.";

/// The degradation-triggered pass: blind readers locate where the history
/// went wrong, a synthesis names the problems, and the synthesis becomes
/// the successor's summary; only the newest message is kept verbatim.
async fn succeed_on_degradation(
    site: &SuccessionSite<'_>,
    walls: &WallSignal,
    history: &[ChatMessage],
) -> Result<succession::Succession, SuccessionError> {
    let request = SuccessionRequest {
        carried: &[],
        history,
        focus: site.focus,
        retain_at_most: Some(1),
    };
    let summarised = succession::split_point(&request)?;
    let older = &history[..summarised];
    let slice_len = older.len().div_ceil(LOCATOR_COUNT).max(1);
    let findings = futures::future::join_all(older.chunks(slice_len).map(|slice| {
        site.provider.complete(reader_request(
            LOCATOR_INSTRUCTIONS,
            succession::transcript(slice),
        ))
    }))
    .await;
    let findings: Vec<String> = findings
        .into_iter()
        .filter_map(|finding| {
            finding
                .inspect_err(|error| tracing::warn!(%error, "a blind reader failed"))
                .ok()
        })
        .map(|response| response.content)
        .filter(|content| !content.trim().is_empty())
        .collect();
    if findings.is_empty() {
        return Err(SuccessionError::Diagnosis(
            "no reader located anything".to_string(),
        ));
    }
    let focus = site
        .focus
        .map(|focus| {
            format!("The assistant asked that the notes attend to the following:\n{focus}\n\n")
        })
        .unwrap_or_default();
    let reports = findings
        .iter()
        .enumerate()
        .map(|(index, finding)| format!("### Reader {}\n\n{}", index + 1, finding.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");
    let synthesis = site
        .provider
        .complete(reader_request(
            SYNTHESIS_INSTRUCTIONS,
            format!(
                "{focus}## What the record shows\n\n{}\n\n## Readers' reports\n\n{reports}",
                walls.describe()
            ),
        ))
        .await?;
    succession::successor(
        &request,
        synthesis.content,
        DEGRADATION_STRATEGY,
        site.tokeniser,
    )
}

fn reader_request(instructions: &str, material: String) -> CompletionRequest {
    CompletionRequest {
        effort: None,
        messages: vec![
            ChatMessage::new(Role::System, instructions),
            ChatMessage::new(Role::User, material),
        ],
        model: None,
        max_tokens: Some(1024),
        temperature: Some(0.3),
        stop_sequences: None,
        metadata: None,
        tool_definitions: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rounds(outcomes: &[serde_json::Value]) -> Vec<ProviderRoundRecord> {
        outcomes
            .iter()
            .enumerate()
            .map(|(index, outcome)| ProviderRoundRecord {
                id: Uuid::new_v4(),
                turn_id: Uuid::nil(),
                round: index as i64 + 1,
                started_at: chrono::Utc::now(),
                completed_at: None,
                outcome: outcome.clone(),
            })
            .collect()
    }

    #[test]
    fn the_last_round_decides_whether_a_failed_turn_failed_on_its_own() {
        let responded = json!({"outcome": "responded", "tool_calls": 1});
        let rate_limited =
            json!({"outcome": "failed", "message": "rate limited by provider anthropic"});
        let refused =
            json!({"outcome": "failed", "message": "HTTP error 400: unexpected tool_use_id"});
        let cases = [
            (
                vec![responded.clone(), refused.clone()],
                true,
                "a refusal of what the agent sent",
            ),
            (
                vec![rate_limited.clone(), responded.clone()],
                true,
                "an earlier transport failure recovered",
            ),
            (
                vec![responded.clone(), rate_limited],
                false,
                "ended on a rate limit",
            ),
            (
                vec![json!({"outcome": "failed", "message": "HTTP error 429: slow down"})],
                false,
                "429",
            ),
            (
                vec![
                    json!({"outcome": "failed", "message": "request to provider x timed out after 30s"}),
                ],
                false,
                "timeout",
            ),
            (
                vec![
                    json!({"outcome": "failed", "message": "network error communicating with provider x: reset"}),
                ],
                false,
                "network",
            ),
            (
                vec![responded.clone(), json!({"outcome": "interrupted"})],
                false,
                "interrupted",
            ),
            (
                vec![json!({"outcome": "outstanding"})],
                false,
                "ending never recorded",
            ),
            (
                vec![json!({"outcome": "garbled"})],
                false,
                "ending unreadable",
            ),
            (vec![], false, "no rounds"),
            (vec![responded], true, "responded, then the turn failed"),
        ];
        for (outcomes, expected, case) in cases {
            assert_eq!(ended_on_its_own(&rounds(&outcomes)), expected, "{case}");
        }
    }

    fn failing(tool: &str, arguments: serde_json::Value) -> (String, serde_json::Value) {
        (tool.to_string(), arguments)
    }

    fn turn(failed_calls: Vec<(String, serde_json::Value)>) -> TurnWalls {
        TurnWalls {
            agent_failed: false,
            failed_calls,
        }
    }

    #[test]
    fn the_same_call_failing_three_times_is_a_wall() {
        let call = || failing("file_read", json!({"path": "a.md"}));
        let turns = vec![turn(vec![call()]), turn(vec![call()]), turn(vec![call()])];
        assert_eq!(
            detect_walls(&turns),
            Some(WallSignal::RepeatedFailingCall {
                tool: "file_read".to_string(),
                failures: 3,
            })
        );
    }

    #[test]
    fn different_arguments_are_different_attempts() {
        let turns = vec![
            turn(vec![failing("file_read", json!({"path": "a.md"}))]),
            turn(vec![failing("file_read", json!({"path": "b.md"}))]),
            turn(vec![failing("file_read", json!({"path": "c.md"}))]),
        ];
        assert_eq!(detect_walls(&turns), None);
    }

    #[test]
    fn five_failures_of_either_kind_are_a_wall() {
        let mut turns = vec![
            turn(vec![failing("shell", json!({"cmd": "make"}))]),
            turn(vec![failing("shell", json!({"cmd": "make test"}))]),
            turn(vec![failing("file_read", json!({"path": "x"}))]),
        ];
        turns.push(TurnWalls {
            agent_failed: true,
            failed_calls: vec![],
        });
        assert_eq!(detect_walls(&turns), None, "four is under the threshold");
        turns.push(TurnWalls {
            agent_failed: true,
            failed_calls: vec![],
        });
        assert_eq!(
            detect_walls(&turns),
            Some(WallSignal::RepeatedFailures {
                failed_calls: 3,
                failed_turns: 2,
            })
        );
    }

    /// A provider that keeps every request and answers each with a fixed
    /// reply, so a test reads what the readers and the synthesis were shown.
    #[derive(Default)]
    struct RecordingProvider {
        requests: std::sync::Mutex<Vec<CompletionRequest>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for RecordingProvider {
        fn model_id(&self) -> &str {
            "recording"
        }

        async fn complete(
            &self,
            request: CompletionRequest,
        ) -> Result<swage_core::CompletionResponse, swage_core::ProviderError> {
            self.requests.lock().expect("requests").push(request);
            Ok(swage_core::CompletionResponse {
                content: "The read of notes/missing.md kept failing.".to_string(),
                input_tokens: 1,
                output_tokens: 1,
                finish_reason: swage_core::FinishReason::Stop,
                cost: None,
            })
        }

        async fn complete_with_tools(
            &self,
            _request: swage_core::ToolCompletionRequest,
        ) -> Result<swage_core::ToolCompletionResponse, swage_core::ProviderError> {
            unreachable!("the diagnosis never calls tools")
        }

        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> std::pin::Pin<Box<dyn futures::Stream<Item = swage_core::StreamChunk> + Send>>
        {
            Box::pin(futures::stream::empty())
        }
    }

    #[tokio::test]
    async fn blind_readers_see_only_history_and_the_synthesis_sees_the_record() {
        let provider = RecordingProvider::default();
        let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");
        let variables = BTreeMap::new();
        let threshold = CompactionThreshold::MessageCount(100);
        let site = SuccessionSite {
            provider: &provider,
            threshold: &threshold,
            model: None,
            focus: None,
            variables: &variables,
            tokeniser: &tokeniser,
            walls: None,
        };
        let history: Vec<ChatMessage> = (0..7)
            .map(|index| ChatMessage::new(Role::User, format!("history message {index}")))
            .collect();
        let walls = WallSignal::RepeatedFailingCall {
            tool: "file_read".to_string(),
            failures: 3,
        };

        let succession = succeed_on_degradation(&site, &walls, &history)
            .await
            .expect("the diagnosis runs");

        let requests = provider.requests.lock().expect("requests");
        assert_eq!(
            requests.len(),
            LOCATOR_COUNT + 1,
            "three readers and one synthesis"
        );
        let shown = |request: &CompletionRequest| request.messages[1].text_content();
        let readers: Vec<String> = requests[..LOCATOR_COUNT].iter().map(shown).collect();
        for index in 0..6 {
            let message = format!("history message {index}");
            assert_eq!(
                readers
                    .iter()
                    .filter(|text| text.contains(&message))
                    .count(),
                1,
                "each summarised message goes to exactly one reader: {readers:?}"
            );
        }
        assert!(
            readers
                .iter()
                .all(|text| !text.contains("history message 6")),
            "the newest message is kept, not diagnosed"
        );
        let synthesis = shown(&requests[LOCATOR_COUNT]);
        assert!(synthesis.contains("`file_read` tool was called 3 times"));
        assert!(
            !synthesis.contains("history message"),
            "the synthesis reads the readers, not the history"
        );
        assert_eq!(
            succession.record.requested_strategy,
            "succession-degradation"
        );
        assert_eq!(succession.retained_history.len(), 1);
    }
}
