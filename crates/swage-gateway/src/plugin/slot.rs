//! A plugin filling a turn slot or attached at a hook point, as the gateway
//! runs it.
//!
//! A slot filler is a plugin's `swage.provide.slot(name, handler)`, invoked
//! with the step's input and answering with the step's output; a hook is
//! its `swage.provide.hook(point, handler)`, invoked with the event and
//! answering with an action. Both run in the plugin's isolate under a host
//! of their own: a slot runs before the session's tools are built, so its
//! host answers no tool calls, but it answers `complete` through the
//! gateway's active provider — recorded as the plugin's completion — and
//! writes to the log. A hook's action is read against the mode its
//! attachment declares: a rewrite from an observer is a warning, not a
//! rewrite, and a rewritten tool call still meets the dispatcher's gates.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use swage_context::element::ContextElement;
use swage_context::report::AssemblyOutput;
use swage_context::succession::SuccessionError;
use swage_core::{ChatMessage, CompletionRequest, Role};
use swage_plugin::{HostCallFailure, InvocationContext, PluginHost, PluginRuntime};
use swage_protocol::client::turn_graph::HookMode;
use swage_storage::model::NewAuditEvent;
use swage_tool::context::ToolContext;
use swage_tool::dispatch::{CallHookDecision, ToolCallHook};
use uuid::Uuid;

use crate::state::AppState;

