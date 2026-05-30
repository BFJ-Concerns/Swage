use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tempfile::TempDir;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;
use uuid::Uuid;

use swage_config::model::PythonExecuteToolConfig;
use swage_sandbox::{ChildProcessManager, find_bwrap};

use crate::runtime_resolver::{PythonRuntimeError, ResolvedPythonRuntime};
use crate::worker_protocol::{
    AssemblyExecutionPayload, AssemblyExecutionResult, ExecutePayload, HostToWorkerMessage,
    TopologyExecutionPayload, WorkerToHostMessage, WorkerToolError,
};

const BOOTSTRAP_SCRIPT: &str = include_str!("../resources/worker.py");

#[derive(Debug, Error)]
pub enum PythonWorkerError {
    #[error(transparent)]
    Runtime(#[from] PythonRuntimeError),
    #[error("sandboxed Python workers are unavailable: {0}")]
    SandboxUnavailable(String),
    #[error("failed to spawn Python worker: {0}")]
    SpawnFailed(String),
    #[error("worker protocol error: {0}")]
    Protocol(String),
    #[error("worker startup timed out after {0}s")]
    StartupTimeout(u64),
    #[error("worker execution timed out after {0}s")]
    ExecutionTimeout(u64),
}

#[derive(Debug, Clone)]
pub struct PythonExecutionResult {
    pub stdout: String,
    pub exception_type: Option<String>,
    pub traceback: Option<String>,
    pub result: Option<Value>,
}

#[async_trait]
pub trait NestedToolHost: Send + Sync {
    async fn dispatch_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<Value, WorkerToolError>;
}

#[derive(Debug, Clone)]
pub struct PythonWorkerRuntime {
    config: PythonExecuteToolConfig,
    runtime: Arc<ResolvedPythonRuntime>,
}

impl PythonWorkerRuntime {
    pub fn new(config: PythonExecuteToolConfig) -> Result<Self, PythonWorkerError> {
        let runtime = ResolvedPythonRuntime::resolve(&config)?;
        Ok(Self {
            config,
            runtime: Arc::new(runtime),
        })
    }

    pub fn config(&self) -> &PythonExecuteToolConfig {
        &self.config
    }

    pub fn sandbox_supported(&self) -> bool {
        cfg!(target_os = "linux") && find_bwrap().is_ok()
    }

    pub fn python_execute_available(&self) -> bool {
        self.config.enabled && (!self.config.require_sandbox || self.sandbox_supported())
    }

