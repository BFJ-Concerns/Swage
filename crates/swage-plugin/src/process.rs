//! A plugin that runs as a process the gateway brokers.
//!
//! A manifest with a `[process]` declares a command instead of a script.
//! The gateway starts one such process per session on the session's first
//! call to one of the plugin's tools, connects it over a Unix socket, and
//! speaks the plugin-process contract (`swage_protocol::plugin`) with it:
//! one JSON object per line, an `invoke` answered by a `result` or a
//! `failure`, and `call`s from the process answered under the plugin's
//! grants exactly as the isolate's door answers them. The process is told
//! why it is being stopped — the session closed, it sat idle, the gateway
//! is stopping, a newer revision took over — and has a grace period to
//! exit before it is killed.
//!
//! The lifecycle is a [`ProcessKeeper`], which any gateway-held process can
//! ride: start on first use for a scope, stop on that scope's close, after
//! an idle period, and at shutdown. [`ProcessPlugin`] is the keeper for a
//! plugin's sessions; the embedding process supplies how the command is
//! placed (a [`ProcessLauncher`]: under a sandbox, or plain) and what it
//! records of each start and stop (a [`ProcessObserver`]).

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;
use swage_protocol::plugin::{
    PLUGIN_SOCKET_ENV, PluginHostReply, PluginProcessMessage, PluginProcessRequest,
    PluginProcessStopReason,
};
use swage_sandbox::{ChildProcessManager, SocketManager};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio::task::JoinHandle;

use crate::manifest::{HostGrant, PluginManifest};
use crate::runtime::{InvocationContext, PluginError, PluginHost, answer_host_call};

/// How long a process has to connect and announce its tools.
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a stopped process has to exit after `stop` before it is
/// signalled.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// How long a signalled process has before it is killed.
const KILL_GRACE: Duration = Duration::from_secs(1);
/// The longest message either side may send, before its newline.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
/// How long a write to the process may take before the process is taken
/// to have stopped reading.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// What a process plugin needs to start: the installed command, its
/// arguments, the idle period after which a session's process stops, the
/// grants its host calls are answered under, and the tools it must serve.
#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub plugin: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub idle: Duration,
    pub grants: Vec<HostGrant>,
    pub tools: Vec<String>,
}

impl ProcessSpec {
    /// The spec for a process plugin installed at `install_root`; `None`
    /// for an isolate plugin.
    pub fn new(manifest: &PluginManifest, install_root: &Path) -> Option<Self> {
        let process = manifest.process.as_ref()?;
        Some(Self {
            plugin: manifest.name.clone(),
            command: install_root.join(&process.command),
            args: process.args.clone(),
            idle: process.idle(),
            grants: manifest.grants.clone(),
            tools: manifest
                .provides
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect(),
        })
    }
}

/// How the embedding process places a plugin's process: the command to
/// spawn for `spec`, with the socket at `socket` reachable from inside it
/// and named in [`PLUGIN_SOCKET_ENV`]. The gateway's launcher wraps the
/// command in the session's sandbox; [`PlainLauncher`] runs it as it is.
pub trait ProcessLauncher: Send + Sync {
    fn command(
        &self,
        spec: &ProcessSpec,
        socket: &Path,
    ) -> Result<tokio::process::Command, PluginError>;
}

/// Runs the command as it is, from the install directory, with no boundary
/// around it: for tests, and for an embedder that has none to offer.
pub struct PlainLauncher;

impl ProcessLauncher for PlainLauncher {
    fn command(
        &self,
        spec: &ProcessSpec,
        socket: &Path,
    ) -> Result<tokio::process::Command, PluginError> {
        let mut command = tokio::process::Command::new(&spec.command);
        command.args(&spec.args);
        if let Some(root) = spec.command.parent() {
            command.current_dir(root);
        }
        // The process gets the socket and a path to find its interpreter,
        // never the gateway's environment — a provider key lives there.
        command.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        command.env(PLUGIN_SOCKET_ENV, socket);
        command.stdin(std::process::Stdio::null());
        command.stdout(std::process::Stdio::null());
        Ok(command)
    }
}

