use std::collections::VecDeque;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::worker_host::{PythonWorkerError, PythonWorkerRuntime, ReadyWorker};

pub struct WorkerPool {
    runtime: Arc<PythonWorkerRuntime>,
    semaphore: Arc<Semaphore>,
    idle_ttl: Duration,
    warm_target: usize,
    spawn_sandboxed: bool,
    state: Mutex<PoolState>,
}

struct PoolState {
    idle: VecDeque<IdleWorker>,
    warming: usize,
}

struct IdleWorker {
    id: Uuid,
    worker: ReadyWorker,
    permit: OwnedSemaphorePermit,
    expires_at: Instant,
}

pub struct WorkerLease {
    pool: Weak<WorkerPool>,
    worker: Option<ReadyWorker>,
    permit: Option<OwnedSemaphorePermit>,
}

impl WorkerPool {
    pub fn new(runtime: Arc<PythonWorkerRuntime>) -> Result<Arc<Self>, PythonWorkerError> {
        let pool = Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(runtime.config().max_workers)),
            idle_ttl: Duration::from_secs(runtime.config().idle_ttl_secs),
            warm_target: if runtime.config().require_sandbox {
                0
            } else {
                runtime
                    .config()
                    .warm_target
                    .min(runtime.config().max_workers)
            },
            spawn_sandboxed: runtime.config().require_sandbox,
            runtime,
            state: Mutex::new(PoolState {
                idle: VecDeque::new(),
                warming: 0,
            }),
        });

        pool.ensure_warm_workers();
        Ok(pool)
    }

    pub fn runtime(&self) -> &Arc<PythonWorkerRuntime> {
        &self.runtime
    }

    pub async fn acquire(self: &Arc<Self>) -> Result<WorkerLease, PythonWorkerError> {
        if let Some(idle_worker) = self.take_idle_worker().await {
            return Ok(WorkerLease {
                pool: Arc::downgrade(self),
                worker: Some(idle_worker.worker),
                permit: Some(idle_worker.permit),
            });
        }

        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| PythonWorkerError::Protocol("worker semaphore closed".into()))?;
        let worker = self
            .runtime
            .spawn_ready_worker(self.spawn_sandboxed)
            .await?;
        Ok(WorkerLease {
            pool: Arc::downgrade(self),
            worker: Some(worker),
            permit: Some(permit),
        })
    }

    /// Caller-scoped workers are never taken from the unscoped warm pool.
    pub async fn acquire_for_turn(
        self: &Arc<Self>,
        policy: Option<swage_sandbox::SandboxConfig>,
        egress: swage_sandbox::EgressSetup,
    ) -> Result<WorkerLease, PythonWorkerError> {
        if policy.is_none() && self.spawn_sandboxed {
            return Err(PythonWorkerError::SandboxUnavailable(
                "the calling turn supplied no sandbox policy".into(),
            ));
        }
        if self.warm_target > 0 {
            // Acquire through the pool so an in-flight warm-up cannot hold
            // the last permit while a scoped caller waits for it forever.
            let mut lease = self.acquire().await?;
            lease.worker_mut().terminate().await;
            lease.worker = Some(self.runtime.spawn_worker_for_turn(policy, egress).await?);
            return Ok(lease);
        }
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| PythonWorkerError::Protocol("worker semaphore closed".into()))?;
        let worker = self.runtime.spawn_worker_for_turn(policy, egress).await?;
        Ok(WorkerLease {
            pool: Arc::downgrade(self),
            worker: Some(worker),
            permit: Some(permit),
        })
    }

    async fn take_idle_worker(&self) -> Option<IdleWorker> {
        let now = Instant::now();
        let (ready_worker, expired_workers) = {
            let mut state = self.state.lock().await;
            let mut expired_workers = Vec::new();
            let mut ready_worker = None;

            while let Some(idle_worker) = state.idle.pop_front() {
                if idle_worker.expires_at > now {
                    ready_worker = Some(idle_worker);
                    break;
                }
                expired_workers.push(idle_worker);
            }

            (ready_worker, expired_workers)
        };

        for mut idle_worker in expired_workers {
            idle_worker.worker.terminate().await;
        }

        ready_worker
    }

    fn ensure_warm_workers(self: &Arc<Self>) {
        let pool = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let permit = {
                    let mut state = pool.state.lock().await;
                    if state.idle.len() + state.warming >= pool.warm_target {
                        return;
                    }
                    match pool.semaphore.clone().try_acquire_owned() {
                        Ok(permit) => {
                            state.warming += 1;
                            permit
                        }
                        Err(_) => return,
                    }
                };

                let worker_result = pool.runtime.spawn_ready_worker(pool.spawn_sandboxed).await;
                let mut state = pool.state.lock().await;
                state.warming = state.warming.saturating_sub(1);

                match worker_result {
                    Ok(worker) => {
                        let id = Uuid::new_v4();
                        state.idle.push_back(IdleWorker {
                            id,
                            worker,
                            permit,
                            expires_at: Instant::now() + pool.idle_ttl,
                        });
                        drop(state);
                        pool.schedule_idle_eviction(id);
                    }
                    Err(_) => {
                        drop(permit);
                        return;
                    }
                }
            }
        });
    }

    fn schedule_idle_eviction(self: &Arc<Self>, id: Uuid) {
        let pool = Arc::downgrade(self);
        let idle_ttl = self.idle_ttl;
        tokio::spawn(async move {
            tokio::time::sleep(idle_ttl).await;
            let Some(pool) = pool.upgrade() else {
                return;
            };

            let idle_worker = {
                let mut state = pool.state.lock().await;
                if let Some(index) = state.idle.iter().position(|worker| worker.id == id) {
                    state.idle.remove(index)
                } else {
                    None
                }
            };

            if let Some(mut idle_worker) = idle_worker {
                idle_worker.worker.terminate().await;
                pool.ensure_warm_workers();
            }
        });
    }

    async fn release_spent_worker(self: &Arc<Self>) {
        self.ensure_warm_workers();
    }
}

