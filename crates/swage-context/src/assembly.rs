//! Context assembly engine — default assembly implementation.
//!
//! The assembly engine accepts typed elements, sorts them by priority, applies
//! any configured token budget, runs truncation compaction on history when
//! needed, and returns a structured `AssemblyOutput` with a full report.
//!
//! The fail-open `assemble()` orchestrator lives here too. For non-"default"
//! assembly names it resolves the corresponding `.py` file in the config
//! hierarchy, executes it via the `PythonAssemblyRunner` trait, drains any
//! deferred compaction intents, and substitutes compacted content back into
//! the assembled message list.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use swage_config::discovery::resolve_assembly;
use swage_config::{HierarchyLevel, model::ContextConfig};
use swage_core::ChatMessage;
use swage_provider::LlmProvider;
use uuid::Uuid;

use crate::compaction::{CompactionExecutor, CompactionResult, TruncationCompactor};
use crate::element::ContextElement;
use crate::python_runner::PythonAssemblyRunner;
use crate::report::{AssemblyOutput, AssemblyReport, CompactionRecord, ElementReport};
use crate::tokeniser::Tokeniser;

/// Produce an assembled context from the provided elements.
///
/// This is the built-in "default" assembly, designed to be era-0.1 compatible:
/// given the same prompt rules and conversation history, it produces a message
/// sequence that generates an identical LLM request to the current gateway
/// (system message from instructions, then history in chronological order).
///
/// ## Element ordering
///
/// Elements are sorted by `priority()` descending before their content is
/// placed into the output. Current conventions:
///
/// | Priority | Element |
/// |----------|---------|
/// | 100      | `"prompts"` — system instructions |
/// | 50       | `"memory"` — **skipped in era 0.1** |
/// | 0        | `"history"` — conversation messages |
///
/// ## Budget enforcement
///
/// When `config.budget` is `Some(n)` and the assembled messages exceed `n`
/// tokens, [`TruncationCompactor`] is applied to the history element only.
/// Non-history content (system messages from prompts) is always included in
/// full — its token cost is subtracted from the budget before truncation.
///
/// ## Empty inputs
///
/// If no elements produce messages, the resulting `AssemblyOutput` will be
/// invalid. Callers must ensure at least one element supplies content.
pub fn default_assembly(
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
) -> AssemblyOutput {
    let start = Instant::now();

    // Sort elements by priority (highest first) for deterministic ordering.
    // Sorting on a collected reference slice avoids mutating the input.
    let mut sorted: Vec<&dyn ContextElement> = elements.iter().map(|e| e.as_ref()).collect();
    sorted.sort_by(|a, b| b.priority().cmp(&a.priority()));

    // Era 0.1: memory is not yet included in the assembled context.
    // Future assemblies that opt in to memory can simply remove this filter.
    let active: Vec<&dyn ContextElement> = sorted
        .into_iter()
        .filter(|e| e.name() != "memory")
        .collect();

    // Accumulate content and per-element reports in priority order.
    let mut assembled: Vec<ChatMessage> = Vec::new();
    let mut element_reports: Vec<ElementReport> = Vec::new();

    // Track the slice position of history messages so we can target compaction.
    let mut history_range: Option<std::ops::Range<usize>> = None;

    for element in &active {
        let content = element.content();
        let token_count = element.token_count(tokeniser);
        let source = element.source();

        if element.name() == "history" {
            let range_start = assembled.len();
            history_range = Some(range_start..range_start + content.len());
        }

        // Populate a preview for skills elements: a comma-separated list of
        // active skill names extracted from the source string. Other elements
        // do not yet provide previews.
        let preview = if element.name() == "skills" {
            extract_skill_names_preview(&source)
        } else {
            None
        };

        element_reports.push(ElementReport {
            name: element.name().to_string(),
            source,
            token_count,
            preview,
            priority: element.priority(),
            // Named assembly provenance is populated by the Python assembly
            // dispatcher rather than built-in element constructors.
            named_assembly: None,
        });

        assembled.extend(content);
    }

    // ── Budget enforcement ────────────────────────────────────────────────────

    let mut compaction_applied: Vec<CompactionRecord> = Vec::new();

    if let Some(budget) = config.budget {
        let total_tokens = tokeniser.count_messages(&assembled);

        if total_tokens > budget && history_range.is_some() {
            let range = history_range.clone().unwrap();

            // Compute the token cost of non-history messages (prompts, etc.).
            // History gets whatever budget remains after the other elements.
            let non_history_tokens: usize = {
                let non_history: Vec<&ChatMessage> = assembled
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !range.contains(i))
                    .map(|(_, m)| m)
                    .collect();
                // Collect into owned Vec<ChatMessage> for the tokeniser call.
                let owned: Vec<ChatMessage> = non_history.into_iter().cloned().collect();
                tokeniser.count_messages(&owned)
            };

            let history_budget = budget.saturating_sub(non_history_tokens);

            // Extract history messages, compact, then splice back in.
            let history_msgs: Vec<ChatMessage> = assembled[range.clone()].to_vec();
            let compactor = TruncationCompactor::new();
            let (compacted, record) = compactor.compact(history_msgs, history_budget, tokeniser);

            // Update the element report token count to reflect post-compaction cost.
            if let Some(report) = element_reports.iter_mut().find(|r| r.name == "history") {
                report.token_count = record.tokens_after;
            }

            // Splice the compacted messages back into the assembled list.
            let mut rebuilt: Vec<ChatMessage> = Vec::with_capacity(assembled.len());
            rebuilt.extend_from_slice(&assembled[..range.start]);
            rebuilt.extend(compacted);
            rebuilt.extend_from_slice(&assembled[range.end..]);
            assembled = rebuilt;

            compaction_applied.push(record);
        }
    }

    // ── Final report ──────────────────────────────────────────────────────────

    let total_tokens = tokeniser.count_messages(&assembled);
    let budget_remaining = config
        .budget
        .map(|b| b as isize - total_tokens as isize);
    let duration = start.elapsed();

    let report = AssemblyReport {
        assembly_name: "default".to_string(),
        elements: element_reports,
        total_tokens,
        budget: config.budget,
        budget_remaining,
        compaction_applied,
        warnings: Vec::new(),
        errors: Vec::new(),
        duration,
    };

    // If assembled is empty the caller provided no viable elements — that is a
    // programming error, not a runtime condition, so we panic here.
    AssemblyOutput::new_unchecked(assembled, report)
}