/// What the embedding process records of a plugin's processes.
pub trait ProcessObserver: Send + Sync {
    fn started(&self, plugin: &str, session: &str, pid: u32);
    fn stopped(&self, plugin: &str, session: &str, reason: PluginProcessStopReason);
    /// The process ended on its own, with `detail` saying how.
    fn exited(&self, plugin: &str, session: &str, detail: &str);
}

/// Records nothing.
pub struct SilentObserver;

impl ProcessObserver for SilentObserver {
    fn started(&self, _: &str, _: &str, _: u32) {}
    fn stopped(&self, _: &str, _: &str, _: PluginProcessStopReason) {}
    fn exited(&self, _: &str, _: &str, _: &str) {}
}

/// A process the gateway holds for a scope until it is stopped.
#[async_trait]
pub trait Held: Send + Sync + 'static {
    /// Tell the process why it is stopping and end it; idempotent.
    async fn stop(&self, reason: PluginProcessStopReason);
    /// Whether the process is serving a call now: a busy process is never
    /// idle, however long the call has run.
    fn busy(&self) -> bool {
        false
    }
}

struct Kept<P> {
    process: Arc<P>,
    last_used: Instant,
}

/// One scope's slot: locked for the length of a start, so a second use of
/// the scope waits for the first start rather than starting again.
type Slot<P> = Arc<AsyncMutex<Option<Kept<P>>>>;

/// One owner's processes, one per scope (a session, or the install): a
/// process starts on the scope's first use, is touched by every use, and
/// stops when the scope closes, when it has sat idle for the keeper's idle
/// period, or at shutdown. Starting is serialised per scope and does not
/// hold the other scopes up.
pub struct ProcessKeeper<P: Held> {
    entries: Mutex<HashMap<String, Slot<P>>>,
    /// The scopes with a process serving, kept beside the entries so a
    /// listing never waits on a start in progress.
    serving: Mutex<BTreeSet<String>>,
    /// Set by `stop_all`: nothing starts again through a closed keeper.
    closed: std::sync::atomic::AtomicBool,
    idle: Duration,
}

