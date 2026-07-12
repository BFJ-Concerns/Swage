//! Swage gateway — HTTP and WebSocket server for the Swage platform.
//!
//! The gateway is the primary runtime surface of Swage. It exposes a REST API
//! for conversation and space management, a WebSocket interface for streaming
//! agent turns in real time, and an embedded Svelte SPA served from the same
//! process.
//!
//! ## Modules
//!
//! | Module | Responsibility |
//! |--------|----------------|
//! | [`handlers`] | HTTP route handlers for conversations, spaces, and health |
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
pub mod authoring_definition;
pub mod automated_turn;
pub mod background_loop;
pub mod browser;
pub mod builder_runtime;
pub mod capabilities;
pub mod capability_bundle;
pub mod channel_gateway;
pub mod cli;
pub mod context;
pub mod device;
pub mod error;
pub mod handlers;
pub mod lens;
pub mod lifecycle;
pub mod material;
pub mod onboarding;
pub mod operator;
pub mod presence;
pub mod schedule;
pub mod secret;
pub mod server;
pub mod service;
pub mod settings;
pub mod shared_reference;
pub mod shutdown;
pub mod skill;
pub mod space_extension;
pub mod startup;
pub mod state;
pub mod subscription;
pub mod supervision;
pub mod telegram;
pub mod thread_selection;
pub mod tool;
pub mod tool_executor;
pub mod topology;
pub mod turn;
pub mod voice;
pub mod websocket;

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        ffi::OsString,
        path::{Path, PathBuf},
    };

    static PROCESS_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Serialises tests that mutate `SWAGE_HOME`, which is process-wide state.
    pub(crate) struct SwageHomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        _temp_dir: Option<tempfile::TempDir>,
        root: PathBuf,
    }

    impl SwageHomeGuard {
        pub(crate) fn new() -> Self {
            let lock = PROCESS_ENV_LOCK
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
            let lock = PROCESS_ENV_LOCK
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
            let lock = PROCESS_ENV_LOCK
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
}
