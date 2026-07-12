use std::sync::Arc;

use rquickjs::{
    AsyncContext, AsyncRuntime, CatchResultExt, Function, Promise,
    context::intrinsic::{Eval, Json, MapSet, Promise as PromiseIntrinsic, RegExp, RegExpCompiler},
    prelude::Async,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{AgentCallRequest, EffectPayload, WorkflowHost};

/// Hard limits applied by the host to each fresh JavaScript runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineLimits {
    pub memory_bytes: usize,
    pub stack_bytes: usize,
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
        self.execute_hosted(source, "null", cancellation, None, false)
            .await
    }

    pub async fn execute_hosted(
        &self,
        source: &str,
        args_json: &str,
        cancellation: CancellationToken,
        host: Option<Arc<dyn WorkflowHost>>,
        delivery_granted: bool,
    ) -> Result<String, WorkflowEngineError> {
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

                if let Some(host) = host {
                    let agent_host = Arc::clone(&host);
                    let agent_cancellation = host_cancellation.clone();
                    let agent = Function::new(
                        ctx.clone(),
                        Async(move |prompt: String, options_json: String| {
                            let host = Arc::clone(&agent_host);
                            let cancellation = agent_cancellation.clone();
                            async move {
                                let request: AgentCallRequest = serde_json::from_str(&options_json)
                                    .map_err(|error| rquickjs::Error::new_from_js_message("options", "AgentCallRequest", error.to_string()))?;
                                let mut request = request;
                                request.prompt = prompt;
                                let result = host.run_agent_call(request, cancellation).await
                                    .map_err(|error| rquickjs::Error::new_from_js_message("host", "agent result", error))?;
                                serde_json::to_string(&result.output)
                                    .map_err(|error| rquickjs::Error::new_from_js_message("agent result", "JSON", error.to_string()))
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

                    if delivery_granted {
                        let delivery_host = Arc::clone(&host);
                        let delivery_cancellation = host_cancellation.clone();
                        let deliver = Function::new(
                            ctx.clone(),
                            Async(move |payload_json: String| {
                                let host = Arc::clone(&delivery_host);
                                let cancellation = delivery_cancellation.clone();
                                async move {
                                    let payload: EffectPayload = serde_json::from_str(&payload_json)
                                        .map_err(|error| rquickjs::Error::new_from_js_message("payload", "EffectPayload", error.to_string()))?;
                                    let receipt = host.deliver(payload, cancellation).await
                                        .map_err(|error| rquickjs::Error::new_from_js_message("host", "delivery receipt", error))?;
                                    serde_json::to_string(&receipt)
                                        .map_err(|error| rquickjs::Error::new_from_js_message("delivery receipt", "JSON", error.to_string()))
                                }
                            }),
                        ).map_err(|error| error.to_string())?;
                        ctx.globals().set("__deliver", deliver).map_err(|error| error.to_string())?;
                    }

                    ctx.eval::<(), _>(r#"
                        globalThis.agent = async (prompt, options = {}) => {
                            if (!options.agent) {
                                const error = new Error("opts.agent is required");
                                error.name = "AgentRequiredError";
                                throw error;
                            }
                            const request = {
                                prompt: "",
                                agent: options.agent,
                                model: options.model ?? null,
                                schema: options.schema ?? null,
                                max_attempts: options.maxAttempts ?? 3,
                                label: options.label ?? null,
                            };
                            return JSON.parse(await __agent(String(prompt), JSON.stringify(request)));
                        };
                        if (typeof __deliver !== "undefined") {
                            globalThis.deliver = async (payload, options = {}) => JSON.parse(
                                await __deliver(JSON.stringify({
                                    content: payload.content,
                                    metadata: payload.metadata ?? null,
                                    target: options.target ?? null,
                                }))
                            );
                        }
                        globalThis.parallel = async (thunks) => Promise.all(
                            thunks.map(async (thunk) => { try { return await thunk(); } catch (_) { return null; } })
                        );
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
            true,
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
                false,
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
        WorkflowEngine::new(EngineLimits::default()).execute_hosted("return await parallel([() => agent('a',{agent:'one'}), () => agent('b',{agent:'two'})]);","null",CancellationToken::new(),Some(Arc::clone(&host) as Arc<dyn WorkflowHost>),false).await.expect("parallel run");
        assert_eq!(host.peak.load(Ordering::SeqCst), 2);
    }
}
