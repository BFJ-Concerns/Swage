use std::sync::Arc;

use rquickjs::{
    AsyncContext, AsyncRuntime, CatchResultExt, Function, Promise,
    context::intrinsic::{Eval, Json, MapSet, Promise as PromiseIntrinsic, RegExp, RegExpCompiler},
    prelude::Async,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{AgentCallRequest, EffectPayload, WorkflowCallRequest, WorkflowHost};

fn named_host_error(error: &str, fallback: &str) -> (String, String) {
    const NAMES: [(&str, &str); 7] = [
        ("BudgetExceededError", "BudgetExceededError"),
        ("DeliveryNotGrantedError", "DeliveryNotGrantedError"),
        ("WorkflowCancelledError", "WorkflowCancelledError"),
        ("NondeterministicReplayError", "NondeterministicCallError"),
        ("EmptyAgentOutputError", "EmptyAgentOutputError"),
        ("WorkflowCompositionError", "WorkflowCompositionError"),
        ("WaitTimerError", "WaitTimerError"),
    ];
    NAMES
        .iter()
        .find_map(|(prefix, public_name)| {
            error.strip_prefix(prefix).map(|message| {
                (
                    (*public_name).to_string(),
                    message.trim_start_matches([':', ' ']).to_string(),
                )
            })
        })
        .unwrap_or_else(|| (fallback.to_string(), error.to_string()))
}

/// Hard limits applied by the host to each fresh JavaScript runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineLimits {
    pub memory_bytes: usize,
    pub stack_bytes: usize,
}

/// What the host grants this script instance. Capability comes from the host,
/// never from script content: `deliver` exists only under a delivery grant,
/// and `workflow()` only at the top level — a child run's engine is built
/// with `composition: false`, which is what bounds nesting to one level.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScriptCapabilities {
    pub delivery: bool,
    pub composition: bool,
}

/// Splits an optional `export const defaults = { … };` header off a script
/// body. The body is wrapped in an async function before evaluation, where
/// `export` is a syntax error — so the header can only be the first
/// statement (comments permitted before it), and an existing script cannot
/// already contain one.
fn extract_defaults(source: &str) -> Result<(Option<String>, &str), String> {
    let header_end = skip_whitespace_and_comments(source);
    let rest = &source[header_end..];
    let Some(after_keyword) = rest.strip_prefix("export") else {
        return Ok((None, source));
    };
    let after_keyword = after_keyword.trim_start();
    let Some(after_const) = after_keyword.strip_prefix("const") else {
        return Err("workflow `export` may only introduce `const defaults`".into());
    };
    let after_const = after_const.trim_start();
    let Some(after_name) = after_const.strip_prefix("defaults") else {
        return Err("workflow `export const` may only declare `defaults`".into());
    };
    let after_name = after_name.trim_start();
    let Some(after_equals) = after_name.strip_prefix('=') else {
        return Err("workflow defaults must be assigned an object literal".into());
    };
    let object = after_equals.trim_start();
    if !object.starts_with('{') {
        return Err("workflow defaults must be an object literal".into());
    }
    let end = find_balanced_object_end(object)?;
    let literal = &object[..=end];
    let mut body = object[end + 1..].trim_start();
    if let Some(after_semicolon) = body.strip_prefix(';') {
        body = after_semicolon;
    }
    Ok((Some(literal.to_string()), body))
}

/// Byte offset of the first token past leading whitespace and `//`/`/* */`
/// comments — where a `defaults` header may legitimately start.
fn skip_whitespace_and_comments(source: &str) -> usize {
    let bytes = source.as_bytes();
    let mut index = 0usize;
    loop {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if bytes[index..].starts_with(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes[index..].starts_with(b"/*") {
            match source[index + 2..].find("*/") {
                Some(end) => {
                    index += 2 + end + 2;
                    continue;
                }
                None => return index,
            }
        }
        return index;
    }
}

/// Finds the closing brace of an object literal starting at byte 0, skipping
/// strings, template literals, and comments so their braces do not count.
fn find_balanced_object_end(source: &str) -> Result<usize, String> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' => {
                depth -= 1;
                if depth == 0 {
                    if bytes[index] != b'}' {
                        return Err(
                            "workflow defaults object closed with the wrong delimiter".into()
                        );
                    }
                    return Ok(index);
                }
            }
            quote @ (b'"' | b'\'' | b'`') => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index += 1;
                    } else if bytes[index] == quote {
                        break;
                    }
                    index += 1;
                }
                if index >= bytes.len() {
                    return Err("workflow defaults string is not terminated".into());
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => match source[index + 2..].find("*/") {
                Some(end) => index += 2 + end + 1,
                None => return Err("workflow defaults comment is not terminated".into()),
            },
            _ => {}
        }
        index += 1;
    }
    Err("workflow defaults object is not balanced".into())
}

impl Default for EngineLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 32 * 1024 * 1024,
            stack_bytes: 512 * 1024,
        }
    }
}

#[derive(Debug, Error)]
pub enum WorkflowEngineError {
    #[error("failed to initialise the JavaScript runtime: {0}")]
    Runtime(#[from] rquickjs::Error),

    #[error("workflow execution was cancelled")]
    Cancelled,

    #[error("workflow script failed: {0}")]
    Script(String),
}

/// Creates one isolated JavaScript runtime per workflow attempt.
pub struct WorkflowEngine {
    limits: EngineLimits,
}

impl WorkflowEngine {
    pub const fn new(limits: EngineLimits) -> Self {
        Self { limits }
    }