    // Startup timeout only cancels the socket accept; the error path below
    // terminates and reaps the just-spawned worker before returning.
    #[allow(clippy::disallowed_methods)]
    pub async fn spawn_ready_worker(
        &self,
        sandboxed: bool,
    ) -> Result<ReadyWorker, PythonWorkerError> {
        if sandboxed && !self.sandbox_supported() {
            return Err(PythonWorkerError::SandboxUnavailable(
                "bubblewrap is required".to_string(),
            ));
        }

        let worker_dir = WorkerDirectory::new()?;
        let listener = UnixListener::bind(worker_dir.socket_path()).map_err(|error| {
            PythonWorkerError::SpawnFailed(format!(
                "failed to bind worker socket {}: {error}",
                worker_dir.socket_path().display()
            ))
        })?;

        let mut command = if sandboxed {
            build_sandboxed_command(&self.runtime, &worker_dir)?
        } else {
            build_unsandboxed_command(&self.runtime, &worker_dir)
        };

        command.stdin(Stdio::piped());
        let mut child = ChildProcessManager::spawn(command).map_err(|error| {
            PythonWorkerError::SpawnFailed(format!("child spawn failed: {error}"))
        })?;

        let stdin = match child.take_stdin() {
            Some(stdin) => stdin,
            None => {
                let _ = child.graceful_shutdown(Duration::from_secs(2)).await;
                return Err(PythonWorkerError::SpawnFailed(
                    "worker stdin was not piped".into(),
                ));
            }
        };

        tokio::spawn(async move {
            let mut stdin = stdin;
            let _ = stdin.write_all(BOOTSTRAP_SCRIPT.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });

        let startup_result = async {
            let stream = tokio::time::timeout(
                Duration::from_secs(self.config.startup_timeout_secs),
                listener.accept(),
            )
            .await
            .map_err(|_| PythonWorkerError::StartupTimeout(self.config.startup_timeout_secs))?
            .map_err(|error| PythonWorkerError::SpawnFailed(error.to_string()))?
            .0;

            let mut transport = WorkerTransport::new(stream);
            match transport
                .receive_timeout(Duration::from_secs(self.config.startup_timeout_secs))
                .await?
            {
                WorkerToHostMessage::Ready => Ok(transport),
                WorkerToHostMessage::Error { message } => Err(PythonWorkerError::Protocol(message)),
                other => Err(PythonWorkerError::Protocol(format!(
                    "expected ready message, got {other:?}"
                ))),
            }
        }
        .await;

        match startup_result {
            Ok(transport) => Ok(ReadyWorker {
                child,
                transport,
                sandboxed,
                _worker_dir: worker_dir,
            }),
            Err(error) => {
                let _ = child.graceful_shutdown(Duration::from_secs(2)).await;
                Err(error)
            }
        }
    }

    pub async fn execute_python(
        &self,
        worker: &mut ReadyWorker,
        code: &str,
        logical_working_directory: &Path,
        tool_host: &dyn NestedToolHost,
    ) -> Result<PythonExecutionResult, PythonWorkerError> {
        let execution_working_directory =
            worker.execution_working_directory(logical_working_directory);
        let payload = ExecutePayload::PythonExecute {
            code: code.to_string(),
            working_directory: execution_working_directory.display().to_string(),
        };
        self.run_payload(worker, payload, Some(tool_host)).await
    }

    pub async fn execute_assembly(
        &self,
        payload: AssemblyExecutionPayload,
        timeout: Duration,
    ) -> Result<AssemblyExecutionResult, PythonWorkerError> {
        let mut worker = self.spawn_ready_worker(false).await?;
        let execution = self
            .run_payload_with_timeout(
                &mut worker,
                ExecutePayload::Assembly(payload),
                timeout,
                None,
            )
            .await?;

        if let Some(traceback) = execution.traceback {
            return Err(PythonWorkerError::Protocol(traceback));
        }

        let result = execution.result.ok_or_else(|| {
            PythonWorkerError::Protocol("assembly worker returned no result".into())
        })?;
        serde_json::from_value(result)
            .map_err(|error| PythonWorkerError::Protocol(error.to_string()))
    }

    pub async fn execute_topology(
        &self,
        payload: TopologyExecutionPayload,
        timeout: Duration,
    ) -> Result<PythonExecutionResult, PythonWorkerError> {
        let mut worker = self.spawn_ready_worker(false).await?;
        self.run_payload_with_timeout(
            &mut worker,
            ExecutePayload::Topology(payload),
            timeout,
            None,
        )
        .await
    }

    async fn run_payload(
        &self,
        worker: &mut ReadyWorker,
        payload: ExecutePayload,
        tool_host: Option<&dyn NestedToolHost>,
    ) -> Result<PythonExecutionResult, PythonWorkerError> {
        self.run_payload_with_timeout(
            worker,
            payload,
            Duration::from_secs(self.config.timeout_secs),
            tool_host,
        )
        .await
    }

    // Execution timeout intentionally abandons the in-flight protocol receive
    // because this path immediately kills the worker and removes it from reuse.
    #[allow(clippy::disallowed_methods)]
    async fn run_payload_with_timeout(
        &self,
        worker: &mut ReadyWorker,
        payload: ExecutePayload,
        timeout: Duration,
        tool_host: Option<&dyn NestedToolHost>,
    ) -> Result<PythonExecutionResult, PythonWorkerError> {
        let request_id = Uuid::new_v4().to_string();
        worker
            .transport
            .send(&HostToWorkerMessage::Execute {
                request_id: request_id.clone(),
                payload: Box::new(payload),
            })
            .await?;

        let execution = match tokio::time::timeout(
            timeout,
            async {
                loop {
                    match worker.transport.receive().await? {
                        WorkerToHostMessage::DispatchTool {
                            request_id: incoming_request_id,
                            call_id,
                            tool_name,
                            arguments,
                        } => {
                            if incoming_request_id != request_id {
                                return Err(PythonWorkerError::Protocol(
                                    "dispatch request_id mismatch".into(),
                                ));
                            }

                            let tool_host = tool_host.ok_or_else(|| {
                                PythonWorkerError::Protocol(
                                    "worker attempted nested tool dispatch in a mode that does not support it"
                                        .into(),
                                )
                            })?;

                            let reply = match tool_host.dispatch_tool(&tool_name, arguments).await {
                                Ok(result) => HostToWorkerMessage::DispatchToolResult {
                                    request_id: request_id.clone(),
                                    call_id,
                                    result: Some(result),
                                    error: None,
                                },
                                Err(error) => HostToWorkerMessage::DispatchToolResult {
                                    request_id: request_id.clone(),
                                    call_id,
                                    result: None,
                                    error: Some(error),
                                },
                            };
                            worker.transport.send(&reply).await?;
                        }
                        WorkerToHostMessage::ExecutionResult {
                            request_id: incoming_request_id,
                            stdout,
                            exception_type,
                            traceback,
                            timed_out,
                            result,
                        } => {
                            if incoming_request_id != request_id {
                                return Err(PythonWorkerError::Protocol(
                                    "execution_result request_id mismatch".into(),
                                ));
                            }

                            return Ok(PythonExecutionResult {
                                stdout,
                                exception_type: if timed_out {
                                    Some("TimeoutError".to_string())
                                } else {
                                    exception_type
                                },
                                traceback,
                                result,
                            });
                        }
                        WorkerToHostMessage::Error { message } => {
                            return Err(PythonWorkerError::Protocol(message));
                        }
                        WorkerToHostMessage::Ready | WorkerToHostMessage::ShutdownAck => {
                            return Err(PythonWorkerError::Protocol(
                                "unexpected worker message during execution".into(),
                            ));
                        }
                    }
                }
            },
        )
        .await
        {
            Ok(execution) => execution,
            Err(_) => Err(PythonWorkerError::ExecutionTimeout(timeout.as_secs())),
        };

        let shutdown_result = worker.shutdown().await;
        if shutdown_result.is_err() {
            worker.force_kill().await;
        }

        execution
    }
}

pub struct ReadyWorker {
    child: ChildProcessManager,
    transport: WorkerTransport,
    sandboxed: bool,
    _worker_dir: WorkerDirectory,
}

impl ReadyWorker {
    fn execution_working_directory<'a>(&'a self, logical_working_directory: &'a Path) -> &'a Path {
        if self.sandboxed {
            // Sandboxed workers are pooled before a turn supplies its logical
            // workspace, so only the worker directory is guaranteed mounted.
            // Host-side nested tools still receive the logical workspace.
            self._worker_dir.work_dir()
        } else {
            logical_working_directory
        }
    }

