//! One caged isolate per plugin, on a thread of its own.
//!
//! A [`PluginRuntime`] owns a thread that builds the plugin's isolate — the
//! cage, the host shim, then the plugin's script — and serves invocations
//! from a queue, one at a time. The script registers a handler per tool it
//! provides; an invocation runs that handler to completion and returns its
//! JSON result. Everything the handler wants from outside its isolate goes
//! through the host shim to the [`PluginHost`] the invocation carries, so
//! the embedding process answers every effect where its own gates apply.
//!
//! A termination — the heap or time limit, as `swage_isolate` reports it —
//! ends the isolate, not the plugin: the invocation fails with the reason,
//! the runtime records it, and the next invocation builds a fresh isolate
//! from the same script. One plugin's exhaustion is its own.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use swage_isolate::{HostCalls, IsolateError, Program, Termination};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::manifest::{HostGrant, PluginManifest};

/// Where a tool call comes from, as the handler sees it (`sessionId`,
/// `agentId`, `callId`, `workingDirectory`): the plugin-process contract's
/// context, shared with the isolate.
pub use swage_protocol::plugin::PluginCallContext as InvocationContext;

/// A host call the host refused or that failed. `name` is the error name
/// the script catches; `message` is what it reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCallFailure {
    pub name: String,
    pub message: String,
}

/// The embedding process's side of a plugin's door, supplied per invocation
/// so every effect is answered in the context of the call being served.
#[async_trait]
pub trait PluginHost: Send + Sync {
    /// Run one of the session's tools on the plugin's behalf. The host owns
    /// the gate: grants, approvals and the record apply here, and a refusal
    /// comes back as a failure the script can catch.
    async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, HostCallFailure>;
    /// Ask the session's model for a completion on the plugin's behalf:
    /// `request` is `{ messages: [{ role, content }], model? }`, the reply
    /// `{ content, input_tokens, output_tokens }`. The host records it as
    /// the plugin's. A host with no model refuses by name.
    async fn complete(&self, request: Value) -> Result<Value, HostCallFailure> {
        let _ = request;
        Err(HostCallFailure {
            name: "HostCapabilityError".into(),
            message: "this host answers no completions".into(),
        })
    }
    /// Context composition operations, under the `context` grant. Hosts
    /// without a turn's assembly inputs refuse the operation.
    async fn context(&self, request: Value) -> Result<Value, HostCallFailure> {
        let _ = request;
        Err(HostCallFailure {
            name: "HostCapabilityError".into(),
            message: "this host answers no context operations".into(),
        })
    }
    /// A line the plugin wants in the host's log.
    fn log(&self, message: &str);
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// The isolate was terminated at a limit; the next invocation runs on a
    /// fresh one.
    #[error("the plugin's isolate was terminated: {0}")]
    Terminated(Termination),
    /// The script threw, at load or while serving a call.
    #[error("{0}")]
    Script(String),
    #[error("the plugin engine failed: {0}")]
    Engine(String),
    /// The runtime's thread is gone, so nothing can serve the call.
    #[error("the plugin runtime is no longer serving")]
    Stopped,
    /// The plugin's process could not be started or spoken to.
    #[error("the plugin's process failed: {0}")]
    Process(String),
    /// The plugin's process ended while a call was in flight; the next
    /// call starts a fresh one.
    #[error("the plugin's process exited: {0}")]
    ProcessExited(String),
}

impl From<IsolateError> for PluginError {
    fn from(error: IsolateError) -> Self {
        match error {
            IsolateError::Terminated(termination) => Self::Terminated(termination),
            IsolateError::Script(exception) => Self::Script(exception.to_string()),
            IsolateError::Engine(message) => Self::Engine(message),
        }
    }
}

/// Whether the plugin's isolate is up, as the settings surface reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginRuntimeState {
    Serving,
    /// The last invocation ended the isolate; the next one rebuilds it.
    Terminated(Termination),
}

/// What a plugin is asked to run: a tool for the model, a turn slot in the
/// kernel's place, or a hook over a platform event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationKind {
    Tool,
    Slot,
    Hook,
}

impl InvocationKind {
    fn entry(self) -> &'static str {
        match self {
            InvocationKind::Tool => "invokeTool",
            InvocationKind::Slot => "invokeSlot",
            InvocationKind::Hook => "invokeHook",
        }
    }
}

struct Invocation {
    kind: InvocationKind,
    tool: String,
    params_json: String,
    context_json: String,
    host: Arc<dyn PluginHost>,
    /// The caller's runtime, on which the host's work runs: the isolate's
    /// thread only waits for it.
    caller: tokio::runtime::Handle,
    reply: oneshot::Sender<Result<String, PluginError>>,
}

