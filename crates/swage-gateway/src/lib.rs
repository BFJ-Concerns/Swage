//! Swage gateway — HTTP and WebSocket server for the Swage platform.
//!
//! The gateway is the primary runtime surface of Swage. It exposes a REST API
//! for conversation and topic management, a WebSocket interface for streaming
//! agent turns in real time, and an embedded Svelte SPA served from the same
//! process.
//!
//! ## Modules
//!
//! | Module | Responsibility |
//! |--------|----------------|
//! | [`handlers`] | HTTP route handlers for conversations, topics, and health |
//! | [`websocket`] | WebSocket upgrade and bidirectional message protocol |
//! | [`turn`] | Agent turn orchestration: provider streaming, tool dispatch, storage |
//! | [`tool`] | Tool dispatch pipeline with permission checks and rate limiting |
//! | [`service`] | Request/response DTOs shared by handlers and the websocket layer |
//! | [`state`] | [`state::AppState`] — shared runtime state cloned into every handler |
//! | [`server`] | Router construction and request-pipeline middleware |
//! | [`lifecycle`] | [`lifecycle::ActivityTracker`] — atomic activity timestamp for presence |
//! | [`shutdown`] | Idle monitor, graceful shutdown, and orphaned turn recovery |
//! | [`context`] | Provider and tool context assembly |
//! | [`assets`] | Embedded frontend SPA served at the fallback route |
//! | [`error`] | [`error::AppError`] — axum-compatible error type |
//!
//! ## Structured Logging
//!
//! The gateway emits structured JSON logs to stderr via `tracing`. Two
//! correlation identifiers propagate through the log stream:
//!
//! - `request_id` — a UUID assigned by [`server::build_router`] middleware to
//!   every inbound HTTP request; returned to clients as `X-Request-Id`.
//! - `turn_id` — the database turn UUID recorded on the tracing span at the
//!   start of each agent turn in [`turn::process_turn`].
//!
//! ## Graceful Shutdown
//!
//! [`shutdown::graceful_shutdown`] waits for SIGTERM or SIGINT. Once either
//! resolves, axum drains in-flight connections and
//! [`shutdown::shutdown_active_turns`] cancels any remaining agent turns.
//! [`shutdown::recover_orphaned_turns`] runs at startup to mark turns left
//! in `Processing` state by a previous unclean exit as `Interrupted`.