impl<P: Held> ProcessKeeper<P> {
    pub fn new(idle: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            serving: Mutex::new(BTreeSet::new()),
            closed: std::sync::atomic::AtomicBool::new(false),
            idle,
        }
    }

    /// Whether `stop_all` has closed this keeper.
    pub fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Drop the entry for `scope` when nothing serves it and nothing else
    /// holds its slot, so the map holds only scopes in use.
    fn prune(&self, scope: &str) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = entries.get(scope)
            && Arc::strong_count(entry) == 1
            && entry.try_lock().is_ok_and(|slot| slot.is_none())
        {
            entries.remove(scope);
        }
    }

    /// Mark `scope`'s process used now: a call that just ended restarts
    /// its idle period.
    pub async fn touch(&self, scope: &str) {
        let entry = self.entry(scope);
        if let Some(kept) = entry.lock().await.as_mut() {
            kept.last_used = Instant::now();
        }
    }

    fn mark(&self, scope: &str, serving: bool) {
        let mut set = self
            .serving
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if serving {
            set.insert(scope.to_owned());
        } else {
            set.remove(scope);
        }
    }

    pub fn idle(&self) -> Duration {
        self.idle
    }

    fn entry(&self, scope: &str) -> Slot<P> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(
            entries
                .entry(scope.to_owned())
                .or_insert_with(|| Arc::new(AsyncMutex::new(None))),
        )
    }

    /// The process serving `scope`, started by `start` if none is; marks
    /// it used. A closed keeper (after `stop_all`) answers with `closed`
    /// instead of starting anything.
    pub async fn get_or_start<F, Fut, E>(
        &self,
        scope: &str,
        start: F,
        closed: impl FnOnce() -> E,
    ) -> Result<Arc<P>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Arc<P>, E>>,
    {
        if self.is_closed() {
            return Err(closed());
        }
        let entry = self.entry(scope);
        let mut slot = entry.lock().await;
        if let Some(kept) = slot.as_mut() {
            kept.last_used = Instant::now();
            return Ok(Arc::clone(&kept.process));
        }
        let process = start().await?;
        if self.is_closed() {
            // Closed while this start ran: the process is not kept.
            drop(slot);
            process.stop(PluginProcessStopReason::GatewayStopping).await;
            return Err(closed());
        }
        *slot = Some(Kept {
            process: Arc::clone(&process),
            last_used: Instant::now(),
        });
        self.mark(scope, true);
        Ok(process)
    }

    /// The process serving `scope` now, if any, without starting one.
    pub async fn current(&self, scope: &str) -> Option<Arc<P>> {
        let entry = self.entry(scope);
        let slot = entry.lock().await;
        slot.as_ref().map(|kept| Arc::clone(&kept.process))
    }

    /// Forget `process` as `scope`'s if it still is — for a process that
    /// ended on its own, so the next use starts afresh.
    pub async fn forget(&self, scope: &str, process: &Arc<P>) {
        let entry = self.entry(scope);
        let mut slot = entry.lock().await;
        if slot
            .as_ref()
            .is_some_and(|kept| Arc::ptr_eq(&kept.process, process))
        {
            *slot = None;
            self.mark(scope, false);
        }
        drop(slot);
        drop(entry);
        self.prune(scope);
    }

    /// Stop `scope`'s process for `reason`; `false` if none was serving.
    pub async fn stop(&self, scope: &str, reason: PluginProcessStopReason) -> bool {
        let entry = self.entry(scope);
        // The listing changes under the slot lock, as a start changes it,
        // so a replacement started after this stop is never unlisted by it.
        let kept = {
            let mut slot = entry.lock().await;
            let kept = slot.take();
            if kept.is_some() {
                self.mark(scope, false);
            }
            kept
        };
        drop(entry);
        self.prune(scope);
        match kept {
            Some(kept) => {
                kept.process.stop(reason).await;
                true
            }
            None => false,
        }
    }

    /// Stop every process that has sat idle for the keeper's period;
    /// returns the scopes stopped.
    pub async fn sweep_idle(&self, now: Instant) -> Vec<String> {
        let mut stopped = Vec::new();
        for (scope, entry) in self.entries() {
            let idle = {
                let mut slot = entry.lock().await;
                match slot.as_ref() {
                    Some(kept)
                        if now.duration_since(kept.last_used) >= self.idle
                            && !kept.process.busy() =>
                    {
                        self.mark(&scope, false);
                        slot.take()
                    }
                    _ => None,
                }
            };
            drop(entry);
            if let Some(kept) = idle {
                kept.process.stop(PluginProcessStopReason::Idle).await;
                stopped.push(scope.clone());
            }
            self.prune(&scope);
        }
        stopped
    }

    /// Stop every process for `reason` and close the keeper: nothing starts
    /// through it again. Returns the scopes stopped.
    pub async fn stop_all(&self, reason: PluginProcessStopReason) -> Vec<String> {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut stopped = Vec::new();
        for (scope, entry) in self.entries() {
            let kept = {
                let mut slot = entry.lock().await;
                let kept = slot.take();
                if kept.is_some() {
                    self.mark(&scope, false);
                }
                kept
            };
            drop(entry);
            if let Some(kept) = kept {
                kept.process.stop(reason).await;
                stopped.push(scope.clone());
            }
            self.prune(&scope);
        }
        stopped
    }

    /// The scopes with a process serving now, in order.
    pub fn serving(&self) -> Vec<String> {
        self.serving
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    fn entries(&self) -> Vec<(String, Slot<P>)> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|(scope, entry)| (scope.clone(), Arc::clone(entry)))
            .collect()
    }
}

/// A process plugin: its spec and the process serving each session.
pub struct ProcessPlugin {
    spec: ProcessSpec,
    keeper: ProcessKeeper<ServingProcess>,
    observer: Arc<dyn ProcessObserver>,
}

