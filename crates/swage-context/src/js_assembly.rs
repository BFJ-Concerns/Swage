//! JavaScript context assemblies — operator-programmable composition behind
//! the stable assembly-name surface.
//!
//! A named assembly is a `<name>.js` file in the config hierarchy
//! (`assemblies/` under global, space, or agent — the most specific wins).
//! The script runs in a caged QuickJS runtime — no IO, no network, no
//! timers, no randomness — and composes the turn's context from the typed
//! elements the gateway built. Assemblies compose context; they never call
//! models.
//!
//! Failure is fail-open by contract: any script problem (missing file,
//! syntax error, runtime error, empty output) falls back to the built-in
//! default assembly with the error carried in the report.
//!
//! ## The script contract
//!
//! The script body runs with three globals and communicates by calling `add`:
//!
//! - `elements` — the turn's elements, read-only:
//!   `[{ name, priority, source, tokenCount, messages, render? }]` where
//!   `messages` is `[{ role, text }]`. Template-backed elements expose a
//!   non-enumerable `render()` that returns a copy whose messages have every
//!   `{{name}}` slot replaced from the current session values; absent values
//!   become `⟦unset:name⟧`.
//! - `ctx` — read-only assembly context. `ctx.variable(name)` returns the
//!   current session value or `undefined` when the name is absent.
//! - `add(element_or_messages)` — appends an element's messages (pass an
//!   entry from `elements`) or a raw `[{ role, text }]` array to the output,
//!   in call order.
//! - `omit(name, reason)` — declares that the composition deliberately leaves
//!   the named element out, and why.
//!
//! ## The baseline floor
//!
//! Composition is programmable; the baseline is not silently droppable. Any
//! element the built-in default assembly would have carried from the same
//! inputs, and that the script neither added nor declared through `omit`, is
//! restored into the output at its own priority, with a warning naming it. A
//! declared omission is honoured and recorded in the report's
//! `declared_omissions`, so an absent baseline element is always visibly a
//! choice rather than an accident.
//!
//! A minimal reordering assembly:
//!
//! ```js
//! const byName = Object.fromEntries(elements.map(e => [e.name, e]));
//! add(byName.orientation);
//! add(byName.prompts);
//! add(byName.history);
//! ```

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rquickjs::{
    Context, Runtime,
    context::intrinsic::{Eval, Json, MapSet, RegExp, RegExpCompiler},
};
use swage_config::HierarchyLevel;
use swage_config::model::ContextConfig;
use swage_core::{ChatMessage, Role};

use crate::assembly::{
    default_assembly_carries, default_assembly_with_variables, safe_metadata_preview,
};
use crate::briefing::substitute_variables;
use crate::element::ContextElement;
use crate::report::{AssemblyError, AssemblyOutput, DeclaredOmission, ElementReport};
use crate::tokeniser::Tokeniser;

/// Memory and stack caps for assembly scripts. Assemblies transform message
/// lists; they get a fraction of the workflow engine's room.
const MEMORY_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const STACK_LIMIT_BYTES: usize = 256 * 1024;

/// Locate `<name>.js` in the hierarchy's `assemblies/` directories, most
/// specific level first.
pub fn resolve_js_assembly(
    name: &str,
    search_levels: &[(HierarchyLevel, &Path)],
) -> Option<PathBuf> {
    if swage_core::validate_context_assembly_name(name).is_err() {
        return None;
    }
    // Later entries are more specific (global → space → agent); search in
    // reverse so the most specific hit wins.
    search_levels.iter().rev().find_map(|(_, base)| {
        let candidate = base.join("assemblies").join(format!("{name}.js"));
        candidate.is_file().then_some(candidate)
    })
}