    pub async fn shutdown(&mut self) -> Result<(), PythonWorkerError> {
        self.transport.send(&HostToWorkerMessage::Shutdown).await?;
        match self
            .transport
            .receive_timeout(Duration::from_secs(2))
            .await?
        {
            WorkerToHostMessage::ShutdownAck => {
                self.child
                    .graceful_shutdown(Duration::from_secs(2))
                    .await
                    .map_err(|error| PythonWorkerError::SpawnFailed(error.to_string()))?;
                Ok(())
            }
            other => Err(PythonWorkerError::Protocol(format!(
                "expected shutdown_ack, got {other:?}"
            ))),
        }
    }

    pub async fn terminate(&mut self) {
        if self.shutdown().await.is_err() {
            let _ = self.child.graceful_shutdown(Duration::from_secs(2)).await;
        }
    }

    pub async fn force_kill(&mut self) {
        let _ = self.child.force_kill();
    }

    #[cfg(test)]
    pub fn pid(&self) -> u32 {
        self.child.pid()
    }
}

struct WorkerTransport {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: BufWriter<tokio::net::unix::OwnedWriteHalf>,
}

impl WorkerTransport {
    fn new(stream: UnixStream) -> Self {
        let (read_half, write_half) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer: BufWriter::new(write_half),
        }
    }

    async fn send(&mut self, message: &HostToWorkerMessage) -> Result<(), PythonWorkerError> {
        let mut line = serde_json::to_string(message)
            .map_err(|error| PythonWorkerError::Protocol(error.to_string()))?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .await
            .map_err(|error| PythonWorkerError::Protocol(error.to_string()))?;
        self.writer
            .flush()
            .await
            .map_err(|error| PythonWorkerError::Protocol(error.to_string()))
    }

