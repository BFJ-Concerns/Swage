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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use swage_isolate::{HostCalls, IsolateError, Program, Termination};
use tokio::sync::{mpsc, oneshot};

use crate::manifest::{HostGrant, PluginManifest};

/// What a handler is told about the call it serves, as the script reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InvocationContext {
    pub session_id: String,
    pub agent_id: String,
    pub call_id: Option<String>,
    pub working_directory: String,
}

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

struct Invocation {
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
                    let mut program = match load_program(&manifest, &script, &host_calls) {
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
                            match load_program(&manifest, &script, &host_calls) {
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
        let (reply, response) = oneshot::channel();
        self.invocations
            .send(Invocation {
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
) -> Result<Program, PluginError> {
    let mut program = Program::new(
        manifest.limits.limits(),
        Some(Arc::clone(host_calls) as Arc<dyn HostCalls>),
        None,
    )?;
    program.execute("swage:plugin-host", host_shim(manifest))?;
    program.execute("swage:plugin", script.to_owned())?;
    let provided = program.execute_for_string(
        "swage:plugin-provided",
        "__swagePlugin.providedTools()".to_owned(),
    )?;
    let provided: Vec<String> = serde_json::from_str(&provided).map_err(|error| {
        PluginError::Engine(format!("the provided-tools list was not JSON: {error}"))
    })?;
    for tool in &manifest.provides.tools {
        if !provided.contains(&tool.name) {
            return Err(PluginError::Script(format!(
                "the manifest declares tool '{}' but the script provides no handler for it: call swage.provide.tool(\"{}\", handler)",
                tool.name, tool.name
            )));
        }
    }
    Ok(program)
}

async fn invoke(
    program: &mut Program,
    tool: &str,
    params_json: &str,
    context_json: &str,
) -> Result<String, PluginError> {
    let literal = |text: &str| serde_json::to_string(text).expect("a string serialises");
    let pending = program.execute(
        "swage:plugin-call",
        format!(
            "__swagePlugin.invokeTool({}, {}, {})",
            literal(tool),
            literal(params_json),
            literal(context_json)
        ),
    )?;
    Ok(program.settle_to_string(pending).await?)
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
    const swage = {
        provide: Object.freeze({
            tool(name, handler) {
                if (typeof name !== "string" || typeof handler !== "function") {
                    throw new TypeError("swage.provide.tool(name, handler) takes a tool name and a function");
                }
                handlers[name] = handler;
            },
        }),
        log(message) {
            door.callSync("log", JSON.stringify(String(message)));
        },
    };
    if (__GRANT_TOOLS__) {
        swage.callTool = async (name, args) =>
            unwrap(await door.call("callTool", JSON.stringify({ name, arguments: args === undefined ? {} : args })));
    }
    Object.defineProperty(globalThis, "swage", {
        value: Object.freeze(swage), configurable: false, enumerable: false, writable: false,
    });
    Object.defineProperty(globalThis, "__swagePlugin", {
        value: Object.freeze({
            async invokeTool(name, paramsJson, contextJson) {
                const handler = handlers[name];
                if (typeof handler !== "function") {
                    const error = new Error(`the plugin provides no tool named '${name}'`);
                    error.name = "PluginToolError";
                    throw error;
                }
                const result = await handler(JSON.parse(paramsJson), JSON.parse(contextJson));
                return JSON.stringify(result === undefined ? null : result);
            },
            providedTools() {
                return JSON.stringify(Object.keys(handlers));
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

#[async_trait]
impl HostCalls for PluginHostCalls {
    async fn call(&self, name: &str, payload: String) -> String {
        let Some(serving) = self.serving() else {
            return failure("HostUnavailableError", "no call is being served");
        };
        match name {
            "callTool" if self.grants.contains(&HostGrant::Tools) => {
                let request: CallToolRequest = match serde_json::from_str(&payload) {
                    Ok(request) => request,
                    Err(error) => return failure("TypeError", error.to_string()),
                };
                let host = serving.host;
                let outcome = serving
                    .caller
                    .spawn(async move { host.call_tool(&request.name, request.arguments).await })
                    .await;
                match outcome {
                    Ok(Ok(value)) => ok(value),
                    Ok(Err(refusal)) => failure(&refusal.name, refusal.message),
                    Err(error) => failure("HostUnavailableError", error.to_string()),
                }
            }
            other => ungranted(other),
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
    /// the calls, and refuses a tool named `refused`.
    #[derive(Default)]
    struct RecordingHost {
        calls: Mutex<Vec<(String, Value)>>,
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
                typeof swage.callTool, typeof Date, typeof eval,
            ].join(","));"#,
        )
        .await;
        let result = runtime
            .invoke_tool("probe", &json!({}), &context(), host)
            .await
            .expect("serves");
        assert_eq!(
            result,
            "undefined,undefined,undefined,undefined,undefined,undefined,undefined,undefined"
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
}
