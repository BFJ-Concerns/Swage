//! Assembly context PyO3 bindings — the Python-side interface for
//! programmable context assembly.
//!
//! `AssemblyContext` is the object passed to Python `assemble(ctx)` functions.
//! It tracks budget, accumulates messages via `add(element)`, and records
//! element reports and pending compaction intents for the Rust assembly engine
//! to consume after Python returns.

use std::ffi::CString;
use std::path::PathBuf;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use swage_config::{HierarchyLevel, resolve_assembly};
use swage_context::compaction::PendingCompaction;
use swage_context::report::{ElementReport, NamedAssemblyReport};
use swage_core::ChatMessage;

// ─────────────────────────────────────────────────────────────────────────────
// AssemblyData — Rust-side element data passed at construction
// ─────────────────────────────────────────────────────────────────────────────

/// Pre-computed element data from the Rust assembly engine, passed to
/// `AssemblyContext` at construction time. Each field carries messages and
/// pre-counted token costs so the Python side doesn't need a tokeniser.
pub struct AssemblyData {
    /// System instruction messages from prompt rules.
    pub prompts_messages: Vec<ChatMessage>,
    pub prompts_token_count: usize,
    pub prompts_source: String,

    /// Skill instruction messages.
    pub skills_messages: Vec<ChatMessage>,
    pub skills_token_count: usize,
    pub skills_source: String,

    /// Agent memory messages.
    pub memory_messages: Vec<ChatMessage>,
    pub memory_token_count: usize,
    pub memory_source: String,

    /// Full conversation history messages (chronological order).
    pub history_messages: Vec<ChatMessage>,
    pub history_token_count: usize,
    pub history_source: String,

    /// Per-message token counts for history, enabling partial selection
    /// without re-counting. Parallel to `history_messages`.
    pub history_per_message_tokens: Vec<usize>,

    /// Hierarchy search levels for resolving named assemblies via
    /// `ctx.include()`. Ordered highest to lowest priority
    /// (e.g. Agent → Project → Global).
    pub search_levels: Vec<(HierarchyLevel, PathBuf)>,
}

// ─────────────────────────────────────────────────────────────────────────────
// PyElement — read-only wrapper for element data
// ─────────────────────────────────────────────────────────────────────────────

/// A read-only wrapper around element data, used by `AssemblyContext.add()`.
///
/// Exposes token count, message count, source provenance, and an optional
/// content preview for inspection.
#[pyclass]
#[derive(Debug, Clone)]
pub struct PyElement {
    /// Element type name (e.g. "history", "prompts", "skills").
    pub(crate) name: String,

    /// Provenance/source description.
    pub(crate) source: String,

    /// The messages this element contributes.
    pub(crate) messages: Vec<ChatMessage>,

    /// Token count for this element's messages.
    pub(crate) token_count: usize,

    /// Ordering priority.
    pub(crate) priority: i32,

    /// Truncated content preview for inspection. Built from the first
    /// message's text content, capped at 100 characters.
    pub(crate) content_preview: Option<String>,
}

impl PyElement {
    /// Construct a PyElement from Rust-side data.
    pub fn new(
        name: String,
        source: String,
        messages: Vec<ChatMessage>,
        token_count: usize,
        priority: i32,
    ) -> Self {
        let content_preview = build_preview(&messages);
        Self {
            name,
            source,
            messages,
            token_count,
            priority,
            content_preview,
        }
    }
}

#[pymethods]
impl PyElement {
    /// Element type name (e.g. "history", "prompts").
    #[getter]
    fn element_name(&self) -> &str {
        &self.name
    }

    /// Number of messages in this element.
    #[getter]
    fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Estimated token count.
    #[getter]
    fn token_count(&self) -> usize {
        self.token_count
    }

    /// Source/provenance description.
    #[getter]
    fn source(&self) -> &str {
        &self.source
    }