impl WorkerLease {
    pub fn worker_mut(&mut self) -> &mut ReadyWorker {
        self.worker
            .as_mut()
            .expect("worker lease must hold a worker before execution")
    }

    pub async fn mark_spent(mut self) {
        if let Some(pool) = self.pool.upgrade() {
            if let Some(mut worker) = self.worker.take() {
                worker.terminate().await;
            }
            self.permit.take();
            pool.release_spent_worker().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde::Deserialize;
    use swage_config::model::PythonExecuteToolConfig;
    use tempfile::TempDir;

    #[derive(Deserialize)]
    struct RuntimeProbeFixture {
        executable: String,
        base_prefix: String,
        stdlib: Option<String>,
        platstdlib: Option<String>,
    }

    fn host_python_executable() -> PathBuf {
        crate::runtime_resolver::ResolvedPythonRuntime::resolve(&PythonExecuteToolConfig::default())
            .expect("host Python should resolve for tests")
            .executable_path
    }

    fn host_python_probe() -> RuntimeProbeFixture {
        let host_python = host_python_executable();
        let output = Command::new(&host_python)
            .arg("-S")
            .arg("-c")
            .arg(
                r#"
import json
import sys
import sysconfig

paths = sysconfig.get_paths()
print(json.dumps({
    "executable": sys.executable,
    "base_prefix": sys.base_prefix,
    "stdlib": paths.get("stdlib"),
    "platstdlib": paths.get("platstdlib"),
}))
"#,
            )
            .output()
            .expect("probe should execute");
        assert!(
            output.status.success(),
            "probe should succeed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("probe output should be valid JSON")
    }

    fn write_runtime_wrapper(temp_dir: &Path, mode: &str, counter_file: Option<&Path>) -> PathBuf {
        let host_python = host_python_executable();
        let probe = host_python_probe();
        let wrapper_path = temp_dir.join("python-wrapper");
        let counter_literal = counter_file
            .map(|path| format!("{path:?}"))
            .unwrap_or_else(|| "None".to_string());
        let stdlib_literal = probe
            .stdlib
            .as_ref()
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|| "None".to_string());
        let platstdlib_literal = probe
            .platstdlib
            .as_ref()
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|| "None".to_string());

        let script = format!(
            r#"#!{host_python}
import json
import os
import sys

WRAPPER = {wrapper_path:?}
REAL_PYTHON = {real_python:?}
BASE_PREFIX = {base_prefix:?}
STDLIB = {stdlib}
PLATSTDLIB = {platstdlib}
MODE = {mode:?}
COUNTER_FILE = {counter}

if len(sys.argv) >= 3 and sys.argv[1] == "-S" and sys.argv[2] == "-c":
    print(json.dumps({{
        "executable": WRAPPER,
        "base_prefix": BASE_PREFIX,
        "stdlib": STDLIB,
        "platstdlib": PLATSTDLIB,
    }}))
    raise SystemExit(0)

if MODE == "success":
    os.execv(REAL_PYTHON, [REAL_PYTHON, *sys.argv[1:]])

if COUNTER_FILE is not None:
    with open(COUNTER_FILE, "a", encoding="utf-8") as handle:
        handle.write("x")

raise SystemExit(1)
"#,
            host_python = host_python.display(),
            wrapper_path = wrapper_path.display().to_string(),
            real_python = probe.executable,
            base_prefix = probe.base_prefix,
            stdlib = stdlib_literal,
            platstdlib = platstdlib_literal,
            mode = mode,
            counter = counter_literal,
        );

        fs::write(&wrapper_path, script).expect("wrapper script should be written");
        let mut permissions = fs::metadata(&wrapper_path)
            .expect("wrapper metadata should be readable")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper_path, permissions)
            .expect("wrapper script should be executable");
        wrapper_path
    }