/// A plugin's isolate and the thread it lives on.
pub struct PluginRuntime {
    name: String,
    invocations: mpsc::UnboundedSender<Invocation>,
    state: Arc<Mutex<PluginRuntimeState>>,
    /// Cancelled when the last owner drops: an invocation waiting on a host
    /// that has not answered ends, the isolate goes with it, and the host
    /// work it was waiting on is abandoned.
    cancellation: CancellationToken,
}

impl Drop for PluginRuntime {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl std::fmt::Debug for PluginRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRuntime")
            .field("name", &self.name)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl PluginRuntime {
    /// Build the plugin's isolate on a new thread and load its script. A
    /// script that throws at load, or that provides no handler for a tool the
    /// manifest declares, fails the start and leaves no thread behind.
    pub async fn start(manifest: PluginManifest, script: String) -> Result<Arc<Self>, PluginError> {
        let (invocations, mut queue) = mpsc::unbounded_channel::<Invocation>();
        let (loaded_tx, loaded_rx) = oneshot::channel::<Result<(), PluginError>>();
        let state = Arc::new(Mutex::new(PluginRuntimeState::Serving));
        let thread_state = Arc::clone(&state);
        let cancellation = CancellationToken::new();
        let thread_cancellation = cancellation.clone();
        let name = manifest.name.clone();
        std::thread::Builder::new()
            .name(format!("plugin-{name}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = loaded_tx.send(Err(PluginError::Engine(error.to_string())));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let host_calls = Arc::new(PluginHostCalls::new(&manifest));
                    let mut program =
                        match load_program(&manifest, &script, &host_calls, &thread_cancellation) {
                            Ok(program) => {
                                let _ = loaded_tx.send(Ok(()));
                                Some(program)
                            }
                            Err(error) => {
                                let _ = loaded_tx.send(Err(error));
                                return;
                            }
                        };
                    while let Some(invocation) = queue.recv().await {
                        if program.is_none() {
                            match load_program(
                                &manifest,
                                &script,
                                &host_calls,
                                &thread_cancellation,
                            ) {
                                Ok(fresh) => {
                                    program = Some(fresh);
                                    set_state(&thread_state, PluginRuntimeState::Serving);
                                }
                                Err(error) => {
                                    let _ = invocation.reply.send(Err(error));
                                    continue;
                                }
                            }
                        }
                        let live = program.as_mut().expect("the isolate was just built");
                        host_calls.enter(invocation.host, invocation.caller);
                        let result = invoke(
                            live,
                            invocation.kind,
                            &invocation.tool,
                            &invocation.params_json,
                            &invocation.context_json,
                        )
                        .await;
                        host_calls.leave();
                        if let Err(PluginError::Terminated(termination)) = &result {
                            set_state(&thread_state, PluginRuntimeState::Terminated(*termination));
                            program = None;
                        }
                        let _ = invocation.reply.send(result);
                    }
                });
            })
            .map_err(|error| PluginError::Engine(error.to_string()))?;
        loaded_rx.await.map_err(|_| PluginError::Stopped)??;
        Ok(Arc::new(Self {
            name,
            invocations,
            state,
            cancellation,
        }))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn state(&self) -> PluginRuntimeState {
        *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Run the handler the script registered for `tool` and return its
    /// result. Host calls the handler makes are answered by `host`, with the
    /// host's own work run on the calling tokio runtime.
    pub async fn invoke_tool(
        &self,
        tool: &str,
        params: &Value,
        context: &InvocationContext,
        host: Arc<dyn PluginHost>,
    ) -> Result<Value, PluginError> {
        self.invoke(InvocationKind::Tool, tool, params, context, host)
            .await
    }

    /// Run the handler the script registered for the turn slot `slot` with
    /// the step's `input`; its result is the step's output.
    pub async fn invoke_slot(
        &self,
        slot: &str,
        input: &Value,
        context: &InvocationContext,
        host: Arc<dyn PluginHost>,
    ) -> Result<Value, PluginError> {
        self.invoke(InvocationKind::Slot, slot, input, context, host)
            .await
    }

    /// Run the handler the script attached at `point` over `event`; the
    /// result is `{ action: "continue" | "rewrite" | "veto", event?, reason? }`.
    pub async fn invoke_hook(
        &self,
        point: &str,
        event: &Value,
        context: &InvocationContext,
        host: Arc<dyn PluginHost>,
    ) -> Result<Value, PluginError> {
        self.invoke(InvocationKind::Hook, point, event, context, host)
            .await
    }

    async fn invoke(
        &self,
        kind: InvocationKind,
        tool: &str,
        params: &Value,
        context: &InvocationContext,
        host: Arc<dyn PluginHost>,
    ) -> Result<Value, PluginError> {
        let (reply, response) = oneshot::channel();
        self.invocations
            .send(Invocation {
                kind,
                tool: tool.to_owned(),
                params_json: params.to_string(),
                context_json: serde_json::to_string(context).expect("a context serialises"),
                host,
                caller: tokio::runtime::Handle::current(),
                reply,
            })
            .map_err(|_| PluginError::Stopped)?;
        let result = response.await.map_err(|_| PluginError::Stopped)??;
        serde_json::from_str(&result).map_err(|error| {
            PluginError::Engine(format!("the handler's result was not JSON: {error}"))
        })
    }
}

/// What the script registered, by kind, as `__swagePlugin.provided()` lists it.
#[derive(Deserialize)]
struct ProvidedHandlers {
    tools: Vec<String>,
    slots: Vec<String>,
    hooks: Vec<String>,
}

fn set_state(state: &Mutex<PluginRuntimeState>, next: PluginRuntimeState) {
    *state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = next;
}

/// Build the isolate: the cage (in `Program::new`), the host shim, the
/// script, and a check that every declared tool has its handler.
fn load_program(
    manifest: &PluginManifest,
    script: &str,
    host_calls: &Arc<PluginHostCalls>,
    cancellation: &CancellationToken,
) -> Result<Program, PluginError> {
    let mut program = Program::new(
        manifest.limits.limits(),
        Some(Arc::clone(host_calls) as Arc<dyn HostCalls>),
        Some(cancellation.child_token()),
    )?;
    program.execute("swage:plugin-host", host_shim(manifest))?;
    program.execute("swage:plugin", script.to_owned())?;
    let provided = program.execute_for_string(
        "swage:plugin-provided",
        "__swagePlugin.provided()".to_owned(),
    )?;
    let provided: ProvidedHandlers = serde_json::from_str(&provided).map_err(|error| {
        PluginError::Engine(format!("the provided-handlers list was not JSON: {error}"))
    })?;
    for tool in &manifest.provides.tools {
        if !provided.tools.contains(&tool.name) {
            return Err(PluginError::Script(format!(
                "the manifest declares tool '{}' but the script provides no handler for it: call swage.provide.tool(\"{}\", handler)",
                tool.name, tool.name
            )));
        }
    }
    for slot in &manifest.provides.slots {
        if !provided.slots.contains(&slot.name) {
            return Err(PluginError::Script(format!(
                "the manifest declares slot '{}' but the script provides no handler for it: call swage.provide.slot(\"{}\", handler)",
                slot.name, slot.name
            )));
        }
    }
    for hook in &manifest.provides.hooks {
        if !provided.hooks.contains(&hook.point) {
            return Err(PluginError::Script(format!(
                "the manifest declares a hook at '{}' but the script attaches none: call swage.provide.hook(\"{}\", handler)",
                hook.point, hook.point
            )));
        }
    }
    Ok(program)
}

async fn invoke(
    program: &mut Program,
    kind: InvocationKind,
    tool: &str,
    params_json: &str,
    context_json: &str,
) -> Result<String, PluginError> {
    let literal = |text: &str| serde_json::to_string(text).expect("a string serialises");
    let pending = program.execute(
        "swage:plugin-call",
        format!(
            "__swagePlugin.{}({}, {}, {})",
            kind.entry(),
            literal(tool),
            literal(params_json),
            literal(context_json)
        ),
    )?;
    let result = program.settle_to_string(pending).await?;
    // The handler's promise settling is not the end of its work: a promise
    // chain it started and did not await still runs, and it must run now,
    // under this invocation's host, never under the next call's.
    program.drain().await?;
    Ok(result)
}

/// The host shim: evaluated after the cage and before the script. It takes
/// the door the cage left, deletes it, and exposes `swage` — `provide.tool`
/// to register handlers, `log`, and under the `tools` grant `callTool` —
/// plus the kernel's own `__swagePlugin` entry points. A host function the
/// manifest does not grant is absent, not stubbed.
fn host_shim(manifest: &PluginManifest) -> String {
    const SHIM: &str = r#"
(() => {
    const door = globalThis.__swageHost;
    delete globalThis.__swageHost;
    const unwrap = (reply) => {
        const parsed = JSON.parse(reply);
        if (parsed.ok) return parsed.value;
        const error = new Error(parsed.error.message);
        error.name = parsed.error.name;
        throw error;
    };
    const handlers = Object.create(null);
    const slots = Object.create(null);
    const hooks = Object.create(null);
    const register = (table, what, name, handler) => {
        if (typeof name !== "string" || typeof handler !== "function") {
            throw new TypeError(`swage.provide.${what}(name, handler) takes a name and a function`);
        }
        table[name] = handler;
    };
    const swage = {
        provide: Object.freeze({
            tool(name, handler) { register(handlers, "tool", name, handler); },
            slot(name, handler) { register(slots, "slot", name, handler); },
            hook(point, handler) { register(hooks, "hook", point, handler); },
        }),
        log(message) {
            unwrap(door.callSync("log", JSON.stringify(String(message))));
        },
    };
    if (__GRANT_TOOLS__) {
        swage.callTool = async (name, args) =>
            unwrap(await door.call("callTool", JSON.stringify({ name, arguments: args === undefined ? {} : args })));
    }
    if (__GRANT_MODEL__) {
        swage.complete = async (request) =>
            unwrap(await door.call("complete", JSON.stringify(request === undefined ? {} : request)));
    }
    if (__GRANT_CONTEXT__) {
        swage.context = async (request) =>
            unwrap(await door.call("context", JSON.stringify(request)));
    }
    Object.defineProperty(globalThis, "swage", {
        value: Object.freeze(swage), configurable: false, enumerable: false, writable: false,
    });
    const run = async (table, what, name, inputJson, contextJson) => {
        const handler = table[name];
        if (typeof handler !== "function") {
            const error = new Error(`the plugin provides no ${what} named '${name}'`);
            error.name = "PluginHandlerError";
            throw error;
        }
        const result = await handler(JSON.parse(inputJson), JSON.parse(contextJson));
        return JSON.stringify(result === undefined ? null : result);
    };
    Object.defineProperty(globalThis, "__swagePlugin", {
        value: Object.freeze({
            async invokeTool(name, paramsJson, contextJson) {
                return run(handlers, "tool", name, paramsJson, contextJson);
            },
            async invokeSlot(name, inputJson, contextJson) {
                return run(slots, "slot", name, inputJson, contextJson);
            },
            async invokeHook(point, eventJson, contextJson) {
                return run(hooks, "hook", point, eventJson, contextJson);
            },
            provided() {
                return JSON.stringify({
                    tools: Object.keys(handlers), slots: Object.keys(slots), hooks: Object.keys(hooks),
                });
            },
        }),
        configurable: false, enumerable: false, writable: false,
    });
})();
"#;
    SHIM.replace(
        "__GRANT_TOOLS__",
        if manifest.grants(HostGrant::Tools) {
            "true"
        } else {
            "false"
        },
    )
    .replace(
        "__GRANT_CONTEXT__",
        if manifest.grants(HostGrant::Context) {
            "true"
        } else {
            "false"
        },
    )
    .replace(
        "__GRANT_MODEL__",
        if manifest.grants(HostGrant::Model) {
            "true"
        } else {
            "false"
        },
    )
}

/// The call being served: its host, and the runtime the host's work runs on.
#[derive(Clone)]
struct Serving {
    host: Arc<dyn PluginHost>,
    caller: tokio::runtime::Handle,
}

/// The isolate's door, answered for the invocation being served. Capability
/// is decided here and in the shim: a name the manifest does not grant is
/// refused even if a script reaches it.
struct PluginHostCalls {
    grants: Vec<HostGrant>,
    serving: Mutex<Option<Serving>>,
}

#[derive(Deserialize)]
struct CallToolRequest {
    name: String,
    #[serde(default = "empty_object")]
    arguments: Value,
}

fn empty_object() -> Value {
    json!({})
}

impl PluginHostCalls {
    fn new(manifest: &PluginManifest) -> Self {
        Self {
            grants: manifest.grants.clone(),
            serving: Mutex::new(None),
        }
    }