    async fn receive(&mut self) -> Result<WorkerToHostMessage, PythonWorkerError> {
        let mut line = String::new();
        let bytes = self
            .reader
            .read_line(&mut line)
            .await
            .map_err(|error| PythonWorkerError::Protocol(error.to_string()))?;
        if bytes == 0 {
            return Err(PythonWorkerError::Protocol(
                "worker connection closed".into(),
            ));
        }
        serde_json::from_str(line.trim_end())
            .map_err(|error| PythonWorkerError::Protocol(error.to_string()))
    }

    // Worker messages are line-framed; dropping `read_until` preserves buffered
    // bytes for a later receive instead of committing partial protocol state.
    #[allow(clippy::disallowed_methods)]
    async fn receive_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<WorkerToHostMessage, PythonWorkerError> {
        tokio::time::timeout(timeout, self.receive())
            .await
            .map_err(|_| PythonWorkerError::Protocol("timeout waiting for worker message".into()))?
    }
}

struct WorkerDirectory {
    _temp_dir: TempDir,
    socket_dir: PathBuf,
    work_dir: PathBuf,
}

impl WorkerDirectory {
    fn new() -> Result<Self, PythonWorkerError> {
        let temp_dir = tempfile::tempdir()
            .map_err(|error| PythonWorkerError::SpawnFailed(error.to_string()))?;
        let socket_dir = temp_dir.path().join("socket");
        let work_dir = temp_dir.path().join("work");
        std::fs::create_dir_all(&socket_dir)
            .and_then(|_| std::fs::create_dir_all(&work_dir))
            .map_err(|error| PythonWorkerError::SpawnFailed(error.to_string()))?;

        Ok(Self {
            _temp_dir: temp_dir,
            socket_dir,
            work_dir,
        })
    }

    fn socket_path(&self) -> PathBuf {
        self.socket_dir.join("worker.sock")
    }

    fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    fn socket_dir(&self) -> &Path {
        &self.socket_dir
    }
}

fn build_unsandboxed_command(
    runtime: &ResolvedPythonRuntime,
    worker_dir: &WorkerDirectory,
) -> Command {
    let mut command = Command::new(&runtime.executable_path);
    command.args(worker_python_startup_args(runtime));
    command.current_dir(worker_dir.work_dir());
    apply_environment(&mut command, &runtime.environment);
    command.env(
        "SWAGE_PYTHON_WORKER_SOCKET",
        worker_dir.socket_path().display().to_string(),
    );
    command
}

fn build_sandboxed_command(
    runtime: &ResolvedPythonRuntime,
    worker_dir: &WorkerDirectory,
) -> Result<Command, PythonWorkerError> {
    let bwrap =
        find_bwrap().map_err(|error| PythonWorkerError::SandboxUnavailable(error.to_string()))?;
    let mut command = Command::new(bwrap);
    command.arg("--unshare-all");
    command.arg("--die-with-parent");
    command.args(["--dev", "/dev"]);
    command.args(["--proc", "/proc"]);
    command.args(["--tmpfs", "/tmp"]);
    command
        .arg("--bind")
        .arg(worker_dir.socket_dir())
        .arg(worker_dir.socket_dir());
    command
        .arg("--bind")
        .arg(worker_dir.work_dir())
        .arg(worker_dir.work_dir());

    for mount in &runtime.read_only_mounts {
        command.arg("--ro-bind").arg(mount).arg(mount);
    }

    command.arg("--chdir").arg(worker_dir.work_dir());
    command.arg("--setenv");
    command.arg("SWAGE_PYTHON_WORKER_SOCKET");
    command.arg(worker_dir.socket_path());
    for (key, value) in &runtime.environment {
        command.arg("--setenv").arg(key).arg(value);
    }
    command
        .arg("--")
        .arg(&runtime.executable_path)
        .args(worker_python_startup_args(runtime));
    Ok(command)
}

fn worker_python_startup_args(runtime: &ResolvedPythonRuntime) -> &'static [&'static str] {
    if runtime.environment.contains_key("PYTHONHOME")
        || runtime.environment.contains_key("PYTHONPATH")
    {
        // Bundled runtimes rely on these variables to bootstrap their stdlib path.
        &["-B", "-S", "-u", "-"]
    } else {
        &["-I", "-B", "-S", "-u", "-"]
    }
}

fn apply_environment(command: &mut Command, environment: &BTreeMap<String, String>) {
    for (key, value) in environment {
        command.env(key, value);
    }
}