    fn unsandboxed_test_runtime() -> Arc<PythonWorkerRuntime> {
        let config = PythonExecuteToolConfig {
            require_sandbox: false,
            warm_target: 0,
            max_workers: 2,
            ..PythonExecuteToolConfig::default()
        };
        Arc::new(
            PythonWorkerRuntime::new(config)
                .expect("test runtime should resolve a Python executable"),
        )
    }

    fn wrapper_runtime(
        mode: &str,
        counter_file: Option<&Path>,
        startup_timeout_secs: u64,
        warm_target: usize,
    ) -> (TempDir, Arc<PythonWorkerRuntime>) {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let wrapper_path = write_runtime_wrapper(temp_dir.path(), mode, counter_file);
        let config = PythonExecuteToolConfig {
            require_sandbox: false,
            runtime_path: Some(wrapper_path.display().to_string()),
            startup_timeout_secs,
            warm_target,
            max_workers: warm_target.max(1),
            ..PythonExecuteToolConfig::default()
        };
        let runtime =
            Arc::new(PythonWorkerRuntime::new(config).expect("wrapper runtime should resolve"));
        (temp_dir, runtime)
    }

    #[tokio::test]
    async fn unsandboxed_turn_records_http_through_its_proxy() {
        use swage_sandbox::{
            EgressConnection, EgressPolicy, EgressRecorder, EgressRequest, EgressSetup,
            EgressVerdict,
        };
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        struct LocalOnly;
        #[async_trait::async_trait]
        impl EgressPolicy for LocalOnly {
            async fn decide(&self, request: &EgressRequest) -> EgressVerdict {
                if request.host == "127.0.0.1" {
                    EgressVerdict::Allowed {
                        basis: "local fixture".into(),
                    }
                } else {
                    EgressVerdict::Refused {
                        reason: "fixture only".into(),
                    }
                }
            }
        }
        #[derive(Default)]
        struct Records(std::sync::Mutex<Vec<EgressConnection>>);
        #[async_trait::async_trait]
        impl EgressRecorder for Records {
            async fn record(&self, connection: EgressConnection) -> Result<(), String> {
                self.0.lock().unwrap().push(connection);
                Ok(())
            }
        }
        struct NoTools;
        #[async_trait::async_trait]
        impl crate::worker_host::NestedToolHost for NoTools {
            async fn dispatch_tool(
                &self,
                _: &str,
                _: serde_json::Value,
            ) -> Result<serde_json::Value, crate::worker_protocol::WorkerToolError> {
                panic!("unexpected tool call");
            }
        }
        let root = tempfile::tempdir().unwrap();
        let server = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = server.local_addr().unwrap();
        let fixture = tokio::spawn(async move {
            let (stream, _) = server.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            assert_eq!(line, "GET /recorded HTTP/1.1\r\n");
            reader
                .get_mut()
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nfixture",
                )
                .await
                .unwrap();
        });
        let recorded = Arc::new(Records::default());
        let runtime = unsandboxed_test_runtime();
        let pool = WorkerPool::new(runtime.clone()).unwrap();
        let mut lease = pool
            .acquire_for_turn(
                None,
                EgressSetup {
                    policy: Arc::new(LocalOnly),
                    recorder: recorded.clone(),
                },
            )
            .await
            .expect("unsandboxed scoped worker must start");
        let code = format!(
            r#"
import os
import urllib.request
# Loopback is normally exempt. This fixture explicitly sends it through
# the worker's configured proxy while keeping every connection local.
os.environ['no_proxy'] = ''
os.environ['NO_PROXY'] = ''
print(urllib.request.urlopen('http://{address}/recorded', timeout=3).read().decode())
"#
        );
        let result = runtime
            .execute_python(lease.worker_mut(), &code, root.path(), &NoTools)
            .await
            .unwrap();
        lease.mark_spent().await;
        assert_eq!(result.exception_type, None, "{:?}", result.traceback);
        assert_eq!(result.stdout, "fixture\n");
        fixture.await.unwrap();
        let records = recorded.0.lock().unwrap();
        assert_eq!(
            records.len(),
            1,
            "the turn's proxy must record the HTTP request"
        );
        assert_eq!(records[0].host, "127.0.0.1");
        assert_eq!(records[0].port, address.port());
        assert_eq!(records[0].method, "GET");
        assert!(records[0].verdict.is_allowed());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn take_idle_worker_terminates_expired_workers_before_reusing_pool() {
        let runtime = unsandboxed_test_runtime();
        let semaphore = Arc::new(Semaphore::new(2));
        let expired_permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("expired permit");
        let live_permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("live permit");

        let expired_worker = runtime
            .spawn_ready_worker(false)
            .await
            .expect("expired worker should start");
        let expired_pid = expired_worker.pid();
        let live_worker = runtime
            .spawn_ready_worker(false)
            .await
            .expect("live worker should start");
        let live_id = Uuid::new_v4();

        let pool = WorkerPool {
            runtime,
            semaphore,
            idle_ttl: Duration::from_secs(60),
            warm_target: 0,
            spawn_sandboxed: false,
            state: Mutex::new(PoolState {
                idle: VecDeque::from([
                    IdleWorker {
                        id: Uuid::new_v4(),
                        worker: expired_worker,
                        permit: expired_permit,
                        expires_at: Instant::now() - Duration::from_secs(1),
                    },
                    IdleWorker {
                        id: live_id,
                        worker: live_worker,
                        permit: live_permit,
                        expires_at: Instant::now() + Duration::from_secs(60),
                    },
                ]),
                warming: 0,
            }),
        };

        let mut acquired_worker = pool
            .take_idle_worker()
            .await
            .expect("live worker should still be available");

        assert_eq!(acquired_worker.id, live_id);

        let expired_proc_path = format!("/proc/{expired_pid}");
        assert!(
            !std::path::Path::new(&expired_proc_path).exists(),
            "expired worker PID {expired_pid} should be gone after cleanup",
        );

        acquired_worker.worker.terminate().await;
    }