    fn enter(&self, host: Arc<dyn PluginHost>, caller: tokio::runtime::Handle) {
        *self
            .serving
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Serving { host, caller });
    }

    fn leave(&self) {
        *self
            .serving
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    fn serving(&self) -> Option<Serving> {
        self.serving
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// The host's work for one call, abandoned if the isolate stops waiting for
/// it: a cancelled program drops its pending host calls, and the task must
/// not outlive the call it served.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn ok(value: Value) -> String {
    json!({ "ok": true, "value": value }).to_string()
}

fn failure(name: &str, message: impl Into<String>) -> String {
    json!({ "ok": false, "error": { "name": name, "message": message.into() } }).to_string()
}

fn ungranted(name: &str) -> String {
    failure(
        "HostCapabilityError",
        format!("the host grants no call named '{name}' to this plugin"),
    )
}

/// Answer one host call for a plugin under `grants`: `callTool` runs
/// `host.call_tool` (only under the `tools` grant), `log` writes to the
/// host's log, and any other name is refused. The reply is the envelope
/// every door speaks — `{"ok": true, "value"}` or
/// `{"ok": false, "error": {"name", "message"}}` — shared by the isolate's
/// door and the brokered process's socket.
pub(crate) async fn answer_host_call(
    grants: &[HostGrant],
    host: &dyn PluginHost,
    name: &str,
    payload: &str,
) -> String {
    match name {
        "callTool" if grants.contains(&HostGrant::Tools) => {
            let request: CallToolRequest = match serde_json::from_str(payload) {
                Ok(request) => request,
                Err(error) => return failure("TypeError", error.to_string()),
            };
            match host.call_tool(&request.name, request.arguments).await {
                Ok(value) => ok(value),
                Err(refusal) => failure(&refusal.name, refusal.message),
            }
        }
        "complete" if grants.contains(&HostGrant::Model) => {
            let request: Value = match serde_json::from_str(payload) {
                Ok(request) => request,
                Err(error) => return failure("TypeError", error.to_string()),
            };
            match host.complete(request).await {
                Ok(value) => ok(value),
                Err(refusal) => failure(&refusal.name, refusal.message),
            }
        }
        "context" if grants.contains(&HostGrant::Context) => {
            let request: Value = match serde_json::from_str(payload) {
                Ok(request) => request,
                Err(error) => return failure("TypeError", error.to_string()),
            };
            match host.context(request).await {
                Ok(value) => ok(value),
                Err(refusal) => failure(&refusal.name, refusal.message),
            }
        }
        "log" => {
            let message: String =
                serde_json::from_str(payload).unwrap_or_else(|_| payload.to_owned());
            host.log(&message);
            ok(Value::Null)
        }
        other => ungranted(other),
    }
}

#[async_trait]
impl HostCalls for PluginHostCalls {
    async fn call(&self, name: &str, payload: String) -> String {
        let Some(serving) = self.serving() else {
            return failure("HostUnavailableError", "no call is being served");
        };
        let grants = self.grants.clone();
        let host = serving.host;
        let name = name.to_owned();
        // The host's work runs on the caller's runtime; the isolate's
        // thread only waits for it, and stops waiting if the program is
        // cancelled — the task goes with the wait.
        let mut task =
            AbortOnDrop(serving.caller.spawn(async move {
                answer_host_call(&grants, host.as_ref(), &name, &payload).await
            }));
        match (&mut task.0).await {
            Ok(reply) => reply,
            Err(error) => failure("HostUnavailableError", error.to_string()),
        }
    }

    fn call_sync(&self, name: &str, payload: String) -> String {
        let Some(serving) = self.serving() else {
            return failure("HostUnavailableError", "no call is being served");
        };
        match name {
            "log" => {
                let message: String = serde_json::from_str(&payload).unwrap_or(payload);
                serving.host.log(&message);
                ok(Value::Null)
            }
            other => ungranted(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    fn manifest(grants: &str, limits: &str) -> PluginManifest {
        PluginManifest::parse(&format!(
            r#"
            name = "probe"
            version = "1"
            description = "A probe."
            script = "plugin.js"
            grants = [{grants}]
            [limits]
            {limits}
            [[provides.tools]]
            name = "probe"
            description = "Probe."
            parameters = {{ type = "object" }}
            "#
        ))
        .expect("manifest")
    }

    fn context() -> InvocationContext {
        InvocationContext {
            session_id: "session-1".into(),
            agent_id: "agent-1".into(),
            call_id: Some("call-1".into()),
            working_directory: "/work".into(),
        }
    }

    /// A host that answers every tool call with what it was asked, records
    /// the calls and completions, and refuses a tool named `refused`.
    #[derive(Default)]
    struct RecordingHost {
        calls: Mutex<Vec<(String, Value)>>,
        completions: Mutex<Vec<Value>>,
        logged: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl PluginHost for RecordingHost {
        async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, HostCallFailure> {
            tokio::task::yield_now().await;
            self.calls
                .lock()
                .unwrap()
                .push((name.to_owned(), arguments.clone()));
            if name == "refused" {
                return Err(HostCallFailure {
                    name: "PermissionDenied".into(),
                    message: "the session does not grant 'refused'".into(),
                });
            }
            Ok(json!({ "ran": name, "with": arguments }))
        }

        async fn complete(&self, request: Value) -> Result<Value, HostCallFailure> {
            self.completions.lock().unwrap().push(request.clone());
            Ok(json!({ "content": "a summary", "input_tokens": 3, "output_tokens": 2 }))
        }

        fn log(&self, message: &str) {
            self.logged.lock().unwrap().push(message.to_owned());
        }
    }

    async fn probe(grants: &str, script: &str) -> (Arc<PluginRuntime>, Arc<RecordingHost>) {
        let runtime = PluginRuntime::start(manifest(grants, ""), script.to_owned())
            .await
            .expect("starts");
        (runtime, Arc::new(RecordingHost::default()))
    }

    #[tokio::test]
    async fn context_operations_require_a_grant_at_both_doors() {
        struct ContextHost;
        #[async_trait]
        impl PluginHost for ContextHost {
            async fn call_tool(&self, _: &str, _: Value) -> Result<Value, HostCallFailure> {
                unreachable!()
            }
            async fn context(&self, request: Value) -> Result<Value, HostCallFailure> {
                Ok(json!({ "operation": request["operation"] }))
            }
            fn log(&self, _: &str) {}
        }
        for (grants, available) in [("", false), ("\"context\"", true)] {
            let runtime = PluginRuntime::start(
                manifest(grants, ""),
                r#"swage.provide.tool("probe", async () => ({
                    available: typeof swage.context === "function",
                    answer: typeof swage.context === "function"
                        ? await swage.context({ operation: "count_messages", messages: [] }) : null,
                }));"#
                    .to_owned(),
            )
            .await
            .unwrap();
            let result = runtime
                .invoke_tool("probe", &json!({}), &context(), Arc::new(ContextHost))
                .await
                .unwrap();
            assert_eq!(result["available"], available);
            if available {
                assert_eq!(result["answer"]["operation"], "count_messages");
            }
        }
        let refused: Value = serde_json::from_str(
            &answer_host_call(
                &[],
                &ContextHost,
                "context",
                r#"{"operation":"count_messages","messages":[]}"#,
            )
            .await,
        )
        .unwrap();
        assert_eq!(refused["error"]["name"], "HostCapabilityError");
    }

    #[tokio::test]
    async fn a_handler_round_trips_its_parameters_and_context() {
        let (runtime, host) = probe(
            "",
            r#"swage.provide.tool("probe", async (params, context) => ({
                echoed: params.text, session: context.sessionId, call: context.callId, cwd: context.workingDirectory,
            }));"#,
        )
        .await;
        let result = runtime
            .invoke_tool("probe", &json!({ "text": "hi" }), &context(), host)
            .await
            .expect("serves");
        assert_eq!(
            result,
            json!({ "echoed": "hi", "session": "session-1", "call": "call-1", "cwd": "/work" })
        );
        assert_eq!(runtime.state(), PluginRuntimeState::Serving);
    }

