//! Typed gateway client library for the Swage platform.
//!
//! Provides HTTP and WebSocket access to a running Swage gateway for the
//! CLI's gateway lifecycle management and prompt runs.
//!
//! # Modules
//!
//! | Module | Responsibility |
//! |--------|----------------|
//! | [`runtime`] | Runtime metadata (PID, address, start time) — file I/O and discovery |
//! | [`error`] | Typed client errors with address context and underlying cause |
//! | [`http`] | HTTP client covering the gateway REST API |
//! | [`websocket`] | WebSocket client for the streaming event protocol |

pub mod error;
pub mod http;
pub mod runtime;
pub mod websocket;

/// Shared test utilities — compiled only in test builds.
#[cfg(test)]
pub(crate) mod test_helpers {
    use std::sync::Mutex;

    /// Crate-wide lock for tests that modify environment variables.
    ///
    /// Both http.rs and runtime.rs tests manipulate XDG_RUNTIME_DIR. Using a
    /// single lock ensures the two modules cannot race against each other when
    /// Cargo runs their tests in parallel threads.
    pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());
}
