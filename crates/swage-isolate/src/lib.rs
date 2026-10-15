//! One V8 isolate per program, with limits.
//!
//! Every JavaScript program Swage runs — a workflow, a context assembly, a
//! plugin — gets an isolate of its own, built here. The isolate has no ambient
//! capability: the cage removes what the engine ships that could reach outside
//! the program or make its runs differ (`eval`, the clocks `Date` and
//! `Temporal`, `Math.random`, `Intl`, WebAssembly, shared memory, the engine's
//! own host object and its `console`), and the only door out is the host-call
//! pair that a [`HostCalls`] implementation answers. A program with no host
//! has no door at all.
//!
//! Two limits bound every program: a heap ceiling, and a ceiling on how long it
//! may execute without yielding to the host. Crossing either terminates the
//! isolate with the reason recorded as a [`Termination`], which the caller
//! surfaces into its own record — the engine knows how a program ended, the
//! caller knows what the program was.
//!
//! The isolate is built and used on one thread: V8 isolates do not move
//! between threads, so a [`Program`] is not `Send`. The gateway already gives
//! each workflow run a thread of its own; a context assembly runs on the
//! thread that composes the turn. The engine schedules its own housekeeping
//! through a tokio runtime, so a program built outside one carries a
//! runtime of its own.

use std::cell::RefCell;
use std::fmt;
use std::future::poll_fn;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once};
use std::task::Poll;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use deno_core::error::{CoreError, CoreErrorKind, JsError};
use deno_core::{JsRuntime, OpState, PollEventLoopOptions, RuntimeOptions, extension, op2, v8};
use futures::FutureExt;
use tokio_util::sync::CancellationToken;

/// The engine identity recorded on durable runs: the V8 version this binary
/// embeds.
pub fn engine_version() -> String {
    format!("v8-{}", v8::VERSION_STRING)
}

/// The two ceilings every program runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IsolateLimits {
    /// The most heap the program's isolate may hold before it is terminated.
    pub heap_bytes: usize,
    /// The longest the program may execute without yielding to the host. A
    /// program awaiting a host call is not executing; a loop that never
    /// awaits is, and is terminated when this elapses.
    pub continuous_execution: Duration,
}

/// Why an isolate was terminated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    HeapLimit,
    TimeLimit,
    Cancelled,
}

impl Termination {
    /// The reason as a record field.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::HeapLimit => "heap_limit",
            Self::TimeLimit => "time_limit",
            Self::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for Termination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::HeapLimit => "the program exceeded its heap limit and was terminated",
            Self::TimeLimit => {
                "the program ran past its execution time limit without yielding and was terminated"
            }
            Self::Cancelled => "the program was cancelled",
        })
    }
}

/// An exception the program threw, rendered from the thrown value's live
/// `name` and `message` with the engine's frames beneath — a renamed error
/// reads under the name the script gave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptException {
    pub name: String,
    pub message: String,
    pub frames: Vec<String>,
}