    pub async fn execute(
        &self,
        source: &str,
        cancellation: CancellationToken,
    ) -> Result<String, WorkflowEngineError> {
        self.execute_hosted(
            source,
            "null",
            cancellation,
            None,
            ScriptCapabilities::default(),
        )
        .await
    }

    pub async fn execute_hosted(
        &self,
        source: &str,
        args_json: &str,
        cancellation: CancellationToken,
        host: Option<Arc<dyn WorkflowHost>>,
        capabilities: ScriptCapabilities,
    ) -> Result<String, WorkflowEngineError> {
        let (defaults_json, source) =
            extract_defaults(source).map_err(WorkflowEngineError::Script)?;
        let runtime = AsyncRuntime::new()?;
        runtime.set_memory_limit(self.limits.memory_bytes).await;
        runtime.set_max_stack_size(self.limits.stack_bytes).await;

        let cancellation = Arc::new(cancellation);
        let interrupt_cancellation = Arc::clone(&cancellation);
        runtime
            .set_interrupt_handler(Some(Box::new(move || {
                interrupt_cancellation.is_cancelled()
            })))
            .await;

        // QuickJS requires the Eval intrinsic for host-side source evaluation too. The
        // JavaScript-visible global is removed before untrusted workflow code runs.
        type WorkflowIntrinsics = (Eval, Json, PromiseIntrinsic, MapSet, RegExpCompiler, RegExp);
        let context = AsyncContext::custom::<WorkflowIntrinsics>(&runtime).await?;
        let source = format!(
            "(async () => {{ const value = await (async () => {{ {source} }})(); return JSON.stringify(value === undefined ? null : value); }})()"
        );
        let args_json = args_json.to_string();
        let host_cancellation = (*cancellation).clone();

        let result = context
            .async_with(async move |ctx| {
                // QuickJS installs Math as a base object and seeds random from ambient time.
                // Replacing the function makes the nondeterministic surface fail explicitly.
                ctx.eval::<(), _>(
                    r#"
                    delete globalThis.eval;
                    if (typeof Math !== "undefined") {
                        Math.random = () => {
                            const error = new Error(
                                "Math.random is not available in workflow scripts; randomness would break resume"
                            );
                            error.name = "NondeterministicCallError";
                            throw error;
                        };
                    }
                    "#,
                )
                .catch(&ctx)
                .map_err(|error| error.to_string())?;

                ctx.globals().set(
                    "args",
                    ctx.json_parse(args_json.as_bytes())
                        .map_err(|error| error.to_string())?,
                ).map_err(|error| error.to_string())?;

                ctx.eval::<(), _>(format!(
                    "globalThis.__defaults = {};",
                    defaults_json.as_deref().unwrap_or("{}")
                ))
                .catch(&ctx)
                .map_err(|error| error.to_string())?;

                if let Some(host) = host {
                    let agent_host = Arc::clone(&host);
                    let agent_cancellation = host_cancellation.clone();
                    let agent = Function::new(
                        ctx.clone(),
                        Async(move |prompt: String, options_json: String| {
                            let host = Arc::clone(&agent_host);
                            let cancellation = agent_cancellation.clone();
                            async move {
                                let request: AgentCallRequest = match serde_json::from_str(&options_json) {
                                    Ok(request) => request,
                                    Err(error) => return serde_json::json!({
                                        "ok": false,
                                        "error": {"name": "AgentCallError", "message": error.to_string()}
                                    }).to_string(),
                                };
                                let mut request = request;
                                request.prompt = prompt;
                                match host.run_agent_call(request.clone(), cancellation).await {
                                    Ok(result) => {
                                        if request.schema.is_none()
                                            && result.output.as_str().is_some_and(|value| value.trim().is_empty())
                                        {
                                            return serde_json::json!({
                                                "ok": false,
                                                "error": {"name": "EmptyAgentOutputError", "message": "agent returned empty output"}
                                            }).to_string();
                                        }
                                        // The worker's continuation token rides beside the
                                        // output so `agentStart` handles can expose it
                                        // without touching agent()'s bare-value contract.
                                        let session = result
                                            .session_id
                                            .zip(result.thread_id)
                                            .map(|(session_id, thread_id)| serde_json::json!({
                                                "session_id": session_id,
                                                "thread_id": thread_id,
                                            }));
                                        serde_json::json!({"ok": true, "value": result.output, "session": session}).to_string()
                                    }
                                    Err(error) => {
                                        let (name, message) = named_host_error(&error, "AgentCallError");
                                        serde_json::json!({
                                            "ok": false,
                                            "error": {"name": name, "message": message}
                                        }).to_string()
                                    }
                                }
                            }
                        }),
                    ).map_err(|error| error.to_string())?;
                    ctx.globals().set("__agent", agent).map_err(|error| error.to_string())?;

                    let log_host = Arc::clone(&host);
                    let log = Function::new(ctx.clone(), move |message: String| log_host.record("log", message)).map_err(|error| error.to_string())?;
                    ctx.globals().set("__log", log).map_err(|error| error.to_string())?;
                    let phase_host = Arc::clone(&host);
                    let phase = Function::new(ctx.clone(), move |message: String| phase_host.record("phase", message)).map_err(|error| error.to_string())?;
                    ctx.globals().set("__phase", phase).map_err(|error| error.to_string())?;

                    if capabilities.composition {
                        let workflow_host = Arc::clone(&host);
                        let workflow_cancellation = host_cancellation.clone();
                        let workflow = Function::new(
                            ctx.clone(),
                            Async(move |name: String, args_json: String| {
                                let host = Arc::clone(&workflow_host);
                                let cancellation = workflow_cancellation.clone();
                                async move {
                                    let args: serde_json::Value = match serde_json::from_str(&args_json) {
                                        Ok(args) => args,
                                        Err(error) => return serde_json::json!({
                                            "ok": false,
                                            "error": {"name": "WorkflowCompositionError", "message": error.to_string()}
                                        }).to_string(),
                                    };
                                    match host
                                        .run_workflow_call(WorkflowCallRequest { name, args }, cancellation)
                                        .await
                                    {
                                        Ok(output) => serde_json::json!({"ok": true, "value": output}).to_string(),
                                        Err(error) => {
                                            let (name, message) = named_host_error(&error, "WorkflowCompositionError");
                                            serde_json::json!({
                                                "ok": false,
                                                "error": {"name": name, "message": message}
                                            }).to_string()
                                        }
                                    }
                                }
                            }),
                        ).map_err(|error| error.to_string())?;
                        ctx.globals().set("__workflow", workflow).map_err(|error| error.to_string())?;
                    }

                    let timer_host = Arc::clone(&host);
                    let timer_cancellation = host_cancellation.clone();
                    let timer = Function::new(
                        ctx.clone(),
                        Async(move |ms: f64, timer_id: f64| {
                            let host = Arc::clone(&timer_host);
                            let cancellation = timer_cancellation.clone();
                            async move {
                                if !ms.is_finite() || ms < 0.0 {
                                    return serde_json::json!({
                                        "ok": false,
                                        "error": {"name": "WaitTimerError", "message": "timerMs must be a non-negative finite number"}
                                    }).to_string();
                                }
                                match host.timer(ms as u64, timer_id as u64, cancellation).await {
                                    Ok(value) => serde_json::json!({"ok": true, "value": value}).to_string(),
                                    Err(error) => {
                                        let (name, message) = named_host_error(&error, "WaitTimerError");
                                        serde_json::json!({
                                            "ok": false,
                                            "error": {"name": name, "message": message}
                                        }).to_string()
                                    }
                                }
                            }
                        }),
                    ).map_err(|error| error.to_string())?;
                    ctx.globals().set("__timer", timer).map_err(|error| error.to_string())?;
                    let abandon_host = Arc::clone(&host);
                    let abandon = Function::new(ctx.clone(), move |timer_id: f64| {
                        abandon_host.abandon_timer(timer_id as u64);
                    })
                    .map_err(|error| error.to_string())?;
                    ctx.globals().set("__abandonTimer", abandon).map_err(|error| error.to_string())?;
                    let confirm_host = Arc::clone(&host);
                    let confirm = Function::new(
                        ctx.clone(),
                        Async(move |timer_id: f64| {
                            let host = Arc::clone(&confirm_host);
                            async move {
                                match host.confirm_timer(timer_id as u64).await {
                                    Ok(()) => serde_json::json!({"ok": true, "value": null}).to_string(),
                                    Err(error) => {
                                        let (name, message) = named_host_error(&error, "WaitTimerError");
                                        serde_json::json!({
                                            "ok": false,
                                            "error": {"name": name, "message": message}
                                        }).to_string()
                                    }
                                }
                            }
                        }),
                    ).map_err(|error| error.to_string())?;
                    ctx.globals().set("__confirmTimer", confirm).map_err(|error| error.to_string())?;

                    if capabilities.delivery {
                        let delivery_host = Arc::clone(&host);
                        let delivery_cancellation = host_cancellation.clone();
                        let deliver = Function::new(
                            ctx.clone(),
                            Async(move |payload_json: String| {
                                let host = Arc::clone(&delivery_host);
                                let cancellation = delivery_cancellation.clone();
                                async move {
                                    let payload: EffectPayload = match serde_json::from_str(&payload_json) {
                                        Ok(payload) => payload,
                                        Err(error) => return serde_json::json!({
                                            "ok": false,
                                            "error": {"name": "DeliveryError", "message": error.to_string()}
                                        }).to_string(),
                                    };
                                    match host.deliver(payload, cancellation).await {
                                        Ok(receipt) => serde_json::json!({"ok": true, "value": receipt}).to_string(),
                                        Err(error) => {
                                            let (name, message) = named_host_error(&error, "DeliveryError");
                                            serde_json::json!({
                                                "ok": false,
                                                "error": {"name": name, "message": message}
                                            }).to_string()
                                        }
                                    }
                                }
                            }),
                        ).map_err(|error| error.to_string())?;
                        ctx.globals().set("__deliver", deliver).map_err(|error| error.to_string())?;
                    }

                    let spent_host = Arc::clone(&host);
                    let spent = Function::new(ctx.clone(), move || spent_host.budget_spent())
                        .map_err(|error| error.to_string())?;
                    ctx.globals().set("__budgetSpent", spent).map_err(|error| error.to_string())?;
                    let remaining_host = Arc::clone(&host);
                    let remaining = Function::new(ctx.clone(), move || remaining_host.budget_remaining())
                        .map_err(|error| error.to_string())?;
                    ctx.globals().set("__budgetRemaining", remaining).map_err(|error| error.to_string())?;

                    ctx.eval::<(), _>(r#"
                        const unwrapHostResponse = (json) => {
                            const response = JSON.parse(json);
                            if (!response.ok) {
                                const error = new Error(response.error.message);
                                error.name = response.error.name;
                                throw error;
                            }
                            return response.value;
                        };
                        // Agent responses carry a continuation token beside the
                        // value; only agentStart handles surface it.
                        const unwrapAgentResponse = (json) => {
                            const response = JSON.parse(json);
                            if (!response.ok) {
                                const error = new Error(response.error.message);
                                error.name = response.error.name;
                                throw error;
                            }
                            return { value: response.value, session: response.session ?? null };
                        };
                        // Script-level defaults fill options a call site leaves
                        // unset; only *defined* call-site values override, so a
                        // conditional spread leaving `model: undefined` still
                        // falls back to the default it was meant to.
                        const mergeDefaults = (options) => {
                            const merged = { ...__defaults };
                            for (const key of Object.keys(options)) {
                                if (options[key] !== undefined) merged[key] = options[key];
                            }
                            return merged;
                        };
                        const buildAgentRequest = (options) => {
                            const merged = mergeDefaults(options);
                            if (!merged.agent) {
                                const error = new Error("opts.agent is required");
                                error.name = "AgentRequiredError";
                                throw error;
                            }
                            return {
                                prompt: "",
                                agent: merged.agent,
                                model: merged.model ?? null,
                                schema: merged.schema ?? null,
                                max_attempts: merged.maxAttempts ?? 3,
                                label: merged.label ?? null,
                                session: merged.session ?? null,
                            };
                        };
                        globalThis.agent = async (prompt, options = {}) => {
                            const request = buildAgentRequest(options);
                            const response = unwrapAgentResponse(await __agent(String(prompt), JSON.stringify(request)));
                            return response.value;
                        };
                        // Dispatch without awaiting: the returned handle
                        // carries the result promise wait() races, plus
                        // `label`/`done` for the script's own bookkeeping and
                        // `session` — the worker's continuation token, set
                        // when the call completes. Pass it back whole as the
                        // `session` option to land the worker's next turn in
                        // the same session: worker restates, worker proceeds,
                        // one session.
                        globalThis.agentStart = (prompt, options = {}) => {
                            const request = buildAgentRequest(options);
                            const handle = {
                                label: request.label,
                                done: false,
                                session: request.session ?? null,
                            };
                            handle.result = __agent(String(prompt), JSON.stringify(request))
                                .then((json) => {
                                    const response = unwrapAgentResponse(json);
                                    handle.done = true;
                                    handle.session = response.session ?? handle.session;
                                    return response.value;
                                }, (error) => {
                                    handle.done = true;
                                    throw error;
                                });
                            // A wait() loser's rejection is observed by no one
                            // by design; without this the run dies on it.
                            handle.result.catch(() => {});
                            return handle;
                        };
                        // Race completion watches against a journalled host
                        // timer. Resolves {wake:"completion", handle, value}
                        // or {wake:"timer"}. A watched call's own failure is
                        // reported as its wake so the script can act on it.
                        // The journal entry settles on the RACE's verdict,
                        // not the firing: a timer wake is confirmed (entry
                        // completed) before wait() returns, and a beaten
                        // timer is stood down (entry stays started) — so a
                        // completed timer entry means exactly "the timer won
                        // a wait", and replay repeats the winner.
                        let __timerSeq = 0;
                        globalThis.wait = async ({ watches = [], timerMs = null } = {}) => {
                            const legs = watches.map((handle) => handle.result.then(
                                (value) => ({ wake: "completion", handle, value }),
                                (error) => ({ wake: "completion", handle, error }),
                            ));
                            let timerId = null;
                            if (timerMs !== null) {
                                timerId = __timerSeq++;
                                const timerLeg = __timer(timerMs, timerId).then((json) => {
                                    unwrapHostResponse(json);
                                    return { wake: "timer" };
                                });
                                // An abandoned timer's leg rejects after the
                                // race resolved; nobody awaits it by design.
                                timerLeg.catch(() => {});
                                legs.push(timerLeg);
                            }
                            if (legs.length === 0) {
                                const error = new Error("wait() needs at least one watch or a timerMs");
                                error.name = "WaitTimerError";
                                throw error;
                            }
                            const wake = await Promise.race(legs);
                            if (timerId !== null) {
                                if (wake.wake === "timer") {
                                    unwrapHostResponse(await __confirmTimer(timerId));
                                } else {
                                    __abandonTimer(timerId);
                                }
                            }
                            return wake;
                        };
                        if (typeof __workflow !== "undefined") {
                            globalThis.workflow = async (name, args = null) =>
                                unwrapHostResponse(await __workflow(String(name), JSON.stringify(args ?? null)));
                        }
                        if (typeof __deliver !== "undefined") {
                            globalThis.deliver = async (payload, options = {}) => unwrapHostResponse(
                                await __deliver(JSON.stringify({
                                    content: payload.content,
                                    metadata: payload.metadata ?? null,
                                    target: options.target ?? null,
                                }))
                            );
                        }
                        globalThis.parallel = async (thunks) => Promise.all(
                            thunks.map(async (thunk, index) => {
                                try { return await thunk(); }
                                catch (error) {
                                    __log(`parallel thunk ${index} failed: ${error.name}: ${error.message}`);
                                    return null;
                                }
                            })
                        );
                        // Per-item stage chains with no cross-item barrier: a
                        // failing item collapses to null at the failing stage
                        // and later stages for it are skipped; other items
                        // proceed unaffected.
                        globalThis.pipeline = async (items, ...stages) => Promise.all(
                            items.map(async (item, index) => {
                                let previous = item;
                                for (let stageIndex = 0; stageIndex < stages.length; stageIndex += 1) {
                                    try {
                                        previous = await stages[stageIndex](previous, item, index);
                                    } catch (error) {
                                        __log(`pipeline item ${index} stage ${stageIndex} failed: ${error.name}: ${error.message}`);
                                        return null;
                                    }
                                }
                                return previous;
                            })
                        );
                        globalThis.budget = Object.freeze({
                            spent: () => __budgetSpent(),
                            remaining: () => __budgetRemaining(),
                        });
                        globalThis.log = (message) => __log(String(message));
                        globalThis.phase = (title) => __phase(String(title));
                        globalThis.console = { log, warn: log, error: log };
                    "#).catch(&ctx).map_err(|error| error.to_string())?;
                }

                let promise: Promise<'_> = ctx
                    .eval(source)
                    .catch(&ctx)
                    .map_err(|error| error.to_string())?;
                promise
                    .into_future::<String>()
                    .await
                    .catch(&ctx)
                    .map_err(|error| error.to_string())
            })
            .await;

        if cancellation.is_cancelled() {
            return Err(WorkflowEngineError::Cancelled);
        }
        result.map_err(WorkflowEngineError::Script)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use serde_json::{Value, json};

    use super::*;
    use crate::{AgentCallResult, EffectReceipt};

    struct FakeHost {
        outputs: Mutex<VecDeque<Value>>,
        deliveries: Mutex<Vec<EffectPayload>>,
        records: Mutex<Vec<(String, String)>>,
        spent: u64,
        remaining: Option<u64>,
    }

    struct ConcurrentHost {
        active: AtomicUsize,
        peak: AtomicUsize,
    }
    #[async_trait]
    impl WorkflowHost for ConcurrentHost {
        async fn run_agent_call(
            &self,
            _: AgentCallRequest,
            _: CancellationToken,
        ) -> Result<AgentCallResult, String> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(AgentCallResult {
                output: json!("ok"),
                input_tokens: 0,
                output_tokens: 0,
                session_id: None,
                thread_id: None,
                turn_id: None,
            })
        }
        async fn deliver(
            &self,
            _: EffectPayload,
            _: CancellationToken,
        ) -> Result<EffectReceipt, String> {
            Err("unused".into())
        }
    }

    #[async_trait]
    impl WorkflowHost for FakeHost {
        fn record(&self, kind: &str, message: String) {
            self.records
                .lock()
                .expect("records")
                .push((kind.to_string(), message));
        }

        fn budget_spent(&self) -> u64 {
            self.spent
        }

        fn budget_remaining(&self) -> Option<u64> {
            self.remaining
        }

        async fn run_agent_call(
            &self,
            _: AgentCallRequest,
            _: CancellationToken,
        ) -> Result<AgentCallResult, String> {
            Ok(AgentCallResult {
                output: self
                    .outputs
                    .lock()
                    .expect("outputs")
                    .pop_front()
                    .expect("scripted output"),
                input_tokens: 1,
                output_tokens: 1,
                session_id: None,
                thread_id: None,
                turn_id: None,
            })
        }

        async fn deliver(
            &self,
            payload: EffectPayload,
            _: CancellationToken,
        ) -> Result<EffectReceipt, String> {
            self.deliveries.lock().expect("deliveries").push(payload);
            Ok(EffectReceipt {
                delivered: true,
                target: "test".into(),
                effect_seq: 0,
            })
        }
    }

    /// Records every request it serves; scripts timer and child-workflow
    /// outcomes so the imported surface (defaults, pipeline, wait,
    /// composition) is observable from the host side.
    struct SurfaceHost {
        agent_requests: Mutex<Vec<AgentCallRequest>>,
        agent_outputs: Mutex<VecDeque<Result<Value, String>>>,
        workflow_calls: Mutex<Vec<WorkflowCallRequest>>,
        workflow_output: Value,
        timer_calls: Mutex<Vec<u64>>,
        timer_settles: bool,
        agent_delay_ms: u64,
    }

    impl SurfaceHost {
        fn new() -> Self {
            Self {
                agent_requests: Mutex::new(Vec::new()),
                agent_outputs: Mutex::new(VecDeque::new()),
                workflow_calls: Mutex::new(Vec::new()),
                workflow_output: Value::Null,
                timer_calls: Mutex::new(Vec::new()),
                timer_settles: true,
                agent_delay_ms: 0,
            }
        }
    }

    #[async_trait]
    impl WorkflowHost for SurfaceHost {
        async fn run_agent_call(
            &self,
            request: AgentCallRequest,
            _: CancellationToken,
        ) -> Result<AgentCallResult, String> {
            self.agent_requests.lock().expect("requests").push(request);
            if self.agent_delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.agent_delay_ms)).await;
            }
            let output = self
                .agent_outputs
                .lock()
                .expect("outputs")
                .pop_front()
                .unwrap_or(Ok(json!("ok")))?;
            Ok(AgentCallResult {
                output,
                input_tokens: 1,
                output_tokens: 1,
                session_id: Some(uuid::Uuid::from_u128(0xA)),
                thread_id: Some(uuid::Uuid::from_u128(0xB)),
                turn_id: None,
            })
        }