pub mod approval_scope;
pub mod assets;
pub mod attachment;
pub mod audit;
pub mod automated_turn;
pub mod background_loop;
pub mod browser;
pub mod capabilities;
pub mod capability_bundle;
pub mod channel_gateway;
pub mod cli;
pub mod codex;
pub mod config_change;
pub mod context;
pub mod device;
pub mod error;
pub mod experimentation;
pub mod handlers;
pub mod knowledge;
pub mod knowledge_mirror;
pub mod lifecycle;
pub mod material;
pub mod onboarding;
pub mod operator;
pub mod presence;
pub mod recent_sessions;
pub mod record_durability;
pub mod schedule;
pub mod secret;
pub mod server;
pub mod service;
pub mod session_archive;
pub mod settings;
pub mod shipped_docs;
mod shipped_skill_projection;
mod shipped_skills;
pub mod shutdown;
pub mod skill;
pub mod startup;
pub mod state;
pub mod subscription;
pub mod supervision;
pub mod surface;
mod synthetic_id;
pub mod telegram;
pub mod thread_selection;
pub mod tool;
pub mod tool_executor;
pub mod turn;
pub mod voice;
pub mod websocket;
pub mod workflow;
mod workflow_delivery;
pub mod worktree;

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        ffi::OsString,
        path::{Path, PathBuf},
    };

    use async_trait::async_trait;
    use serde_json::{Value, json};
    use swage_core::ToolError;
    use swage_tool::{context::ToolContext, tool::Tool};

    /// Gateway state for tests of storage-backed surfaces with no provider turns.
    pub(crate) fn state(
        storage: std::sync::Arc<swage_storage::SqliteStorage>,
    ) -> crate::state::AppState {
        use swage_storage::{Storage, TopicStore};

        crate::state::AppState::new(
            std::sync::Arc::clone(&storage) as std::sync::Arc<dyn Storage>,
            storage as std::sync::Arc<dyn TopicStore>,
            std::sync::Arc::new(crate::turn::tests::MockProvider::new(vec![])),
            swage_config::ResolvedConfig::default(),
            crate::tool::ToolDispatcher::allow_all(vec![]),
            None,
        )
    }

    static SWAGE_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static XDG_RUNTIME_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Minimal named tool for tests that exercise dispatcher selection only.
    pub(crate) struct NamedTool {
        name: &'static str,
        result: NamedToolResult,
    }

    enum NamedToolResult {
        Sentinel,
        EchoArguments,
    }

    impl NamedTool {
        pub(crate) fn new(name: &'static str) -> Self {
            Self {
                name,
                result: NamedToolResult::Sentinel,
            }
        }

        pub(crate) fn echoing(name: &'static str) -> Self {
            Self {
                name,
                result: NamedToolResult::EchoArguments,
            }
        }
    }

    #[async_trait]
    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "Test tool"
        }

        fn parameters(&self) -> Value {
            json!({ "type": "object" })
        }

        async fn execute(
            &self,
            arguments: Value,
            _ctx: &dyn ToolContext,
        ) -> Result<Value, ToolError> {
            Ok(match self.result {
                NamedToolResult::Sentinel => json!({ "unexpected": "execution" }),
                NamedToolResult::EchoArguments => arguments,
            })
        }
    }

    /// Serialises tests that mutate `SWAGE_HOME`, which is process-wide state.
    pub(crate) struct SwageHomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        _temp_dir: Option<tempfile::TempDir>,
        root: PathBuf,
    }

    impl SwageHomeGuard {
        pub(crate) fn new() -> Self {
            let lock = SWAGE_HOME_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let temp_dir = tempfile::TempDir::new().expect("temporary swage home");
            let root = temp_dir.path().to_path_buf();
            unsafe { std::env::set_var("SWAGE_HOME", &root) };
            Self {
                _lock: lock,
                _temp_dir: Some(temp_dir),
                root,
            }
        }

        pub(crate) fn set_to(path: impl AsRef<Path>) -> Self {
            let lock = SWAGE_HOME_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let root = path.as_ref().to_path_buf();
            unsafe { std::env::set_var("SWAGE_HOME", &root) };
            Self {
                _lock: lock,
                _temp_dir: None,
                root,
            }
        }

        pub(crate) fn isolate() -> Self {
            Self::new()
        }

        pub(crate) fn path(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for SwageHomeGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("SWAGE_HOME") };
        }
    }

    /// Serialises tests that mutate `XDG_RUNTIME_DIR`, which controls runtime
    /// gateway metadata discovery.
    pub(crate) struct XdgRuntimeDirGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        _temp_dir: tempfile::TempDir,
        previous: Option<OsString>,
    }

    impl XdgRuntimeDirGuard {
        pub(crate) fn new() -> Self {
            let lock = XDG_RUNTIME_DIR_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let temp_dir = tempfile::TempDir::new().expect("temporary runtime dir");
            let previous = std::env::var_os("XDG_RUNTIME_DIR");
            unsafe { std::env::set_var("XDG_RUNTIME_DIR", temp_dir.path()) };
            Self {
                _lock: lock,
                _temp_dir: temp_dir,
                previous,
            }
        }

        pub(crate) fn path(&self) -> &Path {
            self._temp_dir.path()
        }
    }

    impl Drop for XdgRuntimeDirGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                unsafe { std::env::set_var("XDG_RUNTIME_DIR", previous) };
            } else {
                unsafe { std::env::remove_var("XDG_RUNTIME_DIR") };
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::{
            process::{Command, Stdio},
            thread,
            time::{Duration, Instant},
        };

        use super::{SwageHomeGuard, XdgRuntimeDirGuard};

        const CHILD_TIMEOUT: Duration = Duration::from_secs(2);

        #[test]
        fn guards_can_be_acquired_in_either_order() {
            for child_test in [
                "test_support::tests::home_then_runtime_child",
                "test_support::tests::runtime_then_home_child",
            ] {
                let mut child = Command::new(
                    std::env::current_exe().expect("current test executable should resolve"),
                )
                .args(["--exact", child_test, "--ignored", "--nocapture"])
                .env_remove("SWAGE_HOME")
                .env_remove("XDG_RUNTIME_DIR")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("isolated guard regression child should start");

                let deadline = Instant::now() + CHILD_TIMEOUT;
                let status = loop {
                    if let Some(status) = child
                        .try_wait()
                        .expect("isolated guard regression child should be inspectable")
                    {
                        break status;
                    }
                    if Instant::now() >= deadline {
                        child
                            .kill()
                            .expect("deadlocked guard child should be killable");
                        let output = child
                            .wait_with_output()
                            .expect("killed guard child output should be readable");
                        panic!(
                            "{child_test} deadlocked while acquiring independent environment guards; stdout:\n{}\nstderr:\n{}",
                            String::from_utf8_lossy(&output.stdout),
                            String::from_utf8_lossy(&output.stderr),
                        );
                    }
                    thread::sleep(Duration::from_millis(10));
                };
                let output = child
                    .wait_with_output()
                    .expect("guard child output should be readable");
                assert!(
                    status.success(),
                    "{child_test} failed; stdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
                assert!(
                    String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
                    "{child_test} did not report exactly one passing child test; stdout:\n{}\nstderr:\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                );
            }
        }

        #[test]
        #[ignore = "run in an isolated child process by guards_can_be_acquired_in_either_order"]
        fn home_then_runtime_child() {
            let home = SwageHomeGuard::new();
            assert_eq!(
                std::env::var_os("SWAGE_HOME").as_deref(),
                Some(home.path().as_os_str())
            );
            let runtime = XdgRuntimeDirGuard::new();
            assert_eq!(
                std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
                Some(runtime.path().as_os_str())
            );
            drop(runtime);
            drop(home);
            assert!(std::env::var_os("SWAGE_HOME").is_none());
            assert!(std::env::var_os("XDG_RUNTIME_DIR").is_none());
        }

        #[test]
        #[ignore = "run in an isolated child process by guards_can_be_acquired_in_either_order"]
        fn runtime_then_home_child() {
            let runtime = XdgRuntimeDirGuard::new();
            assert_eq!(
                std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
                Some(runtime.path().as_os_str())
            );
            let home = SwageHomeGuard::new();
            assert_eq!(
                std::env::var_os("SWAGE_HOME").as_deref(),
                Some(home.path().as_os_str())
            );
            drop(home);
            drop(runtime);
            assert!(std::env::var_os("SWAGE_HOME").is_none());
            assert!(std::env::var_os("XDG_RUNTIME_DIR").is_none());
        }
    }
}