/// Run the named assembly, falling back to the default on any failure.
///
/// The report always states what actually ran: a fallback carries the
/// configured name, the error that forced it, and the default assembly's
/// element accounting.
pub fn assemble_named(
    name: &str,
    source_path: Option<&Path>,
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
    variables: &BTreeMap<String, String>,
) -> AssemblyOutput {
    let Some(path) = source_path else {
        return fallback(
            name,
            format!("assembly '{name}' was not found in any assemblies/ directory"),
            elements,
            config,
            tokeniser,
            variables,
        );
    };
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            return fallback(
                name,
                format!("assembly '{name}' is not readable: {error}"),
                elements,
                config,
                tokeniser,
                variables,
            );
        }
    };

    let timeout = Duration::from_secs(config.timeout.max(1));
    match run_script(&source, elements, tokeniser, variables, timeout) {
        Ok(outcome) if outcome.messages.is_empty() => fallback(
            name,
            format!("assembly '{name}' produced no messages"),
            elements,
            config,
            tokeniser,
            variables,
        ),
        Ok(outcome) => compose(name, outcome, elements, config, tokeniser, variables),
        Err(error) => fallback(
            name,
            format!("assembly '{name}' failed: {error}"),
            elements,
            config,
            tokeniser,
            variables,
        ),
    }
}

/// Turn a completed script run into the turn's context and its report.
///
/// The report describes what actually ran: one element row per `add()` call,
/// in composition order, attributed to the element the script added (or to
/// the script itself for raw messages). Everything the default assembly would
/// have done — compaction, budget enforcement — did not happen here and is
/// reported as such rather than inherited.
///
/// Between the run and the report sits the baseline floor: every element the
/// default assembly would have carried and the script did not add is either a
/// declared omission (honoured, recorded) or a breach (restored, warned).
fn compose(
    name: &str,
    outcome: ScriptOutcome,
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
    variables: &BTreeMap<String, String>,
) -> AssemblyOutput {
    let started = Instant::now();
    let ScriptOutcome {
        messages,
        added,
        declared,
    } = outcome;

    let mut report = crate::report::AssemblyReport::new(name);

    // Slice the flat output back into the blocks the script composed, so a
    // restored element can be placed among them by priority.
    let mut blocks: Vec<ComposedBlock> = Vec::with_capacity(added.len());
    let mut offset = 0usize;
    for block in added {
        let slice = messages[offset..offset + block.message_count].to_vec();
        offset += block.message_count;
        blocks.push(ComposedBlock {
            name: block.name,
            source: block.source,
            priority: block.priority,
            messages: slice,
        });
    }

    // The floor: what the default assembly would have carried from these same
    // inputs, minus what the script already composed. Highest priority first,
    // so restoring in order keeps orientation ahead of the norms it precedes.
    let mut missing: Vec<ComposedBlock> = baseline_floor(elements, variables)
        .into_iter()
        .filter(|element| !blocks.iter().any(|block| block.name == element.name))
        .collect();
    missing.sort_by_key(|element| Reverse(element.priority));

    for declaration in declared {
        match missing
            .iter()
            .position(|element| element.name == declaration.element)
        {
            Some(index) => {
                missing.remove(index);
                report.declared_omissions.push(declaration);
            }
            // A declaration that matches nothing left out is not an omission:
            // the element was composed anyway, or was never on this turn.
            None => report.warnings.push(format!(
                "assembly '{name}' declared the omission of '{}', which is not \
                 a baseline element missing from the composed context",
                declaration.element
            )),
        }
    }

    for element in missing {
        report.warnings.push(format!(
            "assembly '{name}' left the baseline element '{}' out without \
             declaring it; the runtime restored it",
            element.name
        ));
        let at = blocks
            .iter()
            .position(|block| block.priority < element.priority)
            .unwrap_or(blocks.len());
        blocks.insert(at, element);
    }

    let assembled: Vec<ChatMessage> = blocks
        .iter()
        .flat_map(|block| block.messages.iter().cloned())
        .collect();

    report.elements = blocks
        .iter()
        .map(|block| ElementReport {
            name: block.name.clone(),
            source: block.source.clone(),
            token_count: tokeniser.count_messages(&block.messages),
            preview: safe_metadata_preview(&block.name, &block.source),
            priority: block.priority,
            named_assembly: None,
        })
        .collect();
    report.total_tokens = tokeniser.count_messages(&assembled);
    report.budget = config.budget;
    if let Some(budget) = config.budget
        && report.total_tokens > budget
    {
        report.warnings.push(format!(
            "assembly '{name}' composed {} tokens over the {budget}-token \
             budget; scripted compositions are not compacted",
            report.total_tokens
        ));
    }
    report.duration = started.elapsed();
    AssemblyOutput::new_unchecked(assembled, report)
}