#[cfg(test)]
mod tests {
    // Test timeouts bound polling loops around dedicated worker processes; the
    // assertions verify those processes have already been terminated.
    #![allow(clippy::disallowed_methods)]

    use super::*;

    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::worker_protocol::{
        AssemblyExecutionPayload, AssemblyWorkerData, TopologyExecutionPayload,
    };

    struct NoopToolHost;

    #[async_trait]
    impl NestedToolHost for NoopToolHost {
        async fn dispatch_tool(
            &self,
            _tool_name: &str,
            _arguments: Value,
        ) -> Result<Value, WorkerToolError> {
            panic!("unexpected nested tool dispatch in worker_host test");
        }
    }

    fn unsandboxed_test_runtime(timeout_secs: u64) -> PythonWorkerRuntime {
        let config = PythonExecuteToolConfig {
            require_sandbox: false,
            warm_target: 0,
            timeout_secs,
            ..PythonExecuteToolConfig::default()
        };
        PythonWorkerRuntime::new(config).expect("test runtime should resolve a Python executable")
    }

    fn custom_test_runtime(
        executable_path: PathBuf,
        startup_timeout_secs: u64,
        environment: BTreeMap<String, String>,
    ) -> PythonWorkerRuntime {
        let config = PythonExecuteToolConfig {
            require_sandbox: false,
            warm_target: 0,
            startup_timeout_secs,
            timeout_secs: startup_timeout_secs,
            ..PythonExecuteToolConfig::default()
        };

        PythonWorkerRuntime {
            config,
            runtime: Arc::new(ResolvedPythonRuntime {
                executable_path,
                runtime_root: None,
                environment,
                read_only_mounts: vec![],
            }),
        }
    }

