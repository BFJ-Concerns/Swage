use std::sync::Arc;

use serde_json::Value;

use rquickjs::{
    AsyncContext, AsyncRuntime, CatchResultExt, Function, Promise,
    context::intrinsic::{Eval, Json, MapSet, Promise as PromiseIntrinsic, RegExp, RegExpCompiler},
    prelude::Async,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    AgentCallRequest, AwaitEventRequest, EffectPayload, GateRequest, RunEventPost,
    WorkflowCallRequest, WorkflowHost, WorkflowHostError,
};

fn named_host_error(error: &str, fallback: &str) -> (String, String) {
    const NAMES: [(&str, &str); 13] = [
        ("AgentCallError", "AgentCallError"),
        ("AgentCallTimeoutError", "AgentCallTimeoutError"),
        ("BudgetExceededError", "BudgetExceededError"),
        ("BudgetWindDownError", "BudgetWindDownError"),
        ("DeliveryNotGrantedError", "DeliveryNotGrantedError"),
        ("WorkflowCancelledError", "WorkflowCancelledError"),
        ("NondeterministicReplayError", "NondeterministicCallError"),
        ("WorkerPlacementError", "WorkerPlacementError"),
        ("EmptyAgentOutputError", "EmptyAgentOutputError"),
        ("WorkflowCompositionError", "WorkflowCompositionError"),
        ("WaitTimerError", "WaitTimerError"),
        ("RunEventError", "RunEventError"),
        ("BudgetSnapshotError", "BudgetSnapshotError"),
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

/// Metadata carried by portable workflow scripts. It is deliberately save-time
/// information: the caged runtime never exposes it to the script.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptMeta {
    pub name: Option<String>,
    pub description: Option<String>,
    /// Optional JSON Schema governing the value passed as this workflow's
    /// arguments. Header metadata stays outside the caged runtime.
    pub argument_schema: Option<Value>,
}

/// Read an optional leading portable `export const meta = { … };` header.
pub fn script_meta(source: &str) -> Result<ScriptMeta, String> {
    let Some((object, _)) = split_meta_header(source)? else {
        return Ok(ScriptMeta::default());
    };
    Ok(ScriptMeta {
        name: meta_string_field(object, "name"),
        description: meta_string_field(object, "description"),
        argument_schema: meta_json_field(object, "argumentSchema")?,
    })
}

/// Return a leading portable `meta` object and the source following it.
fn split_meta_header(source: &str) -> Result<Option<(&str, &str)>, String> {
    let rest = &source[skip_whitespace_and_comments(source)..];
    let Some(after_export) = rest.strip_prefix("export") else {
        return Ok(None);
    };
    let Some(after_const) = after_export.trim_start().strip_prefix("const") else {
        return Ok(None);
    };
    let Some(after_meta) = after_const.trim_start().strip_prefix("meta") else {
        return Ok(None);
    };
    let after_equals = after_meta
        .trim_start()
        .strip_prefix('=')
        .ok_or("workflow meta must be assigned an object literal")?;
    let object = after_equals.trim_start();
    if !object.starts_with('{') {
        return Err("workflow meta must be an object literal".into());
    }
    let end = find_balanced_object_end(object)?;
    let mut body = object[end + 1..].trim_start();
    if let Some(after_semicolon) = body.strip_prefix(';') {
        body = after_semicolon;
    }
    Ok(Some((&object[..=end], body)))
}

/// Extract a double-quoted portable metadata value from either JavaScript or
/// JSON object syntax. The portable runtime emits unquoted JavaScript keys.
fn meta_string_field(object: &str, field: &str) -> Option<String> {
    meta_value_range(object, field)
        .and_then(|(start, end)| serde_json::from_str(&object[start..end]).ok())
}

/// Locate a metadata value without confusing nested commas for another field.
/// Both string metadata and JSON-valued metadata use this one walk so their
/// JavaScript-header handling stays identical.
fn meta_value_range(object: &str, field: &str) -> Option<(usize, usize)> {
    let bytes = object.as_bytes();
    let mut index = 1;
    while index < bytes.len() {
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace)
            || bytes.get(index) == Some(&b',')
        {
            index += 1;
        }
        let (key, after_key) = if bytes.get(index) == Some(&b'"') {
            json_string_at(object, index)?
        } else {
            let key_start = index;
            while bytes
                .get(index)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                index += 1;
            }
            if key_start == index {
                return None;
            }
            (object[key_start..index].to_owned(), index)
        };
        index = after_key;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index) != Some(&b':') {
            return None;
        }
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        let value_start = index;
        let mut value_end = skip_meta_value(object, index);
        if bytes.get(value_end.saturating_sub(1)) == Some(&b',') {
            value_end -= 1;
        }
        if key == field {
            return Some((value_start, value_end));
        }
        index = value_end;
    }
    None
}

/// Read one metadata value as JSON. Schema values deliberately use JSON even
/// though metadata accepts JavaScript-style unquoted keys around them: JSON
/// Schema itself is a JSON document and must be portable across script hosts.
fn meta_json_field(object: &str, field: &str) -> Result<Option<Value>, String> {
    meta_value_range(object, field)
        .map(|(start, end)| serde_json::from_str(&object[start..end]))
        .transpose()
        .map_err(|error| format!("workflow meta {field} must be valid JSON Schema: {error}"))
}

/// Return the decoded JSON string at `start` and the first byte after it.
fn json_string_at(source: &str, start: usize) -> Option<(String, usize)> {
    let bytes = source.as_bytes();
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut index = start + 1;
    while let Some(byte) = bytes.get(index) {
        if *byte == b'\\' {
            index += 2;
        } else if *byte == b'"' {
            index += 1;
            return serde_json::from_str(&source[start..index])
                .ok()
                .map(|value| (value, index));
        } else {
            index += 1;
        }
    }
    None
}

