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

use std::collections::BTreeMap;

use swage_agent::compaction::exceeds_threshold_with_model;
use swage_agent::config::CompactionThreshold;
use swage_context::assembly::default_assembly_carries;
use swage_context::element::ContextElement;
use swage_context::history::HistoryElement;
use swage_context::js_assembly::rendered_content;
use swage_context::report::AssemblyOutput;
use swage_context::succession::{self, SuccessionRequest};
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
}

/// Return `output` unchanged when it is within the threshold; otherwise the
/// successor's assembly, produced by `assemble` over the successor's
/// elements, with the pass recorded in its report.
pub(super) async fn succeed_over_threshold(
    site: SuccessionSite<'_>,
    elements: Vec<Box<dyn ContextElement>>,
    output: AssemblyOutput,
    assemble: impl Fn(&[Box<dyn ContextElement>]) -> AssemblyOutput,
) -> AssemblyOutput {
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
    let Some(history) = elements
        .iter()
        .find(|element| element.name() == HISTORY_ELEMENT)
        .map(|element| element.content())
    else {
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

    let request = SuccessionRequest {
        carried: &carried,
        history: &history,
        focus: site.focus,
    };
    match succession::succeed(request, site.provider, site.tokeniser).await {
        Ok(succession) => {
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
                    &error,
                ));
            output
        }
    }
}
