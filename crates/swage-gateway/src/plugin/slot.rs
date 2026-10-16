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
use swage_context::tokeniser::Tokeniser;
use swage_core::{ChatMessage, CompletionRequest, Role};
use swage_plugin::{HostCallFailure, InvocationContext, PluginHost, PluginRuntime};
use swage_protocol::client::turn_graph::{HookAttachment, HookMode, HookPoint, TurnGraph};
use swage_storage::model::NewAuditEvent;
use swage_tool::context::ToolContext;
use swage_tool::dispatch::{CallHookDecision, ToolCallHook};
use uuid::Uuid;

use crate::state::AppState;

/// Ask the gateway's active provider for a completion on a plugin's
/// behalf and record it as the plugin's: `request` is
/// `{ messages: [{ role, content }], model?, max_tokens?, temperature? }`.
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
            message: "complete takes { messages: [{ role, content }], model?, max_tokens?, temperature? }".into(),
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
    let max_tokens = request
        .get("max_tokens")
        .and_then(Value::as_u64)
        .and_then(|limit| u32::try_from(limit).ok());
    let temperature = request.get("temperature").and_then(Value::as_f64);
    let completion = CompletionRequest {
        messages,
        model: model.clone(),
        effort: None,
        max_tokens,
        temperature,
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
    /// provider would get, as `{ messages, model, max_tokens, temperature }`
    /// — goes to the plugin, which answers `{ summary }`.
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
            "max_tokens": completion.max_tokens,
            "temperature": completion.temperature,
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
    /// `{ elements: [{ name, messages }], variables }`, each message in the
    /// conversation's own shape (`role`, `content` parts, a tool result's
    /// `tool_call_id`, an assistant turn's `tool_calls`), and it answers
    /// `{ messages }` — each a message handed back as given, or a fresh
    /// `{ role: system | user | assistant, content: text }`. A message in
    /// neither shape, or a `tool` message without its call, is the slot's
    /// failure, never a silent recast. The report is the kernel's over the
    /// same elements — the elements the program was given — with the
    /// messages the plugin composed and their own token count.
    ///
    /// The assembly runs on the turn's thread synchronously, so the isolate
    /// is awaited from here through `block_in_place`, which the
    /// multi-thread runtime the gateway runs on allows; on a
    /// current-thread runtime the slot cannot run and says so, so the
    /// kernel's assembly serves there rather than a panic.
    pub fn context_program(
        &self,
        elements: &[Box<dyn ContextElement>],
        variables: &std::collections::BTreeMap<String, String>,
        tokeniser: &dyn Tokeniser,
        report_of: impl FnOnce() -> AssemblyOutput,
    ) -> Result<AssemblyOutput, String> {
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| format!("{}: no async runtime to run the slot on", self.plugin))?;
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread {
            return Err(format!(
                "{}: the context-program slot needs the multi-thread runtime",
                self.plugin
            ));
        }
        let input = json!({
            "elements": elements
                .iter()
                .map(|element| json!({
                    "name": element.name(),
                    "messages": element.content(),
                }))
                .collect::<Vec<_>>(),
            "variables": variables,
        });
        let output = tokio::task::block_in_place(|| {
            handle.block_on(self.runtime.invoke_slot(
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
            .enumerate()
            .map(|(index, message)| {
                composed_message(message).map_err(|reason| {
                    format!(
                        "{}: message {index} of the composition {reason}",
                        self.plugin
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if messages.is_empty() {
            return Err(format!("{}: the slot composed no messages", self.plugin));
        }
        let mut assembled = report_of();
        assembled.report.total_tokens = tokeniser.count_messages(&messages);
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

/// A message of a plugin's composition: one of the conversation's own
/// handed back whole, or a fresh text message under a conversational role.
fn composed_message(message: &Value) -> Result<ChatMessage, String> {
    if let Ok(whole) = serde_json::from_value::<ChatMessage>(message.clone()) {
        return Ok(whole);
    }
    let role = match message.get("role").and_then(Value::as_str) {
        Some("system") => Role::System,
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        Some("tool") => {
            return Err(
                "is a tool result without its call: a tool message is handed back as given, never composed afresh"
                    .into(),
            );
        }
        other => return Err(format!("has no conversational role: {other:?}")),
    };
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| "has no text content".to_owned())?;
    Ok(ChatMessage::new(role, content))
}

/// Who records what a `tool_call` hook rewrote: the audit trail, keyed to
/// the call the model raised, so the call record (the model's call, with
/// the result of what ran) and this event together reconstruct the turn.
pub type RewriteRecorder = Arc<dyn Fn(RewrittenCall) + Send + Sync>;

/// A tool call as the model raised it and as a hook left it.
#[derive(Debug, Clone)]
pub struct RewrittenCall {
    pub call_id: Option<String>,
    pub plugin: String,
    pub from_name: String,
    pub from_arguments: Value,
    pub to_name: String,
    pub to_arguments: Value,
}

/// The plugin hooks attached at `tool_call`, in the revision's order, as
/// the dispatcher's call hook: each sees the call as the one before left it,
/// and each rewrite is recorded with the plugin that made it.
pub struct PluginCallHook {
    hooks: Vec<(HookMode, PluginSlot)>,
    recorder: Option<RewriteRecorder>,
}

impl PluginCallHook {
    pub fn new(hooks: Vec<(HookMode, PluginSlot)>, recorder: Option<RewriteRecorder>) -> Self {
        Self { hooks, recorder }
    }
}

#[async_trait]
impl ToolCallHook for PluginCallHook {
    async fn before_dispatch(
        &self,
        tool_name: &str,
        params: Value,
        ctx: &dyn ToolContext,
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
                    let to_name = event
                        .get("name")
                        .and_then(Value::as_str)
                        .map_or_else(|| name.clone(), str::to_owned);
                    let to_arguments = event.get("arguments").cloned().unwrap_or(json!({}));
                    if let Some(record) = &self.recorder {
                        record(RewrittenCall {
                            call_id: ctx.call_id().map(str::to_owned),
                            plugin: slot.plugin.clone(),
                            from_name: name.clone(),
                            from_arguments: params.clone(),
                            to_name: to_name.clone(),
                            to_arguments: to_arguments.clone(),
                        });
                    }
                    name = to_name;
                    params = to_arguments;
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

/// A rewrite recorder writing `tool_call_rewritten` to the audit trail
/// under `session_id`.
pub fn audit_rewrite_recorder(state: AppState, session_id: Option<Uuid>) -> RewriteRecorder {
    Arc::new(move |rewrite: RewrittenCall| {
        let storage = state.storage.clone();
        tokio::spawn(async move {
            if let Err(error) = storage
                .create_audit_event(NewAuditEvent {
                    id: Uuid::new_v4().to_string(),
                    session_id,
                    entity_kind: "plugin".to_owned(),
                    entity_id: rewrite.plugin.clone(),
                    event_kind: "tool_call_rewritten".to_owned(),
                    summary: format!(
                        "Plugin {} rewrote the tool call {} to {}",
                        rewrite.plugin, rewrite.from_name, rewrite.to_name
                    ),
                    metadata: json!({
                        "call_id": rewrite.call_id,
                        "from": { "name": rewrite.from_name, "arguments": rewrite.from_arguments },
                        "to": { "name": rewrite.to_name, "arguments": rewrite.to_arguments },
                    }),
                })
                .await
            {
                tracing::warn!(%error, "a hook's rewrite of a tool call could not be recorded");
            }
        });
    })
}

/// What the `message_inbound` hooks left: the content, and the plugins
/// whose rewrite changed it, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct InboundOutcome {
    pub content: String,
    pub rewritten_by: Vec<String>,
}

/// The plugin hooks attached at `message_inbound` over the raw content, in
/// order; a rewrite is the content the next sees, a veto refuses it. An
/// observer, and a handler that failed, leave no mark on the attribution.
pub async fn inbound_hooks(
    hooks: &[(HookMode, PluginSlot)],
    content: String,
) -> Result<InboundOutcome, String> {
    let mut content = content;
    let mut rewritten_by = Vec::new();
    for (mode, slot) in hooks {
        match slot
            .hook("message_inbound", *mode, json!({ "content": content }))
            .await
        {
            HookAction::Continue => {}
            HookAction::Rewrite(event) => {
                if let Some(rewritten) = event.get("content").and_then(Value::as_str)
                    && rewritten != content
                {
                    content = rewritten.to_owned();
                    rewritten_by.push(slot.plugin.clone());
                }
            }
            HookAction::Veto(reason) => return Err(format!("{}: {reason}", slot.plugin)),
        }
    }
    Ok(InboundOutcome {
        content,
        rewritten_by,
    })
}

/// The plugin hooks at an observe-only point (`turn_start`, `turn_end`):
/// each sees the event; nothing it answers changes the turn.
pub async fn observe(hooks: &[(HookMode, PluginSlot)], point: &str, event: Value) {
    for (_, slot) in hooks {
        slot.hook(point, HookMode::Observe, event.clone()).await;
    }
}

/// The hooks a revision attaches at one point, as they stand for one turn:
/// the attachments whose plugin serves, as slots, and the attachments
/// skipped because their plugin does not — the revision validated against
/// what was installed when it activated, and a plugin gone or switched off
/// since is not a veto. A turn reports both.
#[derive(Default)]
pub struct TurnHooks {
    pub attached: Vec<(HookMode, PluginSlot)>,
    /// The attachments behind `attached`, as declared.
    pub serving: Vec<HookAttachment>,
    pub skipped: Vec<HookAttachment>,
}

/// What a turn reports of its hooks: every attachment that served and every
/// one skipped, across the points it resolved.
#[derive(Default, Debug, Clone)]
pub struct TurnHookAccount {
    pub attached: Vec<HookAttachment>,
    pub skipped: Vec<HookAttachment>,
}

impl TurnHookAccount {
    pub fn note(&mut self, hooks: &TurnHooks) {
        self.attached.extend(hooks.serving.iter().cloned());
        self.skipped.extend(hooks.skipped.iter().cloned());
    }
}

/// The hooks `graph` attaches at `point`, for a session in `topic_id`.
pub fn hooks_for_turn(
    state: &AppState,
    graph: &TurnGraph,
    point: HookPoint,
    context: &InvocationContext,
    topic_id: Option<Uuid>,
) -> TurnHooks {
    let mut hooks = TurnHooks::default();
    for attachment in graph.hooks_at(point) {
        match slot_for_plugin(state, &attachment.plugin, context, topic_id) {
            Ok(slot) => {
                hooks.attached.push((attachment.mode, slot));
                hooks.serving.push(attachment);
            }
            Err(_) => hooks.skipped.push(attachment),
        }
    }
    hooks
}

/// What a slot's draw settled on: the plugin to resolve, and why it is the
/// named filler rather than the draw's own answer where the draw could not
/// stand.
pub struct SlotDraw {
    pub name: String,
    pub refusal: Option<String>,
}

/// The plugin that fills `slot` this turn: the revision's named filler, or
/// an installed variant of the slot's function drawn in its place. A plugin
/// declaring `variant_of = "slot.<slot>"` is a candidate wherever it is
/// enabled; the draw runs through the experimentation seam at location
/// `slot.<slot>` with the named filler as the default, so a topic's
/// configuration pins a variant over the shipped part, experimentation off
/// keeps the named filler, and every draw — a default-only one included —
/// is recorded. A session's recorded choice serves while it is the named
/// filler or still a candidate for the scope; a choice that is neither (the
/// variant disabled or gone since, the revision's filler changed) gives way
/// to the named filler, as does a draw the seam refuses, each with its
/// reason for the turn's record.
pub async fn slot_variant(
    state: &AppState,
    config: &swage_config::model::ExperimentationConfig,
    slot: swage_protocol::client::turn_graph::TurnSlot,
    named: &str,
    session_id: Uuid,
    turn_id: Option<Uuid>,
    topic_id: Option<Uuid>,
) -> SlotDraw {
    let location = format!("slot.{}", slot.as_str());
    let candidates = state.plugins.variants_of(&location, topic_id);
    let registered: Vec<&str> = candidates.iter().map(String::as_str).collect();
    match crate::experimentation::select_session_variant(
        state,
        config,
        &location,
        &registered,
        named,
        session_id,
        turn_id,
    )
    .await
    {
        Ok(decision) if decision.variant == named || candidates.contains(&decision.variant) => {
            SlotDraw {
                name: decision.variant,
                refusal: None,
            }
        }
        Ok(decision) => {
            let refusal = format!(
                "the session's recorded choice '{}' at {location} is neither the named filler nor a candidate for this scope now",
                decision.variant
            );
            tracing::warn!(slot = slot.as_str(), %refusal, "the named filler serves");
            SlotDraw {
                name: named.to_owned(),
                refusal: Some(refusal),
            }
        }
        Err(error) => {
            let refusal = format!("the slot's variant could not be drawn: {error}");
            tracing::warn!(slot = slot.as_str(), %refusal, "the named filler serves");
            SlotDraw {
                name: named.to_owned(),
                refusal: Some(refusal),
            }
        }
    }
}

/// A plugin's isolate as a slot for one turn, when it is installed,
/// enabled for the session's scope, and serving as an isolate; otherwise
/// why it cannot serve, for the turn's record.
pub fn slot_for_plugin(
    state: &AppState,
    plugin: &str,
    context: &InvocationContext,
    topic_id: Option<Uuid>,
) -> Result<PluginSlot, String> {
    match state.plugins.execution_of(plugin, topic_id) {
        Some(crate::plugin::Execution::Isolate(runtime)) => Ok(PluginSlot::new(
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
            let reason =
                format!("plugin '{plugin}' runs as a process, which cannot fill a slot or hook");
            tracing::warn!(plugin, %reason, "skipped");
            Err(reason)
        }
        None => {
            let reason = format!("plugin '{plugin}' is not serving or not enabled for this scope");
            tracing::warn!(plugin, %reason, "skipped");
            Err(reason)
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

    struct History(Vec<ChatMessage>);

    impl ContextElement for History {
        fn name(&self) -> &str {
            "history"
        }
        fn priority(&self) -> i32 {
            0
        }
        fn content(&self) -> Vec<ChatMessage> {
            self.0.clone()
        }
        fn token_count(&self, _: &dyn swage_context::tokeniser::Tokeniser) -> usize {
            self.0.len()
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

    /// The shipped summariser forwards the step's request to the model
    /// under its grant and answers the model's content — the kernel's own
    /// behaviour, on the plugin contract.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_shipped_summariser_answers_the_request_through_its_model_grant() {
        let host = Arc::new(SummarisingHost::default());
        let manifest = PluginManifest::parse(include_str!(
            "../../../../shipped/plugins/succession-summariser/plugin.toml"
        ))
        .unwrap();
        let runtime = PluginRuntime::start(
            manifest,
            include_str!("../../../../shipped/plugins/succession-summariser/plugin.js").to_owned(),
        )
        .await
        .expect("starts");
        let slot = PluginSlot::new(
            "succession-summariser".into(),
            runtime,
            host.clone(),
            context(),
        );
        let completion = CompletionRequest {
            messages: vec![
                ChatMessage::new(Role::System, "Summarise."),
                ChatMessage::new(Role::User, "one"),
            ],
            model: Some("m".into()),
            effort: None,
            max_tokens: Some(1024),
            temperature: Some(0.3),
            stop_sequences: None,
            metadata: None,
            tool_definitions: Vec::new(),
        };
        let summary = slot.summarise(completion).await.expect("summarises");
        assert_eq!(summary, "the gist");
        let asked = host.completions.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0]["model"], "m");
        assert_eq!(asked[0]["messages"][0]["content"], "Summarise.");
        assert_eq!(
            asked[0]["max_tokens"], 1024,
            "the kernel's limits travel with the request"
        );
        assert_eq!(asked[0]["temperature"], 0.3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_plugin_context_program_composes_the_messages_over_the_elements() {
        let composer = slot(
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
            Box::new(History(vec![
                ChatMessage::new(Role::User, "hello"),
                ChatMessage::tool_result("call-1", "the tool's answer"),
            ])),
        ];
        let mut variables = std::collections::BTreeMap::new();
        variables.insert("who".to_owned(), "ben".to_owned());
        let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("gpt-4o");
        let kernel = || {
            swage_context::assembly::default_assembly_with_variables(
                &elements,
                &swage_config::ResolvedConfig::default().context,
                &tokeniser,
                &variables,
            )
        };
        let output = composer
            .context_program(&elements, &variables, &tokeniser, kernel)
            .expect("composes");
        assert_eq!(output.messages.len(), 4);
        assert_eq!(output.messages[0].role(), Role::System);
        assert_eq!(
            output.messages[0].text_content(),
            "Elements: prompts,history; who=ben"
        );
        assert_eq!(output.messages[2].text_content(), "hello");
        // A message handed back as given keeps its shape: the tool result
        // is still a tool result, answering its call.
        assert_eq!(output.messages[3].role(), Role::Tool);
        assert_eq!(output.messages[3].tool_call_id(), Some("call-1"));
        assert_eq!(
            output.report.total_tokens,
            tokeniser.count_messages(&output.messages),
            "the report counts the composed messages"
        );

        // A composition that invents a tool message, or a role the
        // conversation has none of, is the slot's failure by position.
        let lossy = slot(
            "lossy",
            "[[provides.slots]]\nname = \"context_program\"",
            r#"swage.provide.slot("context_program", (input) => ({
                messages: [{ role: "system", content: "fine" }, { role: "tool", content: "no call" }],
            }));"#,
            Arc::new(SummarisingHost::default()),
        )
        .await;
        let error = lossy
            .context_program(&elements, &variables, &tokeniser, kernel)
            .expect_err("refused");
        assert!(
            error.contains("message 1 of the composition is a tool result without its call"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn on_a_current_thread_runtime_the_context_program_slot_declines_rather_than_panics() {
        let slot = slot(
            "composer",
            "[[provides.slots]]\nname = \"context_program\"",
            r#"swage.provide.slot("context_program", (input) => ({ messages: [{ role: "system", content: "x" }] }));"#,
            Arc::new(SummarisingHost::default()),
        )
        .await;
        let elements: Vec<Box<dyn ContextElement>> = vec![Box::new(Element("prompts", "be brief"))];
        let variables = std::collections::BTreeMap::new();
        let tokeniser = swage_context::tokeniser::TiktokenTokeniser::new("gpt-4o");
        let error = slot
            .context_program(&elements, &variables, &tokeniser, || unreachable!())
            .expect_err("declines");
        assert!(error.contains("needs the multi-thread runtime"), "{error}");
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
        let rewrites = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&rewrites);
        let recorder: RewriteRecorder = Arc::new(move |rewrite: RewrittenCall| {
            recorded.lock().unwrap().push(rewrite);
        });
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
            .with_call_hook(Arc::new(PluginCallHook::new(
                hooks,
                Some(Arc::clone(&recorder)),
            )))
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
        // Each rewrite is recorded with the plugin that made it, from the
        // call as raised to the call as left.
        let recorded = rewrites.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].plugin, "guard");
        assert_eq!(
            (recorded[0].from_name.as_str(), recorded[0].to_name.as_str()),
            ("probe_read", "probe_denied")
        );
        assert_eq!(recorded[0].from_arguments, json!({ "path": "/etc" }));
        assert_eq!(recorded[1].to_arguments, json!({ "marked": true }));
        drop(recorded);

        // Attached as an observer, the same handler's rewrites and vetoes are
        // beyond its mode: the calls continue as raised.
        let observing = dispatcher_for(vec![(HookMode::Observe, guard)]);
        let ran = observing
            .dispatch("probe_shell", json!({}), &ctx, "agent-a")
            .await
            .expect("an observer cannot veto");
        assert_eq!(ran.result, json!({ "ran": "probe_shell", "with": {} }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_inbound_rewrite_is_attributed_to_the_plugin_that_made_it_alone() {
        let hook = |name: &'static str, script: &'static str| async move {
            slot(
                name,
                "[[provides.hooks]]\npoint = \"message_inbound\"\nmode = \"veto\"",
                script,
                Arc::new(SummarisingHost::default()),
            )
            .await
        };
        let watcher = hook(
            "watcher",
            r#"swage.provide.hook("message_inbound", (event) => ({ action: "rewrite", event: { content: "watched" } }));"#,
        )
        .await;
        let tidier = hook(
            "tidier",
            r#"swage.provide.hook("message_inbound", (event) => ({ action: "rewrite", event: { content: event.content.trim() } }));"#,
        )
        .await;
        let broken = hook(
            "broken",
            r#"swage.provide.hook("message_inbound", (event) => { throw new Error("no"); });"#,
        )
        .await;
        let outcome = inbound_hooks(
            &[
                (HookMode::Observe, watcher),
                (HookMode::Rewrite, tidier.clone()),
                (HookMode::Rewrite, broken),
            ],
            "  hello  ".into(),
        )
        .await
        .expect("continues");
        assert_eq!(outcome.content, "hello");
        assert_eq!(
            outcome.rewritten_by,
            vec!["tidier".to_owned()],
            "the observer could not rewrite and the failed handler did not"
        );
        let unchanged = inbound_hooks(&[(HookMode::Rewrite, tidier)], "hello".into())
            .await
            .expect("continues");
        assert!(
            unchanged.rewritten_by.is_empty(),
            "a rewrite to the same content is no rewrite"
        );
    }
}