/// The elements a named assembly is held to: those the built-in default
/// assembly would carry from this same slice, that have content to give.
///
/// The membership test is the default assembly's own disposition, read from
/// `default_assembly_carries` rather than restated here, so the floor cannot
/// drift from what "the baseline" means where it is defined.
fn baseline_floor(
    elements: &[Box<dyn ContextElement>],
    variables: &BTreeMap<String, String>,
) -> Vec<ComposedBlock> {
    elements
        .iter()
        .filter(|element| default_assembly_carries(element.name()))
        .filter_map(|element| {
            let messages = rendered_content(element.as_ref(), variables);
            (!messages.is_empty()).then(|| ComposedBlock {
                name: element.name().to_string(),
                source: element.source(),
                priority: element.priority(),
                messages,
            })
        })
        .collect()
}

/// An element's content with session-value slots filled, matching what the
/// default assembly would have placed in the context.
fn rendered_content(
    element: &dyn ContextElement,
    variables: &BTreeMap<String, String>,
) -> Vec<ChatMessage> {
    let content = element.content();
    if !element.renders_session_values() {
        return content;
    }
    content
        .into_iter()
        .map(|message| {
            ChatMessage::new(
                message.role(),
                substitute_variables(&message.text_content(), variables),
            )
        })
        .collect()
}

/// One contiguous run of messages in the composed context, carrying the
/// identity its report row reports.
struct ComposedBlock {
    name: String,
    source: String,
    priority: i32,
    messages: Vec<ChatMessage>,
}

fn fallback(
    name: &str,
    message: String,
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
    variables: &BTreeMap<String, String>,
) -> AssemblyOutput {
    tracing::warn!(assembly = name, %message, "assembly fell back to default");
    let mut output = default_assembly_with_variables(elements, config, tokeniser, variables);
    output.report.errors.push(AssemblyError {
        assembly_name: name.to_owned(),
        error_kind: "script_failure".to_owned(),
        message,
        fallback_used: "default".to_owned(),
    });
    output
}

/// What one `add()` call contributed, for the report: the element's own
/// identity when an element was added, or a script marker for raw messages.
#[derive(serde::Deserialize)]
struct AddedBlock {
    name: String,
    source: String,
    priority: i32,
    #[serde(rename = "messageCount")]
    message_count: usize,
}