impl ProcessPlugin {
    pub fn new(spec: ProcessSpec, observer: Arc<dyn ProcessObserver>) -> Arc<Self> {
        let idle = spec.idle;
        Arc::new(Self {
            spec,
            keeper: ProcessKeeper::new(idle),
            observer,
        })
    }

    pub fn spec(&self) -> &ProcessSpec {
        &self.spec
    }

    /// Run `tool` for the call's session, starting the session's process
    /// through `launcher` if none is serving. Host calls the process makes
    /// while serving this call are answered by `host`.
    pub async fn invoke_tool(
        self: &Arc<Self>,
        tool: &str,
        params: &Value,
        context: &InvocationContext,
        host: Arc<dyn PluginHost>,
        launcher: &dyn ProcessLauncher,
    ) -> Result<Value, PluginError> {
        let session = context.session_id.clone();
        let process = self
            .keeper
            .get_or_start(
                &session,
                || async {
                    let plugin = Arc::clone(self);
                    let session = session.clone();
                    let process = ServingProcess::start(&self.spec, launcher, move |detail| {
                        plugin
                            .observer
                            .exited(&plugin.spec.plugin, &session, detail);
                    })
                    .await?;
                    self.observer
                        .started(&self.spec.plugin, &context.session_id, process.pid());
                    Ok::<_, PluginError>(process)
                },
                || PluginError::Stopped,
            )
            .await?;
        let outcome = process.invoke(tool, params, context, host).await;
        if let Err(PluginError::ProcessExited(_)) = &outcome {
            self.keeper.forget(&context.session_id, &process).await;
        } else {
            self.keeper.touch(&context.session_id).await;
        }
        outcome
    }

    /// The session has closed: stop its process, if one is serving.
    pub async fn stop_session(&self, session: &str) -> bool {
        let stopped = self
            .keeper
            .stop(session, PluginProcessStopReason::SessionClosed)
            .await;
        if stopped {
            self.observer.stopped(
                &self.spec.plugin,
                session,
                PluginProcessStopReason::SessionClosed,
            );
        }
        stopped
    }

    /// Stop every session's process that has sat idle for the manifest's
    /// period; returns the sessions stopped.
    pub async fn sweep_idle(&self, now: Instant) -> Vec<String> {
        let stopped = self.keeper.sweep_idle(now).await;
        for session in &stopped {
            self.observer
                .stopped(&self.spec.plugin, session, PluginProcessStopReason::Idle);
        }
        stopped
    }

    /// Stop every process for `reason` — the gateway stopping, or a newer
    /// revision taking over, after which nothing starts again through this
    /// plugin; returns the sessions stopped.
    pub async fn stop_all(&self, reason: PluginProcessStopReason) -> Vec<String> {
        let stopped = self.keeper.stop_all(reason).await;
        for session in &stopped {
            self.observer.stopped(&self.spec.plugin, session, reason);
        }
        stopped
    }

    /// The sessions with a process serving now.
    pub fn serving_sessions(&self) -> Vec<String> {
        self.keeper.serving()
    }
}

type Pending = HashMap<String, oneshot::Sender<Result<Value, PluginError>>>;

/// One running plugin process and the socket to it.
pub struct ServingProcess {
    pid: u32,
    child: AsyncMutex<ChildProcessManager>,
    writer: AsyncMutex<OwnedWriteHalf>,
    /// Invocations awaiting their result, by id.
    pending: Mutex<Pending>,
    /// The host answering each in-flight invocation's calls, by id: a call
    /// is answered under the authority of the invocation it names.
    hosts: Mutex<HashMap<String, Arc<dyn PluginHost>>>,
    grants: Vec<HostGrant>,
    reader: Mutex<Option<JoinHandle<()>>>,
    /// The host's work for each in-flight call, aborted when the process
    /// stops: an effect never outlives the process that raised it.
    calls: Mutex<tokio::task::JoinSet<()>>,
    /// Why the process is no longer served, once the reader has ended.
    ending: Mutex<Option<String>>,
    /// Keeps the socket file until the process is dropped.
    _socket: SocketManager,
}

