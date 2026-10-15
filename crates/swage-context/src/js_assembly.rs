//! JavaScript context assemblies — operator-programmable composition behind
//! the stable assembly-name surface.
//!
//! A named assembly is a `<name>.js` file in the config hierarchy
//! (`assemblies/` under global, topic, or agent — the most specific wins).
//! The script runs in a caged V8 isolate — no IO, no network, no
//! timers, no clock, no randomness — and composes the turn's context from the typed
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

use swage_config::HierarchyLevel;
use swage_config::model::ContextConfig;
use swage_core::{ChatMessage, Role};
use swage_isolate::{IsolateLimits, Program};

use crate::assembly::{
    default_assembly_carries, default_assembly_with_variables, safe_metadata_preview,
};
use crate::briefing::substitute_variables;
use crate::element::ContextElement;
use crate::report::{AssemblyError, AssemblyOutput, DeclaredOmission, ElementReport};
use crate::tokeniser::Tokeniser;

/// Heap cap for assembly scripts. Assemblies transform message lists; they
/// get a fraction of the workflow engine's room.
const MEMORY_LIMIT_BYTES: usize = 32 * 1024 * 1024;

/// Locate `<name>.js` in the hierarchy's `assemblies/` directories, most
/// specific level first.
pub fn resolve_js_assembly(
    name: &str,
    search_levels: &[(HierarchyLevel, &Path)],
) -> Option<PathBuf> {
    if swage_core::validate_context_assembly_name(name).is_err() {
        return None;
    }
    // Later entries are more specific (global → topic → agent); search in
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
        Ok(outcome) => match compose(name, outcome, elements, config, tokeniser, variables) {
            Ok(output) => output,
            Err(error) => fallback(name, error, elements, config, tokeniser, variables),
        },
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
/// default assembly would have carried and the script did not add from its
/// supplied element instance is either a declared omission (honoured,
/// recorded) or a breach (restored, warned).
fn compose(
    name: &str,
    outcome: ScriptOutcome,
    elements: &[Box<dyn ContextElement>],
    config: &ContextConfig,
    tokeniser: &dyn Tokeniser,
    variables: &BTreeMap<String, String>,
) -> Result<AssemblyOutput, String> {
    let started = Instant::now();
    let ScriptOutcome {
        messages,
        added,
        declared,
    } = outcome;

    let mut report = crate::report::AssemblyReport::new(name);

    // Slice the flat output back into the blocks the script composed, so a
    // restored element can be placed among them by priority.
    let added_element_indices: Vec<Option<usize>> =
        added.iter().map(|block| block.element_index).collect();
    let mut blocks: Vec<ComposedBlock> = Vec::with_capacity(added.len());
    let mut offset = 0usize;
    for block in added {
        // The counts are checked against the output before composition; the
        // slice stays saturating so a malformed account can never index past
        // the messages it describes.
        let end = offset
            .saturating_add(block.message_count)
            .min(messages.len());
        let slice = messages[offset.min(end)..end].to_vec();
        offset = end;
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
        .filter(|(index, _)| !added_element_indices.contains(&Some(*index)))
        .map(|(_, element)| element)
        .collect();
    missing.sort_by_key(|element| Reverse(element.priority));

    for declaration in declared {
        if declaration.element.is_empty() || declaration.reason.trim().is_empty() {
            report.warnings.push(format!(
                "assembly '{name}' made an invalid omission declaration for '{}'; the runtime restored the baseline element if needed",
                declaration.element
            ));
            continue;
        }
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
    // Accounted against what the context finally carries, restored baseline
    // included — the same subtraction the default assembly reports.
    report.budget_remaining = config
        .budget
        .map(|budget| budget as isize - report.total_tokens as isize);
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
    AssemblyOutput::new(assembled, report)
        .map_err(|_| format!("assembly '{name}' produced no messages"))
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
) -> Vec<(usize, ComposedBlock)> {
    elements
        .iter()
        .enumerate()
        .filter(|(_, element)| default_assembly_carries(element.name()))
        .filter_map(|(index, element)| {
            let messages = rendered_content(element.as_ref(), variables);
            (!messages.is_empty()).then(|| {
                (
                    index,
                    ComposedBlock {
                        name: element.name().to_string(),
                        source: element.source(),
                        priority: element.priority(),
                        messages,
                    },
                )
            })
        })
        .collect()
}

/// An element's content with session-value slots filled, matching what the
/// default assembly would have placed in the context.
pub fn rendered_content(
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
    #[serde(rename = "elementIndex")]
    element_index: Option<usize>,
    #[serde(rename = "messageCount")]
    message_count: usize,
}

/// The host's own account of a run, serialised after the script returns:
/// the composed messages and the records `add()` and `omit()` created. None
/// of it is reachable from the script's scope, so it describes what the host
/// observed rather than what the script left behind.
#[derive(serde::Deserialize)]
struct ScriptBookkeeping {
    output: Vec<RawMessage>,
    added: Vec<AddedBlock>,
    omitted: Vec<DeclaredOmission>,
}

/// The host side of every assembly run, evaluated before the script source is
/// parsed.
///
/// It turns the supplied element projection into frozen objects with an
/// identity the host alone can attribute, and evaluates to a runner function. The runner holds the run's bookkeeping — the output list,
/// the `add()` records, the `omit()` declarations and the session values — in
/// its own closure and takes the script as an argument, so the script's scope
/// chain reaches the globals and nothing else. Builtins the host depends on
/// are captured here, while they are still the originals.
const HOST_PRELUDE: &str = r#"
(() => {
    const freeze = Object.freeze;
    const isArray = Array.isArray;
    const quote = JSON.stringify;
    const hasOwn = Function.prototype.call.bind(Object.prototype.hasOwnProperty);
    const numberToString = Function.prototype.call.bind(Number.prototype.toString);
    const replace = Function.prototype.call.bind(String.prototype.replace);
    const push = Function.prototype.call.bind(Array.prototype.push);
    const weakGet = Function.prototype.call.bind(WeakMap.prototype.get);
    const weakSet = Function.prototype.call.bind(WeakMap.prototype.set);

    // Bookkeeping is host-owned, but JSON.stringify on its object graph would
    // still consult a toJSON hook a script added to Object.prototype. Encode
    // the host's primitive fields directly instead: quote only receives
    // strings, and the number formatter is captured before script execution.
    const serialiseBookkeeping = (output, added, omitted) => {
        let json = '{"output":[';
        for (let index = 0; index < output.length; index += 1) {
            const message = output[index];
            json += (index === 0 ? '' : ',')
                + '{"role":' + quote(message.role)
                + ',"text":' + quote(message.text) + '}';
        }
        json += '],"added":[';
        for (let index = 0; index < added.length; index += 1) {
            const block = added[index];
            json += (index === 0 ? '' : ',')
                + '{"name":' + quote(block.name)
                + ',"source":' + quote(block.source)
                + ',"priority":' + numberToString(block.priority)
                + ',"elementIndex":'
                + (block.elementIndex === null ? 'null' : numberToString(block.elementIndex))
                + ',"messageCount":' + numberToString(block.messageCount) + '}';
        }
        json += '],"omitted":[';
        for (let index = 0; index < omitted.length; index += 1) {
            const declaration = omitted[index];
            json += (index === 0 ? '' : ',')
                + '{"element":' + quote(declaration.element)
                + ',"reason":' + quote(declaration.reason) + '}';
        }
        return json + ']}';
    };

    const slot = /\{\{([a-z0-9-]+)\}\}/g;
    const variables = globalThis.__variables;
    delete globalThis.__variables;
    const valueFor = name => (hasOwn(variables, name) ? variables[name] : undefined);
    const renderedValueFor = name => valueFor(name) ?? `⟦unset:${name}⟧`;
    globalThis.ctx = freeze({ variable: valueFor });

    // Element identity the script cannot forge or transfer: the host maps
    // each supplied object — and each copy `render()` returns — to the index
    // of the element it actually came from.
    const suppliedElements = globalThis.elements;
    const elementIndices = new WeakMap();
    for (let index = 0; index < suppliedElements.length; index += 1) {
        const element = suppliedElements[index];
        weakSet(elementIndices, element, index);
        if (element.rendersSessionValues) {
            Object.defineProperty(element, "render", {
                enumerable: false,
                value: () => {
                    const messages = [];
                    for (let m = 0; m < element.messages.length; m += 1) {
                        const message = element.messages[m];
                        push(messages, freeze({
                            role: message.role,
                            text: replace(message.text, slot, (_match, name) => renderedValueFor(name)),
                        }));
                    }
                    freeze(messages);
                    const rendered = freeze({
                        name: element.name,
                        priority: element.priority,
                        source: element.source,
                        tokenCount: element.tokenCount,
                        messages,
                    });
                    weakSet(elementIndices, rendered, index);
                    return rendered;
                },
            });
        }
        delete element.rendersSessionValues;
        for (let m = 0; m < element.messages.length; m += 1) {
            freeze(element.messages[m]);
        }
        freeze(element.messages);
        freeze(element);
    }
    freeze(suppliedElements);

    return script => {
        const output = [];
        const added = [];
        const omitted = [];
        globalThis.add = value => {
            const isElement = value !== null
                && typeof value === "object"
                && !isArray(value)
                && isArray(value.messages);
            const messages = isArray(value) ? value : isElement ? value.messages : null;
            if (!isArray(messages)) {
                throw new Error("add() takes an element from `elements` or an array of {role, text} messages");
            }
            for (let m = 0; m < messages.length; m += 1) {
                const message = messages[m];
                if (message === null
                    || typeof message !== "object"
                    || typeof message.role !== "string"
                    || typeof message.text !== "string") {
                    throw new Error("each message needs a string role and text");
                }
                push(output, { role: message.role, text: message.text });
            }
            const index = weakGet(elementIndices, value);
            push(added, {
                name: isElement && typeof value.name === "string" ? value.name : "script",
                source: isElement && typeof value.source === "string" ? value.source : "composed by the assembly script",
                priority: isElement && typeof value.priority === "number" ? value.priority : 0,
                elementIndex: index === undefined ? null : index,
                messageCount: messages.length,
            });
        };
        globalThis.omit = (name, reason) => {
            if (typeof name !== "string" || name.length === 0) {
                throw new Error("omit() takes the name of the element to leave out");
            }
            if (typeof reason !== "string" || reason.trim().length === 0) {
                throw new Error(`omit("${name}") needs a reason for leaving the element out`);
            }
            push(omitted, { element: name, reason });
        };
        try {
            script();
        } finally {
            delete globalThis.add;
            delete globalThis.omit;
        }
        return serialiseBookkeeping(output, added, omitted);
    };
})()
"#;

/// Execute the script against a read-only projection of the elements,
/// returning the composed message list and per-add attribution.
fn run_script(
    source: &str,
    elements: &[Box<dyn ContextElement>],
    tokeniser: &dyn Tokeniser,
    variables: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<ScriptOutcome, String> {
    // The isolate cages the engine (no `Date`, `eval` or `Math.random`, so
    // wall-clock time and randomness are constructively absent) and bounds
    // the script: `timeout` is the fail-open boundary a runaway script hits,
    // terminated so the default assembly can serve.
    let mut program = Program::new(
        IsolateLimits {
            heap_bytes: MEMORY_LIMIT_BYTES,
            continuous_execution: timeout,
        },
        None,
        None,
    )
    .map_err(|error| error.to_string())?;

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
    program
        .set_global_json("elements", &elements_json)
        .map_err(|error| error.to_string())?;
    program
        .set_global_json("__variables", &variables_json)
        .map_err(|error| error.to_string())?;

    // The host prelude runs before the script source is even parsed: it
    // freezes the element projection and returns the runner that owns the
    // run's bookkeeping.
    program
        .execute(
            "swage:assembly-host",
            format!("globalThis.__assemblyRunner = {HOST_PRELUDE};"),
        )
        .map_err(|error| format!("cage setup failed: {error}"))?;

    // The script body becomes a function of its own, defined at global
    // scope. Nothing the host keeps is in that scope chain, so `added`,
    // `omitted`, the output list and the session values have no name the
    // script can reach: `add`, `omit`, `ctx` and `elements` are the whole
    // surface.
    program
        .execute(
            "swage:assembly",
            format!("globalThis.__assemblyScript = function () {{\n{source}\n}};"),
        )
        .map_err(|error| error.to_string())?;
    let bookkeeping_json = program
        .execute_for_string(
            "swage:assembly-run",
            "(() => {
                const runner = globalThis.__assemblyRunner;
                const script = globalThis.__assemblyScript;
                delete globalThis.__assemblyRunner;
                delete globalThis.__assemblyScript;
                return runner(script);
            })()"
                .to_owned(),
        )
        .map_err(|error| error.to_string())?;
    let ScriptBookkeeping {
        output,
        added,
        omitted: declared,
    } = serde_json::from_str(&bookkeeping_json).map_err(|error| error.to_string())?;

    let messages = output
        .into_iter()
        .map(|message| Ok(ChatMessage::new(parse_role(&message.role)?, &message.text)))
        .collect::<Result<Vec<_>, String>>()?;

    // The host wrote these records, but they still cross a JSON boundary,
    // so composition's assumptions about them are checked here rather
    // than trusted: the per-block counts must partition the output
    // exactly, and an attributed element index must name a real element.
    let counted = added
        .iter()
        .try_fold(0usize, |total, block| {
            total.checked_add(block.message_count)
        })
        .ok_or_else(|| "assembly bookkeeping overflowed its message count".to_owned())?;
    if counted != messages.len() {
        return Err(format!(
            "assembly bookkeeping accounts for {counted} messages but the \
             composition produced {}",
            messages.len()
        ));
    }
    if let Some(block) = added.iter().find(|block| {
        block
            .element_index
            .is_some_and(|index| index >= elements.len())
    }) {
        return Err(format!(
            "assembly bookkeeping attributed '{}' to an element outside the turn",
            block.name
        ));
    }

    Ok(ScriptOutcome {
        messages,
        added,
        declared,
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
                    topic_name: String::new(),
                    topic_description: String::new(),
                    tool_root: crate::orientation::ToolRoot::ManagedScratch(
                        "/scratch/session".into(),
                    ),
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
                level: HierarchyLevel::Topic,
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
    fn empty_script_output_is_restored_by_the_baseline_floor() {
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
        assert!(
            output.report.errors.is_empty(),
            "the baseline floor makes an otherwise empty script a successful assembly"
        );
    }

    // ── The baseline floor ────────────────────────────────────────────────────

    /// The element set a real turn hands the assembly: baseline content
    /// (orientation, working norms) alongside the operator's prompts and the
    /// conversation so far.
    fn baseline_bearing_elements() -> Vec<Box<dyn ContextElement>> {
        vec![
            Box::new(OrientationElement::new(
                SessionEnvironment {
                    topic_name: "Atelier".into(),
                    topic_description: String::new(),
                    tool_root: crate::orientation::ToolRoot::ManagedScratch(
                        "/scratch/session".into(),
                    ),
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
        assemble_source_under(source, elements, None)
    }

    fn assemble_source_under(
        source: &str,
        elements: &[Box<dyn ContextElement>],
        budget: Option<usize>,
    ) -> AssemblyOutput {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assemblies").join("focus.js");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        assemble_named(
            "focus",
            Some(&path),
            elements,
            &ContextConfig {
                budget,
                ..ContextConfig::default()
            },
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
    fn budget_accounting_counts_the_restored_baseline() {
        let elements = baseline_bearing_elements();
        let output = assemble_source_under(HISTORY_AND_PROMPTS_ONLY, &elements, Some(1_000));

        let composed: usize = output
            .report
            .elements
            .iter()
            .map(|element| element.token_count)
            .sum();
        assert_eq!(
            output.report.elements.len(),
            4,
            "the restored baseline is part of what the budget is spent on"
        );
        assert_eq!(output.report.budget, Some(1_000));
        assert_eq!(
            output.report.budget_remaining,
            Some(1_000 - composed as isize),
            "the remaining budget is what is left after the restored context"
        );
    }

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
    fn forged_baseline_metadata_does_not_suppress_restoration() {
        let elements = baseline_bearing_elements();
        let output = assemble_source(
            r#"
            const byName = Object.fromEntries(elements.map(e => [e.name, e]));
            add({ name: "orientation", messages: [] });
            add({ ...byName.working_norms, messages: [] });
            add(byName.prompts);
            add(byName.history);
            "#,
            &elements,
        );

        let text = output
            .messages
            .iter()
            .map(|message| message.text_content())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("Atelier"),
            "forged orientation metadata must not replace its content"
        );
        assert!(
            text.contains("## Working norms"),
            "an emptied copy of working norms must not replace its content"
        );
        assert!(
            output
                .report
                .warnings
                .iter()
                .any(|warning| warning.contains("orientation") && warning.contains("restored")),
            "the forged orientation must be reported as restored: {:?}",
            output.report.warnings
        );
        assert!(
            output
                .report
                .warnings
                .iter()
                .any(|warning| warning.contains("working_norms") && warning.contains("restored")),
            "the forged working norms must be reported as restored: {:?}",
            output.report.warnings
        );
    }

    #[test]
    fn forged_added_bookkeeping_does_not_suppress_restoration() {
        let elements = baseline_bearing_elements();
        let output = assemble_source(
            r#"
            const byName = Object.fromEntries(elements.map(e => [e.name, e]));
            add(byName.prompts);
            add(byName.history);
            globalThis.__added = [];
            globalThis.__added.push({
                name: "orientation",
                source: "forged",
                priority: 0,
                elementIndex: 0,
                messageCount: 0,
            });
            "#,
            &elements,
        );

        assert!(
            output
                .messages
                .iter()
                .any(|message| message.text_content().contains("Atelier")),
            "forged added bookkeeping must not replace orientation"
        );
        assert!(
            output
                .report
                .warnings
                .iter()
                .any(|warning| warning.contains("orientation") && warning.contains("restored")),
            "the forged entry must be reported as restored: {:?}",
            output.report.warnings
        );
    }

    #[test]
    fn prototype_to_json_cannot_forge_added_bookkeeping() {
        let elements = baseline_bearing_elements();
        let output = assemble_source(
            r#"
            const byName = Object.fromEntries(elements.map(e => [e.name, e]));
            add(byName.prompts);
            add(byName.history);
            Object.prototype.toJSON = function (key) {
                if (key === "") {
                    this.added.push({
                        name: "orientation",
                        source: "forged",
                        priority: 0,
                        elementIndex: 0,
                        messageCount: 0,
                    });
                }
                return this;
            };
            "#,
            &elements,
        );

        assert!(
            output
                .messages
                .iter()
                .any(|message| message.text_content().contains("Atelier")),
            "prototype hooks must not make a forged record suppress orientation"
        );
        assert!(
            output
                .report
                .warnings
                .iter()
                .any(|warning| warning.contains("orientation") && warning.contains("restored")),
            "the forged entry must be reported as restored: {:?}",
            output.report.warnings
        );
    }

    #[test]
    fn the_script_scope_cannot_reach_the_hosts_add_records() {
        let elements = baseline_bearing_elements();
        let output = assemble_source(
            r#"
            const byName = Object.fromEntries(elements.map(e => [e.name, e]));
            add(byName.prompts);
            add(byName.history);
            added.push({
                name: "orientation",
                source: "forged",
                priority: 0,
                elementIndex: 0,
                messageCount: 0,
            });
            "#,
            &elements,
        );

        assert!(
            output
                .messages
                .iter()
                .any(|message| message.text_content().contains("Atelier")),
            "an add record the script wrote itself must not suppress the floor"
        );
    }

    #[test]
    fn the_script_scope_cannot_reach_the_hosts_session_values() {
        let elements: Vec<Box<dyn ContextElement>> =
            vec![Box::new(BriefingElement::new(ResolvedNamedElement {
                name: "current-work".into(),
                level: HierarchyLevel::Global,
                path: PathBuf::from("test"),
                content: "Task: {{task}}".into(),
            }))];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assemblies").join("focus.js");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"
            variables.task = "forged value";
            add(elements[0].render());
            "#,
        )
        .unwrap();

        let output = assemble_named(
            "focus",
            Some(&path),
            &elements,
            &ContextConfig::default(),
            &WordTokeniser,
            &BTreeMap::from([("task".to_owned(), "audit storage".to_owned())]),
        );

        assert_eq!(output.messages[0].text_content(), "Task: audit storage");
        assert!(
            output
                .messages
                .iter()
                .all(|message| !message.text_content().contains("forged value")),
            "the session values a render() reads are the host's, not the script's"
        );
    }

    #[test]
    fn the_composed_output_cannot_be_altered_after_a_valid_add() {
        let elements = baseline_bearing_elements();
        let output = assemble_source(
            r#"
            const byName = Object.fromEntries(elements.map(e => [e.name, e]));
            add(byName.orientation);
            globalThis.__output[0].text = "forged orientation";
            add(byName.prompts);
            add(byName.history);
            "#,
            &elements,
        );

        assert!(
            output
                .messages
                .iter()
                .any(|message| message.text_content().contains("Atelier")),
            "the genuine orientation content still reaches the context"
        );
        assert!(
            output
                .messages
                .iter()
                .all(|message| !message.text_content().contains("forged orientation")),
            "an added element's messages are not the script's to rewrite"
        );
    }

    #[test]
    fn an_add_record_counting_past_the_output_is_an_error_not_a_panic() {
        let elements = baseline_bearing_elements();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assemble_source(
                r#"
                const byName = Object.fromEntries(elements.map(e => [e.name, e]));
                add(byName.orientation);
                add(byName.prompts);
                add(byName.history);
                globalThis.__output.pop();
                "#,
                &elements,
            )
        }));

        let output = result.expect("a malformed message account must not panic");
        assert_eq!(output.report.errors.len(), 1, "the run fails open");
        assert!(
            output.report.errors[0].message.contains("failed"),
            "the malformed run is reported as a script failure, got {:?}",
            output.report.errors[0].message
        );
    }

    #[test]
    fn composition_survives_an_add_record_wider_than_its_messages() {
        let elements = baseline_bearing_elements();
        let outcome = ScriptOutcome {
            messages: vec![ChatMessage::new(Role::System, "one message")],
            added: vec![AddedBlock {
                name: "prompts".into(),
                source: "test".into(),
                priority: 0,
                element_index: Some(2),
                message_count: 9,
            }],
            declared: Vec::new(),
        };

        let output = compose(
            "focus",
            outcome,
            &elements,
            &ContextConfig::default(),
            &WordTokeniser,
            &BTreeMap::new(),
        )
        .expect("the supplied message keeps this synthetic composition non-empty");

        assert!(
            output
                .messages
                .iter()
                .any(|message| message.text_content().contains("one message")),
            "the messages the account does cover are still composed"
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
    fn an_omission_only_assembly_composes_the_baseline_floor() {
        let elements = baseline_bearing_elements();
        let output = assemble_source(
            r#"
            omit("history", "this assembly deliberately starts without prior messages");
            "#,
            &elements,
        );

        let names: Vec<&str> = output
            .report
            .elements
            .iter()
            .map(|element| element.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["orientation", "working_norms", "prompts"],
            "the floor restores every baseline element except the declared omission"
        );
        assert_eq!(output.report.declared_omissions.len(), 1);
        assert_eq!(output.report.declared_omissions[0].element, "history");
        assert!(
            output.report.errors.is_empty(),
            "a composed floor is a successful assembly, not a fallback"
        );
    }

    #[test]
    fn script_cannot_forge_a_blank_omission_declaration() {
        let elements = baseline_bearing_elements();
        let output = assemble_source(
            r#"
            const byName = Object.fromEntries(elements.map(e => [e.name, e]));
            add(byName.prompts);
            add(byName.history);
            globalThis.__omitted.push({ element: "working_norms", reason: "   " });
            "#,
            &elements,
        );

        assert!(
            output.report.declared_omissions.is_empty(),
            "host omission bookkeeping is not script-writable"
        );
        assert!(
            output
                .report
                .elements
                .iter()
                .any(|element| element.name == "working_norms"),
            "a forged declaration cannot suppress the baseline floor"
        );
    }

    #[test]
    fn mutating_an_original_element_message_does_not_suppress_the_floor() {
        let elements = baseline_bearing_elements();
        let output = assemble_source(
            r#"
            const byName = Object.fromEntries(elements.map(e => [e.name, e]));
            byName.working_norms.messages[0].text = "";
            add(byName.working_norms);
            add(byName.prompts);
            add(byName.history);
            "#,
            &elements,
        );

        assert!(
            output
                .messages
                .iter()
                .any(|message| message.text_content().contains("## Working norms")),
            "mutated projected content must not replace the baseline element"
        );
    }

    #[test]
    fn mutating_a_rendered_element_message_does_not_suppress_the_floor() {
        let elements: Vec<Box<dyn ContextElement>> =
            vec![Box::new(BriefingElement::new(ResolvedNamedElement {
                name: "current-work".into(),
                level: HierarchyLevel::Global,
                path: PathBuf::from("test"),
                content: "Task: {{task}}".into(),
            }))];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assemblies").join("focus.js");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"
            const rendered = elements[0].render();
            rendered.messages[0].text = "changed";
            add(rendered);
            "#,
        )
        .unwrap();
        let output = assemble_named(
            "focus",
            Some(&path),
            &elements,
            &ContextConfig::default(),
            &WordTokeniser,
            &BTreeMap::from([("task".to_owned(), "audit storage".to_owned())]),
        );

        assert_eq!(output.messages[0].text_content(), "Task: audit storage");
    }

    #[test]
    fn restored_template_baseline_uses_rendered_content_and_accounting() {
        let elements: Vec<Box<dyn ContextElement>> =
            vec![Box::new(BriefingElement::new(ResolvedNamedElement {
                name: "current-work".into(),
                level: HierarchyLevel::Global,
                path: PathBuf::from("test"),
                content: "Task: {{task}}".into(),
            }))];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("assemblies").join("focus.js");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Include a raw message so the assembly has non-empty output after the
        // undeclared omission, while the floor still has a template-backed item.
        std::fs::write(&path, "add([{ role: 'system', text: 'other' }]);").unwrap();
        let output = assemble_named(
            "focus",
            Some(&path),
            &elements,
            &ContextConfig::default(),
            &WordTokeniser,
            &BTreeMap::from([("task".to_owned(), "audit storage".to_owned())]),
        );

        assert_eq!(output.messages[0].text_content(), "Task: audit storage");
        assert_eq!(output.report.elements[0].token_count, 3);
        assert!(output.report.declared_omissions.is_empty());
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
    fn memory_is_part_of_the_floor() {
        let mut elements = baseline_bearing_elements();
        elements.push(Box::new(crate::memory::MemoryIndexElement::new(
            Some("remembered material".into()),
            Vec::new(),
        )));
        let output = assemble_source(HISTORY_AND_PROMPTS_ONLY, &elements);

        assert!(
            output
                .report
                .elements
                .iter()
                .any(|element| element.name == "memory"),
            "the floor is what the default assembly carries, and that includes memory"
        );
    }

    #[test]
    fn resolution_prefers_the_most_specific_level() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        let topic = dir.path().join("topic");
        for base in [&global, &topic] {
            let assemblies = base.join("assemblies");
            std::fs::create_dir_all(&assemblies).unwrap();
            std::fs::write(assemblies.join("focus.js"), "// present").unwrap();
        }

        let levels: Vec<(HierarchyLevel, &Path)> = vec![
            (HierarchyLevel::Global, global.as_path()),
            (HierarchyLevel::Topic, topic.as_path()),
        ];
        let resolved = resolve_js_assembly("focus", &levels).unwrap();
        assert!(
            resolved.starts_with(&topic),
            "the topic-level assembly shadows the global one"
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