/// Execute the script against a read-only projection of the elements,
/// returning the composed message list and per-add attribution.
fn run_script(
    source: &str,
    elements: &[Box<dyn ContextElement>],
    tokeniser: &dyn Tokeniser,
    variables: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<ScriptOutcome, String> {
    let runtime = Runtime::new().map_err(|error| error.to_string())?;
    runtime.set_memory_limit(MEMORY_LIMIT_BYTES);
    runtime.set_max_stack_size(STACK_LIMIT_BYTES);
    // The deadline is the fail-open boundary a runaway script hits: the
    // interrupt handler fires periodically during evaluation, and past the
    // deadline the script is aborted so the default assembly can serve.
    let deadline = Instant::now() + timeout;
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    // Explicit intrinsics, the workflow engine's idiom: no Date, so
    // wall-clock time is constructively absent rather than policed. Eval is
    // required for host-side source evaluation; the JS-visible global is
    // deleted in the cage below before script code runs.
    type AssemblyIntrinsics = (Eval, Json, MapSet, RegExpCompiler, RegExp);
    let context =
        Context::custom::<AssemblyIntrinsics>(&runtime).map_err(|error| error.to_string())?;

    let elements_json = serde_json::to_string(
        &elements
            .iter()
            .map(|element| {
                serde_json::json!({
                    "name": element.name(),
                    "priority": element.priority(),
                    "source": element.source(),
                    "tokenCount": element.token_count(tokeniser),
                    "rendersSessionValues": element.renders_session_values(),
                    "messages": element
                        .content()
                        .iter()
                        .map(|message| {
                            serde_json::json!({
                                "role": role_name(message.role()),
                                "text": message.text_content(),
                            })
                        })
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|error| error.to_string())?;
    let variables_json = serde_json::to_string(variables).map_err(|error| error.to_string())?;

    context.with(|ctx| -> Result<ScriptOutcome, String> {
        // The cage: no eval, no ambient nondeterminism. Assemblies must be
        // pure functions of their elements.
        ctx.eval::<(), _>(
            r#"
            delete globalThis.eval;
            if (typeof Math !== "undefined") {
                Math.random = () => {
                    throw new Error("Math.random is not available in assemblies; composition must be deterministic");
                };
            }
            globalThis.__output = [];
            globalThis.__added = [];
            globalThis.add = (value) => {
                const isElement = value && !Array.isArray(value) && Array.isArray(value.messages);
                const messages = Array.isArray(value) ? value : isElement ? value.messages : null;
                if (!Array.isArray(messages)) {
                    throw new Error("add() takes an element from `elements` or an array of {role, text} messages");
                }
                for (const message of messages) {
                    if (typeof message.role !== "string" || typeof message.text !== "string") {
                        throw new Error("each message needs a string role and text");
                    }
                    globalThis.__output.push({ role: message.role, text: message.text });
                }
                globalThis.__added.push({
                    name: isElement && typeof value.name === "string" ? value.name : "script",
                    source: isElement && typeof value.source === "string" ? value.source : "composed by the assembly script",
                    priority: isElement && typeof value.priority === "number" ? value.priority : 0,
                    messageCount: messages.length,
                });
            };
            globalThis.__omitted = [];
            globalThis.omit = (name, reason) => {
                if (typeof name !== "string" || name.length === 0) {
                    throw new Error("omit() takes the name of the element to leave out");
                }
                if (typeof reason !== "string" || reason.trim().length === 0) {
                    throw new Error(`omit("${name}") needs a reason for leaving the element out`);
                }
                globalThis.__omitted.push({ element: name, reason });
            };
            "#,
        )
        .map_err(|error| format!("cage setup failed: {error}"))?;

        let globals = ctx.globals();
        globals
            .set(
                "elements",
                ctx.json_parse(elements_json.as_bytes())
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        globals
            .set("__variables", ctx.json_parse(variables_json.as_bytes()).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
        ctx.eval::<(), _>(r#"
            const slot = /\{\{([a-z0-9-]+)\}\}/g;
            const variables = globalThis.__variables;
            const valueFor = name => Object.prototype.hasOwnProperty.call(variables, name)
                ? variables[name]
                : undefined;
            const renderedValueFor = name => valueFor(name) ?? `⟦unset:${name}⟧`;
            globalThis.ctx = Object.freeze({ variable: valueFor });
            for (const element of globalThis.elements) {
                if (element.rendersSessionValues) {
                    Object.defineProperty(element, "render", {
                        enumerable: false,
                        value: () => ({
                            ...element,
                            messages: element.messages.map(message => ({
                                ...message,
                                text: message.text.replace(slot, (_match, name) => renderedValueFor(name)),
                            })),
                        }),
                    });
                }
                delete element.rendersSessionValues;
                Object.freeze(element.messages);
                Object.freeze(element);
            }
            Object.freeze(globalThis.elements);
            delete globalThis.__variables;
        "#).map_err(|error| format!("variable capability setup failed: {error}"))?;

        ctx.eval::<(), _>(source.as_bytes())
            .map_err(|error| script_error(&ctx, error))?;

        let output_json: String = ctx
            .eval("JSON.stringify(globalThis.__output)")
            .map_err(|error| script_error(&ctx, error))?;
        let raw: Vec<RawMessage> =
            serde_json::from_str(&output_json).map_err(|error| error.to_string())?;
        let messages = raw
            .into_iter()
            .map(|message| {
                Ok(ChatMessage::new(
                    parse_role(&message.role)?,
                    &message.text,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;

        let added_json: String = ctx
            .eval("JSON.stringify(globalThis.__added)")
            .map_err(|error| script_error(&ctx, error))?;
        let added: Vec<AddedBlock> =
            serde_json::from_str(&added_json).map_err(|error| error.to_string())?;

        let omitted_json: String = ctx
            .eval("JSON.stringify(globalThis.__omitted)")
            .map_err(|error| script_error(&ctx, error))?;
        let declared: Vec<DeclaredOmission> =
            serde_json::from_str(&omitted_json).map_err(|error| error.to_string())?;

        Ok(ScriptOutcome {
            messages,
            added,
            declared,
        })
    })
}

/// What one assembly run produced: the composed messages, the per-`add()`
/// attribution behind them, and every omission the script declared.
struct ScriptOutcome {
    messages: Vec<ChatMessage>,
    added: Vec<AddedBlock>,
    declared: Vec<DeclaredOmission>,
}

#[derive(serde::Deserialize)]
struct RawMessage {
    role: String,
    text: String,
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn parse_role(role: &str) -> Result<Role, String> {
    match role {
        "system" => Ok(Role::System),
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        "tool" => Ok(Role::Tool),
        other => Err(format!("unknown message role '{other}'")),
    }
}

fn script_error(ctx: &rquickjs::Ctx<'_>, error: rquickjs::Error) -> String {
    if let rquickjs::Error::Exception = error {
        let exception = ctx.catch();
        if let Some(exception) = exception.as_exception() {
            return exception
                .message()
                .unwrap_or_else(|| "script exception".to_owned());
        }
    }
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::briefing::BriefingElement;
    use crate::history::HistoryElement;
    use crate::named_element::ResolvedNamedElement;
    use crate::orientation::{OrientationElement, SessionEnvironment};
    use crate::prompts::PromptsElement;
    use crate::working_norms::{WorkingNormsElement, WorkingNormsScaffold};
    use swage_config::model::ContextConfig;
    use swage_config::prompt_rule::PromptRule;

    struct WordTokeniser;
    impl Tokeniser for WordTokeniser {
        fn count_text(&self, text: &str) -> usize {
            text.split_whitespace().count()
        }
        fn count_messages(&self, messages: &[ChatMessage]) -> usize {
            messages
                .iter()
                .map(|m| self.count_text(&m.text_content()))
                .sum()
        }
    }

    fn test_elements() -> Vec<Box<dyn ContextElement>> {
        vec![
            Box::new(PromptsElement::new(vec![PromptRule {
                priority: 0,
                content: "operator instructions".into(),
                source_level: HierarchyLevel::Global,
                file_path: PathBuf::from("test"),
            }])),
            Box::new(HistoryElement::new(vec![
                ChatMessage::new(Role::User, "hello"),
                ChatMessage::new(Role::Assistant, "hi"),
            ])),
        ]
    }

    fn run(source: &str) -> Result<Vec<ChatMessage>, String> {
        run_script(
            source,
            &test_elements(),
            &WordTokeniser,
            &BTreeMap::new(),
            Duration::from_secs(5),
        )
        .map(|outcome| outcome.messages)
    }

    #[test]
    fn element_render_substitutes_current_values_and_marks_missing_slots() {
        let variables = BTreeMap::from([("task".to_string(), "audit storage".to_string())]);
        let elements: Vec<Box<dyn ContextElement>> =
            vec![Box::new(BriefingElement::new(ResolvedNamedElement {
                name: "current-work".into(),
                level: HierarchyLevel::Global,
                path: PathBuf::from("test"),
                content: "Do {{task}} for {{owner}}; direct={{task}}".into(),
            }))];
        let outcome = run_script(
            r#"
            const rendered = elements[0].render();
            add(rendered);
            add([{
                role: 'system',
                text: `${ctx.variable('task')}|${ctx.variable('owner') === undefined}`,
            }]);
            "#,
            &elements,
            &WordTokeniser,
            &variables,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            outcome.messages[0].text_content(),
            "Do audit storage for ⟦unset:owner⟧; direct=audit storage"
        );
        assert_eq!(outcome.messages[1].text_content(), "audit storage|true");
        assert_eq!(
            outcome.added[0].name, "current-work",
            "render() preserves element metadata"
        );
    }

    #[test]
    fn literal_history_does_not_expose_render() {
        let messages = run(r#"
            const history = elements.find(element => element.name === "history");
            add([{ role: "system", text: typeof history.render }]);
            "#)
        .unwrap();

        assert_eq!(messages[0].text_content(), "undefined");
    }

    /// The report attributes each add() to what was actually added, so a
    /// scripted composition's stored account matches what ran.
    #[test]
    fn attribution_follows_the_script_not_the_inputs() {
        let outcome = run_script(
            r#"
            const byName = Object.fromEntries(elements.map(e => [e.name, e]));
            add(byName.history);
            add([{ role: "system", text: "inline note" }]);
            "#,
            &test_elements(),
            &WordTokeniser,
            &BTreeMap::new(),
            Duration::from_secs(5),
        )
        .unwrap();

        assert_eq!(outcome.messages.len(), 3);
        assert_eq!(outcome.added.len(), 2);
        assert_eq!(outcome.added[0].name, "history");
        assert_eq!(outcome.added[0].message_count, 2);
        assert_eq!(
            outcome.added[1].name, "script",
            "raw messages attribute to the script"
        );
        // The prompts element was never added and must not appear.
        assert!(outcome.added.iter().all(|block| block.name != "prompts"));
    }

    /// The most important error boundary: a runaway script must abort at the
    /// deadline and reach the fail-open path rather than hanging the turn.
    #[test]
    fn runaway_script_is_aborted_at_the_deadline() {
        let started = Instant::now();
        let result = run_script(
            "while (true) {}",
            &test_elements(),
            &WordTokeniser,
            &BTreeMap::new(),
            Duration::from_millis(200),
        )
        .map(|outcome| outcome.messages);
        assert!(result.is_err(), "an interrupted script is an error");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the deadline must abort the script promptly"
        );
    }

    #[test]
    fn script_composes_from_elements_in_call_order() {
        let messages = run(r#"
            const byName = Object.fromEntries(elements.map(e => [e.name, e]));
            add(byName.history);
            add(byName.prompts);
            "#)
        .unwrap();

        // History first, prompts last — exactly the call order, however
        // unusual, because composition belongs to the script.
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].text_content(), "hello");
        assert_eq!(messages[2].text_content(), "operator instructions");
    }

    #[test]
    fn documented_reordering_assembly_executes() {
        let elements: Vec<Box<dyn ContextElement>> = vec![
            Box::new(OrientationElement::new(
                SessionEnvironment {
                    space_name: String::new(),
                    space_description: String::new(),
                    working_directory: None,
                    started_at: chrono::Utc::now(),
                    channel: None,
                },
                vec![],
            )),
            Box::new(PromptsElement::new(vec![PromptRule {
                priority: 0,
                content: "operator instructions".into(),
                source_level: HierarchyLevel::Global,
                file_path: PathBuf::from("test"),
            }])),
            Box::new(HistoryElement::new(vec![ChatMessage::new(
                Role::User,
                "hello",
            )])),
        ];

        let messages = run_script(
            r#"
            const byName = Object.fromEntries(elements.map(e => [e.name, e]));
            add(byName.orientation);
            add(byName.prompts);
            add(byName.history);
            "#,
            &elements,
            &WordTokeniser,
            &BTreeMap::new(),
            Duration::from_secs(5),
        )
        .expect("the documented assembly must run");

        assert_eq!(messages.messages.len(), 3);
        assert_eq!(messages.messages[1].text_content(), "operator instructions");
        assert_eq!(messages.messages[2].text_content(), "hello");
    }

    #[test]
    fn script_can_add_raw_messages() {
        let messages = run(r#"
            add([{ role: "system", text: "composed inline" }]);
            "#)
        .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role(), Role::System);
    }

    #[test]
    fn nondeterminism_is_caged() {
        let result = run("add([{ role: 'system', text: String(Math.random()) }]);");
        let error = result.unwrap_err();
        assert!(
            error.contains("deterministic"),
            "Math.random must fail loudly: {error}"
        );

        // Wall-clock time is constructively absent — the Date intrinsic is
        // not installed, so scripts cannot depend on when they run.
        let result = run("add([{ role: 'system', text: String(Date.now()) }]);");
        assert!(
            result.is_err(),
            "Date must not exist in the assembly cage: {result:?}"
        );
    }

    #[test]
    fn script_errors_surface_with_their_message() {
        let error = run("throw new Error('deliberate');").unwrap_err();
        assert!(error.contains("deliberate"));
    }

    #[test]
    fn missing_assembly_falls_back_to_default_with_error_report() {
        let elements: Vec<Box<dyn ContextElement>> =
            vec![Box::new(BriefingElement::new(ResolvedNamedElement {
                name: "current-work".into(),
                level: HierarchyLevel::Space,
                path: "elements/current-work.md".into(),
                content: "Task: {{task}}".into(),
            }))];
        let variables = BTreeMap::from([("task".to_owned(), "audit storage".to_owned())]);
        let output = assemble_named(
            "no-such-assembly",
            None,
            &elements,
            &ContextConfig::default(),
            &WordTokeniser,
            &variables,
        );

        assert!(!output.messages.is_empty(), "fallback still assembles");
        assert_eq!(output.messages[0].text_content(), "Task: audit storage");
        assert_eq!(output.report.errors.len(), 1);
        assert_eq!(output.report.errors[0].fallback_used, "default");
    }

    #[test]
    fn empty_script_output_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assemblies").join("empty.js");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "// adds nothing").unwrap();

        let elements = test_elements();
        let output = assemble_named(
            "empty",
            Some(&path),
            &elements,
            &ContextConfig::default(),
            &WordTokeniser,
            &BTreeMap::new(),
        );

        assert!(!output.messages.is_empty());
        assert!(output.report.errors[0].message.contains("no messages"));
    }

    // ── The baseline floor ────────────────────────────────────────────────────

    /// The element set a real turn hands the assembly: baseline content
    /// (orientation, working norms) alongside the operator's prompts and the
    /// conversation so far.
    fn baseline_bearing_elements() -> Vec<Box<dyn ContextElement>> {
        vec![
            Box::new(OrientationElement::new(
                SessionEnvironment {
                    space_name: "Atelier".into(),
                    space_description: String::new(),
                    working_directory: None,
                    started_at: chrono::Utc::now(),
                    channel: None,
                },
                vec![],
            )),
            Box::new(WorkingNormsElement::for_model(
                "claude-opus-5",
                WorkingNormsScaffold::FamilyFallback,
            )),
            Box::new(PromptsElement::new(vec![PromptRule {
                priority: 0,
                content: "operator instructions".into(),
                source_level: HierarchyLevel::Global,
                file_path: PathBuf::from("test"),
            }])),
            Box::new(HistoryElement::new(vec![ChatMessage::new(
                Role::User,
                "hello",
            )])),
        ]
    }

    fn assemble_source(source: &str, elements: &[Box<dyn ContextElement>]) -> AssemblyOutput {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assemblies").join("focus.js");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        assemble_named(
            "focus",
            Some(&path),
            elements,
            &ContextConfig::default(),
            &WordTokeniser,
            &BTreeMap::new(),
        )
    }

    const HISTORY_AND_PROMPTS_ONLY: &str = r#"
        const byName = Object.fromEntries(elements.map(e => [e.name, e]));
        add(byName.prompts);
        add(byName.history);
        "#;

    #[test]
    fn an_undeclared_baseline_omission_is_restored_at_its_priority() {
        let elements = baseline_bearing_elements();
        let output = assemble_source(HISTORY_AND_PROMPTS_ONLY, &elements);

        let names: Vec<&str> = output
            .report
            .elements
            .iter()
            .map(|element| element.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["orientation", "working_norms", "prompts", "history"],
            "the restored baseline leads the composition it was left out of"
        );

        let text = output
            .messages
            .iter()
            .map(|message| message.text_content())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("Atelier"),
            "the restored orientation carries its own content, not a placeholder"
        );
        assert!(
            output.report.declared_omissions.is_empty(),
            "nothing was declared, so nothing is recorded as chosen"
        );
        assert!(
            output
                .report
                .warnings
                .iter()
                .any(|warning| warning.contains("working_norms") && warning.contains("restored")),
            "the breach is named, not silently repaired: {:?}",
            output.report.warnings
        );
        assert!(
            output.report.errors.is_empty(),
            "a restored baseline is not a script failure"
        );
    }

    #[test]
    fn a_declared_omission_is_honoured_and_recorded() {
        let elements = baseline_bearing_elements();
        let output = assemble_source(
            &format!(
                r#"{HISTORY_AND_PROMPTS_ONLY}
                omit("working_norms", "the operator instructions carry their own");
                "#
            ),
            &elements,
        );

        let names: Vec<&str> = output
            .report
            .elements
            .iter()
            .map(|element| element.name.as_str())
            .collect();
        assert!(
            !names.contains(&"working_norms"),
            "a declared omission is honoured: {names:?}"
        );
        assert!(
            names.contains(&"orientation"),
            "declaring one omission does not waive the rest of the floor"
        );

        assert_eq!(output.report.declared_omissions.len(), 1);
        assert_eq!(output.report.declared_omissions[0].element, "working_norms");
        assert_eq!(
            output.report.declared_omissions[0].reason,
            "the operator instructions carry their own"
        );
        assert!(
            !output
                .report
                .warnings
                .iter()
                .any(|warning| warning.contains("working_norms")),
            "a declared omission is a choice, not a breach: {:?}",
            output.report.warnings
        );
    }

    #[test]
    fn a_declaration_matching_nothing_left_out_is_a_warning_not_a_record() {
        let elements = baseline_bearing_elements();
        let output = assemble_source(
            r#"
            const byName = Object.fromEntries(elements.map(e => [e.name, e]));
            add(byName.orientation);
            add(byName.working_norms);
            add(byName.prompts);
            add(byName.history);
            omit("working_norms", "declared, yet added anyway");
            omit("cartography", "an element this turn never had");
            "#,
            &elements,
        );

        assert!(
            output.report.declared_omissions.is_empty(),
            "nothing was left out, so nothing is recorded as omitted"
        );
        assert_eq!(
            output
                .report
                .warnings
                .iter()
                .filter(|warning| warning.contains("declared the omission"))
                .count(),
            2,
            "both hollow declarations are named: {:?}",
            output.report.warnings
        );
    }

    #[test]
    fn an_omission_without_a_reason_fails_open_to_the_default() {
        let elements = baseline_bearing_elements();
        let output = assemble_source(
            r#"
            const byName = Object.fromEntries(elements.map(e => [e.name, e]));
            add(byName.prompts);
            omit("working_norms", "   ");
            "#,
            &elements,
        );

        assert_eq!(output.report.errors.len(), 1, "the run failed");
        assert_eq!(output.report.errors[0].assembly_name, "focus");
        assert!(
            output.report.errors[0].message.contains("needs a reason"),
            "the error says what the script got wrong: {}",
            output.report.errors[0].message
        );
        assert_eq!(output.report.errors[0].fallback_used, "default");
        let text = output
            .messages
            .iter()
            .map(|message| message.text_content())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("Atelier"),
            "the default assembly serves, baseline included"
        );
    }

    #[test]
    fn memory_is_not_part_of_the_floor() {
        let mut elements = baseline_bearing_elements();
        elements.push(Box::new(crate::memory::MemoryElement::new(
            "remembered material".into(),
            "Atelier".into(),
        )));
        let output = assemble_source(HISTORY_AND_PROMPTS_ONLY, &elements);

        assert!(
            !output
                .report
                .elements
                .iter()
                .any(|element| element.name == "memory"),
            "the floor is what the default assembly carries, and it discards memory"
        );
    }

    #[test]
    fn resolution_prefers_the_most_specific_level() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        let space = dir.path().join("space");
        for base in [&global, &space] {
            let assemblies = base.join("assemblies");
            std::fs::create_dir_all(&assemblies).unwrap();
            std::fs::write(assemblies.join("focus.js"), "// present").unwrap();
        }

        let levels: Vec<(HierarchyLevel, &Path)> = vec![
            (HierarchyLevel::Global, global.as_path()),
            (HierarchyLevel::Space, space.as_path()),
        ];
        let resolved = resolve_js_assembly("focus", &levels).unwrap();
        assert!(
            resolved.starts_with(&space),
            "the space-level assembly shadows the global one"
        );
    }

    #[test]
    fn invalid_names_do_not_escape_the_assemblies_directory() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside.js");
        std::fs::write(&outside, "// must not resolve").unwrap();
        let config = root.path().join("config");
        std::fs::create_dir_all(config.join("assemblies")).unwrap();

        assert!(
            resolve_js_assembly("../../outside", &[(HierarchyLevel::Global, &config)]).is_none()
        );
        assert!(resolve_js_assembly("/outside", &[(HierarchyLevel::Global, &config)]).is_none());
    }
}
