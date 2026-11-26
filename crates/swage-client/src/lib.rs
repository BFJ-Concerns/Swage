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
    use tempfile::TempDir;

    /// Crate-wide lock for tests that modify environment variables.
    ///
    /// Both http.rs and runtime.rs tests manipulate XDG_RUNTIME_DIR. Using a
    /// single lock ensures the two modules cannot race against each other when
    /// Cargo runs their tests in parallel threads.
    pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());
    /// Run `f` with `XDG_RUNTIME_DIR` pointing at a temporary directory,
    /// cleaning up the variable afterwards regardless of panics.
    pub(crate) fn with_temp_xdg<F: FnOnce(&TempDir)>(f: F) {
        let _guard = ENV_LOCK.lock().unwrap();
        with_temp_xdg_locked(f);
    }

    pub(crate) fn with_temp_xdg_locked<F: FnOnce(&TempDir)>(f: F) {
        let dir = TempDir::new().unwrap();
        let original_runtime = std::env::var_os("XDG_RUNTIME_DIR");
        let original_home = std::env::var_os("SWAGE_HOME");
        // SAFETY: callers hold ENV_LOCK for the entire environment scope.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", dir.path());
            std::env::remove_var("SWAGE_HOME");
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&dir)));
        for (key, value) in [
            ("SWAGE_HOME", original_home),
            ("XDG_RUNTIME_DIR", original_runtime),
        ] {
            match value {
                Some(value) => unsafe { std::env::set_var(key, value) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
}
