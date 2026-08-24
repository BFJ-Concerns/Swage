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
//!
//! A minimal reordering assembly:
//!
//! ```js
//! const byName = Object.fromEntries(elements.map(e => [e.name, e]));
//! add(byName.orientation);
//! add(byName.prompts);
//! add(byName.history);
//! ```

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

use crate::assembly::{default_assembly_with_variables, safe_metadata_preview};
use crate::element::ContextElement;
use crate::report::{AssemblyError, AssemblyOutput};
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
        Ok((messages, _)) if messages.is_empty() => fallback(
            name,
            format!("assembly '{name}' produced no messages"),
            elements,
            config,
            tokeniser,
            variables,
        ),
        Ok((messages, added)) => {
            // The report describes what actually ran: one element row per
            // add() call, in composition order, attributed to the element
            // the script added (or to the script itself for raw messages).
            // Everything the default assembly would have done — compaction,
            // budget enforcement — did not happen here and is reported as
            // such rather than inherited.
            let started = Instant::now();
            let mut report = crate::report::AssemblyReport::new(name);
            let mut offset = 0usize;
            report.elements = added
                .iter()
                .map(|block| {
                    let slice = &messages[offset..offset + block.message_count];
                    offset += block.message_count;
                    crate::report::ElementReport {
                        name: block.name.clone(),
                        source: block.source.clone(),
                        token_count: tokeniser.count_messages(slice),
                        preview: safe_metadata_preview(&block.name, &block.source),
                        priority: block.priority,
                        named_assembly: None,
                    }
                })
                .collect();
            report.total_tokens = tokeniser.count_messages(&messages);
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
            AssemblyOutput::new_unchecked(messages, report)
        }
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
) -> Result<(Vec<ChatMessage>, Vec<AddedBlock>), String> {
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

    context.with(|ctx| -> Result<(Vec<ChatMessage>, Vec<AddedBlock>), String> {
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
        Ok((messages, added))
    })
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
        .map(|(messages, _)| messages)
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
        let (messages, added) = run_script(
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
            messages[0].text_content(),
            "Do audit storage for ⟦unset:owner⟧; direct=audit storage"
        );
        assert_eq!(messages[1].text_content(), "audit storage|true");
        assert_eq!(
            added[0].name, "current-work",
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
        let (messages, added) = run_script(
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

        assert_eq!(messages.len(), 3);
        assert_eq!(added.len(), 2);
        assert_eq!(added[0].name, "history");
        assert_eq!(added[0].message_count, 2);
        assert_eq!(
            added[1].name, "script",
            "raw messages attribute to the script"
        );
        // The prompts element was never added and must not appear.
        assert!(added.iter().all(|block| block.name != "prompts"));
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
        .map(|(messages, _)| messages);
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

        assert_eq!(messages.0.len(), 3);
        assert_eq!(messages.0[1].text_content(), "operator instructions");
        assert_eq!(messages.0[2].text_content(), "hello");
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