/// Advance through one metadata value without mistaking nested commas for a
/// following field. Metadata only saves string fields, but unrelated object,
/// array, and scalar fields must not hide later `name` or `description` data.
fn skip_meta_value(source: &str, start: usize) -> usize {
    let bytes = source.as_bytes();
    let mut index = start;
    let mut depth = 0usize;
    while let Some(byte) = bytes.get(index) {
        match *byte {
            b'"' | b'\'' | b'`' => {
                let quote = *byte;
                index += 1;
                while let Some(byte) = bytes.get(index) {
                    if *byte == b'\\' {
                        index += 2;
                    } else if *byte == quote {
                        index += 1;
                        break;
                    } else {
                        index += 1;
                    }
                }
                continue;
            }
            b'{' | b'[' | b'(' => depth += 1,
            b'}' | b']' | b')' if depth > 0 => depth -= 1,
            b'}' if depth == 0 => return index,
            b',' if depth == 0 => return index + 1,
            _ => {}
        }
        index += 1;
    }
    index
}

/// Splits optional portable `meta` and `defaults` headers off a script
/// body. The body is wrapped in an async function before evaluation, where
/// `export` is a syntax error — so the header can only be the first
/// statement (comments permitted before it), and an existing script cannot
/// already contain one.
fn extract_defaults(source: &str) -> Result<(Option<String>, &str), String> {
    let body = split_meta_header(source)?.map_or(source, |(_, body)| body);
    let header_end = skip_whitespace_and_comments(body);
    let rest = &body[header_end..];
    let Some(after_keyword) = rest.strip_prefix("export") else {
        return Ok((None, body));
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

    #[error("workflow script failed: {message}")]
    AgentCall {
        host_reference: String,
        message: String,
        stack: Option<String>,
    },
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
            "(async () => {{ try {{ const value = await (async () => {{ {source} }})(); return JSON.stringify({{ result: JSON.stringify(value === undefined ? null : value) }}); }} catch (error) {{ if (error && typeof error.__swageAgentCallReference === 'string') return JSON.stringify({{ agent_call: {{ host_reference: error.__swageAgentCallReference, message: String(error.name) + ': ' + String(error.message), stack: typeof error.stack === 'string' ? error.stack : null }} }}); throw error; }} }})()",
        );
        let args_json = args_json.to_string();
        let host_cancellation = (*cancellation).clone();

        let execution = context.async_with(async move |ctx| {
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
                                match host.dispatch_agent_call(request.clone(), cancellation).await {
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
                                        let (host_reference, error) = match error {
                                            WorkflowHostError::AgentCall { host_reference, message } => (Some(host_reference), message),
                                            WorkflowHostError::Message(message) => (None, message),
                                        };
                                        let (name, message) = named_host_error(&error, "AgentCallError");
                                        serde_json::json!({
                                            "ok": false,
                                            "error": {"name": name, "message": message, "host_reference": host_reference}
                                        }).to_string()
                                    }
                                }
                            }
                        }),
                    ).map_err(|error| error.to_string())?;
                    ctx.globals().set("__agent", agent).map_err(|error| error.to_string())?;

                    let placement_host = Arc::clone(&host);
                    let placement_cancellation = host_cancellation.clone();
                    let placements = Function::new(
                        ctx.clone(),
                        Async(move || {
                            let host = Arc::clone(&placement_host);
                            let cancellation = placement_cancellation.clone();
                            async move {
                                match host.list_worker_placements(cancellation).await {
                                    Ok(value) => serde_json::json!({"ok": true, "value": value}).to_string(),
                                    Err(error) => serde_json::json!({"ok": false, "error": error}).to_string(),
                                }
                            }
                        }),
                    ).map_err(|error| error.to_string())?;
                    ctx.globals().set("__workerPlacements", placements).map_err(|error| error.to_string())?;

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

                    let post_event_host = Arc::clone(&host);
                    let post_event_cancellation = host_cancellation.clone();
                    let post_event = Function::new(
                        ctx.clone(),
                        Async(move |kind: String, payload_json: String| {
                            let host = Arc::clone(&post_event_host);
                            let cancellation = post_event_cancellation.clone();
                            async move {
                                let payload: serde_json::Value = match serde_json::from_str(&payload_json) {
                                    Ok(payload) => payload,
                                    Err(error) => return serde_json::json!({
                                        "ok": false,
                                        "error": {"name": "RunEventError", "message": error.to_string()}
                                    }).to_string(),
                                };
                                match host.post_run_event(RunEventPost { kind, payload }, cancellation).await {
                                    Ok(value) => serde_json::json!({"ok": true, "value": value}).to_string(),
                                    Err(error) => {
                                        let (name, message) = named_host_error(&error, "RunEventError");
                                        serde_json::json!({
                                            "ok": false,
                                            "error": {"name": name, "message": message}
                                        }).to_string()
                                    }
                                }
                            }
                        }),
                    ).map_err(|error| error.to_string())?;
                    ctx.globals().set("__postEvent", post_event).map_err(|error| error.to_string())?;

                    let await_event_host = Arc::clone(&host);
                    let await_event_cancellation = host_cancellation.clone();
                    let await_event = Function::new(
                        ctx.clone(),
                        Async(move |request_json: String| {
                            let host = Arc::clone(&await_event_host);
                            let cancellation = await_event_cancellation.clone();
                            async move {
                                let request: AwaitEventRequest = match serde_json::from_str(&request_json) {
                                    Ok(request) => request,
                                    Err(error) => return serde_json::json!({"ok": false, "error": {"name": "RunEventError", "message": error.to_string()}}).to_string(),
                                };
                                match host.await_run_event(request, cancellation).await {
                                    Ok(value) => serde_json::json!({"ok": true, "value": value}).to_string(),
                                    Err(error) => {
                                        let (name, message) = named_host_error(&error, "RunEventError");
                                        serde_json::json!({"ok": false, "error": {"name": name, "message": message}}).to_string()
                                    }
                                }
                            }
                        }),
                    ).map_err(|error| error.to_string())?;
                    ctx.globals().set("__awaitEvent", await_event).map_err(|error| error.to_string())?;

                    let gate_host = Arc::clone(&host);
                    let gate_cancellation = host_cancellation.clone();
                    let gate = Function::new(
                        ctx.clone(),
                        Async(move |request_json: String| {
                            let host = Arc::clone(&gate_host);
                            let cancellation = gate_cancellation.clone();
                            async move {
                                let request: GateRequest = match serde_json::from_str(&request_json) {
                                    Ok(request) => request,
                                    Err(error) => return serde_json::json!({
                                        "ok": false,
                                        "error": {"name": "RunEventError", "message": error.to_string()}
                                    }).to_string(),
                                };
                                match host.evaluate_wake_gate(request, cancellation).await {
                                    Ok(value) => serde_json::json!({"ok": true, "value": value}).to_string(),
                                    Err(error) => {
                                        let (name, message) = named_host_error(&error, "RunEventError");
                                        serde_json::json!({
                                            "ok": false,
                                            "error": {"name": name, "message": message}
                                        }).to_string()
                                    }
                                }
                            }
                        }),
                    ).map_err(|error| error.to_string())?;
                    ctx.globals().set("__gate", gate).map_err(|error| error.to_string())?;

                    let deliver_events_host = Arc::clone(&host);
                    let deliver_events_cancellation = host_cancellation.clone();
                    let deliver_events = Function::new(
                        ctx.clone(),
                        Async(move |up_to_seq: f64| {
                            let host = Arc::clone(&deliver_events_host);
                            let cancellation = deliver_events_cancellation.clone();
                            async move {
                                if !up_to_seq.is_finite() || up_to_seq < 0.0 {
                                    return serde_json::json!({
                                        "ok": false,
                                        "error": {"name": "RunEventError", "message": "upToSeq must be a non-negative finite number"}
                                    }).to_string();
                                }
                                match host.deliver_run_events(up_to_seq as u64, cancellation).await {
                                    Ok(value) => serde_json::json!({"ok": true, "value": value}).to_string(),
                                    Err(error) => {
                                        let (name, message) = named_host_error(&error, "RunEventError");
                                        serde_json::json!({
                                            "ok": false,
                                            "error": {"name": name, "message": message}
                                        }).to_string()
                                    }
                                }
                            }
                        }),
                    ).map_err(|error| error.to_string())?;
                    ctx.globals().set("__deliverEvents", deliver_events).map_err(|error| error.to_string())?;

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

                    let budget_snapshot_host = Arc::clone(&host);
                    let budget_snapshot_cancellation = host_cancellation.clone();
                    let budget_snapshot = Function::new(
                        ctx.clone(),
                        Async(move || {
                            let host = Arc::clone(&budget_snapshot_host);
                            let cancellation = budget_snapshot_cancellation.clone();
                            async move {
                                match host.budget_snapshot(cancellation).await {
                                    Ok(value) => serde_json::json!({"ok": true, "value": value}).to_string(),
                                    Err(error) => {
                                        let (name, message) = named_host_error(&error, "BudgetSnapshotError");
                                        serde_json::json!({
                                            "ok": false,
                                            "error": {"name": name, "message": message}
                                        }).to_string()
                                    }
                                }
                            }
                        }),
                    ).map_err(|error| error.to_string())?;
                    ctx.globals().set("__budgetSnapshot", budget_snapshot).map_err(|error| error.to_string())?;

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
                                if (typeof response.error.host_reference === "string") {
                                    Object.defineProperty(error, "__swageAgentCallReference", {
                                        value: response.error.host_reference,
                                    });
                                }
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
                            const knownOptions = new Set([
                                "agent", "task", "tools", "model", "effort", "timeoutMs",
                                "cwd", "isolation", "schema", "maxAttempts", "label",
                                "session", "elements", "variables", "assembly",
                            ]);
                            for (const key of Object.keys(merged)) {
                                if (!knownOptions.has(key)) {
                                    const error = new Error(`unknown agent() option '${key}'`);
                                    error.name = "UnknownAgentOptionError";
                                    throw error;
                                }
                            }
                            if (merged.agent !== undefined && merged.agent !== null && !merged.agent) {
                                const error = new Error("opts.agent is required");
                                error.name = "AgentRequiredError";
                                throw error;
                            }
                            return {
                                prompt: "",
                                ...(merged.agent === undefined || merged.agent === null
                                    ? {}
                                    : { agent: merged.agent }),
                                ...(merged.task === undefined || merged.task === null
                                    ? {}
                                    : { task: String(merged.task) }),
                                ...(merged.tools === undefined || merged.tools === null
                                    ? {}
                                    : { tools: merged.tools.map(String) }),
                                model: merged.model ?? null,
                                ...(merged.effort === undefined || merged.effort === null
                                    ? {}
                                    : { effort: String(merged.effort) }),
                                ...(merged.timeoutMs === undefined || merged.timeoutMs === null
                                    ? {}
                                    : { timeout_ms: merged.timeoutMs }),
                                ...(merged.cwd === undefined || merged.cwd === null
                                    ? {}
                                    : { cwd: String(merged.cwd) }),
                                ...(merged.isolation === undefined || merged.isolation === null
                                    ? {}
                                    : { isolation: String(merged.isolation) }),
                                schema: merged.schema ?? null,
                                max_attempts: merged.maxAttempts ?? 3,
                                label: merged.label ?? null,
                                session: merged.session ?? null,
                                elements: merged.elements ?? null,
                                variables: merged.variables ?? null,
                                assembly: merged.assembly ?? null,
                            };
                        };
                        const validateAgentPrompt = (prompt, caller) => {
                            if (typeof prompt !== "string") {
                                throw new TypeError(`${caller} prompt must be a string`);
                            }
                        };
                        globalThis.agent = async (prompt, options = {}) => {
                            validateAgentPrompt(prompt, "agent");
                            const request = buildAgentRequest(options);
                            const response = unwrapAgentResponse(await __agent(prompt, JSON.stringify(request)));
                            return response.value;
                        };
                        globalThis.workerPlacements = async () => {
                            const response = JSON.parse(await __workerPlacements());
                            if (!response.ok) {
                                const error = new Error(response.error);
                                error.name = "WorkerPlacementError";
                                throw error;
                            }
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
                            validateAgentPrompt(prompt, "agentStart");
                            const request = buildAgentRequest(options);
                            const handle = {
                                label: request.label,
                                done: false,
                                session: request.session ?? null,
                            };
                            handle.result = __agent(prompt, JSON.stringify(request))
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
                        // The run's durable pending-event queue: completions,
                        // failures, and escalations accumulate as data
                        // between lead wakes. postEvent appends (journalled,
                        // exactly-once on resume); gate() asks a cheap model
                        // whether the pending events are worth waking the
                        // lead for — a suppressed event stays pending and
                        // rides the next wake; deliverEvents(upToSeq) stamps
                        // events delivered once a wake has carried them.
                        globalThis.postEvent = async (kind, payload = null) =>
                            unwrapHostResponse(await __postEvent(String(kind), JSON.stringify(payload ?? null)));
                        globalThis.awaitEvent = async ({ kinds }) =>
                            unwrapHostResponse(await __awaitEvent(JSON.stringify({ kinds })));
                        globalThis.gate = async (options = {}) =>
                            unwrapHostResponse(await __gate(JSON.stringify({
                                criteria: options.criteria ?? "",
                                model: options.model ?? null,
                            })));
                        globalThis.deliverEvents = async (upToSeq) =>
                            unwrapHostResponse(await __deliverEvents(upToSeq));
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
                        // Synchronous budget reads are live diagnostics — an
                        // unbounded budget reads as undefined, and replay sees
                        // end-state, not what this moment saw. A script that
                        // BRANCHES on budget state must use budgetSnapshot():
                        // journalled, so replay repeats the recorded reading.
                        // The snapshot also names the working and reserved
                        // closing allowances and whether wind-down has begun.
                        globalThis.budget = Object.freeze({
                            spent: () => __budgetSpent(),
                            remaining: () => __budgetRemaining(),
                        });
                        globalThis.budgetSnapshot = async () =>
                            unwrapHostResponse(await __budgetSnapshot());
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
            });
        tokio::pin!(execution);
        let result = tokio::select! {
            result = &mut execution => result,
            () = cancellation.cancelled() => return Err(WorkflowEngineError::Cancelled),
        };

        if cancellation.is_cancelled() {
            return Err(WorkflowEngineError::Cancelled);
        }
        match result {
            Ok(output) => {
                let envelope: serde_json::Value = serde_json::from_str(&output)
                    .map_err(|error| WorkflowEngineError::Script(error.to_string()))?;
                if let Some(failure) = envelope.get("agent_call") {
                    return Err(WorkflowEngineError::AgentCall {
                        host_reference: failure
                            .get("host_reference")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        message: failure
                            .get("message")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("AgentCallError: agent call failed")
                            .to_owned(),
                        stack: failure
                            .get("stack")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                    });
                }
                envelope
                    .get("result")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        WorkflowEngineError::Script("workflow result envelope is malformed".into())
                    })
            }
            Err(error) => Err(WorkflowEngineError::Script(error)),
        }
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
            _: Option<crate::host::ResolvedWorkerPlacement>,
            _: CancellationToken,
        ) -> Result<AgentCallResult, WorkflowHostError> {
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
            _: Option<crate::host::ResolvedWorkerPlacement>,
            _: CancellationToken,
        ) -> Result<AgentCallResult, WorkflowHostError> {
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
        resolved_working_directories: Mutex<Vec<Option<String>>>,
        placement_requests: Mutex<
            Vec<(
                Option<String>,
                Option<swage_protocol::client::workflow::WorkerIsolation>,
            )>,
        >,
        agent_outputs: Mutex<VecDeque<Result<Value, String>>>,
        workflow_calls: Mutex<Vec<WorkflowCallRequest>>,
        workflow_output: Value,
        timer_calls: Mutex<Vec<u64>>,
        timer_settles: bool,
        agent_delay_ms: u64,
        event_posts: Mutex<Vec<RunEventPost>>,
        awaited_events: Mutex<Vec<AwaitEventRequest>>,
        gate_requests: Mutex<Vec<GateRequest>>,
        gate_verdict: Value,
        deliveries_up_to: Mutex<Vec<u64>>,
        budget_snapshot: Value,
    }

    impl SurfaceHost {
        fn new() -> Self {
            Self {
                agent_requests: Mutex::new(Vec::new()),
                resolved_working_directories: Mutex::new(Vec::new()),
                placement_requests: Mutex::new(Vec::new()),
                agent_outputs: Mutex::new(VecDeque::new()),
                workflow_calls: Mutex::new(Vec::new()),
                workflow_output: Value::Null,
                timer_calls: Mutex::new(Vec::new()),
                timer_settles: true,
                agent_delay_ms: 0,
                event_posts: Mutex::new(Vec::new()),
                awaited_events: Mutex::new(Vec::new()),
                gate_requests: Mutex::new(Vec::new()),
                gate_verdict: Value::Null,
                deliveries_up_to: Mutex::new(Vec::new()),
                budget_snapshot: json!({
                    "spent": 0,
                    "remaining": null,
                    "working_remaining": null,
                    "closing_remaining": null,
                    "winding_down": false,
                }),
            }
        }
    }

    #[async_trait]
    impl WorkflowHost for SurfaceHost {
        async fn run_agent_call(
            &self,
            request: AgentCallRequest,
            resolved_placement: Option<crate::host::ResolvedWorkerPlacement>,
            _: CancellationToken,
        ) -> Result<AgentCallResult, WorkflowHostError> {
            self.agent_requests.lock().expect("requests").push(request);
            self.resolved_working_directories
                .lock()
                .expect("resolved paths")
                .push(resolved_placement.map(|placement| placement.path));
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

        async fn resolve_worker_placement(
            &self,
            request: &AgentCallRequest,
            _: CancellationToken,
        ) -> Result<Option<crate::host::ResolvedWorkerPlacement>, String> {
            self.placement_requests
                .lock()
                .expect("placements")
                .push((request.cwd.clone(), request.isolation));
            Ok(match request.isolation {
                Some(_) => Some(crate::host::ResolvedWorkerPlacement {
                    path: "/runtime/worktrees/worker-0".into(),
                    can_write: true,
                    authority_path: request.cwd.clone().unwrap_or_default(),
                    authority_context_id: uuid::Uuid::nil(),
                }),
                None => request
                    .cwd
                    .clone()
                    .map(|path| crate::host::ResolvedWorkerPlacement {
                        authority_path: path.clone(),
                        path,
                        can_write: true,
                        authority_context_id: uuid::Uuid::nil(),
                    }),
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

        async fn post_run_event(
            &self,
            event: RunEventPost,
            _: CancellationToken,
        ) -> Result<Value, String> {
            let mut posts = self.event_posts.lock().expect("posts");
            posts.push(event.clone());
            Ok(json!({"seq": posts.len(), "kind": event.kind, "payload": event.payload}))
        }

        async fn await_run_event(
            &self,
            request: AwaitEventRequest,
            _: CancellationToken,
        ) -> Result<Value, String> {
            self.awaited_events.lock().expect("awaits").push(request);
            Ok(json!({"seq": 4, "kind": "operator.decision", "payload": {"verdict": "accept"}}))
        }

        async fn evaluate_wake_gate(
            &self,
            request: GateRequest,
            _: CancellationToken,
        ) -> Result<Value, String> {
            self.gate_requests.lock().expect("gates").push(request);
            Ok(self.gate_verdict.clone())
        }

        async fn deliver_run_events(
            &self,
            up_to_seq: u64,
            _: CancellationToken,
        ) -> Result<Value, String> {
            self.deliveries_up_to
                .lock()
                .expect("deliveries")
                .push(up_to_seq);
            Ok(json!({"delivered": up_to_seq}))
        }

        async fn budget_snapshot(&self, _: CancellationToken) -> Result<Value, String> {
            Ok(self.budget_snapshot.clone())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn budget_surface_exposes_wind_down_state_and_named_signal() {
        let host = Arc::new(SurfaceHost {
            budget_snapshot: json!({
                "spent": 8,
                "remaining": 4,
                "working_remaining": 0,
                "closing_remaining": 4,
                "winding_down": true,
            }),
            ..SurfaceHost::new()
        });
        host.agent_outputs.lock().unwrap().extend([Err(
            "BudgetWindDownError: working token allowance exhausted".into(),
        )]);
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"
                const snapshot = await budgetSnapshot();
                let errorName = null;
                try { await agent("ordinary work", {agent: "lead"}); }
                catch (error) { errorName = error.name; }
                return {snapshot, errorName};
                "#,
                "null",
                CancellationToken::new(),
                Some(host as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("wind-down observable");
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({
                "snapshot": {
                    "spent": 8,
                    "remaining": 4,
                    "working_remaining": 0,
                    "closing_remaining": 4,
                    "winding_down": true,
                },
                "errorName": "BudgetWindDownError",
            })
        );
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
        assert_eq!(requests[0].agent.as_deref(), Some("worker"));
        assert_eq!(requests[0].model.as_deref(), Some("cheap"));
        assert_eq!(requests[1].model.as_deref(), Some("strong"));
        // An explicit `undefined` must fall back, not erase the default.
        assert_eq!(requests[1].agent.as_deref(), Some("worker"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn portable_meta_header_is_ignored_when_the_script_runs() {
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"export const meta = {"name":"portable","description":"runs unchanged"}; return {ok: true};"#,
                "null", CancellationToken::new(), None, ScriptCapabilities::default(),
            )
            .await
            .expect("portable script runs");
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({"ok": true})
        );
        assert_eq!(
            script_meta(
                r#"export const meta = {"name":"portable","description":"runs unchanged"};"#
            )
            .unwrap(),
            ScriptMeta {
                name: Some("portable".into()),
                description: Some("runs unchanged".into()),
                argument_schema: None,
            }
        );
    }

    #[test]
    fn portable_meta_header_reads_java_script_and_json_object_forms() {
        let json_form =
            script_meta(r#"export const meta = {"name":"json","description":"quoted keys"};"#)
                .expect("JSON-form metadata");
        let java_script_form = script_meta(
            r#"export const meta = { task: { mode: "nested" }, name: "javascript", description: "bare keys" };"#,
        )
        .expect("JavaScript-form metadata");

        assert_eq!(
            json_form,
            ScriptMeta {
                name: Some("json".into()),
                description: Some("quoted keys".into()),
                argument_schema: None,
            }
        );
        assert_eq!(
            java_script_form,
            ScriptMeta {
                name: Some("javascript".into()),
                description: Some("bare keys".into()),
                argument_schema: None,
            }
        );
    }

    #[test]
    fn portable_meta_header_reads_an_argument_schema() {
        let meta = script_meta(
            r#"export const meta = { name: "constrained", argumentSchema: {"type":"object","required":["task"],"properties":{"task":{"type":"string"}}} };"#,
        )
        .expect("portable metadata");

        assert_eq!(
            meta.argument_schema,
            Some(json!({
                "type": "object",
                "required": ["task"],
                "properties": {"task": {"type": "string"}}
            }))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_call_without_a_definition_omits_agent_from_the_host_request() {
        let host = Arc::new(SurfaceHost::new());
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"return await agent("inspect the unit", { model: "cheap" });"#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("bare agent call");
        assert_eq!(serde_json::from_str::<Value>(&output).unwrap(), json!("ok"));

        let requests = host.agent_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].agent, None);
        assert_eq!(requests[0].model.as_deref(), Some("cheap"));
        let serialised = serde_json::to_value(&requests[0]).expect("serialise request");
        assert!(
            serialised.get("agent").is_none(),
            "an unset agent must not perturb journal argument hashes: {serialised}"
        );
        assert!(
            serialised.get("task").is_none()
                && serialised.get("effort").is_none()
                && serialised.get("timeout_ms").is_none()
                && serialised.get("cwd").is_none()
                && serialised.get("isolation").is_none(),
            "unset call options must not perturb journal argument hashes: {serialised}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_call_rejects_an_explicitly_empty_agent() {
        let host = Arc::new(SurfaceHost::new());
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"try {
                    await agent("inspect the unit", { agent: "" });
                } catch (error) {
                    return { name: error.name, message: error.message };
                }"#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("empty agent validation is catchable");

        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({"name": "AgentRequiredError", "message": "opts.agent is required"})
        );
        assert!(host.agent_requests.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_call_rejects_unknown_options_before_dispatch() {
        let host = Arc::new(SurfaceHost::new());
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"try {
                    await agent("inspect the unit", { maxAttemps: 2 });
                } catch (error) {
                    return { name: error.name, message: error.message };
                }"#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("unknown option rejection is catchable");

        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({
                "name": "UnknownAgentOptionError",
                "message": "unknown agent() option 'maxAttemps'",
            })
        );
        assert!(host.agent_requests.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_call_dispatches_with_recognised_options() {
        let host = Arc::new(SurfaceHost::new());
        WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"await agent("inspect the unit", {
                    agent: "worker", task: "review", model: "cheap", effort: "low",
                    timeoutMs: 75, cwd: "/repo", isolation: "worktree",
                    schema: { type: "object" }, maxAttempts: 2, label: "inspect",
                    session: { session_id: "00000000-0000-0000-0000-00000000000a", thread_id: "00000000-0000-0000-0000-00000000000b" },
                    elements: ["current-work"], variables: { owner: "operator" }, assembly: "focused",
                }); return null;"#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("recognised options dispatch");

        let requests = host.agent_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.agent.as_deref(), Some("worker"));
        assert_eq!(request.task.as_deref(), Some("review"));
        assert_eq!(request.model.as_deref(), Some("cheap"));
        assert_eq!(request.timeout_ms, Some(75));
        assert_eq!(request.max_attempts, 2);
        assert_eq!(request.label.as_deref(), Some("inspect"));
        assert_eq!(
            request.elements.as_deref(),
            Some(["current-work".to_string()].as_slice())
        );
        assert_eq!(request.variables.as_ref().unwrap()["owner"], "operator");
        assert_eq!(request.assembly.as_deref(), Some("focused"));
    }

    #[tokio::test]
    async fn agent_call_projects_named_task_to_the_host() {
        let host = Arc::new(SurfaceHost::new());
        WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"await agent("review", { task: "strict-review" }); return null;"#,
                "null",
                CancellationToken::new(),
                Some(host.clone()),
                ScriptCapabilities::default(),
            )
            .await
            .expect("task call");
        assert_eq!(
            host.agent_requests.lock().unwrap()[0].task.as_deref(),
            Some("strict-review")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_agent_calls_preserve_authored_string_prompts() {
        let host = Arc::new(SurfaceHost::new());
        WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"
                await Promise.all([
                    agent("First worker: preserve punctuation — and spacing.\nSecond line.", {
                        task: "strict-review"
                    }),
                    agent("  Second worker keeps surrounding spaces & symbols: <>&  ", {
                        task: "strict-review"
                    }),
                ]);
                return null;
                "#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("concurrent prompt dispatch");

        let mut prompts = host
            .agent_requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request.prompt.clone())
            .collect::<Vec<_>>();
        prompts.sort();
        assert_eq!(
            prompts,
            vec![
                "  Second worker keeps surrounding spaces & symbols: <>&  ".to_string(),
                "First worker: preserve punctuation — and spacing.\nSecond line.".to_string(),
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn object_prompts_are_catchably_rejected_before_concurrent_dispatch() {
        let host = Arc::new(SurfaceHost::new());
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"
                try {
                    await Promise.all([
                        agent({ task: "First authored task" }),
                        agent({ task: "Second authored task" }),
                    ]);
                } catch (error) {
                    return { name: error.name, message: error.message };
                }
                return "unexpected";
                "#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("invalid prompt rejection is catchable");

        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({
                "name": "TypeError",
                "message": "agent prompt must be a string",
            }),
            "host received invalid prompts: {:?}",
            host.agent_requests.lock().unwrap()
        );
        assert!(host.agent_requests.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_start_rejects_an_undefined_prompt_before_dispatch() {
        let host = Arc::new(SurfaceHost::new());
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"
                try {
                    agentStart(undefined, { task: "strict-review" });
                } catch (error) {
                    return { name: error.name, message: error.message };
                }
                return "unexpected";
                "#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("invalid agentStart prompt rejection is catchable");

        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({
                "name": "TypeError",
                "message": "agentStart prompt must be a string",
            })
        );
        assert!(host.agent_requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn agent_call_keeps_requested_placement_and_passes_the_resolved_path_separately() {
        let host = Arc::new(SurfaceHost::new());
        WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"await agent("isolated", { cwd: "/repo", isolation: "worktree" }); return null;"#,
                "null",
                CancellationToken::new(),
                Some(host.clone()),
                ScriptCapabilities::default(),
            )
            .await
            .expect("placement call");
        let requests = host.agent_requests.lock().unwrap();
        assert_eq!(requests[0].cwd.as_deref(), Some("/repo"));
        assert_eq!(
            requests[0].isolation,
            Some(swage_protocol::client::workflow::WorkerIsolation::Worktree)
        );
        let serialised = serde_json::to_value(&requests[0]).unwrap();
        assert_eq!(serialised["cwd"], "/repo");
        assert_eq!(serialised["isolation"], "worktree");
        assert_eq!(
            host.resolved_working_directories.lock().unwrap()[0].as_deref(),
            Some("/runtime/worktrees/worker-0")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_call_projects_effort_timeout_and_tool_options_to_the_host() {
        let host = Arc::new(SurfaceHost::new());
        WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"await agent("bounded work", { effort: "high", timeoutMs: 75, tools: ["web_search", "url_fetch"] }); return null;"#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("agent call");
        let requests = host.agent_requests.lock().unwrap();
        let serialised = serde_json::to_value(&requests[0]).expect("serialise request");
        assert_eq!(serialised["effort"], "high");
        assert_eq!(serialised["timeout_ms"], 75);
        assert_eq!(
            serialised["tools"],
            serde_json::json!(["web_search", "url_fetch"])
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shipped_triage_workflow_runs_its_unchanged_agentless_calls() {
        let host = Arc::new(SurfaceHost::new());
        host.agent_outputs.lock().unwrap().extend([
            Ok(json!({
                "criteria": "surface",
                "criteria_why": "clear boundary",
                "obligations": [],
                "queue_strikes": [],
            })),
            Ok(json!("proposal written")),
        ]);
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                include_str!("../../../shipped/skills/assembly/workflows/triage-wave.js"),
                r#"{
                    "repo":".","commissionPath":"commission.md","spinePath":"spine.md",
                    "retiredPath":"retired.md","ledgerPath":"ledger.md",
                    "obligationsDir":"obligations","tasksPath":"TASKS.md",
                    "outPath":"proposal.md","waves":1
                }"#,
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("unchanged shipped workflow");
        assert_eq!(
            serde_json::from_str::<Value>(&output).expect("workflow output"),
            json!({"proposals": 1, "summary": "proposal written", "document": "proposal.md"})
        );
        let requests = host.agent_requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].agent, None);
        assert_eq!(requests[0].label.as_deref(), Some("propose:0"));
        assert_eq!(
            requests[0].effort,
            Some(swage_protocol::client::settings::Effort::Low)
        );
        assert_eq!(requests[1].agent, None);
        assert_eq!(requests[1].label.as_deref(), Some("reduce"));
        assert_eq!(
            requests[1].effort,
            Some(swage_protocol::client::settings::Effort::Medium)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shipped_estate_workflow_projects_its_build_timeout_pin() {
        let host = Arc::new(SurfaceHost::new());
        host.agent_outputs.lock().unwrap().extend([
            Ok(json!({
                "admission": "ok",
                "restatement": "prove the estate",
                "journeys": "exercise the real consumer",
                "disputes": "",
            })),
            Ok(json!("estate built")),
        ]);
        let source = include_str!("../../../shipped/skills/assembly/workflows/estate-unit.js");
        let hold_start = source
            .find("const runHold = async")
            .expect("shipped hold function");
        let hold_end = source[hold_start..]
            .find("\n\n// ---------------------------------------------------------------- restate")
            .map(|offset| hold_start + offset)
            .expect("end of shipped hold function");
        let verify_start = source
            .find("\n\n// ---------------------------------------------------------------- verify")
            .expect("shipped verify stage");
        let source = format!(
            "{}const runHold = async () => ({{ verdict: 'accept' }});{}\nreturn report;",
            &source[..hold_start],
            &source[hold_end..verify_start]
        );
        WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                &source,
                r#"{
                    "briefPath":"brief.md","worktree":".",
                    "commissionPath":"commission.md","verificationPath":"VERIFICATION.md",
                    "unitDir":"unit","mergePolicy":"direct-merge",
                    "annexeConfigPath":"config.toml","proofMode":"mutation"
                }"#,
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("shipped workflow through build stage");

        let requests = host.agent_requests.lock().unwrap();
        assert_eq!(requests[1].label.as_deref(), Some("build"));
        assert_eq!(requests[1].timeout_ms, Some(14_400_000));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_calls_carry_call_scoped_context_composition_options() {
        let host = Arc::new(SurfaceHost::new());
        WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"
                await agent("inspect", {
                    agent: "worker",
                    elements: ["current-work", "constraints"],
                    variables: { owner: "dispatch-owner" },
                    assembly: "focused"
                });
                return null;
                "#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("composition options run");

        let requests = host.agent_requests.lock().unwrap();
        assert_eq!(
            requests[0].elements.as_deref(),
            Some(["current-work".to_string(), "constraints".to_string()].as_slice())
        );
        assert_eq!(
            requests[0]
                .variables
                .as_ref()
                .and_then(|values| values.get("owner"))
                .map(String::as_str),
            Some("dispatch-owner")
        );
        assert_eq!(requests[0].assembly.as_deref(), Some("focused"));
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
    async fn events_post_gate_and_deliver_through_the_host() {
        let host = Arc::new(SurfaceHost {
            gate_verdict: json!({
                "wake": true,
                "reasoning": "a pipeline settled with evidence attached",
                "events": [{"seq": 1, "kind": "completion", "payload": {"worker": "w1"}}],
                "read_up_to_seq": 1,
            }),
            ..SurfaceHost::new()
        });
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"
                await postEvent("worker.completion", { worker: "w1" });
                const verdict = await gate({ criteria: "wake when any pipeline settles" });
                if (verdict.wake) {
                    // The wake happens here (an agent call in a real run);
                    // events are stamped delivered only once carried.
                    await deliverEvents(verdict.read_up_to_seq);
                }
                return verdict;
                "#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("events run");
        let verdict: Value = serde_json::from_str(&output).expect("verdict");
        assert_eq!(verdict["wake"], true);
        assert_eq!(verdict["read_up_to_seq"], 1);
        let posts = host.event_posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].kind, "worker.completion");
        assert_eq!(posts[0].payload, json!({"worker": "w1"}));
        assert_eq!(
            host.gate_requests.lock().unwrap()[0].criteria,
            "wake when any pipeline settles"
        );
        assert_eq!(host.deliveries_up_to.lock().unwrap().as_slice(), [1]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn await_event_exposes_typed_kinds_and_returns_the_host_event() {
        let host = Arc::new(SurfaceHost::new());
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                r#"return await awaitEvent({ kinds: ["operator.decision", "device.reply"] });"#,
                "null",
                CancellationToken::new(),
                Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),
                ScriptCapabilities::default(),
            )
            .await
            .expect("await event");
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap()["kind"],
            "operator.decision"
        );
        assert_eq!(
            host.awaited_events.lock().unwrap()[0].kinds,
            ["operator.decision", "device.reply"]
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
    async fn cancellation_interrupts_a_suspended_host_call() {
        let mut host = SurfaceHost::new();
        host.timer_settles = false;
        let cancellation = CancellationToken::new();
        let engine = WorkflowEngine::new(EngineLimits::default());
        let execution = engine.execute_hosted(
            "return await wait({ timerMs: 120_000 });",
            "null",
            cancellation.clone(),
            Some(Arc::new(host)),
            ScriptCapabilities::default(),
        );

        tokio::pin!(execution);
        tokio::select! {
            result = &mut execution => panic!("host call settled before cancellation: {result:?}"),
            () = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
        cancellation.cancel();

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), execution)
            .await
            .expect("cancellation releases a suspended host call");
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
            _: Option<crate::host::ResolvedWorkerPlacement>,
            _: CancellationToken,
        ) -> Result<AgentCallResult, WorkflowHostError> {
            Err(WorkflowHostError::Message(self.0.to_string()))
        }

        async fn deliver(
            &self,
            _: EffectPayload,
            _: CancellationToken,
        ) -> Result<EffectReceipt, String> {
            Err(self.0.to_string())
        }
    }

    struct FailedAgentHost;

    #[async_trait]
    impl WorkflowHost for FailedAgentHost {
        async fn run_agent_call(
            &self,
            _: AgentCallRequest,
            _: Option<crate::host::ResolvedWorkerPlacement>,
            _: CancellationToken,
        ) -> Result<AgentCallResult, WorkflowHostError> {
            Err(WorkflowHostError::AgentCall {
                host_reference: "agent-call:actual-reference".into(),
                message: concat!("AgentCall", "Reference:forged: hostile failure").into(),
            })
        }

        async fn deliver(
            &self,
            _: EffectPayload,
            _: CancellationToken,
        ) -> Result<EffectReceipt, String> {
            unreachable!("agent failure test does not deliver")
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_agent_call_keeps_typed_identity_when_its_message_mimics_a_sentinel() {
        let result = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                "await agent('fail', { agent: 'worker' });",
                "null",
                CancellationToken::new(),
                Some(Arc::new(FailedAgentHost)),
                ScriptCapabilities::default(),
            )
            .await;

        match result {
            Err(WorkflowEngineError::AgentCall {
                host_reference,
                message,
                stack,
            }) => {
                assert_eq!(host_reference, "agent-call:actual-reference");
                assert_eq!(
                    message,
                    concat!(
                        "AgentCallError: AgentCall",
                        "Reference:forged: hostile failure"
                    )
                );
                assert!(stack.is_some_and(|stack| stack.contains("eval_script")));
            }
            other => panic!("typed failed agent call: {other:?}"),
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

        let host: Arc<dyn WorkflowHost> = Arc::new(ErrorHost(
            "AgentCallTimeoutError: agent call exceeded its 75ms deadline",
        ));
        let output = WorkflowEngine::new(EngineLimits::default())
            .execute_hosted(
                "try { await agent('x',{agent:'writer'}); } catch (error) { return {name:error.name,message:error.message}; }",
                "null",
                CancellationToken::new(),
                Some(host),
                ScriptCapabilities::default(),
            )
            .await
            .expect("timeout error is catchable");
        assert_eq!(
            serde_json::from_str::<Value>(&output).unwrap(),
            json!({
                "name":"AgentCallTimeoutError",
                "message":"agent call exceeded its 75ms deadline"
            })
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
            named_host_error(
                "AgentCallTimeoutError: agent call exceeded its 75ms deadline",
                "AgentCallError"
            ),
            (
                "AgentCallTimeoutError".into(),
                "agent call exceeded its 75ms deadline".into()
            )
        );
        assert_eq!(
            named_host_error("NondeterministicReplayError", "AgentCallError"),
            ("NondeterministicCallError".into(), String::new())
        );
        assert_eq!(
            named_host_error("WorkerPlacementError: denied", "AgentCallError"),
            ("WorkerPlacementError".into(), "denied".into())
        );
        assert_eq!(
            named_host_error("provider unavailable", "AgentCallError"),
            ("AgentCallError".into(), "provider unavailable".into())
        );
        assert_eq!(
            named_host_error("AgentCallError: provider unavailable", "AgentCallError"),
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