/// An invocation's registration in the driver, removed when the invocation
/// ends however it ends — settled, failed, or its caller gone — so no
/// later `call` can borrow its authority.
struct Registered<'a> {
    process: &'a ServingProcess,
    id: String,
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.process.unregister(&self.id);
    }
}

impl ServingProcess {
    /// Spawn the process through `launcher`, accept its connection and read
    /// its `ready`; `on_exit` is told when the process ends on its own.
    async fn start(
        spec: &ProcessSpec,
        launcher: &dyn ProcessLauncher,
        on_exit: impl Fn(&str) + Send + Sync + 'static,
    ) -> Result<Arc<Self>, PluginError> {
        let (socket, listener) = SocketManager::create("swage-plugin")
            .map_err(|error| PluginError::Process(error.to_string()))?;
        let command = launcher.command(spec, socket.path())?;
        let mut child = ChildProcessManager::spawn(command)
            .map_err(|error| PluginError::Process(error.to_string()))?;
        let pid = child.pid();
        let accepted = tokio::select! {
            accepted = tokio::time::timeout(START_TIMEOUT, listener.accept()) => accepted,
            exit = child.wait_for_exit() => {
                return Err(PluginError::Process(format!(
                    "the process exited before connecting: {}",
                    exit.map(|status| status.to_string()).unwrap_or_else(|error| error.to_string())
                )));
            }
        };
        let (stream, _) = match accepted {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => {
                let _ = child.graceful_shutdown(STOP_GRACE).await;
                return Err(PluginError::Process(format!(
                    "accepting the process's connection failed: {error}"
                )));
            }
            Err(_) => {
                let _ = child.graceful_shutdown(STOP_GRACE).await;
                return Err(PluginError::Process(format!(
                    "the process did not connect within {}s",
                    START_TIMEOUT.as_secs()
                )));
            }
        };
        let (read_half, write_half) = stream.into_split();
        let mut lines = BufReader::new(read_half);
        let ready = match tokio::time::timeout(START_TIMEOUT, read_message(&mut lines)).await {
            Ok(Ok(Some(PluginProcessMessage::Ready { tools }))) => tools,
            Ok(Ok(Some(other))) => {
                let _ = child.graceful_shutdown(STOP_GRACE).await;
                return Err(PluginError::Process(format!(
                    "the process's first message must be `ready`, not {other:?}"
                )));
            }
            Ok(Ok(None)) => {
                let _ = child.graceful_shutdown(STOP_GRACE).await;
                return Err(PluginError::Process(
                    "the process closed the socket before `ready`".into(),
                ));
            }
            Ok(Err(error)) => {
                let _ = child.graceful_shutdown(STOP_GRACE).await;
                return Err(error);
            }
            Err(_) => {
                let _ = child.graceful_shutdown(STOP_GRACE).await;
                return Err(PluginError::Process(format!(
                    "the process did not announce its tools within {}s",
                    START_TIMEOUT.as_secs()
                )));
            }
        };
        if let Some(missing) = spec.tools.iter().find(|tool| !ready.contains(tool)) {
            let _ = child.graceful_shutdown(STOP_GRACE).await;
            return Err(PluginError::Script(format!(
                "the manifest declares tool '{missing}' but the process does not serve it (it announced {ready:?})"
            )));
        }
        let process = Arc::new(Self {
            pid,
            child: AsyncMutex::new(child),
            writer: AsyncMutex::new(write_half),
            pending: Mutex::new(HashMap::new()),
            hosts: Mutex::new(HashMap::new()),
            grants: spec.grants.clone(),
            reader: Mutex::new(None),
            calls: Mutex::new(tokio::task::JoinSet::new()),
            ending: Mutex::new(None),
            _socket: socket,
        });
        let reader = {
            let process = Arc::clone(&process);
            tokio::spawn(async move {
                let ending = loop {
                    match read_message(&mut lines).await {
                        Ok(Some(message)) => process.receive(message),
                        Ok(None) => break "the process closed the socket".to_owned(),
                        Err(error) => break error.to_string(),
                    }
                };
                // An orderly stop ended the process on purpose and said so
                // already; only a process that ended on its own is reported.
                let already_ended = process
                    .ending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_some();
                if !already_ended {
                    process.end(ending.clone());
                    on_exit(&ending);
                }
            })
        };
        *process
            .reader
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reader);
        Ok(process)
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    async fn invoke(
        self: &Arc<Self>,
        tool: &str,
        params: &Value,
        context: &InvocationContext,
        host: Arc<dyn PluginHost>,
    ) -> Result<Value, PluginError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (reply, response) = oneshot::channel();
        self.hosts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.clone(), host);
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.clone(), reply);
        let registered = Registered {
            process: self,
            id: id.clone(),
        };
        // A reader that ended before this registration drained nothing of
        // ours; the process is not serving, whatever the socket still takes.
        if let Some(ending) = self
            .ending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            return Err(PluginError::ProcessExited(ending));
        }
        self.send(&PluginProcessRequest::Invoke {
            id: id.clone(),
            tool: tool.to_owned(),
            params: params.clone(),
            context: context.clone(),
        })
        .await?;
        let outcome = response
            .await
            .unwrap_or_else(|_| Err(PluginError::ProcessExited("the process ended".into())));
        drop(registered);
        outcome
    }

    /// Route one message from the process: a result or failure to the
    /// invocation awaiting it, a host call to a task that answers it under
    /// the named invocation's host.
    fn receive(self: &Arc<Self>, message: PluginProcessMessage) {
        match message {
            PluginProcessMessage::Ready { .. } => {
                tracing::warn!(pid = self.pid, "plugin process sent `ready` twice; ignored");
            }
            PluginProcessMessage::Result { id, value } => self.settle(&id, Ok(value)),
            PluginProcessMessage::Failure { id, name, message } => {
                self.settle(&id, Err(PluginError::Script(format!("{name}: {message}"))));
            }
            PluginProcessMessage::Call {
                id,
                invocation,
                name,
                payload,
            } => {
                let host = self
                    .hosts
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(&invocation)
                    .cloned();
                let process = Arc::clone(self);
                let mut calls = self
                    .calls
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                // Finished tasks leave the set as new ones join it.
                while calls.try_join_next().is_some() {}
                calls.spawn(async move {
                    let reply = match host {
                        Some(host) => {
                            let envelope = answer_host_call(
                                &process.grants,
                                host.as_ref(),
                                &name,
                                &payload.to_string(),
                            )
                            .await;
                            serde_json::from_str::<PluginHostReply>(&envelope).unwrap_or_else(
                                |error| PluginHostReply {
                                    ok: false,
                                    value: None,
                                    error: Some(swage_protocol::plugin::PluginHostCallError {
                                        name: "HostUnavailableError".into(),
                                        message: format!("the host's reply did not read: {error}"),
                                    }),
                                },
                            )
                        }
                        None => PluginHostReply {
                            ok: false,
                            value: None,
                            error: Some(swage_protocol::plugin::PluginHostCallError {
                                name: "HostUnavailableError".into(),
                                message: format!("no invocation '{invocation}' is being served"),
                            }),
                        },
                    };
                    // A reply that cannot reach the process — too large, or
                    // the socket gone — ends the invocation that asked, so
                    // nothing waits on an answer that never comes.
                    if let Err(error) = process
                        .send(&PluginProcessRequest::Reply { id, reply })
                        .await
                    {
                        process.settle(
                            &invocation,
                            Err(PluginError::Process(format!(
                                "the host's reply to '{name}' could not be sent: {error}"
                            ))),
                        );
                    }
                });
            }
        }
    }

    /// End the invocation `id` with `outcome`; its authority goes with it,
    /// so a `call` naming it after its result is refused.
    fn settle(&self, id: &str, outcome: Result<Value, PluginError>) {
        let waiting = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
        self.hosts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
        match waiting {
            Some(reply) => {
                let _ = reply.send(outcome);
            }
            None => tracing::warn!(
                pid = self.pid,
                id,
                "plugin process answered an invocation nobody awaits"
            ),
        }
    }

    fn unregister(&self, id: &str) {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
        self.hosts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(id);
    }

    /// The process is no longer served: every pending invocation fails,
    /// every call's host task is abandoned, and a later registration is
    /// refused with the reason.
    fn end(&self, ending: String) {
        *self
            .ending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ending.clone());
        let pending = std::mem::take(
            &mut *self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        self.hosts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        for (_, reply) in pending {
            let _ = reply.send(Err(PluginError::ProcessExited(ending.clone())));
        }
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .abort_all();
    }

    async fn send(&self, request: &PluginProcessRequest) -> Result<(), PluginError> {
        let mut line = serde_json::to_string(request).map_err(|error| {
            PluginError::Process(format!("a request did not serialise: {error}"))
        })?;
        if line.len() > MAX_LINE_BYTES {
            return Err(PluginError::Process(format!(
                "a request exceeds the {MAX_LINE_BYTES}-byte line limit"
            )));
        }
        line.push('\n');
        // A process that stops reading must not hold the writer, and with
        // it every later request and the stop, for ever.
        let write = async {
            let mut writer = self.writer.lock().await;
            writer.write_all(line.as_bytes()).await?;
            writer.flush().await
        };
        match tokio::time::timeout(WRITE_TIMEOUT, write).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(PluginError::ProcessExited(format!(
                "writing to the process failed: {error}"
            ))),
            Err(_) => Err(PluginError::ProcessExited(format!(
                "the process stopped reading: a write did not complete within {}s",
                WRITE_TIMEOUT.as_secs()
            ))),
        }
    }
}