    /// Truncated content preview (first 100 characters of first message),
    /// or `None` when the element has no text content.
    #[getter]
    fn content_preview(&self) -> Option<&str> {
        self.content_preview.as_deref()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Preview helper
// ─────────────────────────────────────────────────────────────────────────────

/// Build a preview string from the first text content part of the first
/// message, truncated to 100 characters.
fn build_preview(messages: &[ChatMessage]) -> Option<String> {
    const MAX_PREVIEW_LEN: usize = 100;

    let first_text = messages.iter().find_map(|m| {
        m.content.iter().find_map(|part| match part {
            swage_core::ContentPart::Text { text } if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
    })?;

    if first_text.len() <= MAX_PREVIEW_LEN {
        Some(first_text.to_string())
    } else {
        // Truncate at a char boundary near the limit.
        let truncated = &first_text[..first_text.floor_char_boundary(MAX_PREVIEW_LEN)];
        Some(format!("{truncated}…"))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// AssemblyContext
// ─────────────────────────────────────────────────────────────────────────────

/// The context object passed to Python `assemble(ctx)` functions.
///
/// Tracks the token budget, accumulates messages from `add()` calls, and
/// records element reports and pending compaction intents. After Python
/// returns, the Rust assembly engine extracts results via `into_output()`.
#[pyclass]
pub struct AssemblyContext {
    /// Total token budget for the assembled context.
    total_budget: usize,

    /// Budget remaining after elements added so far.
    remaining_budget: usize,

    /// Model identifier — available to Python for conditional assembly logic.
    model: String,

    /// Agent identifier — scopes the assembly to a specific agent.
    agent_id: String,

    /// Accumulated messages in add() order.
    messages: Vec<ChatMessage>,

    /// Per-element reports for each add() call.
    element_reports: Vec<ElementReport>,

    /// Deferred compaction intents (e.g. from history_older with a strategy).
    pending_compactions: Vec<PendingCompaction>,

    /// Named assembly include stack for cycle detection.
    include_stack: Vec<String>,

    /// Non-fatal warnings accumulated during assembly.
    warnings: Vec<String>,

    /// Hierarchy search levels for resolving named assemblies.
    search_levels: Vec<(HierarchyLevel, PathBuf)>,

    /// Pre-built element data from Rust, used by accessor methods.
    assembly_data: Option<AssemblyData>,
}

impl AssemblyContext {
    /// Construct a new context from Rust. Not exposed to Python — Python
    /// receives a pre-built context from the execution entrypoint.
    pub fn new(budget: usize, model: String, agent_id: String) -> Self {
        Self {
            total_budget: budget,
            remaining_budget: budget,
            model,
            agent_id,
            messages: Vec::new(),
            element_reports: Vec::new(),
            pending_compactions: Vec::new(),
            include_stack: Vec::new(),
            warnings: Vec::new(),
            search_levels: Vec::new(),
            assembly_data: None,
        }
    }

    /// Construct a context with pre-computed element data from the assembly
    /// engine. This is the primary constructor used at runtime.
    pub fn with_data(
        budget: usize,
        model: String,
        agent_id: String,
        data: AssemblyData,
    ) -> Self {
        let search_levels = data.search_levels.clone();
        Self {
            search_levels,
            assembly_data: Some(data),
            ..Self::new(budget, model, agent_id)
        }
    }

    /// Extract accumulated output for the Rust assembly engine.
    ///
    /// Consumes the context and returns the messages, element reports,
    /// pending compaction intents, and warnings.
    pub fn into_output(
        self,
    ) -> (
        Vec<ChatMessage>,
        Vec<ElementReport>,
        Vec<PendingCompaction>,
        Vec<String>,
    ) {
        (
            self.messages,
            self.element_reports,
            self.pending_compactions,
            self.warnings,
        )
    }

    /// Extract accumulated output by moving fields out, leaving defaults.
    ///
    /// Used after Python returns to retrieve results from a borrowed
    /// `PyRefMut<AssemblyContext>` without consuming `self`.
    pub fn take_output(
        &mut self,
    ) -> (
        Vec<ChatMessage>,
        Vec<ElementReport>,
        Vec<PendingCompaction>,
        Vec<String>,
    ) {
        (
            std::mem::take(&mut self.messages),
            std::mem::take(&mut self.element_reports),
            std::mem::take(&mut self.pending_compactions),
            std::mem::take(&mut self.warnings),
        )
    }

    /// Mutable access to the include stack — used by the include() mechanism
    /// for cycle detection.
    pub fn include_stack_mut(&mut self) -> &mut Vec<String> {
        &mut self.include_stack
    }

    /// Read-only access to the include stack.
    pub fn include_stack(&self) -> &[String] {
        &self.include_stack
    }

    /// Record a warning.
    pub fn add_warning(&mut self, warning: String) {
        self.warnings.push(warning);
    }

    /// Record a pending compaction intent.
    pub fn add_pending_compaction(&mut self, compaction: PendingCompaction) {
        self.pending_compactions.push(compaction);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Include wrapper template
// ─────────────────────────────────────────────────────────────────────────────

/// Python wrapper for `ctx.include()`. Compiles and executes the included
/// assembly file in an isolated namespace, then calls its `assemble(ctx)`.
///
/// Re-raises `KeyboardInterrupt` so the outer timeout mechanism still works.
/// All other exceptions are captured into `_inc_err` for the Rust side.
///
/// Injected locals:
/// - `_inc_code`: included file source code (str)
/// - `_inc_path`: file path for tracebacks (str)
/// - `_inc_ctx`: the AssemblyContext (same object as the outer assembly)
///
/// Output locals:
/// - `_inc_err`: error message (None on success)
const INCLUDE_WRAPPER: &str = concat!(
    "import traceback as _tb\n",
    "_inc_err = None\n",
    "_inc_ns = {}\n",
    "\n",
    "try:\n",
    "    ", "exec", "(compile(_inc_code, _inc_path, '", "exec", "'), _inc_ns)\n",
    "except BaseException:\n",
    "    _inc_err = _tb.format_exc()\n",
    "\n",
    "if _inc_err is None:\n",
    "    if 'assemble' not in _inc_ns:\n",
    "        _inc_err = 'included assembly does not define an assemble(ctx) function'\n",
    "    else:\n",
    "        try:\n",
    "            _inc_ns['assemble'](_inc_ctx)\n",
    "        except KeyboardInterrupt:\n",
    "            raise\n",
    "        except BaseException:\n",
    "            _inc_err = _tb.format_exc()\n",
);

#[pymethods]
impl AssemblyContext {
    /// Total token budget for the assembly.
    #[getter]
    fn total_budget(&self) -> usize {
        self.total_budget
    }

    /// Remaining token budget after elements added so far.
    #[getter]
    fn remaining_budget(&self) -> usize {
        self.remaining_budget
    }

    /// Model identifier (e.g. "gpt-4", "claude-3-opus").
    #[getter]
    fn model(&self) -> &str {
        &self.model
    }

    /// Agent identifier.
    #[getter]
    fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Add an element to the assembled context.
    ///
    /// Accumulates the element's messages, decrements `remaining_budget` by
    /// the element's token count, and records an `ElementReport`. If the
    /// element's token count exceeds the remaining budget, a warning is
    /// recorded but the element is still added (fail-open: the engine can
    /// truncate later).
    fn add(&mut self, element: &PyElement) -> PyResult<()> {
        // Warn when adding an element that exceeds the remaining budget,
        // but still include it — fail-open so assembly can always proceed.
        if element.token_count > self.remaining_budget {
            self.warnings.push(format!(
                "element '{}' uses {} tokens but only {} remain",
                element.name, element.token_count, self.remaining_budget
            ));
        }

        // Accumulate messages.
        self.messages.extend(element.messages.clone());

        // Decrement budget (saturating to avoid underflow).
        self.remaining_budget = self.remaining_budget.saturating_sub(element.token_count);

        // Record element report.
        self.element_reports.push(ElementReport {
            name: element.name.clone(),
            source: element.source.clone(),
            token_count: element.token_count,
            preview: element.content_preview.clone(),
            priority: element.priority,
            named_assembly: None,
        });

        Ok(())
    }

    /// Number of messages accumulated so far.
    #[getter]
    fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Number of elements added so far.
    #[getter]
    fn element_count(&self) -> usize {
        self.element_reports.len()
    }

    // ── Element accessor methods ─────────────────────────────────────────────

    /// Get the prompts element (system instructions from prompt rules).
    fn prompts(&self) -> PyResult<PyElement> {
        let data = self.require_data("prompts")?;
        Ok(PyElement::new(
            "prompts".to_string(),
            data.prompts_source.clone(),
            data.prompts_messages.clone(),
            data.prompts_token_count,
            100, // prompts have highest priority
        ))
    }

    /// Get the skills element (active skill instructions).
    fn skills(&self) -> PyResult<PyElement> {
        let data = self.require_data("skills")?;
        Ok(PyElement::new(
            "skills".to_string(),
            data.skills_source.clone(),
            data.skills_messages.clone(),
            data.skills_token_count,
            75, // skills sit between prompts and history
        ))
    }

    /// Get the memory element (agent memory content).
    fn memory(&self) -> PyResult<PyElement> {
        let data = self.require_data("memory")?;
        Ok(PyElement::new(
            "memory".to_string(),
            data.memory_source.clone(),
            data.memory_messages.clone(),
            data.memory_token_count,
            50, // memory sits between skills and history
        ))
    }

    /// Get recent history messages, optionally limited by token budget or
    /// message count. Takes the most recent messages first.
    ///
    /// - `budget`: maximum token count for the returned element
    /// - `count`: maximum number of messages to include
    ///
    /// When both are specified, the tighter constraint wins.
    #[pyo3(signature = (budget=None, count=None))]
    fn history_recent(
        &self,
        budget: Option<usize>,
        count: Option<usize>,
    ) -> PyResult<PyElement> {
        let data = self.require_data("history_recent")?;

        let messages = &data.history_messages;
        let per_msg_tokens = &data.history_per_message_tokens;

        // Select from the end (most recent) backwards.
        let mut selected_messages: Vec<ChatMessage> = Vec::new();
        let mut selected_tokens: usize = 0;

        for (msg, &msg_tokens) in messages.iter().zip(per_msg_tokens).rev() {
            // Check count limit.
            if let Some(max_count) = count {
                if selected_messages.len() >= max_count {
                    break;
                }
            }

            // Check budget limit.
            if let Some(max_budget) = budget {
                if selected_tokens + msg_tokens > max_budget && !selected_messages.is_empty() {
                    break;
                }
            }

            selected_tokens += msg_tokens;
            selected_messages.push(msg.clone());
        }

        // Reverse to restore chronological order.
        selected_messages.reverse();

        Ok(PyElement::new(
            "history".to_string(),
            format!("{} (recent)", data.history_source),
            selected_messages,
            selected_tokens,
            0, // history has lowest priority
        ))
    }

    /// Get older history messages (everything except the most recent exchange).
    ///
    /// - `budget`: maximum token count for the returned element
    /// - `strategy`: when specified, records a `PendingCompaction` intent
    ///   instead of blocking — the assembly engine will compact this element
    ///   asynchronously after Python returns
    #[pyo3(signature = (budget=None, strategy=None))]
    fn history_older(
        &mut self,
        budget: Option<usize>,
        strategy: Option<String>,
    ) -> PyResult<PyElement> {
        let data = self.require_data("history_older")?;

        let history_source = data.history_source.clone();
        let messages = &data.history_messages;
        let per_msg_tokens = &data.history_per_message_tokens;

        // "Older" = everything except the last two messages (the most recent
        // user–assistant exchange). If fewer than 3 messages exist, there is
        // no older history.
        let older_end = messages.len().saturating_sub(2);
        let older_messages: Vec<ChatMessage> = messages[..older_end].to_vec();
        let older_tokens: usize = per_msg_tokens[..older_end].iter().sum();

        // Apply budget limit if specified.
        let (final_messages, final_tokens) = if let Some(max_budget) = budget {
            if older_tokens <= max_budget {
                (older_messages, older_tokens)
            } else {
                // Take from the end of the older range (most recent older messages).
                let mut selected = Vec::new();
                let mut token_sum = 0usize;
                for (msg, &tok) in older_messages.iter().zip(&per_msg_tokens[..older_end]).rev() {
                    if token_sum + tok > max_budget && !selected.is_empty() {
                        break;
                    }
                    token_sum += tok;
                    selected.push(msg.clone());
                }
                selected.reverse();
                (selected, token_sum)
            }
        } else {
            (older_messages, older_tokens)
        };

        // When a compaction strategy is specified, record a PendingCompaction
        // intent. The assembly engine will execute it after Python returns.
        if let Some(strategy_name) = strategy {
            let effective_budget = budget.unwrap_or(self.remaining_budget);
            self.pending_compactions.push(PendingCompaction {
                element_name: "history".to_string(),
                strategy: strategy_name,
                budget: effective_budget,
                messages: final_messages.clone(),
                source_range: if older_end > 0 {
                    Some((0, older_end))
                } else {
                    None
                },
            });
        }

        Ok(PyElement::new(
            "history".to_string(),
            format!("{} (older)", history_source),
            final_messages,
            final_tokens,
            0,
        ))
    }

    // ── Named assembly include ───────────────────────────────────────────────

    /// Include a named assembly, executing its `assemble(ctx)` within this
    /// same context (shared budget, shared messages).
    ///
    /// Uses three borrow phases: (1) borrow for cycle check, resolution, and
    /// stack push, (2) release borrow while Python executes (so `add()` etc.
    /// can re-borrow), (3) borrow for stack pop and report creation.
    ///
    /// Elements added during the include are grouped into a single
    /// `ElementReport` with a nested `NamedAssemblyReport` for provenance.
    fn include(slf: &Bound<'_, Self>, name: String) -> PyResult<()> {
        let py = slf.py();

        // Phase 1: Borrow to check cycles, resolve path, read file.
        let include_info = {
            let mut ctx = slf.borrow_mut();

            // Cycle detection — if the name is already in the stack, we have
            // a circular include. Record a warning and bail out.
            if ctx.include_stack.contains(&name) {
                let chain = ctx.include_stack.join(" \u{2192} "); // →
                ctx.warnings.push(format!("circular reference: {chain} \u{2192} {name}"));
                return Ok(());
            }

            // Resolve the assembly through the config hierarchy.
            let levels_ref: Vec<(HierarchyLevel, &std::path::Path)> = ctx
                .search_levels
                .iter()
                .map(|(l, p)| (l.clone(), p.as_path()))
                .collect();

            let resolved = match resolve_assembly(&name, &levels_ref) {
                Some(r) => r,
                None => {
                    ctx.warnings.push(format!("named assembly '{name}' not found"));
                    return Ok(());
                }
            };

            // Read the included assembly file from disk.
            let code = match std::fs::read_to_string(&resolved.path) {
                Ok(c) => c,
                Err(e) => {
                    ctx.warnings.push(format!(
                        "failed to read assembly '{}': {e}",
                        resolved.path.display()
                    ));
                    return Ok(());
                }
            };

            // Push onto the include stack for cycle detection in nested includes.
            ctx.include_stack.push(name.clone());

            let report_snapshot = ctx.element_reports.len();
            let level_str = resolved.level.to_string().to_lowercase();
            let path_str = resolved.path.display().to_string();

            (code, path_str, level_str, report_snapshot)
        }; // Mutable borrow released here.

        let (code, path_str, resolved_level, report_snapshot) = include_info;

        // Phase 2: Execute the included assembly in Python.
        // The borrow is released, so Python code can call ctx.add(), etc.
        let locals = PyDict::new(py);
        locals.set_item("_inc_code", &code)?;
        locals.set_item("_inc_path", &path_str)?;
        locals.set_item("_inc_ctx", slf)?;

        let wrapper = CString::new(INCLUDE_WRAPPER)
            .expect("include wrapper template should not contain NUL bytes");
        let wrapper_cstr = std::ffi::CStr::from_bytes_with_nul(wrapper.as_bytes_with_nul())
            .expect("CString should produce valid CStr");

        let wrapper_err = py.run(wrapper_cstr, None, Some(&locals)).err();

        // Check for errors from the include wrapper.
        let inc_err: Option<String> = if wrapper_err.is_some() {
            // Wrapper itself failed (should never happen).
            wrapper_err.map(|e| format!("internal include error: {e}"))
        } else {
            locals
                .get_item("_inc_err")
                .ok()
                .flatten()
                .and_then(|v| v.extract::<String>().ok())
        };

        // Phase 3: Borrow to pop the stack and create the nested report.
        {
            let mut ctx = slf.borrow_mut();
            ctx.include_stack.pop();

            if let Some(err) = inc_err {
                ctx.warnings.push(format!("error in named assembly '{name}': {err}"));
            }

            // Group elements added during the include into a NamedAssemblyReport.
            // Drain the child elements from the report list and wrap them.
            let child_elements: Vec<ElementReport> =
                ctx.element_reports.drain(report_snapshot..).collect();

            if !child_elements.is_empty() {
                let total_tokens: usize =
                    child_elements.iter().map(|e| e.token_count).sum();

                ctx.element_reports.push(ElementReport {
                    name: format!("assembly:{name}"),
                    source: format!("named assembly ({resolved_level})"),
                    token_count: total_tokens,
                    preview: None,
                    priority: 0,
                    named_assembly: Some(NamedAssemblyReport {
                        assembly_name: name,
                        resolved_level,
                        child_elements,
                    }),
                });
            }
        }

        Ok(())
    }
}

/// Helper: require assembly_data, returning a clear error if missing.
impl AssemblyContext {
    fn require_data(&self, accessor: &str) -> PyResult<&AssemblyData> {
        self.assembly_data.as_ref().ok_or_else(|| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "{accessor} requires AssemblyData (context not initialised with element data)",
            ))
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module registration
// ─────────────────────────────────────────────────────────────────────────────

/// Register `AssemblyContext` and `PyElement` into a Python module.
///
/// Called by the assembly execution entrypoint (task [s7xz]) when building
/// the `swage_context` namespace module.
pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<AssemblyContext>()?;
    module.add_class::<PyElement>()?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use swage_core::Role;

    fn make_element(name: &str, token_count: usize) -> PyElement {
        PyElement::new(
            name.to_string(),
            format!("{name} source"),
            vec![ChatMessage::new(Role::System, &format!("{name} content"))],
            token_count,
            0,
        )
    }

    fn sample_assembly_data() -> AssemblyData {
        AssemblyData {
            prompts_messages: vec![ChatMessage::new(Role::System, "You are helpful.")],
            prompts_token_count: 10,
            prompts_source: "prompt rules".to_string(),
            skills_messages: vec![ChatMessage::new(Role::System, "Review code carefully.")],
            skills_token_count: 8,
            skills_source: "skills: code-review (project)".to_string(),
            memory_messages: vec![ChatMessage::new(Role::User, "Remember: user prefers Rust.")],
            memory_token_count: 12,
            memory_source: "agent memory".to_string(),
            history_messages: vec![
                ChatMessage::new(Role::User, "Old question"),
                ChatMessage::new(Role::Assistant, "Old answer"),
                ChatMessage::new(Role::User, "Middle question"),
                ChatMessage::new(Role::Assistant, "Middle answer"),
                ChatMessage::new(Role::User, "Recent question"),
                ChatMessage::new(Role::Assistant, "Recent answer"),
            ],
            history_token_count: 60,
            history_source: "thread-123".to_string(),
            history_per_message_tokens: vec![10, 10, 10, 10, 10, 10],
            search_levels: Vec::new(),
        }
    }

    // ── Construction ─────────────────────────────────────────────────────────

    #[test]
    fn new_creates_context_with_full_budget() {
        let ctx = AssemblyContext::new(1000, "gpt-4".to_string(), "agent-1".to_string());
        assert_eq!(ctx.total_budget, 1000);
        assert_eq!(ctx.remaining_budget, 1000);
        assert_eq!(ctx.model, "gpt-4");
        assert_eq!(ctx.agent_id, "agent-1");
        assert!(ctx.messages.is_empty());
        assert!(ctx.element_reports.is_empty());
        assert!(ctx.pending_compactions.is_empty());
        assert!(ctx.include_stack.is_empty());
        assert!(ctx.warnings.is_empty());
    }

    #[test]
    fn with_data_stores_assembly_data() {
        let ctx = AssemblyContext::with_data(
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            sample_assembly_data(),
        );
        assert!(ctx.assembly_data.is_some());
    }

    // ── add() accumulates messages and decrements budget ─────────────────────

    #[test]
    fn add_accumulates_messages_and_decrements_budget() {
        let mut ctx = AssemblyContext::new(1000, "gpt-4".to_string(), "agent-1".to_string());
        let element = make_element("prompts", 100);

        ctx.add(&element).unwrap();

        assert_eq!(ctx.messages.len(), 1);
        assert_eq!(ctx.remaining_budget, 900);
        assert_eq!(ctx.element_reports.len(), 1);
        assert_eq!(ctx.element_reports[0].name, "prompts");
        assert_eq!(ctx.element_reports[0].token_count, 100);
    }

    #[test]
    fn multiple_adds_accumulate_correctly() {
        let mut ctx = AssemblyContext::new(1000, "gpt-4".to_string(), "agent-1".to_string());

        ctx.add(&make_element("prompts", 200)).unwrap();
        ctx.add(&make_element("history", 300)).unwrap();
        ctx.add(&make_element("skills", 50)).unwrap();

        assert_eq!(ctx.messages.len(), 3);
        assert_eq!(ctx.remaining_budget, 450);
        assert_eq!(ctx.element_reports.len(), 3);
    }

    // ── Budget saturation ────────────────────────────────────────────────────

    #[test]
    fn add_saturates_budget_at_zero() {
        let mut ctx = AssemblyContext::new(100, "gpt-4".to_string(), "agent-1".to_string());
        let element = make_element("history", 150);

        // Adding more tokens than budget doesn't underflow.
        ctx.add(&element).unwrap();
        assert_eq!(ctx.remaining_budget, 0);
    }

    #[test]
    fn add_beyond_budget_records_warning() {
        let mut ctx = AssemblyContext::new(100, "gpt-4".to_string(), "agent-1".to_string());
        let element = make_element("history", 150);

        ctx.add(&element).unwrap();

        assert!(!ctx.warnings.is_empty(), "expected budget-exceeded warning");
        assert!(ctx.warnings[0].contains("history"));
    }

    // ── into_output() extracts accumulated data ──────────────────────────────

    #[test]
    fn into_output_returns_accumulated_data() {
        let mut ctx = AssemblyContext::new(1000, "gpt-4".to_string(), "agent-1".to_string());
        ctx.add(&make_element("prompts", 100)).unwrap();
        ctx.add(&make_element("history", 200)).unwrap();
        ctx.add_warning("test warning".to_string());

        let compaction = PendingCompaction {
            element_name: "history".to_string(),
            strategy: "structured_extract".to_string(),
            budget: 500,
            messages: vec![],
            source_range: None,
        };
        ctx.add_pending_compaction(compaction);

        let (messages, reports, compactions, warnings) = ctx.into_output();

        assert_eq!(messages.len(), 2);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].name, "prompts");
        assert_eq!(reports[1].name, "history");
        assert_eq!(compactions.len(), 1);
        assert_eq!(compactions[0].strategy, "structured_extract");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0], "test warning");
    }

    // ── Include stack operations ─────────────────────────────────────────────

    #[test]
    fn include_stack_push_and_check() {
        let mut ctx = AssemblyContext::new(1000, "gpt-4".to_string(), "agent-1".to_string());

        assert!(ctx.include_stack().is_empty());

        ctx.include_stack_mut().push("alpha".to_string());
        assert_eq!(ctx.include_stack(), &["alpha"]);

        ctx.include_stack_mut().push("beta".to_string());
        assert_eq!(ctx.include_stack(), &["alpha", "beta"]);

        ctx.include_stack_mut().pop();
        assert_eq!(ctx.include_stack(), &["alpha"]);
    }

    // ── Element report fields ────────────────────────────────────────────────

    #[test]
    fn element_report_has_correct_fields() {
        let mut ctx = AssemblyContext::new(1000, "gpt-4".to_string(), "agent-1".to_string());
        let element = PyElement::new(
            "prompts".to_string(),
            "prompt rules".to_string(),
            vec![ChatMessage::new(Role::System, "Be helpful.")],
            42,
            100,
        );

        ctx.add(&element).unwrap();

        let report = &ctx.element_reports[0];
        assert_eq!(report.name, "prompts");
        assert_eq!(report.source, "prompt rules");
        assert_eq!(report.token_count, 42);
        assert_eq!(report.priority, 100);
        assert!(report.named_assembly.is_none());
    }

    // ── PyElement getters ────────────────────────────────────────────────────

    #[test]
    fn py_element_getters_return_correct_values() {
        let element = PyElement::new(
            "history".to_string(),
            "thread-abc".to_string(),
            vec![
                ChatMessage::new(Role::User, "Hello"),
                ChatMessage::new(Role::Assistant, "Hi"),
            ],
            50,
            0,
        );

        assert_eq!(element.element_name(), "history");
        assert_eq!(element.source(), "thread-abc");
        assert_eq!(element.token_count(), 50);
        assert_eq!(element.message_count(), 2);
    }

    // ── content_preview ──────────────────────────────────────────────────────

    #[test]
    fn content_preview_shows_first_message_text() {
        let element = PyElement::new(
            "prompts".to_string(),
            "source".to_string(),
            vec![ChatMessage::new(Role::System, "Be helpful and concise.")],
            10,
            100,
        );
        assert_eq!(
            element.content_preview(),
            Some("Be helpful and concise.")
        );
    }

    #[test]
    fn content_preview_truncates_long_text() {
        let long_text = "a".repeat(200);
        let element = PyElement::new(
            "history".to_string(),
            "source".to_string(),
            vec![ChatMessage::new(Role::User, &long_text)],
            50,
            0,
        );
        let preview = element.content_preview().unwrap();
        // Truncated to ~100 chars + ellipsis.
        assert!(preview.len() <= 104); // 100 + "…" (3 bytes UTF-8)
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn content_preview_none_for_empty_messages() {
        let element = PyElement::new(
            "memory".to_string(),
            "source".to_string(),
            vec![],
            0,
            50,
        );
        assert!(element.content_preview().is_none());
    }

    // ── Accessor methods ─────────────────────────────────────────────────────

    #[test]
    fn prompts_accessor_returns_correct_element() {
        let ctx = AssemblyContext::with_data(
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            sample_assembly_data(),
        );

        let element = ctx.prompts().unwrap();
        assert_eq!(element.element_name(), "prompts");
        assert_eq!(element.token_count(), 10);
        assert_eq!(element.message_count(), 1);
        assert_eq!(element.source(), "prompt rules");
    }

    #[test]
    fn skills_accessor_returns_correct_element() {
        let ctx = AssemblyContext::with_data(
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            sample_assembly_data(),
        );

        let element = ctx.skills().unwrap();
        assert_eq!(element.element_name(), "skills");
        assert_eq!(element.token_count(), 8);
        assert_eq!(element.message_count(), 1);
    }

    #[test]
    fn memory_accessor_returns_correct_element() {
        let ctx = AssemblyContext::with_data(
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            sample_assembly_data(),
        );

        let element = ctx.memory().unwrap();
        assert_eq!(element.element_name(), "memory");
        assert_eq!(element.token_count(), 12);
    }

    #[test]
    fn accessor_without_data_returns_error() {
        let ctx = AssemblyContext::new(1000, "gpt-4".to_string(), "agent-1".to_string());

        assert!(ctx.prompts().is_err());
        assert!(ctx.skills().is_err());
        assert!(ctx.memory().is_err());
    }

    // ── history_recent ───────────────────────────────────────────────────────

    #[test]
    fn history_recent_returns_all_when_no_limits() {
        let ctx = AssemblyContext::with_data(
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            sample_assembly_data(),
        );

        let element = ctx.history_recent(None, None).unwrap();
        assert_eq!(element.message_count(), 6);
        assert_eq!(element.token_count(), 60);
    }

    #[test]
    fn history_recent_respects_count_limit() {
        let ctx = AssemblyContext::with_data(
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            sample_assembly_data(),
        );

        let element = ctx.history_recent(None, Some(2)).unwrap();
        assert_eq!(element.message_count(), 2);
        assert_eq!(element.token_count(), 20);
    }

    #[test]
    fn history_recent_respects_budget_limit() {
        let ctx = AssemblyContext::with_data(
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            sample_assembly_data(),
        );

        // Budget of 25: can fit 2 messages (10 each = 20), third would exceed.
        let element = ctx.history_recent(Some(25), None).unwrap();
        assert_eq!(element.message_count(), 2);
        assert_eq!(element.token_count(), 20);
    }

    // ── history_older ────────────────────────────────────────────────────────

    #[test]
    fn history_older_excludes_last_exchange() {
        let mut ctx = AssemblyContext::with_data(
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            sample_assembly_data(),
        );

        let element = ctx.history_older(None, None).unwrap();
        // 6 messages total - 2 recent = 4 older.
        assert_eq!(element.message_count(), 4);
        assert_eq!(element.token_count(), 40);
    }

    #[test]
    fn history_older_with_strategy_records_pending_compaction() {
        let mut ctx = AssemblyContext::with_data(
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            sample_assembly_data(),
        );

        let _element = ctx
            .history_older(Some(500), Some("structured_extract".to_string()))
            .unwrap();

        assert_eq!(ctx.pending_compactions.len(), 1);
        let compaction = &ctx.pending_compactions[0];
        assert_eq!(compaction.element_name, "history");
        assert_eq!(compaction.strategy, "structured_extract");
        assert_eq!(compaction.budget, 500);
        assert_eq!(compaction.source_range, Some((0, 4)));
    }

    #[test]
    fn history_older_without_strategy_records_no_compaction() {
        let mut ctx = AssemblyContext::with_data(
            1000,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            sample_assembly_data(),
        );

        let _element = ctx.history_older(None, None).unwrap();

        assert!(ctx.pending_compactions.is_empty());
    }

    #[test]
    fn history_older_uses_remaining_budget_when_no_budget_specified() {
        let mut ctx = AssemblyContext::with_data(
            800,
            "gpt-4".to_string(),
            "agent-1".to_string(),
            sample_assembly_data(),
        );
        // Consume some budget first.
        ctx.add(&make_element("prompts", 200)).unwrap();
        assert_eq!(ctx.remaining_budget, 600);

        let _element = ctx
            .history_older(None, Some("truncation".to_string()))
            .unwrap();

        // Should use remaining_budget (600) as the compaction budget.
        assert_eq!(ctx.pending_compactions[0].budget, 600);
    }

    // ── add() preserves content_preview in report ────────────────────────────

    #[test]
    fn add_records_content_preview_in_report() {
        let mut ctx = AssemblyContext::new(1000, "gpt-4".to_string(), "agent-1".to_string());
        let element = PyElement::new(
            "prompts".to_string(),
            "source".to_string(),
            vec![ChatMessage::new(Role::System, "Be helpful.")],
            10,
            100,
        );

        ctx.add(&element).unwrap();

        let report = &ctx.element_reports[0];
        assert_eq!(report.preview.as_deref(), Some("Be helpful."));
    }

    // ── include() — cycle detection and not-found ────────────────────────────

    #[test]
    fn include_detects_cycle_and_records_warning() {
        pyo3::prepare_freethreaded_python();
        Python::with_gil(|py| {
            let data = sample_assembly_data();
            let ctx = AssemblyContext::with_data(1000, "gpt-4".into(), "agent-1".into(), data);
            let ctx_py = Py::new(py, ctx).unwrap();

            // Simulate: we're already inside an include of "alpha".
            ctx_py.borrow_mut(py).include_stack_mut().push("alpha".into());

            // Trying to include "alpha" again should detect the cycle.
            let bound = ctx_py.bind(py);
            AssemblyContext::include(bound, "alpha".into()).unwrap();

            let ctx_ref = ctx_py.borrow(py);
            assert_eq!(ctx_ref.warnings.len(), 1);
            assert!(
                ctx_ref.warnings[0].contains("circular reference"),
                "expected cycle warning, got: {}",
                ctx_ref.warnings[0]
            );
            assert!(ctx_ref.warnings[0].contains("alpha"));
        });
    }

    #[test]
    fn include_not_found_records_warning() {
        pyo3::prepare_freethreaded_python();
        Python::with_gil(|py| {
            // Empty search_levels means nothing can be resolved.
            let data = sample_assembly_data();
            let ctx = AssemblyContext::with_data(1000, "gpt-4".into(), "agent-1".into(), data);
            let ctx_py = Py::new(py, ctx).unwrap();

            let bound = ctx_py.bind(py);
            AssemblyContext::include(bound, "nonexistent".into()).unwrap();

            let ctx_ref = ctx_py.borrow(py);
            assert_eq!(ctx_ref.warnings.len(), 1);
            assert!(
                ctx_ref.warnings[0].contains("not found"),
                "expected not-found warning, got: {}",
                ctx_ref.warnings[0]
            );
        });
    }

    #[test]
    fn include_with_data_has_search_levels() {
        let mut data = sample_assembly_data();
        data.search_levels = vec![
            (swage_config::HierarchyLevel::Project, PathBuf::from("/tmp/project")),
        ];
        let ctx = AssemblyContext::with_data(1000, "gpt-4".into(), "agent-1".into(), data);
        assert_eq!(ctx.search_levels.len(), 1);
    }
}
