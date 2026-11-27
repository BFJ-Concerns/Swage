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
//! conversation summary accompanied by a diagnosis written from outside the
//! session — blind readers each locate
//! where things went wrong in a slice of the history, and a synthesis over
//! their findings names the problems. The successor keeps only the newest
//! message verbatim, so the failing attempts do not follow it in. Every turn
//! runs on such a successor while the walls are still in the window; a turn
//! whose window holds the same walls its predecessor's diagnosis answered
//! carries that diagnosis again rather than writing a new one, and a turn
//! whose window no longer shows a wall gets its full history back.
//!
//! The instruction the conversation summary is written under is the
//! session's variant at `prompts.succession_summary`
//! (`SessionSummaryInstructions`), drawn and recorded when a pass first
//! asks for a summary, so the audit trail names the text behind every
//! summary and a session that never summarises holds no selection.

use crate::state::AppState;
use std::collections::{BTreeMap, HashMap};

use swage_core::{CompletionRequest, Role};
use swage_protocol::client::context::{
    CompactionRecord, DiagnosisRecord, SuccessionCall, SuccessionStage,
};
use swage_protocol::client::workflow::WorkflowProviderRoundOutcome;
use swage_storage::{
    ContextSnapshotStore, ProviderRoundRecord, ProviderRoundStore, ToolCallStore, TurnState,
    TurnStore,
};
use uuid::Uuid;

use crate::experimentation::VariantSelectionError;

use swage_agent::compaction::exceeds_threshold_with_model;
use swage_agent::config::CompactionThreshold;
use swage_context::assembly::default_assembly_carries;
use swage_context::element::ContextElement;
use swage_context::history::HistoryElement;
use swage_context::js_assembly::rendered_content;
use swage_context::report::AssemblyOutput;
use swage_context::succession::{
    self, DEGRADATION_STRATEGY, SUMMARY_INSTRUCTIONS, SuccessionError, SuccessionRequest,
    THRESHOLD_STRATEGY,
};
use swage_context::tokeniser::TiktokenTokeniser;
use swage_core::ChatMessage;
use swage_provider::LlmProvider;

/// The name `HistoryElement` reports itself under; the element the pass
/// replaces.
const HISTORY_ELEMENT: &str = "history";

/// What the pass needs from the turn beyond the elements themselves.
pub(super) struct SuccessionSite<'a> {
    /// The provider the summary is written on: the session's entry's own.
    pub provider: &'a dyn LlmProvider,
    /// The providers the diagnosis's readers and synthesis are written on,
    /// each the one serving its stage's catalogue entry.
    pub diagnosis_providers: DiagnosisProviders<'a>,
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
    /// What fills the succession slot under the active turn-graph revision.
    pub summariser: Summariser,
    /// The branches shadowing the succession step this turn: each runs the
    /// same request beside the live one and is recorded, never served.
    pub shadows: Vec<Shadow>,
    /// Where the conversation summary's instruction comes from, asked only
    /// when a pass is about to request a summary.
    pub summary_instructions: &'a dyn SummaryInstructionSource,
}

/// What summarises: the kernel's provider call, a plugin's slot, or a
/// saved workflow run with the summariser's request.
#[derive(Clone)]
pub(crate) enum Summariser {
    Builtin,
    Plugin(crate::plugin::slot::PluginSlot),
    /// The gateway, boxed: the site is held across the turn's awaits, and
    /// the gateway's own size would otherwise sit in every turn's future.
    Workflow {
        name: String,
        state: Box<AppState>,
        /// The session the run is dispatched for, so the run's record names
        /// the turn it served.
        session_id: Option<Uuid>,
    },
}

impl Summariser {
    /// What serves the slot, as the turn's record names it.
    pub(crate) fn describe(&self) -> String {
        match self {
            Summariser::Builtin => "builtin".to_owned(),
            Summariser::Plugin(slot) => format!("plugin {}", slot.plugin),
            Summariser::Workflow { name, .. } => format!("workflow {name}"),
        }
    }
}

/// A shadow of the succession step: a branch's filler and the recorder that
/// writes what it answered beside the live answer.
#[derive(Clone)]
pub(crate) struct Shadow {
    pub branch: String,
    pub summariser: Summariser,
    pub record: std::sync::Arc<dyn Fn(ShadowOutcome) + Send + Sync>,
}

/// What a shadow produced, beside what served.
#[derive(Debug, Clone)]
pub(crate) struct ShadowOutcome {
    pub branch: String,
    pub live_summary: String,
    pub shadow_summary: Result<String, String>,
}

impl Summariser {
    /// Run the succession pass with this summariser.
    async fn succeed(
        &self,
        request: SuccessionRequest<'_>,
        instructions: &str,
        provider: &dyn LlmProvider,
        tokeniser: &TiktokenTokeniser,
        calls: &mut Vec<SuccessionCall>,
    ) -> Result<succession::Succession, succession::SuccessionError> {
        match self {
            Summariser::Builtin => {
                succession::succeed(request, instructions, provider, tokeniser, calls).await
            }
            Summariser::Plugin(slot) => {
                succession::succeed_with(
                    request,
                    instructions,
                    |completion| async move { slot.summarise(completion, calls).await },
                    tokeniser,
                )
                .await
            }
            Summariser::Workflow {
                name,
                state,
                session_id,
            } => {
                succession::succeed_with(
                    request,
                    instructions,
                    |completion| async move {
                        let input = serde_json::json!({
                            "messages": completion
                                .messages
                                .iter()
                                .map(|message| serde_json::json!({ "role": format!("{:?}", message.role()).to_lowercase(), "content": message.text_content() }))
                                .collect::<Vec<_>>(),
                            "model": completion.model,
                        });
                        // A saved run is a large future; boxed so the turn's
                        // own stays small whatever fills the slot.
                        let output = Box::pin(crate::workflow::run_saved_workflow_for_slot(
                            (**state).clone(),
                            name,
                            input,
                            std::time::Duration::from_secs(120),
                            *session_id,
                        ))
                        .await
                        .map_err(succession::SuccessionError::Summariser)?;
                        output
                            .get("summary")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                            .ok_or_else(|| {
                                succession::SuccessionError::Summariser(format!(
                                    "workflow '{name}' returned no summary"
                                ))
                            })
                    },
                    tokeniser,
                )
                .await
            }
        }
    }
}