        async fn deliver(
            &self,
            _: EffectPayload,
            _: CancellationToken,
        ) -> Result<EffectReceipt, String> {
            Err("unused".into())
        }

        async fn run_workflow_call(
            &self,
            request: WorkflowCallRequest,
            _: CancellationToken,
        ) -> Result<Value, String> {
            self.workflow_calls.lock().expect("calls").push(request);
            Ok(self.workflow_output.clone())
        }

        async fn timer(&self, ms: u64, _: u64, _: CancellationToken) -> Result<Value, String> {
            self.timer_calls.lock().expect("timers").push(ms);
            if self.timer_settles {
                tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                Ok(json!({"fired": true}))
            } else {
                std::future::pending::<()>().await;
                unreachable!("pending timer never settles")
            }
        }

        async fn confirm_timer(&self, _: u64) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn defaults_header_fills_unset_options_and_call_sites_override() {
        let host = Arc::new(SurfaceHost::new());
        host.agent_outputs
            .lock()
            .unwrap()
            .extend([Ok(json!("a")), Ok(json!("b"))]);
        WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"export const defaults = { agent: "worker", model: "cheap" };
                await agent("first");
                await agent("second", { model: "strong", label: undefined });
                return null;
                "#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("defaults run");
        let requests = host.agent_requests.lock().unwrap();
        assert_eq!(requests[0].agent, "worker");
        assert_eq!(requests[0].model.as_deref(), Some("cheap"));
        assert_eq!(requests[1].model.as_deref(), Some("strong"));
        // An explicit `undefined` must fall back, not erase the default.
        assert_eq!(requests[1].agent, "worker");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pipeline_runs_stages_per_item_and_collapses_failures_to_null() {
        let host = Arc::new(SurfaceHost::new());
        // Stage-1 calls are made in item order (the map runs synchronously to
        // its first await), so the scripted queue maps: one → ok, two → fail,
        // three → ok.
        host.agent_outputs.lock().unwrap().extend([
            Ok(json!("one-gathered")),
            Err("provider unavailable".into()),
            Ok(json!("three-gathered")),
        ]);
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"
                const results = await pipeline(
                    ["one", "two", "three"],
                    (item) => agent(`gather ${item}`, { agent: "gatherer" }),
                    // A pure second stage pins per-item chaining positionally:
                    // it sees the SAME item's stage-1 value plus the original
                    // item and index.
                    (previous, item, index) => `${previous}|${item}|${index}`,
                );
                return results;
                "#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("pipeline run");
        // Exact positional assertion: ordering preserved, the failed item is
        // null at its own slot with its later stage skipped, and each
        // survivor's stage 2 received its own stage-1 output.
        assert_eq!(
            serde_json::from_str::<Value>(&output).expect("output"),
            json!(["one-gathered|one|0", null, "three-gathered|three|2"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_start_surfaces_the_continuation_token_and_the_session_option_returns_it() {
        let host = Arc::new(SurfaceHost::new());
        host.agent_outputs
            .lock()
            .unwrap()
            .extend([Ok(json!("restated")), Ok(json!("worked"))]);
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"
                const worker = agentStart("restate the brief", { agent: "worker" });
                const before = worker.session;
                await worker.result;
                // The same worker session does the work: pass the token back.
                const value = await agent("proceed", { agent: "worker", session: worker.session });
                return { before, token: worker.session, value };
                "#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("continuity run");
        let value: Value = serde_json::from_str(&output).expect("output");
        // No token before completion; the host's identity after.
        assert_eq!(value["before"], Value::Null);
        assert_eq!(
            value["token"],
            json!({
                "session_id": uuid::Uuid::from_u128(0xA),
                "thread_id": uuid::Uuid::from_u128(0xB),
            })
        );
        assert_eq!(value["value"], "worked");
        // The continuation reached the host typed, not as loose JSON.
        let requests = host.agent_requests.lock().unwrap();
        assert_eq!(requests[0].session, None);
        assert_eq!(
            requests[1].session,
            Some(crate::AgentSessionRef {
                session_id: uuid::Uuid::from_u128(0xA),
                thread_id: uuid::Uuid::from_u128(0xB),
            })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_wakes_on_completion_before_the_timer() {
        let host = Arc::new(SurfaceHost::new());
        host.agent_outputs
            .lock()
            .unwrap()
            .extend([Ok(json!("done"))]);
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"
                const child = agentStart("work", { agent: "lead" });
                const wake = await wait({ watches: [child], timerMs: 60000 });
                return { wake: wake.wake, value: wake.value ?? null };
                "#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("wait run");
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({"wake": "completion", "value": "done"})
        );
        // The timer leg was armed through the host (journalled), even though
        // the completion won the race.
        assert_eq!(host.timer_calls.lock().unwrap().as_slice(), [60000]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wait_falls_back_to_the_timer_when_no_watch_completes() {
        let slow = Arc::new(SurfaceHost {
            agent_delay_ms: 5000,
            ..SurfaceHost::new()
        });
        slow.agent_outputs
            .lock()
            .unwrap()
            .extend([Ok(json!("slow"))]);
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"
                const child = agentStart("work", { agent: "lead" });
                const wake = await wait({ watches: [child], timerMs: 20 });
                return wake.wake;
                "#,
                "null",
                CancellationToken::new(),
                Some(slow as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("timer run");
        assert_eq!(serde_json::from_str::<Value>(&output).unwrap(), "timer");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn composition_reaches_the_host_only_when_granted() {
        let host = Arc::new(SurfaceHost {
            workflow_output: json!({"synthesis": "ready"}),
            ..SurfaceHost::new()
        });
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"return await workflow("gather-sources", { topic: "cast iron" });"#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities {
                    delivery: false,
                    composition: true,
                },
            )
            .await
            .expect("composed run");
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({"synthesis": "ready"})
        );
        let calls = host.workflow_calls.lock().unwrap();
        assert_eq!(calls[0].name, "gather-sources");
        assert_eq!(calls[0].args, json!({"topic": "cast iron"}));

        // Without the capability the global is simply absent — a child run
        // cannot compose, which is what bounds nesting to one level.
        let ungranted = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                "return typeof workflow;",
                "null",
                CancellationToken::new(),
                Some(Arc::new(SurfaceHost::new()) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("ungranted run");
        assert_eq!(
            serde_json::from_str::<Value>(&ungranted).unwrap(),
            "undefined"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_watched_calls_failure_is_reported_as_its_wake_not_a_crash() {
        let host = Arc::new(SurfaceHost::new());
        host.agent_outputs
            .lock()
            .unwrap()
            .extend([Err("BudgetExceededError: token limit reached".into())]);
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"
                const child = agentStart("work", { agent: "lead" });
                const wake = await wait({ watches: [child], timerMs: 60000 });
                return { wake: wake.wake, errorName: wake.error ? wake.error.name : null };
                "#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("failure wake run");
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({"wake": "completion", "errorName": "BudgetExceededError"})
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn caged_context_exposes_no_ambient_time_io_or_randomness() {
        let engine = WorkflowEngine::new(EngineLimits::default());
        let result = engine
            .execute(
                r#"
                let randomError = null;
                if (typeof Math !== "undefined") {
                    try { Math.random(); } catch (error) { randomError = error.message; }
                }
                return {
                    date: typeof Date,
                    eval: typeof eval,
                    performance: typeof performance,
                    fetch: typeof fetch,
                    timeout: typeof setTimeout,
                    process: typeof process,
                    randomError,
                };
                "#,
                CancellationToken::new(),
            )
            .await
            .expect("caged script should run");

        let value: serde_json::Value = serde_json::from_str(&result).expect("JSON result");
        assert_eq!(value["date"], "undefined");
        assert_eq!(value["eval"], "undefined");
        assert_eq!(value["performance"], "undefined");
        assert_eq!(value["fetch"], "undefined");
        assert_eq!(value["timeout"], "undefined");
        assert_eq!(value["process"], "undefined");
        assert!(
            value["randomError"].is_null()
                || value["randomError"]
                    .as_str()
                    .is_some_and(|message| { message.contains("Math.random is not available") })
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_interrupts_a_runaway_script() {
        let engine = WorkflowEngine::new(EngineLimits::default());
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = engine.execute("while (true) {}", cancellation).await;
        assert!(matches!(result, Err(WorkflowEngineError::Cancelled)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn memory_limit_stops_an_allocation_bomb() {
        let engine = WorkflowEngine::new(EngineLimits {
            memory_bytes: 2 * 1024 * 1024,
            stack_bytes: 512 * 1024,
        });
        let result = engine
            .execute(
                "const values=[]; while (true) { values.push(new Array(10000).fill(1)); }",
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(
            result,
            Err(WorkflowEngineError::Script(_)) | Err(WorkflowEngineError::Runtime(_))
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reviewed_content_is_the_only_value_delivered() {
        let host = Arc::new(FakeHost {
            outputs: Mutex::new(VecDeque::from([
                json!("unchecked draft"),
                json!({"approved": true, "reasons": "corrected", "approved_content": "reviewed copy"}),
            ])),
            deliveries: Mutex::new(Vec::new()),
            records: Mutex::new(Vec::new()),
            spent: 0,
            remaining: None,
        });
        let engine = WorkflowEngine::new(EngineLimits::default());
        let output = engine.execute_hosted(
            r#"
            const draft = await agent(args.brief, { agent: "writer" });
            const verdict = await agent(`Review: ${draft}`, { agent: "reviewer", schema: { type: "object" } });
            if (verdict && verdict.approved && verdict.approved_content) {
                const receipt = await deliver({ content: verdict.approved_content });
                return { posted: true, receipt };
            }
            return { posted: false };
            "#,
            r#"{"brief":"write"}"#,
            CancellationToken::new(),
            Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
            ScriptCapabilities { delivery: true, composition: false },
        ).await.expect("composition");

        assert_eq!(
            serde_json::from_str::<Value>(&output).expect("output")["posted"],
            true
        );
        let deliveries = host.deliveries.lock().expect("deliveries");
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].content, "reviewed copy");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejection_and_missing_grant_cannot_deliver() {
        let host = Arc::new(FakeHost {
            outputs: Mutex::new(VecDeque::from([
                json!("draft"),
                json!({"approved": false, "reasons": "mistake"}),
            ])),
            deliveries: Mutex::new(Vec::new()),
            records: Mutex::new(Vec::new()),
            spent: 0,
            remaining: None,
        });
        let engine = WorkflowEngine::new(EngineLimits::default());
        let output = engine
            .execute_hosted(
                r#"
            const draft = await agent("write", { agent: "writer" });
            const verdict = await agent(draft, { agent: "reviewer" });
            return { posted: false, deliverType: typeof deliver, approved: verdict.approved };
            "#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("composition");
        let output: Value = serde_json::from_str(&output).expect("output");
        assert_eq!(output["deliverType"], "undefined");
        assert!(host.deliveries.lock().expect("deliveries").is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parallel_fans_agent_calls_out_concurrently() {
        let host = Arc::new(ConcurrentHost {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        });
        WorkflowEngine::new(EngineLimits::default()).execute_hosted("return await parallel([() => agent('a',{agent:'one'}), () => agent('b',{agent:'two'})]);","null",CancellationToken::new(),Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),ScriptCapabilities::default()).await.expect("parallel run");
        assert_eq!(host.peak.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn committed_write_review_deliver_example_posts_reviewed_content() {
        let host = Arc::new(FakeHost {
            outputs: Mutex::new(VecDeque::from([
                json!("unchecked draft"),
                json!({"accepted": true, "content": "reviewed copy"}),
            ])),
            deliveries: Mutex::new(Vec::new()),
            records: Mutex::new(Vec::new()),
            spent: 2,
            remaining: Some(8),
        });
        let source = include_str!("../../../examples/workflows/write-review-deliver.js");
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                source,
                r#"{"writerPrompt":"draft","writerAgent":"writer","reviewerAgent":"reviewer","deliveryTarget":"public"}"#,
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities { delivery: true, composition: false },
            )
            .await
            .expect("committed example runs");

        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            "reviewed copy"
        );
        let deliveries = host.deliveries.lock().expect("deliveries");
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].content, "reviewed copy");
        assert_eq!(deliveries[0].target.as_deref(), Some("public"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn committed_write_review_deliver_example_does_not_post_rejection() {
        let host = Arc::new(FakeHost {
            outputs: Mutex::new(VecDeque::from([
                json!("unchecked draft"),
                json!({"accepted": false, "content": "must not leave"}),
            ])),
            deliveries: Mutex::new(Vec::new()),
            records: Mutex::new(Vec::new()),
            spent: 2,
            remaining: None,
        });
        let source = include_str!("../../../examples/workflows/write-review-deliver.js");
        let result = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                source,
                r#"{"writerPrompt":"draft","writerAgent":"writer","reviewerAgent":"reviewer","deliveryTarget":"public"}"#,
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities { delivery: true, composition: false },
            )
            .await;

        assert!(matches!(result, Err(WorkflowEngineError::Script(_))));
        assert!(host.deliveries.lock().expect("deliveries").is_empty());
    }

    struct ErrorHost(&'static str);

    #[async_trait]
    impl WorkflowHost for ErrorHost {
        async fn run_agent_call(
            &self,
            _: AgentCallRequest,
            _: CancellationToken,
        ) -> Result<AgentCallResult, String> {
            Err(self.0.to_string())
        }

        async fn deliver(
            &self,
            _: EffectPayload,
            _: CancellationToken,
        ) -> Result<EffectReceipt, String> {
            Err(self.0.to_string())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scripts_can_discriminate_named_host_errors() {
        let host: Arc<dyn WorkflowHost> =
            Arc::new(ErrorHost("BudgetExceededError: agent call limit reached"));
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                "try { await agent('x',{agent:'writer'}); } catch (error) { return {name:error.name,message:error.message}; }",
                "null",
                CancellationToken::new(),
                Some(host),
                ScriptCapabilities::default(),
            )
            .await
            .expect("error is catchable");
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({"name":"BudgetExceededError","message":"agent call limit reached"})
        );
    }

    #[test]
    fn host_error_contract_names_known_and_generic_agent_failures() {
        assert_eq!(
            named_host_error("DeliveryNotGrantedError: no target", "DeliveryError"),
            ("DeliveryNotGrantedError".into(), "no target".into())
        );
        assert_eq!(
            named_host_error("WorkflowCancelledError: stopped", "AgentCallError"),
            ("WorkflowCancelledError".into(), "stopped".into())
        );
        assert_eq!(
            named_host_error("NondeterministicReplayError", "AgentCallError"),
            ("NondeterministicCallError".into(), String::new())
        );
        assert_eq!(
            named_host_error("provider unavailable", "AgentCallError"),
            ("AgentCallError".into(), "provider unavailable".into())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_schemaless_agent_output_is_a_named_error() {
        let host = Arc::new(FakeHost {
            outputs: Mutex::new(VecDeque::from([json!("  ")])),
            deliveries: Mutex::new(Vec::new()),
            records: Mutex::new(Vec::new()),
            spent: 0,
            remaining: Some(10),
        });
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                "try { await agent('x',{agent:'writer'}); } catch (error) { return error.name; }",
                "null",
                CancellationToken::new(),
                Some(host as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("empty output is catchable");
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            "EmptyAgentOutputError"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn budget_reads_and_parallel_failures_use_host_state_and_journal_log() {
        let host = Arc::new(FakeHost {
            outputs: Mutex::new(VecDeque::new()),
            deliveries: Mutex::new(Vec::new()),
            records: Mutex::new(Vec::new()),
            spent: 7,
            remaining: Some(13),
        });
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                "const values=await parallel([()=>{throw new Error('nope')}]); return {values,spent:budget.spent(),remaining:budget.remaining()};",
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("budget surface");
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({"values":[null],"spent":7,"remaining":13})
        );
        assert!(host.records.lock().unwrap().iter().any(|(kind, message)| {
            kind == "log" && message.contains("parallel thunk 0 failed: Error: nope")
        }));
    }
}