    #[tokio::test]
    async fn acquire_honours_require_sandbox_when_pool_workers_are_unsandboxed() {
        let (_temp_dir, runtime) = wrapper_runtime("success", None, 1, 0);
        let pool = WorkerPool::new(runtime).expect("pool should be created");

        let lease = pool.acquire().await.expect(
            "pool acquire should start an unsandboxed worker when require_sandbox is false",
        );

        lease.mark_spent().await;
    }

    #[tokio::test]
    async fn warm_worker_failures_stop_retrying_after_the_first_failure() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let counter_file = temp_dir.path().join("attempts.log");
        let wrapper_path = write_runtime_wrapper(temp_dir.path(), "fail", Some(&counter_file));
        let config = PythonExecuteToolConfig {
            require_sandbox: false,
            runtime_path: Some(wrapper_path.display().to_string()),
            startup_timeout_secs: 1,
            warm_target: 1,
            max_workers: 1,
            ..PythonExecuteToolConfig::default()
        };
        let runtime =
            Arc::new(PythonWorkerRuntime::new(config).expect("wrapper runtime should resolve"));
        let _pool = WorkerPool::new(runtime).expect("pool should be created");

        tokio::time::sleep(Duration::from_millis(2300)).await;

        let attempts = fs::read_to_string(counter_file).unwrap_or_default();
        assert_eq!(
            attempts.len(),
            1,
            "warm-up should stop retrying after the first persistent spawn failure",
        );
    }
}