/// The source of the instruction a conversation summary is written under.
#[async_trait::async_trait]
pub(super) trait SummaryInstructionSource: Sync {
    /// The instruction for the summary about to be requested. A call is a
    /// use of the text: the session's source records its selection here.
    async fn select(&self) -> Result<&'static str, VariantSelectionError>;
}

/// The variant every session's summaries are written under unless it drew
/// another.
const DEFAULT_SUMMARY_INSTRUCTION_VARIANT: &str = "default";

/// The summariser instructions by variant id, the default first. A row here
/// is all a further variant needs: it registers the id for selection and
/// names the text it serves.
const SUMMARY_INSTRUCTION_VARIANTS: &[(&str, &str)] =
    &[(DEFAULT_SUMMARY_INSTRUCTION_VARIANT, SUMMARY_INSTRUCTIONS)];

/// The text a variant id names. A session's record can outlive its variant;
/// such a session is served the default, and the log says so.
fn summary_instructions_for(variant: &str) -> &'static str {
    SUMMARY_INSTRUCTION_VARIANTS
        .iter()
        .find(|(id, _)| *id == variant)
        .map(|(_, text)| *text)
        .unwrap_or_else(|| {
            tracing::warn!(
                variant,
                "the session's summary instruction variant is no longer registered; serving the default"
            );
            SUMMARY_INSTRUCTIONS
        })
}

/// A session's summariser instruction: its recorded choice at
/// `prompts.succession_summary`, or a first draw recorded against the turn
/// whose pass asked for the summary.
pub(super) struct SessionSummaryInstructions<'a> {
    pub state: &'a crate::state::AppState,
    pub config: &'a swage_config::model::ExperimentationConfig,
    pub session_id: Uuid,
    pub turn_id: Uuid,
}

#[async_trait::async_trait]
impl SummaryInstructionSource for SessionSummaryInstructions<'_> {
    async fn select(&self) -> Result<&'static str, VariantSelectionError> {
        let registered: Vec<&str> = SUMMARY_INSTRUCTION_VARIANTS
            .iter()
            .map(|(id, _)| *id)
            .collect();
        let decision = crate::experimentation::select_session_variant(
            self.state,
            self.config,
            crate::experimentation::LOCATION_SUCCESSION_SUMMARY_PROMPT,
            &registered,
            DEFAULT_SUMMARY_INSTRUCTION_VARIANT,
            self.session_id,
            Some(self.turn_id),
        )
        .await?;
        Ok(summary_instructions_for(&decision.variant))
    }
}

/// The instruction for a summary `site` is about to request. A selection
/// that cannot be made (the operator forced a variant nothing registers)
/// fails the pass like any other cause, so the attempt and every call
/// already made stay in the turn's record.
async fn selected_summary_instructions(
    site: &SuccessionSite<'_>,
) -> Result<&'static str, SuccessionError> {
    site.summary_instructions
        .select()
        .await
        .map_err(|error| SuccessionError::Instructions(error.to_string()))
}

/// The provider-facing model ids a diagnosis is sent to; `None` sends a
/// call to the provider's own default. Each stage's catalogue entry is kept
/// beside its id, since two entries may share an id on two providers.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DiagnosisModels {
    pub reader: Option<String>,
    pub synthesis: Option<String>,
    pub reader_entry: Option<String>,
    pub synthesis_entry: Option<String>,
}

/// The providers a diagnosis's two stages are sent through.
#[derive(Clone, Copy)]
pub(super) struct DiagnosisProviders<'a> {
    pub reader: &'a dyn LlmProvider,
    pub synthesis: &'a dyn LlmProvider,
}

#[cfg(test)]
impl<'a> DiagnosisProviders<'a> {
    /// Both stages on one provider.
    pub(super) fn both(provider: &'a dyn LlmProvider) -> Self {
        Self {
            reader: provider,
            synthesis: provider,
        }
    }
}

/// The provider serving a diagnosis stage's catalogue entry: the session's
/// own `session` provider when the stage runs on the session's entry or
/// names none.
pub(crate) async fn diagnosis_stage_provider(
    state: &crate::state::AppState,
    config: &swage_config::ResolvedConfig,
    entry: Option<&str>,
    session: &std::sync::Arc<dyn LlmProvider>,
) -> std::sync::Arc<dyn LlmProvider> {
    match entry {
        Some(entry) if entry != config.models.default => {
            crate::startup::catalogue_entry_provider(state, config, entry).await
        }
        _ => std::sync::Arc::clone(session),
    }
}

