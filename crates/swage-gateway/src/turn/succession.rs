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
//! (`recent_degradation`, `detect_walls`) trigger a pass whose summary is a
//! diagnosis written from outside the session — blind readers each locate
//! where things went wrong in a slice of the history, and a synthesis over
//! their findings names the problems. The successor keeps only the newest
//! message verbatim, so the failing attempts do not follow it in. Every turn
//! runs on such a successor while the walls are still in the window; a turn
//! whose window holds the same walls its predecessor's diagnosis answered
//! carries that diagnosis again rather than writing a new one, and a turn
//! whose window no longer shows a wall gets its full history back.

use std::collections::{BTreeMap, HashMap};

use swage_core::{CompletionRequest, Role};
use swage_protocol::client::context::{
    CompactionRecord, DiagnosisCall, DiagnosisRecord, DiagnosisStage,
};
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
    pub degradation: Option<Degradation>,
    /// The models the diagnosis's readers and synthesis are sent to, or why
    /// they could not be resolved.
    pub diagnosis_models: Result<DiagnosisModels, String>,
}

/// The provider-facing model ids a diagnosis is sent to; `None` sends a
/// call to the provider's own default.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DiagnosisModels {
    pub reader: Option<String>,
    pub synthesis: Option<String>,
}

/// The diagnosis models `config` names (`context.diagnosis_reader_model`,
/// `context.diagnosis_synthesis_model`), each a catalogue entry or an alias
/// of one, and each the session's model when unset.
pub(crate) fn diagnosis_models(
    config: &swage_config::ResolvedConfig,
) -> Result<DiagnosisModels, String> {
    let resolve = |field: &str, named: &Option<String>| match named {
        Some(name) => crate::turn::resolve_model_catalogue_entry(&config.models, name)
            .map(|(_, model)| Some(model.model_id.clone()))
            .ok_or_else(|| {
                format!("context.{field} names '{name}', which is not in the model catalogue")
            }),
        None => Ok(Some(config.default_model_id().to_string()).filter(|model| !model.is_empty())),
    };
    Ok(DiagnosisModels {
        reader: resolve(
            "diagnosis_reader_model",
            &config.context.diagnosis_reader_model,
        )?,
        synthesis: resolve(
            "diagnosis_synthesis_model",
            &config.context.diagnosis_synthesis_model,
        )?,
    })
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
    if let Some(degradation) = &site.degradation
        && let Some(history) = history_of(&elements)
    {
        let (outcome, diagnosis) = succeed_on_degradation(&site, degradation, &history).await;
        match outcome {
            Ok(mut succession) => {
                succession.record.diagnosis = Some(diagnosis);
                return reassemble(
                    elements,
                    succession,
                    &assemble,
                    std::mem::take(&mut output.report.compaction_applied),
                );
            }
            Err(error) => {
                tracing::warn!(%error, "the degradation diagnosis did not run");
                let mut record = succession::failure_record(
                    &history,
                    site.tokeniser,
                    site.focus,
                    DEGRADATION_STRATEGY,
                    &error,
                );
                record.diagnosis = Some(diagnosis);
                output.report.compaction_applied.push(record);
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
        Ok(succession) => reassemble(
            elements,
            succession,
            &assemble,
            output.report.compaction_applied,
        ),
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
    prior_records: Vec<CompactionRecord>,
) -> AssemblyOutput {
    let mut successor: Vec<Box<dyn ContextElement>> = elements
        .into_iter()
        .filter(|element| element.name() != HISTORY_ELEMENT)
        .collect();
    successor.push(Box::new(succession.element));
    successor.push(Box::new(HistoryElement::new(succession.retained_history)));
    let mut output = assemble(&successor);
    let successor_records = std::mem::take(&mut output.report.compaction_applied);
    let mut records = prior_records;
    let new_records: Vec<_> = successor_records
        .into_iter()
        .filter(|current| {
            !records.iter().any(|prior| {
                prior.element == current.element
                    && prior.requested_strategy == current.requested_strategy
                    && prior.actual_strategy == current.actual_strategy
            })
        })
        .collect();
    records.extend(new_records);
    output.report.compaction_applied = records;
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
    pub turn_id: Uuid,
    /// The turn ended failed for a reason other than its provider, as
    /// `ended_on_its_own` reads its last provider round.
    pub agent_failed: bool,
    /// The turn's tool calls whose recorded status is
    /// `FAILED_TOOL_CALL_STATUS`.
    pub failed_calls: Vec<FailedCall>,
}

/// A tool call recorded as failed, by its record id.
#[derive(Debug, Clone)]
pub(super) struct FailedCall {
    pub id: Uuid,
    pub tool: String,
    pub arguments: serde_json::Value,
}

/// Every wall in `turns`, by the record it was read from: `tool_call:<id>`
/// for a failed call, `turn:<id>` for a turn that failed on its own. Sorted,
/// so equal sets compare equal.
pub(super) fn wall_set(turns: &[TurnWalls]) -> Vec<String> {
    let mut walls: Vec<String> = turns
        .iter()
        .flat_map(|turn| {
            let calls = turn
                .failed_calls
                .iter()
                .map(|call| format!("tool_call:{}", call.id));
            calls.chain(turn.agent_failed.then(|| format!("turn:{}", turn.turn_id)))
        })
        .collect();
    walls.sort();
    walls
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
    for call in turns.iter().flat_map(|turn| &turn.failed_calls) {
        // serde_json's map is ordered, so equal arguments print equally.
        *repeats
            .entry((call.tool.as_str(), call.arguments.to_string()))
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

/// A wall the thread's recent turns ran into, with what the pass needs to
/// answer it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Degradation {
    pub signal: WallSignal,
    /// Every wall in the window the signal was read over (`wall_set`).
    pub walls: Vec<String>,
    /// The diagnosis the thread's newest finished turn ran on, when it ran
    /// on one.
    pub previous: Option<PriorDiagnosis>,
}

/// A diagnosis a finished turn's successor carried, read back from that
/// turn's assembly report.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PriorDiagnosis {
    /// The turn whose model calls wrote the diagnosis.
    pub written_in: Uuid,
    /// The walls it answered.
    pub walls: Vec<String>,
    pub synthesis: String,
    /// How many of the history's oldest messages it replaced.
    pub summarised: usize,
}

impl Degradation {
    /// The previous turn's diagnosis, when it answered exactly these walls.
    fn reusable(&self) -> Option<&PriorDiagnosis> {
        self.previous
            .as_ref()
            .filter(|previous| previous.walls == self.walls)
    }
}

/// The wall signal over `thread_id`'s recent finished turns, read from the
/// store: turn states, provider-round outcomes, tool-call statuses — with
/// the diagnosis the newest of them ran on, from its assembly report. Absent
/// when the window shows no wall, including after a diagnosis, once the
/// walls it answered have left the window. A read that fails leaves the
/// signal absent; the threshold still applies.
pub(super) async fn recent_degradation<S>(storage: &S, thread_id: Uuid) -> Option<Degradation>
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
    for turn in window {
        walls.push(turn_walls(storage, turn.id, turn.state == TurnState::Failed).await?);
    }
    let signal = detect_walls(&walls)?;
    let previous = match window.last() {
        Some(turn) => prior_diagnosis(storage, turn.id).await,
        None => None,
    };
    Some(Degradation {
        signal,
        walls: wall_set(&walls),
        previous,
    })
}

/// The diagnosis `turn_id`'s successor carried, when its assembly report
/// records a degradation pass that ran.
async fn prior_diagnosis<S>(storage: &S, turn_id: Uuid) -> Option<PriorDiagnosis>
where
    S: ContextSnapshotStore + ?Sized,
{
    let report = storage
        .get_snapshot(turn_id)
        .await
        .inspect_err(|error| tracing::warn!(%error, "wall signal: report unreadable"))
        .ok()??;
    let report: serde_json::Value = serde_json::from_str(&report).ok()?;
    let records: Vec<CompactionRecord> =
        serde_json::from_value(report.get("compaction_applied")?.clone()).ok()?;
    records.into_iter().find_map(|record| {
        let ran = record.requested_strategy == DEGRADATION_STRATEGY
            && record.actual_strategy == THRESHOLD_STRATEGY;
        let diagnosis = record.diagnosis.filter(|_| ran)?;
        Some(PriorDiagnosis {
            written_in: diagnosis.reused_from_turn.unwrap_or(turn_id),
            walls: diagnosis.walls,
            synthesis: diagnosis.synthesis?,
            summarised: record.messages_removed,
        })
    })
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
        .map(|call| FailedCall {
            id: call.id,
            tool: call.tool,
            arguments: serde_json::from_str(&call.arguments)
                .unwrap_or(serde_json::Value::String(call.arguments)),
        })
        .collect();
    Some(TurnWalls {
        turn_id,
        agent_failed,
        failed_calls,
    })
}

/// Failure text that identifies a transient provider or transport problem.
const TRANSPORT_FAILURE_MARKERS: &[&str] = &[
    "rate limit",
    "429",
    "timed out",
    "timeout",
    "network error",
    "connection",
    "retry",
    "retries",
    "stream error",
    "provider response anomaly",
    "http error 5",
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

/// The degradation-triggered pass. When the previous turn's diagnosis
/// answered these same walls, the successor carries it again over the same
/// split. Otherwise blind readers locate where the history went wrong, a
/// synthesis names the problems, and the synthesis becomes the successor's
/// summary; only the newest message is kept verbatim. The diagnosis record
/// comes back whether or not a successor did, with every model call made.
async fn succeed_on_degradation(
    site: &SuccessionSite<'_>,
    degradation: &Degradation,
    history: &[ChatMessage],
) -> (
    Result<succession::Succession, SuccessionError>,
    DiagnosisRecord,
) {
    let mut diagnosis = DiagnosisRecord {
        signal: degradation.signal.describe(),
        walls: degradation.walls.clone(),
        calls: Vec::new(),
        synthesis: None,
        reused_from_turn: None,
    };
    if let Some(previous) = degradation.reusable() {
        let request = SuccessionRequest {
            carried: &[],
            history,
            focus: site.focus,
            retain_at_most: None,
        };
        match succession::successor_after(
            &request,
            previous.summarised,
            previous.synthesis.clone(),
            DEGRADATION_STRATEGY,
            site.tokeniser,
        ) {
            Ok(succession) => {
                diagnosis.synthesis = Some(previous.synthesis.clone());
                diagnosis.reused_from_turn = Some(previous.written_in);
                return (Ok(succession), diagnosis);
            }
            Err(error) => tracing::warn!(
                %error,
                "the previous diagnosis no longer fits the history; diagnosing afresh"
            ),
        }
    }
    let outcome = diagnose(site, degradation, history, &mut diagnosis).await;
    (outcome, diagnosis)
}

/// A fresh diagnosis of `history`, each model call appended to `diagnosis`.
async fn diagnose(
    site: &SuccessionSite<'_>,
    degradation: &Degradation,
    history: &[ChatMessage],
    diagnosis: &mut DiagnosisRecord,
) -> Result<succession::Succession, SuccessionError> {
    let models = site
        .diagnosis_models
        .as_ref()
        .map_err(|error| SuccessionError::Diagnosis(error.clone()))?;
    let request = SuccessionRequest {
        carried: &[],
        history,
        focus: site.focus,
        retain_at_most: Some(1),
    };
    let summarised = succession::split_point(&request)?;
    let older = &history[..summarised];
    let slice_len = older.len().div_ceil(LOCATOR_COUNT).max(1);
    let reader_model = models.reader.as_deref();
    let slices = reader_slices(older, slice_len)?;
    let findings = futures::future::join_all(slices.iter().map(|slice| {
        site.provider.complete(diagnosis_request(
            LOCATOR_INSTRUCTIONS,
            succession::transcript(slice),
            reader_model,
        ))
    }))
    .await;
    let mut reports = Vec::new();
    let mut failed_reader = false;
    for finding in findings {
        let mut call = diagnosis_call(site, DiagnosisStage::Reader, reader_model, &finding);
        match finding {
            Ok(response) if !response.content.trim().is_empty() => {
                call.report = Some(response.content.clone());
                reports.push(response.content);
            }
            Ok(_) => {
                call.failure_reason = Some("the reader returned an empty report".to_string());
                failed_reader = true;
            }
            Err(error) => {
                tracing::warn!(%error, "a blind reader failed");
                failed_reader = true;
            }
        }
        diagnosis.calls.push(call);
    }
    if failed_reader {
        return Err(SuccessionError::Diagnosis(
            "one or more readers failed to examine their history".to_string(),
        ));
    }
    if reports.is_empty() {
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
    let reports = reports
        .iter()
        .enumerate()
        .map(|(index, report)| format!("### Reader {}\n\n{}", index + 1, report.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");
    let synthesis_model = models.synthesis.as_deref();
    let synthesis = site
        .provider
        .complete(diagnosis_request(
            SYNTHESIS_INSTRUCTIONS,
            format!(
                "{focus}## What the record shows\n\n{}\n\n## Readers' reports\n\n{reports}",
                degradation.signal.describe()
            ),
            synthesis_model,
        ))
        .await;
    diagnosis.calls.push(diagnosis_call(
        site,
        DiagnosisStage::Synthesis,
        synthesis_model,
        &synthesis,
    ));
    let synthesis = synthesis?.content;
    let succession = succession::successor(
        &request,
        synthesis.clone(),
        DEGRADATION_STRATEGY,
        site.tokeniser,
    )?;
    diagnosis.synthesis = Some(synthesis);
    Ok(succession)
}

fn reader_slices(
    history: &[ChatMessage],
    slice_len: usize,
) -> Result<Vec<&[ChatMessage]>, SuccessionError> {
    let mut slices = Vec::new();
    let mut start = 0;
    while start < history.len() {
        if slices.len() == LOCATOR_COUNT - 1 {
            slices.push(&history[start..]);
            break;
        }
        let proposed = (start + slice_len).min(history.len());
        let end = if proposed == history.len() {
            proposed
        } else {
            let boundary = succession::split_boundary(history, proposed)?;
            if boundary > start {
                boundary
            } else {
                // A pair can fill the preceding slice; move it together to
                // the following one rather than create an empty reader.
                ((proposed + 1)..history.len())
                    .filter_map(|candidate| succession::split_boundary(history, candidate).ok())
                    .find(|boundary| *boundary > start)
                    .unwrap_or(history.len())
            }
        };
        slices.push(&history[start..end]);
        start = end;
    }
    Ok(slices)
}

/// The record of one diagnosis call: where it went, what it cost, and why
/// it failed if it did. A reader's report is filled in by the caller.
fn diagnosis_call(
    site: &SuccessionSite<'_>,
    stage: DiagnosisStage,
    model: Option<&str>,
    outcome: &Result<swage_core::CompletionResponse, swage_core::ProviderError>,
) -> DiagnosisCall {
    let model = model.unwrap_or(site.provider.model_id()).to_string();
    match outcome {
        Ok(response) => DiagnosisCall {
            stage,
            model,
            input_tokens: response.input_tokens,
            output_tokens: response.output_tokens,
            cost: response.cost,
            report: None,
            failure_reason: None,
        },
        Err(error) => DiagnosisCall {
            stage,
            model,
            input_tokens: 0,
            output_tokens: 0,
            cost: None,
            report: None,
            failure_reason: Some(error.to_string()),
        },
    }
}

fn diagnosis_request(
    instructions: &str,
    material: String,
    model: Option<&str>,
) -> CompletionRequest {
    CompletionRequest {
        effort: None,
        messages: vec![
            ChatMessage::new(Role::System, instructions),
            ChatMessage::new(Role::User, material),
        ],
        model: model.map(str::to_owned),
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
                vec![json!({"outcome":"failed","message":"HTTP error 503: unavailable"})],
                false,
                "HTTP 5xx",
            ),
            (
                vec![json!({"outcome":"failed","message":"stream error: malformed SSE"})],
                false,
                "stream error",
            ),
            (
                vec![json!({"outcome":"failed","message":"provider response anomaly"})],
                false,
                "response anomaly",
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

    #[test]
    fn provider_refusal_counts_as_a_wall_but_transient_failures_do_not() {
        for (message, expected) in [
            ("HTTP error 400: unexpected tool_use_id", true),
            ("HTTP error 503: unavailable", false),
            ("stream error: malformed SSE", false),
            ("provider response anomaly", false),
        ] {
            let outcome = json!({"outcome": "failed", "message": message});
            assert_eq!(ended_on_its_own(&rounds(&[outcome])), expected, "{message}");
        }
    }

    #[test]
    fn reader_chunks_keep_a_call_with_its_result() {
        let mut history = vec![
            ChatMessage::new(Role::User, "message 0"),
            ChatMessage::new(Role::User, "message 1"),
        ];
        history.extend(swage_core::tool::build_tool_round_messages(
            &[swage_core::tool::ToolCall {
                call_id: "call-1".into(),
                name: "file_read".into(),
                arguments: json!({"path":"notes/a"}),
            }],
            &[swage_core::tool::ToolResult {
                call_id: "call-1".into(),
                content: "result from notes/a".into(),
                is_error: false,
            }],
            "",
        ));
        history
            .extend((4..8).map(|index| ChatMessage::new(Role::User, format!("message {index}"))));
        let slices = reader_slices(&history, 3).expect("history can be chunked");
        assert_eq!(slices.iter().map(|slice| slice.len()).sum::<usize>(), 8);
        assert_eq!(
            slices
                .iter()
                .filter(|slice| {
                    let transcript = succession::transcript(slice);
                    transcript.contains("file_read") && transcript.contains("result from notes/a")
                })
                .count(),
            1,
            "one reader sees both sides of the tool round"
        );
    }

    #[test]
    fn reader_boundary_shift_does_not_add_a_fourth_reader() {
        let mut history: Vec<_> = (0..5)
            .map(|index| ChatMessage::new(Role::User, format!("message {index}")))
            .collect();
        history.extend(swage_core::tool::build_tool_round_messages(
            &[swage_core::tool::ToolCall {
                call_id: "call-2".into(),
                name: "file_read".into(),
                arguments: json!({"path":"notes/b"}),
            }],
            &[swage_core::tool::ToolResult {
                call_id: "call-2".into(),
                content: "result from notes/b".into(),
                is_error: false,
            }],
            "",
        ));
        history
            .extend((7..9).map(|index| ChatMessage::new(Role::User, format!("message {index}"))));
        let slices = reader_slices(&history, 3).expect("history can be chunked");
        assert_eq!(slices.len(), LOCATOR_COUNT);
        assert_eq!(slices.iter().map(|slice| slice.len()).sum::<usize>(), 9);
        assert!(slices.iter().any(|slice| {
            let transcript = succession::transcript(slice);
            transcript.contains("file_read") && transcript.contains("result from notes/b")
        }));
    }

    fn failing(tool: &str, arguments: serde_json::Value) -> FailedCall {
        FailedCall {
            id: Uuid::new_v4(),
            tool: tool.to_string(),
            arguments,
        }
    }

    fn turn(failed_calls: Vec<FailedCall>) -> TurnWalls {
        TurnWalls {
            turn_id: Uuid::new_v4(),
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
            ..turn(vec![])
        });
        assert_eq!(detect_walls(&turns), None, "four is under the threshold");
        turns.push(TurnWalls {
            agent_failed: true,
            ..turn(vec![])
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
        failing_reader: Option<usize>,
        blank_reader: Option<usize>,
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
            let index = {
                let mut requests = self.requests.lock().expect("requests");
                let index = requests.len();
                requests.push(request);
                index
            };
            if self.failing_reader == Some(index) {
                return Err(swage_core::ProviderError::StreamError(
                    "reader unavailable".into(),
                ));
            }
            Ok(swage_core::CompletionResponse {
                content: if self.blank_reader == Some(index) {
                    "  ".to_string()
                } else {
                    "The read of notes/missing.md kept failing.".to_string()
                },
                input_tokens: 11,
                output_tokens: 7,
                finish_reason: swage_core::FinishReason::Stop,
                cost: Some(0.25),
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
            degradation: None,
            diagnosis_models: Ok(DiagnosisModels {
                reader: Some("reader-model".to_string()),
                synthesis: Some("synthesis-model".to_string()),
            }),
        };
        // A failed call as the gateway replays it: an assistant message
        // carrying the call, then a user message carrying its error result.
        let mut history = vec![ChatMessage::new(Role::User, "history message 0")];
        history.extend(swage_core::tool::build_tool_round_messages(
            &[swage_core::tool::ToolCall {
                call_id: "call-1".into(),
                name: "file_read".into(),
                arguments: json!({"path": "notes/missing.md"}),
            }],
            &[swage_core::tool::ToolResult {
                call_id: "call-1".into(),
                content: "no such file: notes/missing.md".into(),
                is_error: true,
            }],
            "",
        ));
        history.extend(
            (1..7).map(|index| ChatMessage::new(Role::User, format!("history message {index}"))),
        );
        let degradation = Degradation {
            signal: WallSignal::RepeatedFailingCall {
                tool: "file_read".to_string(),
                failures: 3,
            },
            walls: vec!["tool_call:a".to_string()],
            previous: None,
        };

        let (outcome, diagnosis) = succeed_on_degradation(&site, &degradation, &history).await;
        let succession = outcome.expect("the diagnosis runs");

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
        assert_eq!(
            readers
                .iter()
                .filter(|text| text.contains("file_read")
                    && text.contains("notes/missing.md")
                    && text.contains("no such file: notes/missing.md"))
                .count(),
            1,
            "a reader sees the failed call and its error result: {readers:?}"
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

        let models: Vec<Option<&str>> = requests
            .iter()
            .map(|request| request.model.as_deref())
            .collect();
        assert_eq!(
            models,
            [
                Some("reader-model"),
                Some("reader-model"),
                Some("reader-model"),
                Some("synthesis-model")
            ],
            "each stage goes to its configured model"
        );
        let reply = "The read of notes/missing.md kept failing.";
        let call = |stage, model: &str, report: Option<&str>| DiagnosisCall {
            stage,
            model: model.to_string(),
            input_tokens: 11,
            output_tokens: 7,
            cost: Some(0.25),
            report: report.map(str::to_owned),
            failure_reason: None,
        };
        let reader = call(DiagnosisStage::Reader, "reader-model", Some(reply));
        assert_eq!(
            diagnosis.calls,
            [
                reader.clone(),
                reader.clone(),
                reader,
                call(DiagnosisStage::Synthesis, "synthesis-model", None),
            ],
            "every call is recorded with its model, usage and report"
        );
        assert_eq!(diagnosis.synthesis.as_deref(), Some(reply));
        assert_eq!(diagnosis.walls, ["tool_call:a"]);
        assert_eq!(diagnosis.reused_from_turn, None);
    }

    #[tokio::test]
    async fn a_missing_reader_report_fails_the_diagnosis_and_records_the_failure() {
        for blank in [false, true] {
            let provider = RecordingProvider {
                failing_reader: (!blank).then_some(1),
                blank_reader: blank.then_some(1),
                ..Default::default()
            };
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
                degradation: None,
                diagnosis_models: Ok(DiagnosisModels::default()),
            };
            let history: Vec<_> = (0..8)
                .map(|index| ChatMessage::new(Role::User, format!("message {index}")))
                .collect();
            let degradation = Degradation {
                signal: WallSignal::RepeatedFailures {
                    failed_calls: 5,
                    failed_turns: 0,
                },
                walls: vec!["tool_call:a".into()],
                previous: None,
            };
            let (result, diagnosis) = succeed_on_degradation(&site, &degradation, &history).await;
            assert!(
                result.is_err(),
                "an incomplete read cannot replace all older history"
            );
            assert_eq!(diagnosis.calls.len(), 3);
            assert!(
                diagnosis.calls[1].failure_reason.is_some(),
                "the missing reader is recorded"
            );
            assert_eq!(
                provider.requests.lock().unwrap().len(),
                3,
                "synthesis must not run"
            );
        }
    }

    #[tokio::test]
    async fn threshold_success_keeps_the_failed_diagnosis_record() {
        let provider = RecordingProvider {
            failing_reader: Some(1),
            ..Default::default()
        };
        let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");
        let variables = BTreeMap::new();
        let threshold = CompactionThreshold::MessageCount(3);
        let history: Vec<_> = (0..8)
            .map(|index| ChatMessage::new(Role::User, format!("message {index}")))
            .collect();
        let assemble = |elements: &[Box<dyn ContextElement>]| {
            let mut report = swage_context::report::AssemblyReport::default();
            report.compaction_applied.push(CompactionRecord {
                element: "history".into(),
                requested_strategy: "scripted".into(),
                actual_strategy: "truncation".into(),
                messages_removed: 1,
                tokens_before: 10,
                tokens_after: 8,
                source_message_range: None,
                budget: Some(8),
                failure_reason: None,
                focus: None,
                diagnosis: None,
            });
            AssemblyOutput::new_unchecked(
                elements
                    .iter()
                    .flat_map(|element| element.content())
                    .collect(),
                report,
            )
        };
        let output = assemble(&[Box::new(HistoryElement::new(history.clone()))]);
        let site = SuccessionSite {
            provider: &provider,
            threshold: &threshold,
            model: None,
            focus: None,
            variables: &variables,
            tokeniser: &tokeniser,
            degradation: Some(Degradation {
                signal: WallSignal::RepeatedFailures {
                    failed_calls: 5,
                    failed_turns: 0,
                },
                walls: vec!["tool_call:a".into()],
                previous: None,
            }),
            diagnosis_models: Ok(DiagnosisModels::default()),
        };
        let result = succeed_over_threshold(
            site,
            vec![Box::new(HistoryElement::new(history))],
            output,
            assemble,
        )
        .await;
        let records = &result.report.compaction_applied;
        assert_eq!(
            records.len(),
            3,
            "the named assembly pass is not duplicated and both succession attempts are retained"
        );
        assert_eq!(records[0].requested_strategy, "scripted");
        assert_eq!(records[1].requested_strategy, DEGRADATION_STRATEGY);
        assert_eq!(records[1].actual_strategy, "none");
        assert!(
            records[1].diagnosis.as_ref().unwrap().calls[1]
                .failure_reason
                .is_some()
        );
        assert_eq!(records[2].requested_strategy, THRESHOLD_STRATEGY);
        assert_eq!(records[2].actual_strategy, THRESHOLD_STRATEGY);
    }

    #[test]
    fn diagnosis_models_resolve_through_the_catalogue_and_default_to_the_session_s() {
        let mut config = swage_config::ResolvedConfig::default();
        for (key, model_id, alias) in [
            ("session", "vendor/session-1", "main"),
            ("cheap", "vendor/cheap-1", "reader"),
        ] {
            config.models.entries.insert(
                key.to_string(),
                swage_config::ModelEndpointConfig {
                    model_id: model_id.to_string(),
                    aliases: vec![alias.to_string()],
                    ..Default::default()
                },
            );
        }
        config.models.default = "session".to_string();
        assert_eq!(
            diagnosis_models(&config),
            Ok(DiagnosisModels {
                reader: Some("vendor/session-1".to_string()),
                synthesis: Some("vendor/session-1".to_string()),
            }),
            "unset, both are the session's model"
        );
        config.context.diagnosis_reader_model = Some("reader".to_string());
        config.context.diagnosis_synthesis_model = Some("cheap".to_string());
        assert_eq!(
            diagnosis_models(&config),
            Ok(DiagnosisModels {
                reader: Some("vendor/cheap-1".to_string()),
                synthesis: Some("vendor/cheap-1".to_string()),
            }),
            "by alias or by entry key"
        );
        config.context.diagnosis_synthesis_model = Some("absent".to_string());
        assert!(diagnosis_models(&config).is_err());
    }

    #[test]
    fn the_wall_set_names_each_failed_call_and_each_turn_that_failed_on_its_own() {
        let call = failing("file_read", json!({"path": "a.md"}));
        let failed_turn = TurnWalls {
            agent_failed: true,
            ..turn(vec![])
        };
        let calling_turn = turn(vec![call.clone()]);
        let expected = {
            let mut walls = vec![
                format!("tool_call:{}", call.id),
                format!("turn:{}", failed_turn.turn_id),
            ];
            walls.sort();
            walls
        };
        assert_eq!(
            wall_set(&[failed_turn.clone(), calling_turn.clone(), turn(vec![])]),
            expected
        );
        assert_eq!(
            wall_set(&[calling_turn, failed_turn]),
            expected,
            "the set, not the order it was read in"
        );
    }
}