    #[tokio::test]
    async fn a_script_reaching_outside_the_host_api_finds_nothing() {
        let (runtime, host) = probe(
            "",
            r#"swage.provide.tool("probe", () => [
                typeof fetch, typeof Deno, typeof require, typeof process, typeof __swageHost,
                typeof swage.callTool, typeof Date,
            ].join(","));"#,
        )
        .await;
        let result = runtime
            .invoke_tool("probe", &json!({}), &context(), host)
            .await
            .expect("serves");
        assert_eq!(
            result,
            "undefined,undefined,undefined,undefined,undefined,undefined,undefined"
        );
    }

    #[tokio::test]
    async fn a_granted_tool_call_passes_through_the_host_and_a_refusal_is_catchable() {
        let (runtime, host) = probe(
            r#""tools""#,
            r#"swage.provide.tool("probe", async (params) => {
                swage.log("about to run " + params.tool);
                const ran = await swage.callTool(params.tool, { command: "ls" });
                let refusal = null;
                try { await swage.callTool("refused"); } catch (error) { refusal = error.name + ": " + error.message; }
                return { ran, refusal };
            });"#,
        )
        .await;
        let result = runtime
            .invoke_tool(
                "probe",
                &json!({ "tool": "shell" }),
                &context(),
                host.clone(),
            )
            .await
            .expect("serves");
        assert_eq!(
            result,
            json!({
                "ran": { "ran": "shell", "with": { "command": "ls" } },
                "refusal": "PermissionDenied: the session does not grant 'refused'",
            })
        );
        let calls = host.calls.lock().unwrap();
        assert_eq!(calls[0], ("shell".to_owned(), json!({ "command": "ls" })));
        assert_eq!(calls[1], ("refused".to_owned(), json!({})));
        assert_eq!(
            host.logged.lock().unwrap().as_slice(),
            ["about to run shell"]
        );
    }

    #[tokio::test]
    async fn a_declared_tool_without_a_handler_fails_the_start() {
        let error = PluginRuntime::start(
            manifest("", ""),
            r#"swage.provide.tool("other", () => 1);"#.to_owned(),
        )
        .await
        .expect_err("refused");
        assert!(
            matches!(&error, PluginError::Script(message) if message.contains("declares tool 'probe'")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_script_that_throws_at_load_fails_the_start_with_its_error() {
        let error = PluginRuntime::start(
            manifest("", ""),
            "throw new RangeError('no good');".to_owned(),
        )
        .await
        .expect_err("refused");
        assert!(
            matches!(&error, PluginError::Script(message) if message.starts_with("RangeError: no good")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_handler_that_throws_renders_its_error_for_the_caller() {
        let (runtime, host) = probe(
            "",
            r#"swage.provide.tool("probe", () => { const e = new Error("bad input"); e.name = "InputError"; throw e; });"#,
        )
        .await;
        let error = runtime
            .invoke_tool("probe", &json!({}), &context(), host)
            .await
            .expect_err("throws");
        assert!(
            matches!(&error, PluginError::Script(message) if message.starts_with("InputError: bad input")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_terminated_isolate_reports_the_reason_and_the_next_call_serves_afresh() {
        let runtime = PluginRuntime::start(
            manifest("", "heap_mib = 8"),
            r#"
            let kept = [];
            swage.provide.tool("probe", (params) => {
                if (params.exhaust) { for (;;) { kept.push(new Array(100000).fill("x")); } }
                return kept.length;
            });"#
                .to_owned(),
        )
        .await
        .expect("starts");
        let host = Arc::new(RecordingHost::default());
        let error = runtime
            .invoke_tool(
                "probe",
                &json!({ "exhaust": true }),
                &context(),
                host.clone(),
            )
            .await
            .expect_err("terminated");
        assert!(
            matches!(error, PluginError::Terminated(Termination::HeapLimit)),
            "{error}"
        );
        assert_eq!(
            runtime.state(),
            PluginRuntimeState::Terminated(Termination::HeapLimit)
        );
        let kept = runtime
            .invoke_tool("probe", &json!({}), &context(), host)
            .await
            .expect("serves again on a fresh isolate");
        assert_eq!(kept, 0, "state does not survive a termination");
        assert_eq!(runtime.state(), PluginRuntimeState::Serving);
    }

    #[tokio::test]
    async fn one_plugins_exhaustion_leaves_another_serving() {
        let spinner = PluginRuntime::start(
            manifest("", "continuous_execution_secs = 1"),
            r#"swage.provide.tool("probe", () => { for (;;) {} });"#.to_owned(),
        )
        .await
        .expect("starts");
        let (steady, host) = probe("", r#"swage.provide.tool("probe", () => "steady");"#).await;
        let spun = tokio::spawn({
            let spinner = Arc::clone(&spinner);
            let host = host.clone();
            async move {
                spinner
                    .invoke_tool("probe", &json!({}), &context(), host)
                    .await
            }
        });
        let served = steady
            .invoke_tool("probe", &json!({}), &context(), host)
            .await
            .expect("serves while the other spins");
        assert_eq!(served, "steady");
        let error = spun.await.expect("joins").expect_err("terminated");
        assert!(
            matches!(error, PluginError::Terminated(Termination::TimeLimit)),
            "{error}"
        );
        assert_eq!(steady.state(), PluginRuntimeState::Serving);
    }

    #[tokio::test]
    async fn work_a_handler_leaves_behind_finishes_inside_its_own_invocation() {
        let (runtime, first_host) = probe(
            r#""tools""#,
            r#"swage.provide.tool("probe", async (params) => {
                if (params.leave) {
                    swage.callTool("first", {}).then(() => swage.callTool("second", { from: params.leave }));
                    return "left";
                }
                return "plain";
            });"#,
        )
        .await;
        let second_host = Arc::new(RecordingHost::default());
        let left = runtime
            .invoke_tool(
                "probe",
                &json!({ "leave": "A" }),
                &context(),
                first_host.clone(),
            )
            .await
            .expect("serves");
        assert_eq!(left, "left");
        let plain = runtime
            .invoke_tool("probe", &json!({}), &context(), second_host.clone())
            .await
            .expect("serves");
        assert_eq!(plain, "plain");
        let first_calls: Vec<String> = first_host
            .calls
            .lock()
            .unwrap()
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        assert_eq!(first_calls, vec!["first", "second"]);
        assert_eq!(
            first_host.calls.lock().unwrap()[1].1,
            json!({ "from": "A" })
        );
        assert!(
            second_host.calls.lock().unwrap().is_empty(),
            "the chain the first call left behind reached the second call's host"
        );
    }

    #[tokio::test]
    async fn dropping_the_runtime_ends_a_wait_on_a_host_that_never_answers() {
        /// A host whose tool call never answers, recording when the wait
        /// begins and when the host's work is abandoned.
        struct Silent {
            waiting: Arc<AtomicBool>,
            abandoned: Arc<AtomicBool>,
        }
        struct Abandons(Arc<AtomicBool>);
        impl Drop for Abandons {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        #[async_trait]
        impl PluginHost for Silent {
            async fn call_tool(&self, _: &str, _: Value) -> Result<Value, HostCallFailure> {
                let _abandons = Abandons(Arc::clone(&self.abandoned));
                self.waiting.store(true, Ordering::SeqCst);
                std::future::pending().await
            }
            fn log(&self, _: &str) {}
        }
        let waiting = Arc::new(AtomicBool::new(false));
        let abandoned = Arc::new(AtomicBool::new(false));
        let host = Arc::new(Silent {
            waiting: Arc::clone(&waiting),
            abandoned: Arc::clone(&abandoned),
        });
        let runtime = PluginRuntime::start(
            manifest(r#""tools""#, ""),
            r#"swage.provide.tool("probe", () => swage.callTool("slow", {}));"#.to_owned(),
        )
        .await
        .expect("starts");
        let caller = Arc::clone(&runtime);
        let call = tokio::spawn(async move {
            caller
                .invoke_tool("probe", &json!({}), &context(), host)
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !waiting.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the host call begins");

        call.abort();
        let _ = call.await;
        drop(runtime);

        tokio::time::timeout(Duration::from_secs(5), async {
            while !abandoned.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the host's work is abandoned once nothing waits for it");
    }

    fn manifest_providing(grants: &str, provides: &str) -> PluginManifest {
        PluginManifest::parse(&format!(
            r#"
            name = "probe"
            version = "1"
            description = "A probe."
            script = "plugin.js"
            grants = [{grants}]
            {provides}
            "#
        ))
        .expect("manifest")
    }

    #[tokio::test]
    async fn a_slot_handler_runs_in_the_step_s_place_and_may_ask_the_model() {
        let runtime = PluginRuntime::start(
            manifest_providing(r#""model""#, "[[provides.slots]]\nname = \"succession\""),
            r#"swage.provide.slot("succession", async (input) => {
                const reply = await swage.complete({ messages: [{ role: "user", content: "Summarise: " + input.history.join(" ") }] });
                return { summary: reply.content, kept: input.retain };
            });"#
            .to_owned(),
        )
        .await
        .expect("starts");
        let host = Arc::new(RecordingHost::default());
        let output = runtime
            .invoke_slot(
                "succession",
                &json!({ "history": ["one", "two"], "retain": 1 }),
                &context(),
                host.clone(),
            )
            .await
            .expect("serves");
        assert_eq!(output, json!({ "summary": "a summary", "kept": 1 }));
        let asked = host.completions.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0]["messages"][0]["content"], "Summarise: one two");
        let no_tool = runtime
            .invoke_tool("succession", &json!({}), &context(), host.clone())
            .await
            .expect_err("a slot is not a tool");
        assert!(no_tool.to_string().contains("no tool named"), "{no_tool}");
    }

    #[tokio::test]
    async fn a_hook_handler_answers_with_an_action_and_complete_is_absent_without_the_grant() {
        let runtime = PluginRuntime::start(
            manifest_providing("", "[[provides.hooks]]\npoint = \"tool_call\"\nmode = \"rewrite\""),
            r#"swage.provide.hook("tool_call", (event) => {
                if (event.name === "shell") return { action: "veto", reason: "no shells here" };
                if (event.name === "file_read") return { action: "rewrite", event: { ...event, arguments: { ...event.arguments, path: "/safe" } } };
                return { action: "continue", hasComplete: typeof swage.complete };
            });"#
            .to_owned(),
        )
        .await
        .expect("starts");
        let host = Arc::new(RecordingHost::default());
        let veto = runtime
            .invoke_hook(
                "tool_call",
                &json!({ "name": "shell", "arguments": {} }),
                &context(),
                host.clone(),
            )
            .await
            .expect("serves");
        assert_eq!(
            veto,
            json!({ "action": "veto", "reason": "no shells here" })
        );
        let rewrite = runtime
            .invoke_hook(
                "tool_call",
                &json!({ "name": "file_read", "arguments": { "path": "/etc" } }),
                &context(),
                host.clone(),
            )
            .await
            .expect("serves");
        assert_eq!(rewrite["action"], "rewrite");
        assert_eq!(rewrite["event"]["arguments"]["path"], "/safe");
        let other = runtime
            .invoke_hook(
                "tool_call",
                &json!({ "name": "echo", "arguments": {} }),
                &context(),
                host,
            )
            .await
            .expect("serves");
        assert_eq!(
            other,
            json!({ "action": "continue", "hasComplete": "undefined" })
        );
    }

    #[tokio::test]
    async fn a_declared_slot_or_hook_without_a_handler_fails_the_start() {
        let error = PluginRuntime::start(
            manifest_providing("", "[[provides.slots]]\nname = \"curator\""),
            "1;".to_owned(),
        )
        .await
        .expect_err("refused");
        assert!(error.to_string().contains("slot 'curator'"), "{error}");
        let error = PluginRuntime::start(
            manifest_providing(
                "",
                "[[provides.hooks]]\npoint = \"turn_start\"\nmode = \"observe\"",
            ),
            "1;".to_owned(),
        )
        .await
        .expect_err("refused");
        assert!(
            error.to_string().contains("hook at 'turn_start'"),
            "{error}"
        );
    }
}