/// The diagnosis models `config` names (`context.diagnosis_reader_model`,
/// `context.diagnosis_synthesis_model`), each a catalogue entry or an alias
/// of one, and each the session's model when unset.
pub(crate) fn diagnosis_models(
    config: &swage_config::ResolvedConfig,
) -> Result<DiagnosisModels, String> {
    // A stage's (entry, provider-facing model id).
    let resolve = |field: &str, named: &Option<String>| match named {
        Some(name) => crate::turn::resolve_model_catalogue_entry(&config.models, name)
            .map(|(entry, model)| (Some(entry.to_owned()), Some(model.model_id.clone())))
            .ok_or_else(|| {
                format!("context.{field} names '{name}', which is not in the model catalogue")
            }),
        None => Ok((
            Some(config.models.default.clone()).filter(|entry| !entry.is_empty()),
            Some(config.default_model_id().to_string()).filter(|model| !model.is_empty()),
        )),
    };
    let (reader_entry, reader) = resolve(
        "diagnosis_reader_model",
        &config.context.diagnosis_reader_model,
    )?;
    let (synthesis_entry, synthesis) = resolve(
        "diagnosis_synthesis_model",
        &config.context.diagnosis_synthesis_model,
    )?;
    Ok(DiagnosisModels {
        reader,
        synthesis,
        reader_entry,
        synthesis_entry,
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
        let (outcome, diagnosis, mut calls) =
            succeed_on_degradation(&site, degradation, &history).await;
        let carried = carried_context(&elements, &output, site.variables);
        // A diagnosis explains the walls; the ordinary summary preserves what
        // the conversation established, including results unrelated to them.
        // Rebuild those notes even when the diagnosis is reused: the history
        // is rebuilt from storage on every turn and may contain new work.
        let outcome = match outcome {
            Ok(successor) => {
                async {
                    let established = succession::summarise(
                        selected_summary_instructions(&site).await?,
                        &carried,
                        &history[..successor.record.messages_removed],
                        site.focus,
                        site.provider,
                        &mut calls,
                    )
                    .await?;
                    succession::successor_after(
                        &SuccessionRequest {
                            carried: &carried,
                            history: &history,
                            focus: site.focus,
                            retain_at_most: None,
                        },
                        successor.record.messages_removed,
                        format!(
                            "{established}\n\n{}",
                            diagnosis
                                .synthesis
                                .as_deref()
                                .expect("a successful diagnosis has a synthesis")
                        ),
                        DEGRADATION_STRATEGY,
                        site.tokeniser,
                    )
                }
                .await
            }
            Err(error) => Err(error),
        };
        match outcome {
            Ok(mut succession) => {
                succession.record.calls = calls;
                succession.record.diagnosis = Some(diagnosis);
                return reassemble(
                    elements,
                    succession,
                    &assemble,
                    std::mem::take(&mut output.report.compaction_applied),
                );
            }
            Err(error) => {
                tracing::warn!(%error, "degradation succession did not run");
                let mut record = succession::failure_record(
                    &history,
                    site.tokeniser,
                    site.focus,
                    DEGRADATION_STRATEGY,
                    &error,
                );
                record.calls = calls;
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

    let carried = carried_context(&elements, &output, site.variables);

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
    let request = || SuccessionRequest {
        carried: &carried,
        history: &history,
        focus: site.focus,
        retain_at_most,
    };
    let mut calls = Vec::new();
    // A history too short to split asks for no summary, so it selects no
    // instruction either.
    let outcome = async {
        succession::split_point(&request())?;
        let instructions = selected_summary_instructions(&site).await?;
        let live = site
            .summariser
            .succeed(
                request(),
                instructions,
                site.provider,
                site.tokeniser,
                &mut calls,
            )
            .await;
        // A shadow runs the same request beside the live answer and is only
        // recorded: the turn serves what the live summariser answered, and
        // the record's calls are the live pass's.
        if let Ok(live) = &live {
            for shadow in &site.shadows {
                let shadow_summary = shadow
                    .summariser
                    .succeed(
                        request(),
                        instructions,
                        site.provider,
                        site.tokeniser,
                        &mut Vec::new(),
                    )
                    .await
                    .map(|succession| succession.element.summary().to_owned())
                    .map_err(|error| error.to_string());
                (shadow.record)(ShadowOutcome {
                    branch: shadow.branch.clone(),
                    live_summary: live.element.summary().to_owned(),
                    shadow_summary,
                });
            }
        }
        live
    }
    .await;
    match outcome {
        Ok(mut succession) => {
            succession.record.calls = calls;
            reassemble(
                elements,
                succession,
                &assemble,
                output.report.compaction_applied,
            )
        }
        Err(error) => {
            tracing::warn!(%error, "succession did not run; the agent runtime compacts this turn instead");
            let mut record = succession::failure_record(
                &history,
                site.tokeniser,
                site.focus,
                THRESHOLD_STRATEGY,
                &error,
            );
            record.calls = calls;
            output.report.compaction_applied.push(record);
            output
        }
    }
}

fn carried_context(
    elements: &[Box<dyn ContextElement>],
    output: &AssemblyOutput,
    variables: &BTreeMap<String, String>,
) -> Vec<ChatMessage> {
    // What the first assembly actually carried, by the report's own element
    // list — so an element a named assembly declared omitted is never shown
    // to the summariser as "already in place".
    let carried_names: std::collections::BTreeSet<&str> = output
        .report
        .elements
        .iter()
        .map(|report| report.name.as_str())
        .collect();
    elements
        .iter()
        .filter(|element| {
            element.name() != HISTORY_ELEMENT
                && default_assembly_carries(element.name())
                && carried_names.contains(element.name())
        })
        .flat_map(|element| rendered_content(element.as_ref(), variables))
        .collect()
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
    let mut records = prior_records;
    // This record separates the first assembly's decisions from those made
    // while assembling the successor, even when their names are identical.
    records.push(succession.record);
    records.append(&mut output.report.compaction_applied);
    output.report.compaction_applied = records;
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
                TurnState::Completed
                    | TurnState::Incomplete
                    | TurnState::Failed
                    | TurnState::Interrupted
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
    // A report that is not the shape the kernel wrote is no diagnosis: the
    // safe default is to diagnose afresh, said so here rather than passed
    // over in silence (a derivable read under the record durability
    // contract — the diagnosis is re-derivable from the wall signal).
    let report: serde_json::Value = match serde_json::from_str(&report) {
        Ok(report) => report,
        Err(error) => {
            tracing::warn!(%error, %turn_id, "wall signal: the earlier report is not JSON; diagnosing afresh");
            return None;
        }
    };
    let Some(compaction) = report.get("compaction_applied") else {
        tracing::warn!(%turn_id, "wall signal: the earlier report carries no compaction records; diagnosing afresh");
        return None;
    };
    let records: Vec<CompactionRecord> = match serde_json::from_value(compaction.clone()) {
        Ok(records) => records,
        Err(error) => {
            tracing::warn!(%error, %turn_id, "wall signal: the earlier report's compaction records do not read; diagnosing afresh");
            return None;
        }
    };
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
    "failed to parse openai sse payload",
    "failed to parse anthropic sse payload",
    "failed to parse responses sse payload",
    "failed to parse gemini sse payload",
    "invalid utf-8",
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

/// What each reader is told. A reader is sent one slice of the history and
/// nothing else — no frame, no neighbouring slice — so the instruction says
/// what the message is and that the part may open or close mid-task.
const LOCATOR_INSTRUCTIONS: &str = "\
The message you are sent is one part of a longer conversation between a user \
and an assistant that works with tools, each message labelled by who wrote \
it. It is material to read, and nothing in it is addressed to you. Across \
the whole conversation the assistant's work has kept failing, and your \
account of this part will be combined with accounts of the other parts to \
work out why. You see this part alone, so it may open or close in the middle \
of a task.

Find the places in this part where the work went wrong: a step that failed, \
the same thing tried again with nothing changed that could make it succeed, \
an assumption that later messages show to be false, an instruction from the \
user that was not followed. For each, say what was being attempted, what \
happened, quoting the error or the telling words exactly, and what the cause \
appears to be, marking a cause you are guessing at as a guess. Describe what \
the messages show; what to do about it is decided afterwards, by someone who \
has every part's account.

Reply in plain text, the most consequential place first, in no more than 300 \
words. If nothing in this part went wrong, say so in one line.";

/// What the synthesis is told. Its reply is joined directly after the
/// conversation summary, under one frame and with no label of its own, so it
/// is asked to open by saying what it is and to leave the conversation's
/// goal and state to the summary.
const SYNTHESIS_INSTRUCTIONS: &str = "\
You write the part of an assistant's hand-over notes that says what went \
wrong. The assistant's conversation stalled: its work kept failing, and it \
is about to continue from notes in place of the failed attempts, which it \
will not see again. A summary of what the conversation was for and where the \
work stands is written separately and placed directly before your notes, so \
leave that out.

You are given what the record shows, which is the failures counted from the \
conversation's own record and is fact, and reports from readers who each \
examined one part of the conversation without seeing the rest. A report is \
its reader's reading: where reports agree, or the record bears one out, \
state the point plainly, and where a single reader suggests a cause, give it \
as a possibility. When the message says what the assistant asked the notes \
to attend to, give that the room it needs.

Tell the assistant what kept failing, the most likely cause of each failure, \
and what not to repeat. Where the reports show a route that was never tried, \
or a fact the attempts overlooked, say so. Open by saying that this is an \
account of what went wrong before this point, since nothing else introduces \
it. Write to the assistant as \"you\", in plain text without headings, in no \
more than 300 words.";

/// The degradation-triggered pass. When the previous turn's diagnosis
/// answered these same walls, the successor carries it again over the same
/// split. Otherwise blind readers locate where the history went wrong, a
/// synthesis names the problems. The coordinator combines this diagnosis
/// with the ordinary conversation summary before reassembly; only the newest
/// message is kept verbatim. The diagnosis record comes back whether or not
/// a successor did; its calls are returned separately for the compaction record.
async fn succeed_on_degradation(
    site: &SuccessionSite<'_>,
    degradation: &Degradation,
    history: &[ChatMessage],
) -> (
    Result<succession::Succession, SuccessionError>,
    DiagnosisRecord,
    Vec<SuccessionCall>,
) {
    let mut diagnosis = DiagnosisRecord {
        signal: degradation.signal.describe(),
        walls: degradation.walls.clone(),
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
                return (Ok(succession), diagnosis, Vec::new());
            }
            Err(error) => tracing::warn!(
                %error,
                "the previous diagnosis no longer fits the history; diagnosing afresh"
            ),
        }
    }
    let mut calls = Vec::new();
    let outcome = diagnose(site, degradation, history, &mut diagnosis, &mut calls).await;
    (outcome, diagnosis, calls)
}

/// A fresh diagnosis of `history`, with each model call appended to `calls`
/// for the compaction record.
async fn diagnose(
    site: &SuccessionSite<'_>,
    degradation: &Degradation,
    history: &[ChatMessage],
    diagnosis: &mut DiagnosisRecord,
    calls: &mut Vec<SuccessionCall>,
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
        site.diagnosis_providers.reader.complete(diagnosis_request(
            LOCATOR_INSTRUCTIONS,
            succession::transcript(slice),
            reader_model,
        ))
    }))
    .await;
    let mut reports = Vec::new();
    let mut failed_reader = false;
    for finding in findings {
        let mut call = succession::recorded_call(
            site.diagnosis_providers.reader,
            SuccessionStage::Reader,
            reader_model,
            &finding,
        );
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
        calls.push(call);
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
        .diagnosis_providers
        .synthesis
        .complete(diagnosis_request(
            SYNTHESIS_INSTRUCTIONS,
            format!(
                "{focus}## What the record shows\n\n{}\n\n## Readers' reports\n\n{reports}",
                degradation.signal.describe()
            ),
            synthesis_model,
        ))
        .await;
    calls.push(succession::recorded_call(
        site.diagnosis_providers.synthesis,
        SuccessionStage::Synthesis,
        synthesis_model,
        &synthesis,
    ));
    let synthesis = synthesis?;
    if synthesis.finish_reason == swage_core::FinishReason::Length {
        let reason = "the diagnosis synthesis reached its output limit";
        calls
            .last_mut()
            .expect("synthesis call was recorded")
            .failure_reason = Some(reason.to_string());
        return Err(SuccessionError::Diagnosis(reason.to_string()));
    }
    let synthesis = synthesis.content;
    if synthesis.trim().is_empty() {
        calls
            .last_mut()
            .expect("synthesis call was recorded")
            .failure_reason = Some(SuccessionError::EmptySummary.to_string());
        return Err(SuccessionError::EmptySummary);
    }
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

    /// A snapshot store holding one report, as a turn's earlier report would
    /// be read back.
    struct OneReport(Option<String>);

    #[async_trait::async_trait]
    impl swage_storage::store::ContextSnapshotStore for OneReport {
        async fn store_snapshot(&self, _: Uuid, _: &str) -> Result<(), swage_core::StorageError> {
            unreachable!("the test never writes")
        }
        async fn get_snapshot(&self, _: Uuid) -> Result<Option<String>, swage_core::StorageError> {
            Ok(self.0.clone())
        }
        async fn list_snapshots_by_topic(
            &self,
            _: Uuid,
            _: usize,
        ) -> Result<Vec<swage_storage::model::ContextSnapshotRecord>, swage_core::StorageError>
        {
            unreachable!("the test never lists")
        }
    }

    /// An earlier report that is not what the kernel writes — not JSON, or
    /// records of another shape — yields no prior diagnosis, so the wall
    /// signal is diagnosed afresh: the documented safe default, never a
    /// reused reading of something unreadable.
    #[tokio::test]
    async fn an_unreadable_earlier_report_yields_no_prior_diagnosis() {
        let turn_id = Uuid::new_v4();
        for report in [
            None,
            Some("not json".to_owned()),
            Some(json!({ "compaction_applied": "not a list" }).to_string()),
            Some(json!({ "compaction_applied": [{ "element": "history" }] }).to_string()),
            Some(json!({ "elements": [] }).to_string()),
        ] {
            assert!(
                prior_diagnosis(&OneReport(report.clone()), turn_id)
                    .await
                    .is_none(),
                "{report:?}"
            );
        }
    }

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
            (
                "failed to parse OpenAI SSE payload: expected value at line 1 column 1",
                false,
            ),
            (
                "failed to parse Anthropic SSE payload: expected value at line 1 column 1",
                false,
            ),
            (
                "failed to parse Responses SSE payload: expected value at line 1 column 1",
                false,
            ),
            (
                "failed to parse Gemini SSE payload: expected value at line 1 column 1",
                false,
            ),
            (
                "invalid UTF-8: invalid utf-8 sequence of 1 bytes from index 0",
                false,
            ),
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

    /// A source that serves one text and counts how often a pass asked for
    /// it, or refuses as a forced variant nothing registers would.
    struct ScriptedInstructions {
        text: &'static str,
        asked: std::sync::atomic::AtomicUsize,
        refuses: bool,
    }

    impl ScriptedInstructions {
        const fn serving(text: &'static str) -> Self {
            Self {
                text,
                asked: std::sync::atomic::AtomicUsize::new(0),
                refuses: false,
            }
        }

        fn asked(&self) -> usize {
            self.asked.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl SummaryInstructionSource for ScriptedInstructions {
        async fn select(&self) -> Result<&'static str, VariantSelectionError> {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.refuses {
                return Err(VariantSelectionError {
                    location_id: crate::experimentation::LOCATION_SUCCESSION_SUMMARY_PROMPT
                        .to_owned(),
                    forced_variant: "unregistered".to_owned(),
                    available_variants: vec![DEFAULT_SUMMARY_INSTRUCTION_VARIANT.to_owned()],
                });
            }
            Ok(self.text)
        }
    }

    /// The authored text, for tests that are not about its selection.
    static AUTHORED_INSTRUCTIONS: ScriptedInstructions =
        ScriptedInstructions::serving(SUMMARY_INSTRUCTIONS);

    /// One pass over eight plain messages, with or without a wall, under a
    /// message-count threshold of `max_messages`.
    async fn pass_over_eight_messages(
        provider: &RecordingProvider,
        instructions: &ScriptedInstructions,
        degradation: bool,
        max_messages: u32,
    ) -> AssemblyOutput {
        let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");
        let variables = BTreeMap::new();
        let threshold = CompactionThreshold::MessageCount(max_messages);
        let history: Vec<_> = (0..8)
            .map(|index| ChatMessage::new(Role::User, format!("message {index}")))
            .collect();
        let elements: Vec<Box<dyn ContextElement>> = vec![Box::new(HistoryElement::new(history))];
        let assemble = |elements: &[Box<dyn ContextElement>]| {
            AssemblyOutput::new_unchecked(
                elements
                    .iter()
                    .flat_map(|element| element.content())
                    .collect(),
                swage_context::report::AssemblyReport::default(),
            )
        };
        let output = assemble(&elements);
        succeed_over_threshold(
            SuccessionSite {
                provider,
                diagnosis_providers: DiagnosisProviders::both(provider),
                threshold: &threshold,
                model: None,
                focus: None,
                variables: &variables,
                tokeniser: &tokeniser,
                degradation: degradation.then(|| Degradation {
                    signal: WallSignal::RepeatedFailures {
                        failed_calls: 5,
                        failed_turns: 0,
                    },
                    walls: vec!["tool_call:a".into()],
                    previous: None,
                }),
                diagnosis_models: Ok(DiagnosisModels::default()),
                summary_instructions: instructions,
                summariser: Summariser::Builtin,
                shadows: Vec::new(),
            },
            elements,
            output,
            assemble,
        )
        .await
    }

    #[tokio::test]
    async fn a_summary_is_written_under_the_instruction_selected_as_it_is_asked_for() {
        for degradation in [false, true] {
            let provider = RecordingProvider::default();
            let instructions = ScriptedInstructions::serving("Write the notes this session drew.");
            let max_messages = if degradation { 100 } else { 3 };
            let result =
                pass_over_eight_messages(&provider, &instructions, degradation, max_messages).await;

            assert_eq!(instructions.asked(), 1, "one summary, one selection");
            let requests = provider.requests.lock().unwrap();
            let summary = requests.last().expect("the summary request");
            assert_eq!(
                summary.messages[0].text_content(),
                "Write the notes this session drew."
            );
            assert_eq!(
                result.report.compaction_applied[0].actual_strategy,
                THRESHOLD_STRATEGY
            );
        }
    }

    #[tokio::test]
    async fn a_turn_that_asks_for_no_summary_selects_no_instruction() {
        // Within its room and against no wall, nothing is summarised.
        let provider = RecordingProvider::default();
        let instructions = ScriptedInstructions::serving(SUMMARY_INSTRUCTIONS);
        pass_over_eight_messages(&provider, &instructions, false, 100).await;
        assert_eq!(instructions.asked(), 0);
        assert!(provider.requests.lock().unwrap().is_empty());

        // A diagnosis that could not be written is followed by no summary.
        let provider = RecordingProvider {
            failing_synthesis: true,
            ..Default::default()
        };
        let result = pass_over_eight_messages(&provider, &instructions, true, 100).await;
        assert_eq!(instructions.asked(), 0);
        assert_eq!(result.report.compaction_applied[0].actual_strategy, "none");
    }

    #[tokio::test]
    async fn an_instruction_that_cannot_be_selected_fails_the_pass_and_keeps_its_record() {
        for degradation in [false, true] {
            let provider = RecordingProvider::default();
            let instructions = ScriptedInstructions {
                refuses: true,
                ..ScriptedInstructions::serving(SUMMARY_INSTRUCTIONS)
            };
            let result = pass_over_eight_messages(&provider, &instructions, degradation, 100).await;

            let records = &result.report.compaction_applied;
            if !degradation {
                assert!(records.is_empty(), "within its room, nothing was attempted");
                continue;
            }
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].actual_strategy, "none");
            let reason = records[0].failure_reason.as_deref().expect("the cause");
            assert!(
                reason.contains("instruction could not be selected")
                    && reason.contains("unregistered"),
                "{reason}"
            );
            assert_eq!(
                records[0].calls.len(),
                LOCATOR_COUNT + 1,
                "the diagnosis's completions stay recorded; no summary was requested"
            );
            assert_eq!(provider.requests.lock().unwrap().len(), LOCATOR_COUNT + 1);
        }

        // Over its room, the refused selection is the threshold pass's
        // recorded failure and the first assembly stands.
        let provider = RecordingProvider::default();
        let instructions = ScriptedInstructions {
            refuses: true,
            ..ScriptedInstructions::serving(SUMMARY_INSTRUCTIONS)
        };
        let result = pass_over_eight_messages(&provider, &instructions, false, 3).await;
        let records = &result.report.compaction_applied;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].requested_strategy, THRESHOLD_STRATEGY);
        assert_eq!(records[0].actual_strategy, "none");
        assert!(provider.requests.lock().unwrap().is_empty());
        assert_eq!(result.messages.len(), 8, "the unsucceeded context");
    }

    #[test]
    fn every_summary_instruction_variant_names_a_text_and_a_stale_one_serves_the_default() {
        assert_eq!(
            SUMMARY_INSTRUCTION_VARIANTS[0],
            (DEFAULT_SUMMARY_INSTRUCTION_VARIANT, SUMMARY_INSTRUCTIONS)
        );
        for (id, text) in SUMMARY_INSTRUCTION_VARIANTS {
            assert_eq!(summary_instructions_for(id), *text);
        }
        assert_eq!(
            summary_instructions_for("retired-variant"),
            SUMMARY_INSTRUCTIONS
        );
    }

    /// A provider that keeps every request and answers each with a fixed
    /// reply, so a test reads what the readers and the synthesis were shown.
    #[derive(Default)]
    struct RecordingProvider {
        requests: std::sync::Mutex<Vec<CompletionRequest>>,
        failing_reader: Option<usize>,
        blank_reader: Option<usize>,
        failing_synthesis: bool,
        blank_synthesis: bool,
        truncated_synthesis: bool,
        summary_mode: Option<&'static str>,
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
            let is_summary = request.messages[0].text_content() == SUMMARY_INSTRUCTIONS;
            let index = {
                let mut requests = self.requests.lock().expect("requests");
                let index = requests.len();
                requests.push(request);
                index
            };
            if (is_summary && self.summary_mode == Some("failed"))
                || self.failing_reader == Some(index)
                || (self.failing_synthesis && index == LOCATOR_COUNT)
            {
                return Err(swage_core::ProviderError::StreamError(
                    "diagnosis provider unavailable".into(),
                ));
            }
            Ok(swage_core::CompletionResponse {
                content: if (is_summary && self.summary_mode == Some("blank"))
                    || self.blank_reader == Some(index)
                    || (self.blank_synthesis && index == LOCATOR_COUNT)
                {
                    "  ".to_string()
                } else if self.truncated_synthesis && index == LOCATOR_COUNT {
                    "TRUNCATED_DIAGNOSIS: the missing file was ca".to_string()
                } else {
                    "The read of notes/missing.md kept failing.".to_string()
                },
                input_tokens: 11,
                output_tokens: 7,
                finish_reason: if (self.truncated_synthesis && index == LOCATOR_COUNT)
                    || (is_summary && self.summary_mode == Some("truncated"))
                {
                    swage_core::FinishReason::Length
                } else {
                    swage_core::FinishReason::Stop
                },
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
        // The summary's provider: a diagnosis is never sent through it.
        let summary_provider = RecordingProvider::default();
        let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");
        let variables = BTreeMap::new();
        let threshold = CompactionThreshold::MessageCount(100);
        let site = SuccessionSite {
            provider: &summary_provider,
            diagnosis_providers: DiagnosisProviders::both(&provider),
            threshold: &threshold,
            model: None,
            focus: None,
            variables: &variables,
            tokeniser: &tokeniser,
            degradation: None,
            diagnosis_models: Ok(DiagnosisModels {
                reader: Some("reader-model".to_string()),
                synthesis: Some("synthesis-model".to_string()),
                ..Default::default()
            }),
            summariser: Summariser::Builtin,
            shadows: Vec::new(),
            summary_instructions: &AUTHORED_INSTRUCTIONS,
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

        let (outcome, diagnosis, calls) =
            succeed_on_degradation(&site, &degradation, &history).await;
        let succession = outcome.expect("the diagnosis runs");

        let requests = provider.requests.lock().expect("requests");
        assert_eq!(
            requests.len(),
            LOCATOR_COUNT + 1,
            "three readers and one synthesis"
        );
        assert!(
            summary_provider
                .requests
                .lock()
                .expect("requests")
                .is_empty(),
            "every diagnosis stage goes to its own entry's provider"
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
        let call = |stage, model: &str, report: Option<&str>| SuccessionCall {
            stage,
            model: model.to_string(),
            input_tokens: 11,
            output_tokens: 7,
            cost: Some(0.25),
            report: report.map(str::to_owned),
            failure_reason: None,
        };
        let reader = call(SuccessionStage::Reader, "reader-model", Some(reply));
        assert_eq!(
            calls,
            [
                reader.clone(),
                reader.clone(),
                reader,
                call(SuccessionStage::Synthesis, "synthesis-model", None),
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
                diagnosis_providers: DiagnosisProviders::both(&provider),
                threshold: &threshold,
                model: None,
                focus: None,
                variables: &variables,
                tokeniser: &tokeniser,
                degradation: None,
                diagnosis_models: Ok(DiagnosisModels::default()),
                summariser: Summariser::Builtin,
                shadows: Vec::new(),
                summary_instructions: &AUTHORED_INSTRUCTIONS,
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
            let (result, _diagnosis, calls) =
                succeed_on_degradation(&site, &degradation, &history).await;
            assert!(
                result.is_err(),
                "an incomplete read cannot replace all older history"
            );
            assert_eq!(calls.len(), 3);
            assert!(
                calls[1].failure_reason.is_some(),
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
    async fn threshold_success_keeps_both_assembly_passes_and_the_failed_diagnosis() {
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
            let history_len = elements
                .iter()
                .find(|element| element.name() == HISTORY_ELEMENT)
                .expect("history element")
                .content()
                .len();
            report.compaction_applied.push(CompactionRecord {
                element: "history".into(),
                requested_strategy: "scripted".into(),
                actual_strategy: "truncation".into(),
                messages_removed: if history_len > 3 { 1 } else { 2 },
                tokens_before: 10,
                tokens_after: 8,
                source_message_range: None,
                budget: Some(8),
                failure_reason: None,
                focus: None,
                diagnosis: None,
                calls: Vec::new(),
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
            diagnosis_providers: DiagnosisProviders::both(&provider),
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
            summariser: Summariser::Builtin,
            shadows: Vec::new(),
            summary_instructions: &AUTHORED_INSTRUCTIONS,
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
            4,
            "both assembly passes and both succession decisions are retained"
        );
        assert_eq!(records[0].requested_strategy, "scripted");
        assert_eq!(records[0].messages_removed, 1);
        assert_eq!(records[1].requested_strategy, DEGRADATION_STRATEGY);
        assert_eq!(records[1].actual_strategy, "none");
        assert_eq!(records[1].calls.len(), LOCATOR_COUNT);
        assert!(records[1].calls[1].failure_reason.is_some());
        assert_eq!(records[2].requested_strategy, THRESHOLD_STRATEGY);
        assert_eq!(records[2].actual_strategy, THRESHOLD_STRATEGY);
        assert_eq!(records[3].requested_strategy, "scripted");
        assert_eq!(records[3].messages_removed, 2);
    }

    #[tokio::test]
    async fn failed_blank_or_truncated_synthesis_records_the_call_and_uses_threshold_succession() {
        for mode in ["failed", "blank", "truncated"] {
            let provider = RecordingProvider {
                failing_synthesis: mode == "failed",
                blank_synthesis: mode == "blank",
                truncated_synthesis: mode == "truncated",
                ..Default::default()
            };
            let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");
            let variables = BTreeMap::new();
            let threshold = CompactionThreshold::MessageCount(3);
            let history: Vec<_> = (0..8)
                .map(|index| ChatMessage::new(Role::User, format!("message {index}")))
                .collect();
            let assemble = |elements: &[Box<dyn ContextElement>]| {
                AssemblyOutput::new_unchecked(
                    elements
                        .iter()
                        .flat_map(|element| element.content())
                        .collect(),
                    swage_context::report::AssemblyReport::default(),
                )
            };
            let output = assemble(&[Box::new(HistoryElement::new(history.clone()))]);
            let result = succeed_over_threshold(
                SuccessionSite {
                    provider: &provider,
                    diagnosis_providers: DiagnosisProviders::both(&provider),
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
                    summariser: Summariser::Builtin,
                    shadows: Vec::new(),
                    summary_instructions: &AUTHORED_INSTRUCTIONS,
                },
                vec![Box::new(HistoryElement::new(history))],
                output,
                assemble,
            )
            .await;
            let records = &result.report.compaction_applied;
            assert_eq!(
                records.len(),
                2,
                "failed diagnosis then threshold succession"
            );
            assert_eq!(records[0].requested_strategy, DEGRADATION_STRATEGY);
            assert_eq!(records[0].actual_strategy, "none");
            let calls = &records[0].calls;
            assert_eq!(calls.len(), LOCATOR_COUNT + 1);
            assert_eq!(calls.last().unwrap().stage, SuccessionStage::Synthesis);
            assert!(
                calls.last().unwrap().failure_reason.is_some(),
                "synthesis failure recorded"
            );
            assert!(records[0].diagnosis.as_ref().unwrap().synthesis.is_none());
            if mode == "truncated" {
                assert!(
                    result
                        .messages
                        .iter()
                        .all(|message| !message.text_content().contains("TRUNCATED_DIAGNOSIS")),
                    "the successor must not carry the cut-off synthesis"
                );
                assert!(
                    calls
                        .last()
                        .unwrap()
                        .failure_reason
                        .as_deref()
                        .unwrap()
                        .contains("output limit")
                );
            }
            assert_eq!(records[1].actual_strategy, THRESHOLD_STRATEGY);
        }
    }

    async fn assert_summary_calls_are_recorded(degradation: bool) {
        for mode in ["complete", "failed", "blank", "truncated"] {
            let provider = RecordingProvider {
                summary_mode: Some(mode),
                ..Default::default()
            };
            let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");
            let variables = BTreeMap::new();
            // A failed degradation summary must stand alone, without a
            // threshold fallback adding a second summary completion.
            let threshold = CompactionThreshold::MessageCount(if degradation { 100 } else { 3 });
            let history: Vec<_> = (0..8)
                .map(|index| ChatMessage::new(Role::User, format!("message {index}")))
                .collect();
            let elements: Vec<Box<dyn ContextElement>> =
                vec![Box::new(HistoryElement::new(history))];
            let assemble = |elements: &[Box<dyn ContextElement>]| {
                AssemblyOutput::new_unchecked(
                    elements
                        .iter()
                        .flat_map(|element| element.content())
                        .collect(),
                    swage_context::report::AssemblyReport::default(),
                )
            };
            let output = assemble(&elements);
            let result = succeed_over_threshold(
                SuccessionSite {
                    provider: &provider,
                    diagnosis_providers: DiagnosisProviders::both(&provider),
                    threshold: &threshold,
                    model: None,
                    focus: None,
                    variables: &variables,
                    tokeniser: &tokeniser,
                    degradation: degradation.then(|| Degradation {
                        signal: WallSignal::RepeatedFailures {
                            failed_calls: 5,
                            failed_turns: 0,
                        },
                        walls: vec!["tool_call:a".into()],
                        previous: None,
                    }),
                    diagnosis_models: Ok(DiagnosisModels::default()),
                    summary_instructions: &AUTHORED_INSTRUCTIONS,
                    summariser: Summariser::Builtin,
                    shadows: Vec::new(),
                },
                elements,
                output,
                assemble,
            )
            .await;
            let records = &result.report.compaction_applied;
            assert_eq!(records.len(), 1, "{mode}");
            let record = &records[0];
            let expected_stages = if degradation {
                vec![
                    SuccessionStage::Reader,
                    SuccessionStage::Reader,
                    SuccessionStage::Reader,
                    SuccessionStage::Synthesis,
                    SuccessionStage::Summary,
                ]
            } else {
                vec![SuccessionStage::Summary]
            };
            assert_eq!(
                record
                    .calls
                    .iter()
                    .map(|call| call.stage)
                    .collect::<Vec<_>>(),
                expected_stages,
                "{mode}"
            );
            assert_eq!(
                record.calls.len(),
                provider.requests.lock().unwrap().len(),
                "one record per actual completion"
            );
            assert_eq!(record.diagnosis.is_some(), degradation);
            let serialised = serde_json::to_value(record).unwrap();
            if degradation {
                assert!(
                    serialised["diagnosis"].get("calls").is_none(),
                    "calls have one home"
                );
            }
            let summary = record.calls.last().unwrap();
            assert_eq!(summary.model, "recording");
            assert_eq!(summary.input_tokens, if mode == "failed" { 0 } else { 11 });
            assert_eq!(summary.output_tokens, if mode == "failed" { 0 } else { 7 });
            assert_eq!(
                summary.cost,
                if mode == "failed" { None } else { Some(0.25) }
            );
            let expected_failure = match mode {
                "failed" => Some("diagnosis provider unavailable"),
                "blank" => Some("empty summary"),
                "truncated" => Some("output limit"),
                _ => None,
            };
            match expected_failure {
                Some(reason) => assert!(
                    summary
                        .failure_reason
                        .as_deref()
                        .is_some_and(|failure| failure.contains(reason)),
                    "{mode}: {summary:?}"
                ),
                None => assert!(summary.failure_reason.is_none(), "{summary:?}"),
            }
            // Notes cut at the output limit are a failed pass, like blank
            // ones: they are never used as though they were whole.
            assert_eq!(
                record.actual_strategy,
                if matches!(mode, "failed" | "blank" | "truncated") {
                    "none"
                } else {
                    THRESHOLD_STRATEGY
                }
            );
        }
    }

    #[tokio::test]
    async fn ordinary_succession_records_every_summary_completion() {
        assert_summary_calls_are_recorded(false).await;
    }

    #[tokio::test]
    async fn degradation_succession_records_every_summary_completion() {
        assert_summary_calls_are_recorded(true).await;
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
                reader_entry: Some("session".to_string()),
                synthesis_entry: Some("session".to_string()),
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
                reader_entry: Some("cheap".to_string()),
                synthesis_entry: Some("cheap".to_string()),
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

    /// A host for a slot under test: no tools, one summary per completion.
    struct SlotTestHost;

    #[async_trait::async_trait]
    impl swage_plugin::PluginHost for SlotTestHost {
        async fn call_tool(
            &self,
            name: &str,
            _: serde_json::Value,
        ) -> Result<serde_json::Value, swage_plugin::HostCallFailure> {
            Err(swage_plugin::HostCallFailure {
                name: "HostCapabilityError".into(),
                message: format!("no tools: {name}"),
            })
        }
        async fn complete(
            &self,
            _: serde_json::Value,
        ) -> Result<serde_json::Value, swage_plugin::HostCallFailure> {
            Ok(
                serde_json::json!({ "content": "the plugin's gist", "input_tokens": 1, "output_tokens": 1 }),
            )
        }
        fn log(&self, _: &str) {}
    }

    #[tokio::test]
    async fn a_plugin_filling_the_succession_slot_runs_in_the_step_s_place_and_a_shadow_is_recorded()
     {
        let manifest = swage_plugin::PluginManifest::parse(
            r#"
            name = "summariser"
            version = "1"
            description = "Summarises."
            script = "plugin.js"
            grants = ["model"]
            [[provides.slots]]
            name = "succession"
            "#,
        )
        .unwrap();
        let runtime = swage_plugin::PluginRuntime::start(
            manifest,
            r#"swage.provide.slot("succession", async (input) => {
                const reply = await swage.complete({ messages: input.messages });
                return { summary: "PLUGIN: " + reply.content };
            });"#
                .to_owned(),
        )
        .await
        .expect("starts");
        let slot = crate::plugin::slot::PluginSlot::new(
            "summariser".into(),
            runtime,
            std::sync::Arc::new(SlotTestHost),
            swage_plugin::InvocationContext {
                session_id: Uuid::nil().to_string(),
                agent_id: "agent-a".into(),
                call_id: None,
                working_directory: "/work".into(),
            },
        );
        let provider = RecordingProvider::default();
        let tokeniser = TiktokenTokeniser::new("claude-sonnet-4-20250514");
        let variables = BTreeMap::new();
        let threshold = CompactionThreshold::MessageCount(3);
        let history: Vec<_> = (0..8)
            .map(|index| ChatMessage::new(Role::User, format!("message {index}")))
            .collect();
        let assemble = |elements: &[Box<dyn ContextElement>]| {
            AssemblyOutput::new_unchecked(
                elements
                    .iter()
                    .flat_map(|element| element.content())
                    .collect(),
                swage_context::report::AssemblyReport::default(),
            )
        };
        let output = assemble(&[Box::new(HistoryElement::new(history.clone()))]);
        let outcomes = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = std::sync::Arc::clone(&outcomes);
        let site = SuccessionSite {
            provider: &provider,
            threshold: &threshold,
            model: None,
            focus: None,
            variables: &variables,
            tokeniser: &tokeniser,
            degradation: None,
            diagnosis_models: Ok(DiagnosisModels::default()),
            summariser: Summariser::Plugin(slot),
            shadows: vec![Shadow {
                branch: "trial".into(),
                summariser: Summariser::Builtin,
                record: std::sync::Arc::new(move |outcome: ShadowOutcome| {
                    recorded.lock().unwrap().push(outcome);
                }),
            }],
            summary_instructions: &AUTHORED_INSTRUCTIONS,
        };
        let result = succeed_over_threshold(
            site,
            vec![Box::new(HistoryElement::new(history))],
            output,
            assemble,
        )
        .await;
        let summary = result
            .messages
            .iter()
            .map(|message| message.text_content())
            .find(|text| text.contains("PLUGIN: the plugin's gist"))
            .expect("the plugin's summary stands in for the summarised history");
        assert!(summary.contains("PLUGIN: the plugin's gist"));
        let outcomes = outcomes.lock().unwrap();
        assert_eq!(outcomes.len(), 1, "the shadow ran once");
        assert_eq!(outcomes[0].branch, "trial");
        assert!(outcomes[0].live_summary.starts_with("PLUGIN:"));
        assert!(
            outcomes[0].shadow_summary.is_ok(),
            "the builtin shadow answered through the provider: {:?}",
            outcomes[0].shadow_summary
        );
        assert_eq!(
            provider.requests.lock().unwrap().len(),
            1,
            "the provider served the shadow only; the live step was the plugin's"
        );
    }
}