impl ScriptException {
    fn from_js(error: &JsError) -> Self {
        let frames = error
            .stack
            .as_deref()
            .map(|stack| {
                stack
                    .lines()
                    .filter(|line| line.trim_start().starts_with("at "))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        match (&error.name, &error.message) {
            (Some(name), message) => Self {
                name: name.clone(),
                message: message.clone().unwrap_or_default(),
                frames,
            },
            // A thrown non-Error value has no name or message of its own;
            // the engine's rendering of it is all there is.
            (None, _) => Self {
                name: "Error".into(),
                message: error
                    .exception_message
                    .strip_prefix("Uncaught ")
                    .unwrap_or(&error.exception_message)
                    .to_owned(),
                frames,
            },
        }
    }
}

impl fmt::Display for ScriptException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.name)?;
        if !self.message.is_empty() {
            write!(f, " {}", self.message)?;
        }
        for frame in &self.frames {
            write!(f, "\n{frame}")?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IsolateError {
    #[error("{0}")]
    Terminated(Termination),
    #[error("{0}")]
    Script(ScriptException),
    #[error("the JavaScript engine failed: {0}")]
    Engine(String),
}

/// The host side of a program's only door. `name` is the call the program's
/// shim made and `payload` its JSON argument; the reply is JSON the shim
/// unwraps. Capability lives here and in the shim that names the calls: a call
/// the host does not grant is absent from the shim, and refused here if a
/// program reaches it anyway.
#[async_trait]
pub trait HostCalls: Send + Sync {
    async fn call(&self, name: &str, payload: String) -> String;
    /// A call answered without suspending the program. Nothing here may wait
    /// on the host's progress — the isolate's thread is blocked while it runs.
    fn call_sync(&self, name: &str, payload: String) -> String;
}

type SharedHostCalls = Arc<dyn HostCalls>;

#[op2]
#[string]
async fn op_swage_host_call(
    state: Rc<RefCell<OpState>>,
    #[string] name: String,
    #[string] payload: String,
) -> String {
    let calls = state.borrow().borrow::<SharedHostCalls>().clone();
    calls.call(&name, payload).await
}

#[op2]
#[string]
fn op_swage_host_call_sync(
    state: &mut OpState,
    #[string] name: String,
    #[string] payload: String,
) -> String {
    state.borrow::<SharedHostCalls>().call_sync(&name, payload)
}

extension!(
    swage_host,
    ops = [op_swage_host_call, op_swage_host_call_sync],
    options = { calls: SharedHostCalls },
    state = |state, options| {
        state.put::<SharedHostCalls>(options.calls);
    },
);

/// Runs before any program source is compiled. It captures the host-call ops
/// into a frozen `__swageHost` door (only when a host registered them), then
/// removes every global that reaches outside the program or makes runs differ
/// — the engine's host object and the `console` it built on a print op, the
/// clocks, the locale tables, WebAssembly, shared memory and the GC observers
/// — and replaces `Math.random` with a loud failure. A program that needs
/// output or a clock gets a host call; the cage never polyfills.
const CAGE: &str = r#"
(() => {
    const core = globalThis.Deno && globalThis.Deno.core;
    const ops = core && core.ops;
    if (ops && typeof ops.op_swage_host_call === "function") {
        const call = ops.op_swage_host_call;
        const callSync = ops.op_swage_host_call_sync;
        Object.defineProperty(globalThis, "__swageHost", {
            value: Object.freeze({
                call: (name, payload) => call(name, payload),
                callSync: (name, payload) => callSync(name, payload),
            }),
            configurable: true,
            enumerable: false,
            writable: false,
        });
    }
    for (const name of [
        "Deno", "__bootstrap", "console", "eval", "Date", "Temporal", "Intl",
        "WebAssembly", "SharedArrayBuffer", "Atomics", "WeakRef",
        "FinalizationRegistry",
    ]) {
        delete globalThis[name];
    }
    Object.defineProperty(Math, "random", {
        value: () => {
            const error = new Error(
                "Math.random is not available: a Swage script must be deterministic, so its runs can be replayed"
            );
            error.name = "NondeterministicCallError";
            throw error;
        },
        configurable: false,
        enumerable: false,
        writable: false,
    });
})();
"#;

/// How often the watchdog looks at the program.
const WATCH_TICK: Duration = Duration::from_millis(10);

/// Room given to the isolate past its heap limit so termination can unwind
/// instead of aborting the process.
const HEAP_UNWIND_ROOM: usize = 16 * 1024 * 1024;

/// What the watchdog thread and the heap callback share with the program.
struct Watch {
    handle: v8::IsolateHandle,
    /// Set while the isolate is executing JavaScript on its thread.
    busy_since: Mutex<Option<Instant>>,
    termination: Mutex<Option<Termination>>,
    stopped: AtomicBool,
}

impl Watch {
    fn terminate(&self, why: Termination) {
        let mut termination = self.termination.lock().expect("termination lock");
        if termination.is_none() {
            *termination = Some(why);
            self.handle.terminate_execution();
        }
    }

    fn termination(&self) -> Option<Termination> {
        *self.termination.lock().expect("termination lock")
    }

    fn busy(&self) {
        *self.busy_since.lock().expect("busy lock") = Some(Instant::now());
    }

    fn idle(&self) {
        *self.busy_since.lock().expect("busy lock") = None;
    }

    fn watch(&self, limits: IsolateLimits, cancellation: Option<&CancellationToken>) {
        while !self.stopped.load(Ordering::Acquire) {
            if cancellation.is_some_and(CancellationToken::is_cancelled) {
                self.terminate(Termination::Cancelled);
            }
            let busy_since = *self.busy_since.lock().expect("busy lock");
            if busy_since.is_some_and(|since| since.elapsed() > limits.continuous_execution) {
                self.terminate(Termination::TimeLimit);
            }
            std::thread::sleep(WATCH_TICK);
        }
    }
}

/// One program's isolate: caged, limited, watched.
pub struct Program {
    /// Present until drop, which releases it inside the runtime context.
    runtime: Option<JsRuntime>,
    watch: Arc<Watch>,
    watchdog: Option<JoinHandle<()>>,
    /// Read by the watchdog (to end running JavaScript) and by
    /// [`Self::settle_to_string`] (to stop waiting on a host call).
    cancellation: Option<CancellationToken>,
    /// The engine posts housekeeping to the current tokio runtime, so a
    /// program built with none current gets one of its own, entered for
    /// every call into the engine.
    runtime_context: Option<tokio::runtime::Runtime>,
}

impl Program {
    /// Build a caged isolate under `limits`. With a `host`, the program's shim
    /// finds `__swageHost.call(name, payload)` and `__swageHost.callSync`;
    /// without one there is no door. A `cancellation` token terminates the
    /// program when it fires, even mid-loop.
    pub fn new(
        limits: IsolateLimits,
        host: Option<SharedHostCalls>,
        cancellation: Option<CancellationToken>,
    ) -> Result<Self, IsolateError> {
        static PLATFORM: Once = Once::new();
        PLATFORM.call_once(|| JsRuntime::init_platform(None));

        let runtime_context = match tokio::runtime::Handle::try_current() {
            Ok(_) => None,
            Err(_) => Some(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| IsolateError::Engine(error.to_string()))?,
            ),
        };
        let entered = runtime_context.as_ref().map(tokio::runtime::Runtime::enter);
        let mut runtime = JsRuntime::try_new(RuntimeOptions {
            extensions: host.into_iter().map(swage_host::init).collect(),
            create_params: Some(v8::CreateParams::default().heap_limits(0, limits.heap_bytes)),
            ..RuntimeOptions::default()
        })
        .map_err(|error| IsolateError::Engine(error.to_string()))?;

        let watch = Arc::new(Watch {
            handle: runtime.v8_isolate().thread_safe_handle(),
            busy_since: Mutex::new(None),
            termination: Mutex::new(None),
            stopped: AtomicBool::new(false),
        });
        let heap_watch = Arc::clone(&watch);
        runtime.add_near_heap_limit_callback(move |current_limit, _initial_limit| {
            heap_watch.terminate(Termination::HeapLimit);
            current_limit + HEAP_UNWIND_ROOM
        });
        let watchdog = {
            let watch = Arc::clone(&watch);
            let cancellation = cancellation.clone();
            std::thread::Builder::new()
                .name("swage-isolate-watch".into())
                .spawn(move || watch.watch(limits, cancellation.as_ref()))
                .map_err(|error| IsolateError::Engine(error.to_string()))?
        };

        drop(entered);
        let mut program = Self {
            runtime: Some(runtime),
            watch,
            watchdog: Some(watchdog),
            cancellation,
            runtime_context,
        };
        program.execute("swage:cage", CAGE.to_owned())?;
        Ok(program)
    }

    /// Define `globalThis.<name>` as the parsed JSON value.
    pub fn set_global_json(&mut self, name: &str, json: &str) -> Result<(), IsolateError> {
        let literal = serde_json::to_string(json).expect("a string serialises");
        self.execute(
            "swage:global",
            format!("globalThis[{name:?}] = JSON.parse({literal});"),
        )
        .map(drop)
    }

    /// Run `source` to completion on the isolate's thread and return what it
    /// evaluated to. A promise comes back unsettled; [`Self::settle_to_string`]
    /// drives it.
    pub fn execute(
        &mut self,
        name: &'static str,
        source: String,
    ) -> Result<v8::Global<v8::Value>, IsolateError> {
        let _entered = self.entered();
        self.watch.busy();
        let result = self.runtime().execute_script(name, source);
        self.watch.idle();
        result.map_err(|error| self.script_failure(CoreErrorKind::Js(error).into()))
    }

    /// [`Self::execute`] for a source that evaluates to a string; any other
    /// value is an engine error, never coerced (coercion would run the
    /// program's own code outside the watch).
    pub fn execute_for_string(
        &mut self,
        name: &'static str,
        source: String,
    ) -> Result<String, IsolateError> {
        let value = self.execute(name, source)?;
        self.string_of(&value)
    }

    /// Drive the event loop — host calls included — until `value` settles,
    /// and return the settled value, which must be a string. Cancellation
    /// ends the wait even while a host call is still pending.
    pub async fn settle_to_string(
        &mut self,
        value: v8::Global<v8::Value>,
    ) -> Result<String, IsolateError> {
        let _entered = self.entered();
        let mut settled = self.runtime().resolve(value);
        let mut cancelled = self
            .cancellation
            .clone()
            .map(|cancellation| Box::pin(async move { cancellation.cancelled().await }));
        let options = PollEventLoopOptions::default();
        let outcome = poll_fn(|cx| {
            if let Some(cancelled) = cancelled.as_mut()
                && cancelled.poll_unpin(cx).is_ready()
            {
                self.watch.terminate(Termination::Cancelled);
                return Poll::Ready(Err(CoreErrorKind::ExecutionTerminated.into()));
            }
            self.watch.busy();
            let outcome = match settled.poll_unpin(cx) {
                Poll::Ready(settled) => {
                    Poll::Ready(settled.map_err(|error| CoreError::from(CoreErrorKind::Js(error))))
                }
                Poll::Pending => match self.runtime().poll_event_loop(cx, options) {
                    Poll::Ready(Ok(())) => match settled.poll_unpin(cx) {
                        Poll::Ready(settled) => Poll::Ready(
                            settled.map_err(|error| CoreError::from(CoreErrorKind::Js(error))),
                        ),
                        Poll::Pending => {
                            Poll::Ready(Err(CoreErrorKind::PendingPromiseResolution.into()))
                        }
                    },
                    Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                    Poll::Pending => Poll::Pending,
                },
            };
            self.watch.idle();
            outcome
        })
        .await;
        match outcome {
            Ok(value) => self.string_of(&value),
            Err(error) => Err(self.script_failure(error)),
        }
    }

    /// Why the program was terminated, if it was.
    pub fn termination(&self) -> Option<Termination> {
        self.watch.termination()
    }

    /// Read a string value without running any of the program's code: a
    /// non-string is refused rather than coerced through its `toString`.
    fn string_of(&mut self, value: &v8::Global<v8::Value>) -> Result<String, IsolateError> {
        let _entered = self.entered();
        let runtime = self.runtime.as_mut().expect("the isolate lives until drop");
        deno_core::scope!(scope, runtime);
        let value = v8::Local::new(scope, value);
        let string = v8::Local::<v8::String>::try_from(value).map_err(|_| {
            IsolateError::Engine(format!(
                "the program produced {} where a string was required",
                value.type_repr()
            ))
        })?;
        Ok(string.to_rust_string_lossy(scope))
    }

    fn runtime(&mut self) -> &mut JsRuntime {
        self.runtime.as_mut().expect("the isolate lives until drop")
    }

    fn entered(&self) -> Option<tokio::runtime::EnterGuard<'_>> {
        self.runtime_context
            .as_ref()
            .map(tokio::runtime::Runtime::enter)
    }