#[async_trait]
impl Held for ServingProcess {
    fn busy(&self) -> bool {
        !self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }

    async fn stop(&self, reason: PluginProcessStopReason) {
        // Said first, so the reader that sees the socket close knows this
        // was an orderly stop. Told why, the process has the grace period
        // to leave on its own terms; then it is signalled, and killed a
        // moment later if it ignores that.
        self.end(format!("stopped: {reason:?}"));
        if let Err(error) = self.send(&PluginProcessRequest::Stop { reason }).await {
            tracing::debug!(pid = self.pid, %error, "plugin process could not be told to stop");
        }
        let mut child = self.child.lock().await;
        if tokio::time::timeout(STOP_GRACE, child.wait_for_exit())
            .await
            .is_err()
            && let Err(error) = child.graceful_shutdown(KILL_GRACE).await
        {
            tracing::warn!(pid = self.pid, %error, "plugin process did not stop cleanly");
        }
        drop(child);
        if let Some(reader) = self
            .reader
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            reader.abort();
        }
    }
}

/// One line from the process as a message; `None` at end of stream.
async fn read_message(
    lines: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> Result<Option<PluginProcessMessage>, PluginError> {
    let mut line = Vec::new();
    let bytes = (&mut *lines)
        .take((MAX_LINE_BYTES + 2) as u64)
        .read_until(b'\n', &mut line)
        .await
        .map_err(|error| {
            PluginError::ProcessExited(format!("reading from the process failed: {error}"))
        })?;
    if bytes == 0 {
        return Ok(None);
    }
    if line.len() > MAX_LINE_BYTES + 1 || line.last() != Some(&b'\n') {
        return Err(PluginError::ProcessExited(format!(
            "the process sent a line over the {MAX_LINE_BYTES}-byte limit"
        )));
    }
    let text = String::from_utf8(line).map_err(|error| {
        PluginError::ProcessExited(format!("the process sent invalid UTF-8: {error}"))
    })?;
    serde_json::from_str(text.trim_end())
        .map(Some)
        .map_err(|error| {
            PluginError::ProcessExited(format!(
                "the process sent a message that did not read: {error}"
            ))
        })
}