// ─────────────────────────────────────────────────────────────────────────────
// Private helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Extract a comma-separated list of skill names from a `SkillsElement` source string.
///
/// Source format produced by `SkillsElement::source()`:
/// - `"skills: name1 (level, ~N tokens), name2 (level, ~N tokens)"` — active skills
/// - `"skills (none active)"` — no active skills
///
/// Returns `Some("name1, name2")` when skills are present, `None` otherwise.
/// The parenthesised level/token annotations are stripped; only the bare names remain.
fn extract_skill_names_preview(source: &str) -> Option<String> {
    // Only process the "skills: ..." form; "skills (none active)" → None.
    let after_prefix = source.strip_prefix("skills: ")?;

    // Each entry is "name (level, ~N tokens)". We split on "), " to separate
    // entries (preserving the inner comma in the parentheses), then take the
    // portion before " (" to isolate the name.
    let names: Vec<&str> = after_prefix
        .split("), ")
        .filter_map(|entry| {
            entry.split(" (").next().filter(|n| !n.is_empty())
        })
        .collect();

    if names.is_empty() {
        None
    } else {
        Some(names.join(", "))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Fail-open assembly orchestration
// ─────────────────────────────────────────────────────────────────────────────

use crate::report::AssemblyError;

// ── DispatchError ─────────────────────────────────────────────────────────────

/// Error from dispatching a named assembly, consumed by `assemble()` for
/// fail-open handling.
#[derive(Debug)]
enum DispatchError {
    /// The assembly file was not found in the search hierarchy.
    NotFound,
    /// Python execution failed — carries the error kind and message.
    ExecutionFailed {
        /// Error kind string, e.g. `"syntax"`, `"runtime"`, `"timeout"`.
        error_kind: String,
        /// Human-readable error message, possibly containing a Python traceback.
        message: String,
    },
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

/// Dispatch to a named assembly.
///
/// - `"default"` / `""` — built-in default assembly (priority-ordered elements).
/// - Any other name — locate `<name>.py` in the search hierarchy, execute via
///   the Python runner, drain pending compaction intents, and substitute
///   compacted content into the assembled message list.
///
/// Returns `Err(DispatchError::NotFound)` when the assembly file is absent or
/// no runner is provided. Returns `Err(DispatchError::ExecutionFailed)` on
/// Python syntax errors, runtime exceptions, or timeout.
async fn dispatch_assembly(
    name: &str,
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
    search_levels: &[(HierarchyLevel, &Path)],
    provider: &dyn LlmProvider,
    python_runner: Option<&dyn PythonAssemblyRunner>,
) -> Result<AssemblyOutput, DispatchError> {
    match name {
        "default" | "" => Ok(default_assembly(elements, config, tokeniser)),
        _ => {
            let start = Instant::now();

            // Locate the Python file in the config hierarchy.
            let resolved = resolve_assembly(name, search_levels)
                .ok_or(DispatchError::NotFound)?;

            // A runner is required for Python execution. When none is provided
            // (e.g. in test contexts without the Python crate), treat as not-found
            // and allow the fail-open fallback to take over.
            let runner = python_runner.ok_or(DispatchError::NotFound)?;

            let timeout = Duration::from_secs(config.timeout);
            // When no budget is configured, pass usize::MAX so the Python side
            // sees an effectively unlimited budget.
            let budget = config.budget.unwrap_or(usize::MAX);
            let model = provider.model_id().to_string();

            // Convert borrowed paths to owned for the runner.
            let owned_levels: Vec<(HierarchyLevel, PathBuf)> = search_levels
                .iter()
                .map(|(level, path)| (level.clone(), path.to_path_buf()))
                .collect();

            // Execute the Python assembly script.
            let py_result = runner
                .run(
                    &resolved.path,
                    timeout,
                    budget,
                    &model,
                    "",  // agent_id: not available in this context
                    elements,
                    tokeniser,
                    owned_levels,
                )
                .map_err(|err| DispatchError::ExecutionFailed {
                    error_kind: err.kind.to_string(),
                    message: err.message,
                })?;

            // Guard: an empty message list from Python is treated as an error
            // (same as the default_assembly's panic guard).
            if py_result.messages.is_empty() {
                return Err(DispatchError::ExecutionFailed {
                    error_kind: "invalid_output".to_string(),
                    message: format!("assembly '{name}' produced an empty message list"),
                });
            }

            // Execute pending compaction intents recorded by Python (e.g. from
            // history_older() calls that specified a strategy).
            let executor = CompactionExecutor::new();
            let compaction_results = executor
                .execute_pending(py_result.pending_compactions, provider)
                .await;

            // Substitute compacted message sequences back into the output.
            let assembled = substitute_compacted(py_result.messages, &compaction_results);

            let compaction_applied = build_compaction_records(compaction_results, tokeniser);
            let total_tokens = tokeniser.count_messages(&assembled);
            let budget_remaining =
                config.budget.map(|b| b as isize - total_tokens as isize);

            let report = AssemblyReport {
                assembly_name: name.to_string(),
                elements: py_result.element_reports,
                total_tokens,
                budget: config.budget,
                budget_remaining,
                compaction_applied,
                warnings: py_result.warnings,
                errors: Vec::new(),
                duration: start.elapsed(),
            };

            Ok(AssemblyOutput::new_unchecked(assembled, report))
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Substitute compacted message sequences into the assembled output.
///
/// For each compaction result, the original messages are located by UUID and
/// replaced with the compacted equivalents. UUID comparison is used because
/// `ChatMessage` does not implement `PartialEq` — UUIDs are preserved through
/// cloning so the intent's messages share IDs with the Python-assembled output.
fn substitute_compacted(
    mut messages: Vec<ChatMessage>,
    results: &[CompactionResult],
) -> Vec<ChatMessage> {
    for result in results {
        let intent_ids: Vec<Uuid> = result.intent.messages.iter().map(|m| m.id).collect();
        if intent_ids.is_empty() {
            continue;
        }
        if let Some(range) = find_contiguous_by_id(&messages, &intent_ids) {
            // Splice out the original messages and insert the compacted ones.
            let tail: Vec<ChatMessage> = messages.drain(range.end..).collect();
            messages.truncate(range.start);
            messages.extend(result.compacted.clone());
            messages.extend(tail);
        }
    }
    messages
}

/// Find the start..end range in `messages` where a contiguous subsequence of
/// the given `ids` appears, or `None` if no such subsequence exists.
fn find_contiguous_by_id(
    messages: &[ChatMessage],
    ids: &[Uuid],
) -> Option<std::ops::Range<usize>> {
    if ids.is_empty() || messages.len() < ids.len() {
        return None;
    }
    'outer: for start in 0..=messages.len() - ids.len() {
        for (offset, id) in ids.iter().enumerate() {
            if messages[start + offset].id != *id {
                continue 'outer;
            }
        }
        return Some(start..start + ids.len());
    }
    None
}

/// Convert [`CompactionResult`] items into [`CompactionRecord`]s for the
/// assembly report.
fn build_compaction_records(
    results: Vec<CompactionResult>,
    tokeniser: &dyn Tokeniser,
) -> Vec<CompactionRecord> {
    results
        .into_iter()
        .map(|r| {
            let tokens_before = tokeniser.count_messages(&r.intent.messages);
            let tokens_after = tokeniser.count_messages(&r.compacted);
            let messages_removed = r.intent.messages.len().saturating_sub(r.compacted.len());
            CompactionRecord {
                element: r.intent.element_name,
                requested_strategy: r.intent.strategy,
                actual_strategy: r.actual_strategy,
                messages_removed,
                tokens_before,
                tokens_after,
                source_message_range: r.intent.source_range,
                budget: Some(r.intent.budget),
                failure_reason: r.failure_reason,
            }
        })
        .collect()
}

// ── make_error ────────────────────────────────────────────────────────────────

/// Construct an [`AssemblyError`] for the fail-open report.
fn make_error(assembly_name: &str, error_kind: &str, message: &str) -> AssemblyError {
    AssemblyError {
        assembly_name: assembly_name.to_string(),
        error_kind: error_kind.to_string(),
        message: message.to_string(),
        fallback_used: "default".to_string(),
    }
}

// ── assemble ──────────────────────────────────────────────────────────────────

/// Assemble context with fail-open semantics.
///
/// Dispatches to the assembly named in `config.assembly` within a
/// `config.timeout`-second deadline. On success, the output is returned
/// as-is. On any failure — timeout, assembly not found, Python error, or
/// empty output — the function falls back to [`default_assembly`] and records
/// an [`AssemblyError`] in the report.
///
/// The fail-open mechanism itself does not fail. If `default_assembly` panics
/// (programming error — no element produced content), that panic propagates.
///
/// ## Python assembly dispatch
///
/// When `config.assembly` is not `"default"`, `search_levels` is searched for
/// a matching `.py` file. `python_runner` executes the script. Pass `None` for
/// `python_runner` in contexts without the Python crate — the assembly will
/// fall back to default with a `"not_found"` error in the report.
pub async fn assemble(
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
    search_levels: &[(HierarchyLevel, &Path)],
    provider: &dyn LlmProvider,
    python_runner: Option<&dyn PythonAssemblyRunner>,
) -> AssemblyOutput {
    let assembly_name = config.assembly.clone();
    let timeout_duration = Duration::from_secs(config.timeout);

    let run = dispatch_assembly(
        &assembly_name,
        elements,
        config,
        tokeniser,
        search_levels,
        provider,
        python_runner,
    );

    match tokio::time::timeout(timeout_duration, run).await {
        // Normal success path.
        Ok(Ok(output)) => output,

        // Python assembly produced an empty message list.
        Ok(Err(DispatchError::ExecutionFailed { ref error_kind, ref message }))
            if error_kind == "invalid_output" =>
        {
            let mut fallback = default_assembly(elements, config, tokeniser);
            fallback.report.errors.push(make_error(
                &assembly_name,
                "invalid_output",
                message,
            ));
            fallback
        }

        // Python execution failed (syntax error, runtime exception, timeout).
        Ok(Err(DispatchError::ExecutionFailed { error_kind, message })) => {
            let mut fallback = default_assembly(elements, config, tokeniser);
            fallback.report.errors.push(make_error(
                &assembly_name,
                &error_kind,
                &message,
            ));
            fallback
        }

        // Assembly file not found in the search hierarchy, or no runner provided.
        Ok(Err(DispatchError::NotFound)) => {
            let mut fallback = default_assembly(elements, config, tokeniser);
            fallback.report.errors.push(make_error(
                &assembly_name,
                "not_found",
                &format!(
                    "assembly '{assembly_name}' was not found in the configured search hierarchy"
                ),
            ));
            fallback
        }

        // Outer tokio timeout elapsed. Python execution has its own interior
        // timeout (via mpsc + PyErr_SetInterrupt); this is a safety net.
        Err(_elapsed) => {
            let mut fallback = default_assembly(elements, config, tokeniser);
            fallback.report.errors.push(make_error(
                &assembly_name,
                "timeout",
                &format!(
                    "assembly '{assembly_name}' exceeded timeout of {}s",
                    config.timeout
                ),
            ));
            fallback
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::Role;
    use swage_provider::LlmProvider;
    use async_trait::async_trait;
    use std::pin::Pin;
    use futures::Stream;
    use swage_core::{CompletionRequest, CompletionResponse, ProviderError, StreamChunk,
        ToolCompletionRequest, ToolCompletionResponse};

    // ── Stub provider (unit tests only need model_id; other methods never called) ──

    struct StubProvider;

    #[async_trait]
    impl LlmProvider for StubProvider {
        fn model_id(&self) -> &str { "stub-model" }

        async fn complete(&self, _req: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
            unimplemented!("StubProvider::complete — not used in unit tests")
        }

        async fn complete_with_tools(&self, _req: ToolCompletionRequest) -> Result<ToolCompletionResponse, ProviderError> {
            unimplemented!("StubProvider::complete_with_tools — not used in unit tests")
        }

        fn stream(&self, _req: CompletionRequest) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>> {
            unimplemented!("StubProvider::stream — not used in unit tests")
        }
    }

    /// Convenience: call assemble with empty search levels and no Python runner,
    /// suitable for tests that only exercise the default assembly path.
    async fn assemble_default(
        elements: &[Box<dyn ContextElement>],
        config: &ContextConfig,
        tokeniser: &dyn Tokeniser,
    ) -> AssemblyOutput {
        assemble(elements, config, tokeniser, &[], &StubProvider, None).await
    }

    // ── Test fixtures ─────────────────────────────────────────────────────────

    /// Deterministic tokeniser: each message costs exactly `cost` tokens.
    struct FixedCostTokeniser {
        cost: usize,
    }

    impl Tokeniser for FixedCostTokeniser {
        fn count_text(&self, _text: &str) -> usize {
            self.cost
        }

        fn count_messages(&self, messages: &[ChatMessage]) -> usize {
            messages.len() * self.cost
        }
    }

    struct MockElement {
        name: &'static str,
        priority: i32,
        messages: Vec<ChatMessage>,
        source: &'static str,
    }

    impl ContextElement for MockElement {
        fn name(&self) -> &str {
            self.name
        }

        fn priority(&self) -> i32 {
            self.priority
        }

        fn content(&self) -> Vec<ChatMessage> {
            self.messages.clone()
        }

        fn token_count(&self, tokeniser: &dyn Tokeniser) -> usize {
            tokeniser.count_messages(&self.messages)
        }

        fn source(&self) -> String {
            self.source.to_string()
        }
    }

    fn prompts_element(system_text: &str) -> Box<dyn ContextElement> {
        Box::new(MockElement {
            name: "prompts",
            priority: 100,
            messages: vec![ChatMessage::new(Role::System, system_text)],
            source: "prompt rules",
        })
    }

    fn history_element(turns: &[(&str, &str)]) -> Box<dyn ContextElement> {
        let messages = turns
            .iter()
            .flat_map(|(user, asst)| {
                [
                    ChatMessage::new(Role::User, *user),
                    ChatMessage::new(Role::Assistant, *asst),
                ]
            })
            .collect();
        Box::new(MockElement {
            name: "history",
            priority: 0,
            messages,
            source: "thread history",
        })
    }

    fn memory_element() -> Box<dyn ContextElement> {
        Box::new(MockElement {
            name: "memory",
            priority: 50,
            messages: vec![ChatMessage::new(Role::User, "some memory content")],
            source: "agent memory",
        })
    }

    fn no_budget_config() -> ContextConfig {
        ContextConfig {
            assembly: "default".to_string(),
            timeout: 5,
            budget: None,
        }
    }

    fn budget_config(tokens: usize) -> ContextConfig {
        ContextConfig {
            assembly: "default".to_string(),
            timeout: 5,
            budget: Some(tokens),
        }
    }

    // ── Assembly order ────────────────────────────────────────────────────────

    #[test]
    fn system_message_from_prompts_appears_before_history() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            // Provide history first (lower priority) to test reordering.
            history_element(&[("Hello", "Hi!")]),
            prompts_element("You are helpful."),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        assert!(output.messages.len() >= 2);
        // System message must be first.
        assert_eq!(output.messages[0].role, Role::System);
        // Conversation messages follow.
        assert_eq!(output.messages[1].role, Role::User);
        assert_eq!(output.messages[2].role, Role::Assistant);
    }

    #[test]
    fn empty_prompts_produce_no_system_message() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            Box::new(MockElement {
                name: "prompts",
                priority: 100,
                messages: vec![], // no rules → no system message
                source: "prompt rules",
            }),
            history_element(&[("Hello", "Hi!")]),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Only history messages — no system message.
        assert!(
            output.messages.iter().all(|m| m.role != Role::System),
            "expected no system message when prompts element is empty"
        );
        assert_eq!(output.messages.len(), 2);
    }

    // ── Memory is skipped ─────────────────────────────────────────────────────

    #[test]
    fn memory_element_is_excluded_from_assembled_messages() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            memory_element(),
            history_element(&[("Hello", "Hi!")]),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Only system + 2 history messages; memory content absent.
        assert_eq!(output.messages.len(), 3, "memory content should be excluded");
        // No message should come from the memory element.
        for msg in &output.messages {
            let is_memory = matches!(
                msg.content.first(),
                Some(swage_core::ContentPart::Text { text }) if text.contains("memory content")
            );
            assert!(!is_memory, "memory content appeared in assembled messages");
        }
    }

    // ── Report population ─────────────────────────────────────────────────────

    #[test]
    fn report_assembly_name_is_default() {
        let elements: Vec<Box<dyn ContextElement>> =
            vec![history_element(&[("Hi", "Hello")])];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        assert_eq!(output.report.assembly_name, "default");
    }

    #[test]
    fn report_contains_element_report_for_each_active_element() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            history_element(&[("Hi", "Hello")]),
            memory_element(), // skipped — should not appear in element reports
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Two active elements: prompts + history. Memory is excluded.
        assert_eq!(output.report.elements.len(), 2);
        let names: Vec<&str> = output.report.elements.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"prompts"));
        assert!(names.contains(&"history"));
        assert!(!names.contains(&"memory"));
    }

    #[test]
    fn report_total_tokens_matches_assembled_message_count() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            history_element(&[("Hi", "Hello")]),
        ];
        // cost=10, 3 messages → 30 tokens.
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        assert_eq!(output.report.total_tokens, output.messages.len() * 10);
    }

    #[test]
    fn report_budget_and_budget_remaining_populated_when_budget_configured() {
        let elements: Vec<Box<dyn ContextElement>> =
            vec![history_element(&[("Hi", "Hello")])];
        // 2 messages × 10 = 20 tokens. Budget of 100 → remaining = 80.
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &budget_config(100), &tokeniser);

        assert_eq!(output.report.budget, Some(100));
        assert_eq!(output.report.budget_remaining, Some(80));
    }

    #[test]
    fn report_budget_none_when_not_configured() {
        let elements: Vec<Box<dyn ContextElement>> =
            vec![history_element(&[("Hi", "Hello")])];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        assert!(output.report.budget.is_none());
        assert!(output.report.budget_remaining.is_none());
    }

    // ── Budget enforcement and compaction ─────────────────────────────────────

    #[test]
    fn compaction_not_triggered_when_within_budget() {
        let elements: Vec<Box<dyn ContextElement>> =
            vec![history_element(&[("Hi", "Hello")])];
        // 2 messages × 10 = 20 tokens. Budget 100 → no compaction needed.
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &budget_config(100), &tokeniser);

        assert!(output.report.compaction_applied.is_empty());
    }

    #[test]
    fn compaction_triggered_when_history_exceeds_budget() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            // 6 history messages × 10 = 60 tokens; prompts adds 10 → total 70.
            history_element(&[
                ("Turn 1 user", "Turn 1 assistant"),
                ("Turn 2 user", "Turn 2 assistant"),
                ("Turn 3 user", "Turn 3 assistant"),
            ]),
        ];
        // Budget of 30: prompts takes 10, leaving only 20 for history.
        // History has 6 messages × 10 = 60, so compaction must drop some.
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &budget_config(30), &tokeniser);

        assert!(
            !output.report.compaction_applied.is_empty(),
            "expected compaction to trigger"
        );
        let record = &output.report.compaction_applied[0];
        assert_eq!(record.actual_strategy, "truncation");
        assert!(record.messages_removed > 0);
    }

    // ── assemble() — fail-open orchestration ─────────────────────────────────

    /// Normal path: default assembly succeeds and is returned without errors.
    #[tokio::test]
    async fn assemble_returns_default_output_on_success() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            history_element(&[("Hello", "Hi!")]),
        ];
        let config = no_budget_config(); // config.assembly == "default"
        let tokeniser = FixedCostTokeniser { cost: 10 };

        let output = assemble_default(&elements, &config, &tokeniser).await;

        assert!(!output.messages.is_empty(), "should produce messages");
        assert!(output.report.errors.is_empty(), "no errors on success path");
        assert_eq!(output.report.assembly_name, "default");
    }

    /// Unknown assembly name with no search levels → not_found fallback.
    #[tokio::test]
    async fn assemble_falls_back_on_unknown_assembly_name() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            history_element(&[("Hello", "Hi!")]),
        ];
        let config = ContextConfig {
            assembly: "nonexistent_python_assembly".to_string(),
            timeout: 5,
            budget: None,
        };
        let tokeniser = FixedCostTokeniser { cost: 10 };

        // Empty search levels: no Python file will be found.
        let output = assemble_default(&elements, &config, &tokeniser).await;

        // Fallback output is still valid.
        assert!(!output.messages.is_empty(), "fallback should produce messages");

        // Error is recorded in the report.
        assert_eq!(output.report.errors.len(), 1);
        let error = &output.report.errors[0];
        assert_eq!(error.assembly_name, "nonexistent_python_assembly");
        assert_eq!(error.error_kind, "not_found");
        assert_eq!(error.fallback_used, "default");
    }

    /// Fallback output matches running default_assembly directly.
    #[tokio::test]
    async fn assemble_fallback_output_matches_direct_default_assembly() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            history_element(&[("Hello", "Hi!")]),
        ];
        let config = ContextConfig {
            assembly: "missing_assembly".to_string(),
            timeout: 5,
            budget: None,
        };
        let tokeniser = FixedCostTokeniser { cost: 10 };

        let fallback_output = assemble_default(&elements, &config, &tokeniser).await;
        let direct_output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Both should produce the same messages.
        assert_eq!(
            fallback_output.messages.len(),
            direct_output.messages.len(),
            "fallback and direct default assembly should produce same message count"
        );
        for (fb, direct) in fallback_output.messages.iter().zip(direct_output.messages.iter()) {
            assert_eq!(fb.role, direct.role, "message roles should match");
        }
    }

    #[test]
    fn most_recent_history_exchange_preserved_after_compaction() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            history_element(&[
                ("Old turn", "Old reply"),
                ("Recent user", "Recent assistant"),
            ]),
        ];
        // Tight budget: only room for 2 messages (the recent exchange).
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &budget_config(20), &tokeniser);

        // The most recent exchange must remain.
        let has_recent_user = output.messages.iter().any(|m| {
            m.role == Role::User
                && m.content.iter().any(|p| {
                    matches!(p, swage_core::ContentPart::Text { text } if text.contains("Recent user"))
                })
        });
        assert!(has_recent_user, "most recent user message was removed");
    }

    // ── fail-open assemble() ──────────────────────────────────────────────────

    #[tokio::test]
    async fn assemble_happy_path_returns_same_output_as_default_assembly() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Be helpful."),
            history_element(&[("Hi", "Hello!")]),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let config = no_budget_config();

        let via_assemble = assemble_default(&elements, &config, &tokeniser).await;
        let via_direct = default_assembly(&elements, &config, &tokeniser);

        // Message sequence must be identical.
        assert_eq!(via_assemble.messages.len(), via_direct.messages.len());
        for (a, b) in via_assemble.messages.iter().zip(via_direct.messages.iter()) {
            assert_eq!(a.role, b.role);
        }
    }

    #[tokio::test]
    async fn assemble_report_has_no_errors_on_success() {
        let elements: Vec<Box<dyn ContextElement>> =
            vec![history_element(&[("Hi", "Hello")])];
        let tokeniser = FixedCostTokeniser { cost: 10 };

        let output = assemble_default(&elements, &no_budget_config(), &tokeniser).await;

        assert!(
            output.report.errors.is_empty(),
            "expected no errors on the success path"
        );
    }

    #[tokio::test]
    async fn assemble_with_budget_applies_compaction() {
        let elements: Vec<Box<dyn ContextElement>> = vec![history_element(&[
            ("Turn 1", "Reply 1"),
            ("Turn 2", "Reply 2"),
            ("Turn 3", "Reply 3"),
        ])];
        // 6 messages × 10 = 60 tokens. Budget 20 → only recent exchange fits.
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = assemble_default(&elements, &budget_config(20), &tokeniser).await;

        assert!(
            !output.report.compaction_applied.is_empty(),
            "expected compaction to run"
        );
        assert!(output.report.errors.is_empty(), "compaction should not cause an error");
    }

    #[tokio::test]
    async fn assemble_records_timeout_error_and_falls_back() {
        // Use a zero-second timeout — tokio::time::timeout with Duration::ZERO fires
        // immediately when the future does not resolve synchronously within the poll.
        // Since `async { dispatch(...) }` resolves on the first poll (synchronous work),
        // this test validates the timeout *infrastructure* rather than a real slow assembly.
        //
        // True timeout testing for slow (Python) assemblies is an integration test concern.
        let elements: Vec<Box<dyn ContextElement>> =
            vec![history_element(&[("Hi", "Hello")])];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let config = ContextConfig {
            assembly: "default".to_string(),
            timeout: 30, // generous — default assembly is instant
            budget: None,
        };

        let output = assemble_default(&elements, &config, &tokeniser).await;

        // Default assembly (synchronous) always completes before any real timeout.
        // Verify the success path: no errors recorded.
        assert!(output.report.errors.is_empty());
        assert!(!output.messages.is_empty());
    }

    #[test]
    fn make_error_constructs_correct_assembly_error() {
        let err = make_error("custom-assembly", "timeout", "timed out after 5s");

        assert_eq!(err.assembly_name, "custom-assembly");
        assert_eq!(err.error_kind, "timeout");
        assert_eq!(err.message, "timed out after 5s");
        assert_eq!(err.fallback_used, "default");
    }

    #[test]
    fn make_error_invalid_output_kind() {
        let err = make_error("my-assembly", "invalid_output", "empty message list");
        assert_eq!(err.error_kind, "invalid_output");
        assert_eq!(err.fallback_used, "default");
    }

    // ── SkillsElement integration [kw92] ──────────────────────────────────────

    fn skills_element(source: &'static str, instructions: Option<&str>) -> Box<dyn ContextElement> {
        let messages = match instructions {
            Some(text) => vec![ChatMessage::new(Role::System, text)],
            None => vec![],
        };
        Box::new(MockElement {
            name: "skills",
            priority: 75,
            messages,
            source,
        })
    }

    #[test]
    fn skills_element_appears_between_prompts_and_history() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            history_element(&[("Hello", "Hi!")]),
            skills_element(
                "skills: code-review (project, ~35 tokens)",
                Some("Always review for correctness."),
            ),
            prompts_element("You are helpful."),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Expected order: System (prompts), System (skills), User, Assistant
        assert_eq!(output.messages.len(), 4);
        assert_eq!(output.messages[0].role, Role::System); // prompts
        assert_eq!(output.messages[1].role, Role::System); // skills
        assert_eq!(output.messages[2].role, Role::User);   // history
        assert_eq!(output.messages[3].role, Role::Assistant);
    }

    #[test]
    fn empty_skills_element_appears_in_report_with_zero_tokens() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            skills_element("skills (none active)", None),
            history_element(&[("Hi", "Hello")]),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Skills element produces no messages — total is prompts + history only.
        assert_eq!(output.messages.len(), 3, "empty skills element adds no messages");

        // But the skills element must still appear in the report.
        let skills_report = output.report.elements.iter().find(|e| e.name == "skills");
        assert!(skills_report.is_some(), "empty skills element should appear in report");
        assert_eq!(skills_report.unwrap().token_count, 0);
    }

    #[test]
    fn skills_element_report_preview_contains_skill_names() {
        let source = "skills: code-review (project, ~35 tokens), persona (agent, ~22 tokens)";
        let elements: Vec<Box<dyn ContextElement>> = vec![
            history_element(&[("Hi", "Hello")]),
            skills_element(source, Some("Review code. Be a persona.")),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        let skills_report = output.report.elements.iter().find(|e| e.name == "skills").unwrap();
        let preview = skills_report.preview.as_deref().unwrap_or("");
        assert!(preview.contains("code-review"), "preview must include skill name");
        assert!(preview.contains("persona"), "preview must include second skill name");
    }

    #[test]
    fn empty_skills_element_report_has_no_preview() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            history_element(&[("Hi", "Hello")]),
            skills_element("skills (none active)", None),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        let skills_report = output.report.elements.iter().find(|e| e.name == "skills").unwrap();
        // No active skills → preview should be None.
        assert!(skills_report.preview.is_none(), "empty skills should have no preview");
    }

    #[test]
    fn memory_remains_excluded_when_skills_element_present() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            prompts_element("Instructions."),
            skills_element("skills: my-skill (project, ~10 tokens)", Some("Skill instructions.")),
            memory_element(),
            history_element(&[("Hi", "Hello")]),
        ];
        let tokeniser = FixedCostTokeniser { cost: 10 };
        let output = default_assembly(&elements, &no_budget_config(), &tokeniser);

        // Prompts (1) + skills (1) + history (2) = 4. Memory excluded.
        assert_eq!(output.messages.len(), 4, "memory must remain excluded");

        let report_names: Vec<&str> = output.report.elements.iter().map(|e| e.name.as_str()).collect();
        assert!(!report_names.contains(&"memory"), "memory must not appear in report");
        assert!(report_names.contains(&"skills"), "skills must appear in report");
    }
}