    fn process_exists(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn empty_assembly_payload(code: &str) -> AssemblyExecutionPayload {
        AssemblyExecutionPayload {
            code: code.to_string(),
            path: "<assembly-import-test>".to_string(),
            budget: 1_000,
            model: "test-model".to_string(),
            agent_id: "test-agent".to_string(),
            data: AssemblyWorkerData {
                prompts_messages: vec![],
                prompts_token_count: 0,
                prompts_source: "prompts".to_string(),
                skills_messages: vec![],
                skills_token_count: 0,
                skills_source: "skills".to_string(),
                memory_messages: vec![],
                memory_token_count: 0,
                memory_source: "memory".to_string(),
                history_messages: vec![],
                history_token_count: 0,
                history_source: "history".to_string(),
                history_per_message_tokens: vec![],
                search_levels: vec![],
            },
        }
    }

    #[test]
    fn worker_startup_args_keep_isolated_mode_without_runtime_python_env() {
        let runtime = ResolvedPythonRuntime {
            executable_path: "python3".into(),
            runtime_root: None,
            environment: BTreeMap::from([("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string())]),
            read_only_mounts: vec![],
        };

        assert_eq!(
            worker_python_startup_args(&runtime),
            ["-I", "-B", "-S", "-u", "-"]
        );
    }

    #[test]
    fn worker_startup_args_drop_isolated_mode_for_bundled_runtime_env() {
        let runtime = ResolvedPythonRuntime {
            executable_path: "python3".into(),
            runtime_root: Some("/tmp/swage-python".into()),
            environment: BTreeMap::from([
                ("PYTHONDONTWRITEBYTECODE".to_string(), "1".to_string()),
                ("PYTHONHOME".to_string(), "/tmp/swage-python".to_string()),
                (
                    "PYTHONPATH".to_string(),
                    "/tmp/swage-python/lib".to_string(),
                ),
            ]),
            read_only_mounts: vec![],
        };

        assert_eq!(
            worker_python_startup_args(&runtime),
            ["-B", "-S", "-u", "-"]
        );
    }

    #[tokio::test]
    async fn python_execute_uses_shared_globals_and_locals_namespace() {
        let runtime = unsandboxed_test_runtime(5);
        let mut worker = runtime
            .spawn_ready_worker(false)
            .await
            .expect("worker should start");
        let working_directory = TempDir::new().expect("temp dir");

        let execution = runtime
            .execute_python(
                &mut worker,
                "x = 1\n\ndef f():\n    print(x)\n\nf()\n",
                working_directory.path(),
                &NoopToolHost,
            )
            .await
            .expect("python execution should succeed");

        assert_eq!(execution.exception_type, None);
        assert_eq!(execution.traceback, None);
        assert_eq!(execution.stdout, "1\n");

        if worker.child.is_running() {
            worker.force_kill().await;
        }
    }

    #[tokio::test]
    async fn python_execute_allows_standard_library_imports() {
        let runtime = unsandboxed_test_runtime(5);
        let mut worker = runtime
            .spawn_ready_worker(false)
            .await
            .expect("worker should start");
        let working_directory = TempDir::new().expect("temp dir");

        let execution = runtime
            .execute_python(
                &mut worker,
                "import math\nprint(math.sqrt(16))\n",
                working_directory.path(),
                &NoopToolHost,
            )
            .await
            .expect("python execution should succeed");

        assert_eq!(execution.exception_type, None);
        assert_eq!(execution.traceback, None);
        assert_eq!(execution.stdout, "4.0\n");

        if worker.child.is_running() {
            worker.force_kill().await;
        }
    }

    #[tokio::test]
    async fn python_execute_uses_requested_working_directory() {
        let runtime = unsandboxed_test_runtime(5);
        let mut worker = runtime
            .spawn_ready_worker(false)
            .await
            .expect("worker should start");
        let working_directory = TempDir::new().expect("temp dir");

        let execution = runtime
            .execute_python(
                &mut worker,
                "import os\nprint(os.getcwd())\n",
                working_directory.path(),
                &NoopToolHost,
            )
            .await
            .expect("python execution should succeed");

        assert_eq!(execution.exception_type, None);
        assert_eq!(execution.traceback, None);
        assert_eq!(
            execution.stdout,
            format!("{}\n", working_directory.path().display())
        );

        if worker.child.is_running() {
            worker.force_kill().await;
        }
    }

    #[tokio::test]
    async fn assembly_execution_allows_standard_library_imports() {
        let runtime = unsandboxed_test_runtime(5);

        let result = runtime
            .execute_assembly(empty_assembly_payload(
                "import json\n\n\ndef assemble(ctx):\n    assert json.loads(\"[1, 2]\") == [1, 2]\n",
            ), Duration::from_secs(5))
            .await
            .expect("assembly execution should succeed");

        assert!(result.messages.is_empty());
        assert!(result.element_reports.is_empty());
        assert!(result.pending_compactions.is_empty());
        assert!(result.warnings.is_empty());
    }

    #[tokio::test]
    async fn execution_timeout_force_kills_the_worker_before_returning() {
        let runtime = unsandboxed_test_runtime(1);
        let mut worker = runtime
            .spawn_ready_worker(false)
            .await
            .expect("worker should start");

        let execution = runtime
            .run_payload_with_timeout(
                &mut worker,
                ExecutePayload::Topology(TopologyExecutionPayload {
                    code: "while True:\n    pass\n".to_string(),
                    path: "<timeout-test>".to_string(),
                    catalog: Default::default(),
                }),
                Duration::from_secs(1),
                None,
            )
            .await;

        assert!(
            matches!(execution, Err(PythonWorkerError::ExecutionTimeout(1))),
            "expected execution timeout, got {execution:?}"
        );

        tokio::time::timeout(Duration::from_secs(2), async {
            while worker.child.is_running() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("timed-out worker should be terminated promptly");
    }

    #[tokio::test]
    async fn startup_timeout_kills_startup_failed_worker_before_returning() {
        let temp_dir = TempDir::new().expect("temp dir");
        let pid_file = temp_dir.path().join("worker.pid");
        let script_path = temp_dir.path().join("hung-worker.sh");
        fs::write(
            &script_path,
            "#!/bin/sh\nprintf '%s' \"$$\" > \"$SWAGE_TEST_PID_FILE\"\nsleep 30\n",
        )
        .expect("script should be written");
        fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("script should be executable");

        let runtime = custom_test_runtime(
            script_path,
            1,
            BTreeMap::from([(
                "SWAGE_TEST_PID_FILE".to_string(),
                pid_file.display().to_string(),
            )]),
        );

        let result = runtime.spawn_ready_worker(false).await;

        assert!(matches!(result, Err(PythonWorkerError::StartupTimeout(1))));

        let worker_pid: u32 = fs::read_to_string(&pid_file)
            .expect("worker pid file should exist")
            .trim()
            .parse()
            .expect("worker pid should parse");

        tokio::time::timeout(Duration::from_secs(2), async {
            while process_exists(worker_pid) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("startup-failed worker should be terminated promptly");
    }
}