/// Ask the gateway's active provider for a completion on a plugin's
/// behalf and record it as the plugin's: `request` is
/// `{ messages: [{ role, content }], model? }`.
pub async fn complete_via_provider(
    state: &AppState,
    plugin: &str,
    session_id: Option<Uuid>,
    request: Value,
) -> Result<Value, HostCallFailure> {
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| HostCallFailure {
            name: "TypeError".into(),
            message: "complete takes { messages: [{ role, content }], model? }".into(),
        })?
        .iter()
        .map(|message| {
            let role = match message.get("role").and_then(Value::as_str) {
                Some("system") => Role::System,
                Some("user") => Role::User,
                Some("assistant") => Role::Assistant,
                other => {
                    return Err(HostCallFailure {
                        name: "TypeError".into(),
                        message: format!(
                            "a message's role must be system, user or assistant, not {other:?}"
                        ),
                    });
                }
            };
            let content = message
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Ok(ChatMessage::new(role, content))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let completion = CompletionRequest {
        messages,
        model: model.clone(),
        effort: None,
        max_tokens: None,
        temperature: None,
        stop_sequences: None,
        metadata: None,
        tool_definitions: Vec::new(),
    };
    let response = state
        .active_provider()
        .complete(completion)
        .await
        .map_err(|error| HostCallFailure {
            name: "ProviderError".into(),
            message: error.to_string(),
        })?;
    if let Err(error) = state
        .storage
        .create_audit_event(NewAuditEvent {
            id: Uuid::new_v4().to_string(),
            session_id,
            entity_kind: "plugin".to_owned(),
            entity_id: plugin.to_owned(),
            event_kind: "plugin_completion".to_owned(),
            summary: format!("Plugin {plugin} asked the model for a completion"),
            metadata: json!({
                "model": model,
                "input_tokens": response.input_tokens,
                "output_tokens": response.output_tokens,
            }),
        })
        .await
    {
        tracing::warn!(plugin, %error, "plugin completion could not be recorded");
    }
    Ok(json!({
        "content": response.content,
        "input_tokens": response.input_tokens,
        "output_tokens": response.output_tokens,
    }))
}

/// The host a slot or hook runs under: the log, the model under the
/// plugin's grant, and no session tools — a slot runs before they exist.
pub struct SlotHost {
    plugin: String,
    state: AppState,
    session_id: Option<Uuid>,
}

impl SlotHost {
    pub fn new(plugin: String, state: AppState, session_id: Option<Uuid>) -> Self {
        Self {
            plugin,
            state,
            session_id,
        }
    }
}

#[async_trait]
impl PluginHost for SlotHost {
    async fn call_tool(&self, name: &str, _: Value) -> Result<Value, HostCallFailure> {
        Err(HostCallFailure {
            name: "HostCapabilityError".into(),
            message: format!(
                "'{name}': a slot or hook runs before the session's tools are built and calls none"
            ),
        })
    }

    async fn complete(&self, request: Value) -> Result<Value, HostCallFailure> {
        complete_via_provider(&self.state, &self.plugin, self.session_id, request).await
    }

    fn log(&self, message: &str) {
        tracing::info!(plugin = %self.plugin, "{message}");
    }
}

/// A plugin's isolate as a slot filler or hook, for one turn.
#[derive(Clone)]
pub struct PluginSlot {
    pub plugin: String,
    runtime: Arc<PluginRuntime>,
    host: Arc<dyn PluginHost>,
    context: InvocationContext,
}

/// What a hook decided about an event.
#[derive(Debug, Clone, PartialEq)]
pub enum HookAction {
    Continue,
    Rewrite(Value),
    Veto(String),
}

impl PluginSlot {
    pub fn new(
        plugin: String,
        runtime: Arc<PluginRuntime>,
        host: Arc<dyn PluginHost>,
        context: InvocationContext,
    ) -> Self {
        Self {
            plugin,
            runtime,
            host,
            context,
        }
    }

    /// The succession slot: the summariser's request — the completion the
    /// provider would get — goes to the plugin, which answers
    /// `{ summary }`.
    pub async fn summarise(
        &self,
        completion: CompletionRequest,
    ) -> Result<String, SuccessionError> {
        let input = json!({
            "messages": completion
                .messages
                .iter()
                .map(|message| json!({ "role": role_name(message.role()), "content": message.text_content() }))
                .collect::<Vec<_>>(),
            "model": completion.model,
        });
        let output = self
            .runtime
            .invoke_slot("succession", &input, &self.context, Arc::clone(&self.host))
            .await
            .map_err(|error| SuccessionError::Summariser(format!("{}: {error}", self.plugin)))?;
        output
            .get("summary")
            .and_then(Value::as_str)
            .filter(|summary| !summary.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                SuccessionError::Summariser(format!(
                    "{}: the slot answered without a summary",
                    self.plugin
                ))
            })
    }

    /// The context-program slot: the turn's elements go to the plugin as
    /// `{ elements: [{ name, messages: [{ role, content }] }], variables }`,
    /// and it answers `{ messages: [{ role, content }] }`. The assembly runs
    /// on the turn's thread synchronously, so the isolate is awaited from
    /// here; the report is the default assembly's over the same elements,
    /// with the messages the plugin composed.
    pub fn context_program(
        &self,
        elements: &[Box<dyn ContextElement>],
        variables: &std::collections::BTreeMap<String, String>,
        report_of: impl FnOnce() -> AssemblyOutput,
    ) -> Result<AssemblyOutput, String> {
        let input = json!({
            "elements": elements
                .iter()
                .map(|element| json!({
                    "name": element.name(),
                    "messages": element
                        .content()
                        .iter()
                        .map(|message| json!({ "role": role_name(message.role()), "content": message.text_content() }))
                        .collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>(),
            "variables": variables,
        });
        let output = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.runtime.invoke_slot(
                "context_program",
                &input,
                &self.context,
                Arc::clone(&self.host),
            ))
        })
        .map_err(|error| format!("{}: {error}", self.plugin))?;
        let messages = output
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{}: the slot answered without messages", self.plugin))?
            .iter()
            .map(|message| {
                let role = match message.get("role").and_then(Value::as_str) {
                    Some("system") => Role::System,
                    Some("assistant") => Role::Assistant,
                    _ => Role::User,
                };
                ChatMessage::new(
                    role,
                    message
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                )
            })
            .collect::<Vec<_>>();
        if messages.is_empty() {
            return Err(format!("{}: the slot composed no messages", self.plugin));
        }
        let mut assembled = report_of();
        assembled.messages = messages;
        Ok(assembled)
    }

    /// A hook over `event` at `point`, read against the attachment's `mode`:
    /// an action the mode does not allow is a warning and a `Continue`.
    pub async fn hook(&self, point: &str, mode: HookMode, event: Value) -> HookAction {
        let answer = match self
            .runtime
            .invoke_hook(point, &event, &self.context, Arc::clone(&self.host))
            .await
        {
            Ok(answer) => answer,
            Err(error) => {
                tracing::warn!(plugin = %self.plugin, point, %error, "plugin hook failed; the event continues");
                return HookAction::Continue;
            }
        };
        match answer.get("action").and_then(Value::as_str) {
            Some("rewrite") if matches!(mode, HookMode::Rewrite | HookMode::Veto) => answer
                .get("event")
                .cloned()
                .map(HookAction::Rewrite)
                .unwrap_or_else(|| {
                    tracing::warn!(plugin = %self.plugin, point, "plugin hook rewrote without an event; the event continues");
                    HookAction::Continue
                }),
            Some("veto") if matches!(mode, HookMode::Veto) => HookAction::Veto(
                answer
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("vetoed")
                    .to_owned(),
            ),
            Some("continue") | None => HookAction::Continue,
            Some(other) => {
                tracing::warn!(plugin = %self.plugin, point, action = other, "plugin hook's action is beyond its mode; the event continues");
                HookAction::Continue
            }
        }
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// The plugin hooks attached at `tool_call`, in the revision's order, as
/// the dispatcher's call hook: each sees the call as the one before left it.
pub struct PluginCallHook {
    hooks: Vec<(HookMode, PluginSlot)>,
}

impl PluginCallHook {
    pub fn new(hooks: Vec<(HookMode, PluginSlot)>) -> Self {
        Self { hooks }
    }
}

#[async_trait]
impl ToolCallHook for PluginCallHook {
    async fn before_dispatch(
        &self,
        tool_name: &str,
        params: Value,
        _: &dyn ToolContext,
    ) -> CallHookDecision {
        let mut name = tool_name.to_owned();
        let mut params = params;
        for (mode, slot) in &self.hooks {
            match slot
                .hook(
                    "tool_call",
                    *mode,
                    json!({ "name": name, "arguments": params }),
                )
                .await
            {
                HookAction::Continue => {}
                HookAction::Rewrite(event) => {
                    if let Some(rewritten) = event.get("name").and_then(Value::as_str) {
                        name = rewritten.to_owned();
                    }
                    params = event.get("arguments").cloned().unwrap_or(json!({}));
                }
                HookAction::Veto(reason) => {
                    return CallHookDecision::Veto(format!("{}: {reason}", slot.plugin));
                }
            }
        }
        if name == tool_name {
            CallHookDecision::Continue(params)
        } else {
            CallHookDecision::Rewrite { name, params }
        }
    }
}

/// The plugin hooks attached at `message_inbound` over the raw content, in
/// order; a rewrite is the content the next sees, a veto refuses it.
pub async fn inbound_hooks(
    hooks: &[(HookMode, PluginSlot)],
    content: String,
) -> Result<String, String> {
    let mut content = content;
    for (mode, slot) in hooks {
        match slot
            .hook("message_inbound", *mode, json!({ "content": content }))
            .await
        {
            HookAction::Continue => {}
            HookAction::Rewrite(event) => {
                if let Some(rewritten) = event.get("content").and_then(Value::as_str) {
                    content = rewritten.to_owned();
                }
            }
            HookAction::Veto(reason) => return Err(format!("{}: {reason}", slot.plugin)),
        }
    }
    Ok(content)
}

/// The plugin hooks at an observe-only point (`turn_start`, `turn_end`):
/// each sees the event; nothing it answers changes the turn.
pub async fn observe(hooks: &[(HookMode, PluginSlot)], point: &str, event: Value) {
    for (_, slot) in hooks {
        slot.hook(point, HookMode::Observe, event.clone()).await;
    }
}

/// The hooks the active revision attaches at `point`, as slots for one
/// turn: an attachment whose plugin is not an isolate serving now is
/// skipped with a warning — the revision validated against what was
/// installed when it activated, and a plugin gone since is not a veto.
pub fn hooks_for_turn(
    state: &AppState,
    point: swage_protocol::client::turn_graph::HookPoint,
    context: &InvocationContext,
) -> Vec<(HookMode, PluginSlot)> {
    state
        .turn_graph
        .hooks_at(point)
        .into_iter()
        .filter_map(|attachment| {
            let slot = slot_for_plugin(state, &attachment.plugin, context)?;
            Some((attachment.mode, slot))
        })
        .collect()
}

/// A plugin's isolate as a slot for one turn, when it is installed and
/// serving as an isolate.
pub fn slot_for_plugin(
    state: &AppState,
    plugin: &str,
    context: &InvocationContext,
) -> Option<PluginSlot> {
    match state.plugins.execution_of(plugin) {
        Some(crate::plugin::Execution::Isolate(runtime)) => Some(PluginSlot::new(
            plugin.to_owned(),
            runtime,
            Arc::new(SlotHost::new(
                plugin.to_owned(),
                state.clone(),
                context.session_id.parse::<Uuid>().ok(),
            )),
            context.clone(),
        )),
        Some(crate::plugin::Execution::Process(_)) => {
            tracing::warn!(
                plugin,
                "a process plugin cannot fill a slot or hook; skipped"
            );
            None
        }
        None => {
            tracing::warn!(
                plugin,
                "the revision names a plugin that is not serving; skipped"
            );
            None
        }
    }
}

/// The context a slot or hook sees for one turn.
pub fn slot_context(
    session_id: Option<Uuid>,
    agent_id: &str,
    working_directory: &std::path::Path,
) -> InvocationContext {
    InvocationContext {
        session_id: session_id.map(|id| id.to_string()).unwrap_or_default(),
        agent_id: agent_id.to_owned(),
        call_id: None,
        working_directory: working_directory.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use swage_plugin::PluginManifest;
    use swage_safety::LeakDetector;
    use swage_tool::context::MockToolContext;
    use swage_tool::dispatch::ToolDispatcher;
    use swage_tool::permission::PermissionConfig;
    use swage_tool::rate_limit::{RateLimitConfig, RateLimiter};
    use swage_tool::registry::ToolRegistry;
    use swage_tool::tool::Tool;

    /// A host that answers every completion with one summary and logs.
    #[derive(Default)]
    struct SummarisingHost {
        completions: Mutex<Vec<Value>>,
    }

    #[async_trait]
    impl PluginHost for SummarisingHost {
        async fn call_tool(&self, name: &str, _: Value) -> Result<Value, HostCallFailure> {
            Err(HostCallFailure {
                name: "HostCapabilityError".into(),
                message: format!("no tools here: {name}"),
            })
        }
        async fn complete(&self, request: Value) -> Result<Value, HostCallFailure> {
            self.completions.lock().unwrap().push(request);
            Ok(json!({ "content": "the gist", "input_tokens": 1, "output_tokens": 1 }))
        }
        fn log(&self, _: &str) {}
    }

    fn context() -> InvocationContext {
        InvocationContext {
            session_id: Uuid::nil().to_string(),
            agent_id: "agent-a".into(),
            call_id: None,
            working_directory: "/work".into(),
        }
    }

    async fn slot(
        name: &str,
        provides: &str,
        script: &str,
        host: Arc<dyn PluginHost>,
    ) -> PluginSlot {
        let manifest = PluginManifest::parse(&format!(
            r#"
            name = "{name}"
            version = "1"
            description = "A {name}."
            script = "plugin.js"
            grants = ["model"]
            {provides}
            "#
        ))
        .unwrap();
        let runtime = PluginRuntime::start(manifest, script.to_owned())
            .await
            .expect("starts");
        PluginSlot::new(name.to_owned(), runtime, host, context())
    }

    struct Element(&'static str, &'static str);

    impl ContextElement for Element {
        fn name(&self) -> &str {
            self.0
        }
        fn priority(&self) -> i32 {
            0
        }
        fn content(&self) -> Vec<ChatMessage> {
            vec![ChatMessage::new(Role::User, self.1)]
        }
        fn token_count(&self, _: &dyn swage_context::tokeniser::Tokeniser) -> usize {
            self.1.len()
        }
        fn source(&self) -> String {
            "test".into()
        }
    }

    struct Named(&'static str);

    #[async_trait]
    impl Tool for Named {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "a session tool"
        }
        fn parameters(&self) -> Value {
            json!({ "type": "object" })
        }
        async fn execute(
            &self,
            params: Value,
            _: &dyn ToolContext,
        ) -> Result<Value, swage_core::ToolError> {
            Ok(json!({ "ran": self.0, "with": params }))
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_plugin_summariser_answers_the_succession_request_through_its_model_grant() {
        let host = Arc::new(SummarisingHost::default());
        let slot = slot(
            "summariser",
            "[[provides.slots]]\nname = \"succession\"",
            r#"swage.provide.slot("succession", async (input) => {
                const reply = await swage.complete({ messages: input.messages, model: input.model });
                return { summary: "Summary: " + reply.content + " (" + input.messages.length + " messages)" };
            });"#,
            host.clone(),
        )
        .await;
        let completion = CompletionRequest {
            messages: vec![
                ChatMessage::new(Role::System, "Summarise."),
                ChatMessage::new(Role::User, "one"),
            ],
            model: Some("m".into()),
            effort: None,
            max_tokens: None,
            temperature: None,
            stop_sequences: None,
            metadata: None,
            tool_definitions: Vec::new(),
        };
        let summary = slot.summarise(completion).await.expect("summarises");
        assert_eq!(summary, "Summary: the gist (2 messages)");
        let asked = host.completions.lock().unwrap().clone();
        assert_eq!(asked[0]["model"], "m");
        assert_eq!(asked[0]["messages"][1]["content"], "one");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_plugin_context_program_composes_the_messages_over_the_elements() {
        let slot = slot(
            "composer",
            "[[provides.slots]]\nname = \"context_program\"",
            r#"swage.provide.slot("context_program", (input) => ({
                messages: [
                    { role: "system", content: "Elements: " + input.elements.map((e) => e.name).join(",") + "; who=" + input.variables.who },
                    ...input.elements.flatMap((e) => e.messages),
                ],
            }));"#,
            Arc::new(SummarisingHost::default()),
        )
        .await;
        let elements: Vec<Box<dyn ContextElement>> = vec![
            Box::new(Element("prompts", "be brief")),
            Box::new(Element("history", "hello")),
        ];
        let mut variables = std::collections::BTreeMap::new();
        variables.insert("who".to_owned(), "ben".to_owned());
        let output = slot
            .context_program(&elements, &variables, || {
                swage_context::assembly::default_assembly_with_variables(
                    &elements,
                    &swage_config::ResolvedConfig::default().context,
                    &swage_context::tokeniser::TiktokenTokeniser::new("gpt-4o"),
                    &variables,
                )
            })
            .expect("composes");
        assert_eq!(output.messages.len(), 3);
        assert_eq!(output.messages[0].role(), Role::System);
        assert_eq!(
            output.messages[0].text_content(),
            "Elements: prompts,history; who=ben"
        );
        assert_eq!(output.messages[2].text_content(), "hello");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_call_hook_rewrites_or_vetoes_by_its_mode_and_the_gates_still_apply() {
        let guard = slot(
            "guard",
            "[[provides.hooks]]\npoint = \"tool_call\"\nmode = \"veto\"",
            r#"swage.provide.hook("tool_call", (event) => {
                if (event.name === "probe_shell") return { action: "veto", reason: "no shells" };
                if (event.name === "probe_read") return { action: "rewrite", event: { name: "probe_denied", arguments: event.arguments } };
                if (event.name === "probe_mark") return { action: "rewrite", event: { name: "probe_mark", arguments: { marked: true } } };
                return { action: "continue" };
            });"#,
            Arc::new(SummarisingHost::default()),
        )
        .await;
        let dispatcher_for = |hooks: Vec<(HookMode, PluginSlot)>| {
            let mut registry = ToolRegistry::new();
            for name in ["probe_shell", "probe_read", "probe_denied", "probe_mark"] {
                registry.register(Arc::new(Named(name))).unwrap();
            }
            ToolDispatcher::new(
                Arc::new(registry),
                PermissionConfig {
                    allow: vec!["*".into()],
                    deny: vec!["probe_denied".into()],
                    ..PermissionConfig::default()
                },
                Arc::new(RateLimiter::new()),
                RateLimitConfig::default(),
                Arc::new(LeakDetector::default()),
                false,
            )
            .with_call_hook(Arc::new(PluginCallHook::new(hooks)))
        };
        let ctx = MockToolContext::new("agent-a", "session");

        let attached = dispatcher_for(vec![(HookMode::Veto, guard.clone())]);
        let vetoed = attached
            .dispatch("probe_shell", json!({}), &ctx, "agent-a")
            .await
            .expect_err("vetoed");
        assert!(vetoed.to_string().contains("no shells"), "{vetoed}");
        let rewritten = attached
            .dispatch("probe_read", json!({ "path": "/etc" }), &ctx, "agent-a")
            .await
            .expect_err("the rewrite names a denied tool and the gate refuses it");
        assert!(
            matches!(rewritten, swage_core::ToolError::PermissionDenied { ref name, .. } if name == "probe_denied"),
            "{rewritten:?}"
        );
        let marked = attached
            .dispatch("probe_mark", json!({}), &ctx, "agent-a")
            .await
            .expect("a rewrite to an allowed tool runs as rewritten");
        assert_eq!(
            marked.result,
            json!({ "ran": "probe_mark", "with": { "marked": true } })
        );

        // Attached as an observer, the same handler's rewrites and vetoes are
        // beyond its mode: the calls continue as raised.
        let observing = dispatcher_for(vec![(HookMode::Observe, guard)]);
        let ran = observing
            .dispatch("probe_shell", json!({}), &ctx, "agent-a")
            .await
            .expect("an observer cannot veto");
        assert_eq!(ran.result, json!({ "ran": "probe_shell", "with": {} }));
    }
}