    /// A termination outranks whatever the engine reported about it: the
    /// exception V8 raises to unwind a terminated program is not the
    /// program's own.
    fn script_failure(&self, error: CoreError) -> IsolateError {
        if let Some(termination) = self.watch.termination() {
            return IsolateError::Terminated(termination);
        }
        match error.into_kind() {
            CoreErrorKind::Js(error) => IsolateError::Script(ScriptException::from_js(&error)),
            other => IsolateError::Engine(other.to_string()),
        }
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        self.watch.stopped.store(true, Ordering::Release);
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
        let _entered = self.entered();
        drop(self.runtime.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> IsolateLimits {
        IsolateLimits {
            heap_bytes: 16 * 1024 * 1024,
            continuous_execution: Duration::from_secs(5),
        }
    }

    fn program() -> Program {
        Program::new(limits(), None, None).expect("isolate")
    }

    struct Echo;

    #[async_trait]
    impl HostCalls for Echo {
        async fn call(&self, name: &str, payload: String) -> String {
            tokio::task::yield_now().await;
            serde_json::json!({ "name": name, "payload": payload }).to_string()
        }

        fn call_sync(&self, name: &str, payload: String) -> String {
            serde_json::json!({ "sync": name, "payload": payload }).to_string()
        }
    }

    #[test]
    fn engine_version_names_v8() {
        assert!(engine_version().starts_with("v8-"), "{}", engine_version());
    }

    #[test]
    fn the_cage_removes_every_door_and_clock() {
        let mut program = program();
        let absent = program
            .execute_for_string(
                "test",
                r#"["Deno", "__bootstrap", "console", "eval", "Date", "Temporal", "Intl",
                    "WebAssembly", "SharedArrayBuffer", "Atomics", "WeakRef",
                    "FinalizationRegistry", "__swageHost"]
                    .filter((name) => typeof globalThis[name] !== "undefined").join(",")"#
                    .to_owned(),
            )
            .expect("runs");
        assert_eq!(absent, "", "still reachable: {absent}");
        let random = program
            .execute_for_string(
                "test",
                "try { Math.random(); 'returned' } catch (error) { error.name + ': ' + error.message }"
                    .to_owned(),
            )
            .expect("runs");
        assert!(
            random.starts_with("NondeterministicCallError: Math.random is not available"),
            "{random}"
        );
    }

    #[test]
    fn a_program_keeps_the_language_it_needs() {
        let mut program = program();
        let value = program
            .execute_for_string(
                "test",
                r#"JSON.stringify({
                    map: [...new Map([["a", 1]]).keys()],
                    re: /x(\d)/.exec("x7")[1],
                    big: (2n ** 64n).toString(),
                    text: "é".normalize("NFD").length,
                })"#
                .to_owned(),
            )
            .expect("runs");
        assert_eq!(
            value,
            r#"{"map":["a"],"re":"7","big":"18446744073709551616","text":2}"#
        );
    }

    #[test]
    fn a_global_set_from_json_is_the_parsed_value() {
        let mut program = program();
        program
            .set_global_json("args", r#"{"topic":"ab\"c","n":[1,2]}"#)
            .expect("set");
        let value = program
            .execute_for_string("test", "args.topic + args.n.length".to_owned())
            .expect("runs");
        assert_eq!(value, "ab\"c2");
    }

    #[test]
    fn an_exception_renders_its_live_name_message_and_frames() {
        let mut program = program();
        let error = program
            .execute(
                "test",
                "function inner() { const e = new Error('boom'); e.name = 'CustomError'; throw e; }\ninner();"
                    .to_owned(),
            )
            .expect_err("throws");
        let IsolateError::Script(exception) = &error else {
            panic!("expected a script exception, got {error:?}");
        };
        assert_eq!(exception.name, "CustomError");
        assert_eq!(exception.message, "boom");
        assert!(
            exception.frames.iter().any(|frame| frame.contains("inner")),
            "{exception:?}"
        );
        assert!(
            error.to_string().starts_with("CustomError: boom\n"),
            "{error}"
        );

        let thrown_value = program
            .execute("test", "throw 'plain text';".to_owned())
            .expect_err("throws");
        assert_eq!(thrown_value.to_string(), "Error: plain text");
    }

    #[test]
    fn a_heap_limit_terminates_the_program_and_names_the_reason() {
        let mut program = Program::new(
            IsolateLimits {
                heap_bytes: 8 * 1024 * 1024,
                continuous_execution: Duration::from_secs(30),
            },
            None,
            None,
        )
        .expect("isolate");
        let error = program
            .execute(
                "test",
                "const held = []; for (;;) { held.push(new Array(100000).fill('x')); }".to_owned(),
            )
            .expect_err("terminates");
        assert!(
            matches!(error, IsolateError::Terminated(Termination::HeapLimit)),
            "{error:?}"
        );
        assert_eq!(program.termination(), Some(Termination::HeapLimit));
    }

    #[test]
    fn a_time_limit_terminates_a_loop_that_never_yields() {
        let mut program = Program::new(
            IsolateLimits {
                heap_bytes: 16 * 1024 * 1024,
                continuous_execution: Duration::from_millis(100),
            },
            None,
            None,
        )
        .expect("isolate");
        let started = Instant::now();
        let error = program
            .execute("test", "for (;;) {}".to_owned())
            .expect_err("terminates");
        assert!(
            matches!(error, IsolateError::Terminated(Termination::TimeLimit)),
            "{error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn cancellation_terminates_a_loop_that_never_yields() {
        let cancellation = CancellationToken::new();
        let mut program =
            Program::new(limits(), None, Some(cancellation.clone())).expect("isolate");
        let canceller = {
            let cancellation = cancellation.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                cancellation.cancel();
            })
        };
        let error = program
            .execute("test", "for (;;) {}".to_owned())
            .expect_err("terminates");
        canceller.join().expect("canceller");
        assert!(
            matches!(error, IsolateError::Terminated(Termination::Cancelled)),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn host_calls_are_the_only_door_and_round_trip() {
        let mut program = Program::new(limits(), Some(Arc::new(Echo)), None).expect("isolate");
        let pending = program
            .execute(
                "test",
                r#"(async () => {
                    const door = __swageHost;
                    const asyncReply = JSON.parse(await door.call("agent", JSON.stringify(["hi", 2])));
                    const syncReply = JSON.parse(door.callSync("log", "\"note\""));
                    return JSON.stringify({ asyncReply, syncReply, deno: typeof Deno });
                })()"#
                    .to_owned(),
            )
            .expect("starts");
        let settled = program.settle_to_string(pending).await.expect("settles");
        assert_eq!(
            settled,
            r#"{"asyncReply":{"name":"agent","payload":"[\"hi\",2]"},"syncReply":{"sync":"log","payload":"\"note\""},"deno":"undefined"}"#
        );
    }

    #[test]
    fn a_result_that_is_not_a_string_is_refused_not_coerced() {
        let mut program = program();
        let error = program
            .execute_for_string("test", "({ toString() { for (;;) {} } })".to_owned())
            .expect_err("refused");
        assert!(
            matches!(&error, IsolateError::Engine(message) if message.contains("where a string was required")),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn cancellation_releases_a_program_waiting_on_a_host_call_that_never_answers() {
        struct Silent;
        #[async_trait]
        impl HostCalls for Silent {
            async fn call(&self, _: &str, _: String) -> String {
                std::future::pending().await
            }
            fn call_sync(&self, _: &str, _: String) -> String {
                "null".into()
            }
        }
        let cancellation = CancellationToken::new();
        let mut program =
            Program::new(limits(), Some(Arc::new(Silent)), Some(cancellation.clone()))
                .expect("isolate");
        let pending = program
            .execute("test", r#"__swageHost.call("wait", "null")"#.to_owned())
            .expect("starts");
        let canceller = {
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                cancellation.cancel();
            })
        };
        let error = program
            .settle_to_string(pending)
            .await
            .expect_err("cancelled");
        canceller.await.expect("canceller");
        assert!(
            matches!(error, IsolateError::Terminated(Termination::Cancelled)),
            "{error:?}"
        );
    }

    #[tokio::test]
    async fn a_rejected_program_promise_renders_as_its_exception() {
        let mut program = Program::new(limits(), Some(Arc::new(Echo)), None).expect("isolate");
        let pending = program
            .execute(
                "test",
                r#"(async () => { await __swageHost.call("x", "null"); throw new TypeError("after the door"); })()"#
                    .to_owned(),
            )
            .expect("starts");
        let error = program
            .settle_to_string(pending)
            .await
            .expect_err("rejects");
        assert!(
            error.to_string().starts_with("TypeError: after the door"),
            "{error}"
        );
    }
}
