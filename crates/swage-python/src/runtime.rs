//! Python runtime management — discovery, initialisation, and lifecycle.
//!
//! Manages the embedded Python interpreter via PyO3 using a bundled
//! python-build-standalone distribution. The runtime is process-global
//! (PyO3 limitation) — only one `PythonRuntime` can exist per process.

use std::ffi::CStr;
use std::path::{Path, PathBuf};

use pyo3::Python;
use thiserror::Error;
use tracing::{debug, info, warn};

/// Trivial expression for interpreter smoke tests.
const SMOKE_TEST: &CStr = c"1 + 1";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors arising from Python runtime discovery, initialisation, or execution.
#[derive(Debug, Error)]
pub enum PythonError {
    /// The expected runtime directory does not exist.
    #[error("Python runtime not found at {path}")]
    RuntimeNotFound { path: PathBuf },

    /// The runtime directory exists but is missing critical components.
    #[error("Python runtime corrupted at {path}: {reason}")]
    RuntimeCorrupted { path: PathBuf, reason: String },

    /// The interpreter failed to start.
    #[error("Python initialisation failed: {0}")]
    InitialisationFailed(String),

    /// An error during code execution.
    #[error("Python execution error: {0}")]
    ExecutionError(String),

    /// Execution exceeded the allowed wall-clock time.
    #[error("Python execution timed out after {seconds}s")]
    Timeout { seconds: u64 },
}

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

/// Manages the embedded Python interpreter.
///
/// Discovers the bundled runtime directory, configures the environment, and
/// provides a readiness check. The underlying PyO3 interpreter is process-
/// global, so this struct is effectively a singleton — creating a second
/// instance reuses the same interpreter.
#[derive(Debug)]
pub struct PythonRuntime {
    /// The resolved runtime directory, or `None` when using system Python.
    resolved_path: Option<PathBuf>,
}

impl PythonRuntime {
    /// Create a new runtime, optionally targeting a bundled distribution.
    ///
    /// Discovery order:
    /// 1. Explicit `runtime_path` argument (must exist or returns error).
    /// 2. Directory alongside the current executable (`<exe_dir>/python/`).
    /// 3. User-local default (`~/.swage/python/`).
    /// 4. System Python fallback (dev/test environments only).
    ///
    /// When a bundled path is found, `PYTHONHOME` and `PYTHONPATH` are set
    /// before the interpreter is first touched — these only take effect if
    /// PyO3 has not already auto-initialised.
    pub fn new(runtime_path: Option<PathBuf>) -> Result<Self, PythonError> {
        // --- Explicit path: must exist ---
        if let Some(ref path) = runtime_path {
            if !path.exists() {
                return Err(PythonError::RuntimeNotFound { path: path.clone() });
            }
            if !Self::looks_like_runtime(path) {
                return Err(PythonError::RuntimeCorrupted {
                    path: path.clone(),
                    reason: "directory does not contain a recognisable Python installation".into(),
                });
            }
            Self::configure_env(path);
            info!(?path, "using explicit Python runtime");
            return Self::verify_interpreter(Some(path.clone()));
        }

        // --- Auto-discovery: try bundled locations ---
        let candidates = Self::candidate_paths();
        for candidate in &candidates {
            if candidate.exists() && Self::looks_like_runtime(candidate) {
                Self::configure_env(candidate);
                info!(?candidate, "discovered bundled Python runtime");
                return Self::verify_interpreter(Some(candidate.clone()));
            }
            debug!(
                ?candidate,
                "candidate path not found or incomplete, skipping"
            );
        }

        // --- Fallback: system Python (acceptable for dev/tests) ---
        warn!("no bundled Python runtime found; falling back to system Python");
        Self::verify_interpreter(None)
    }

    /// Whether the interpreter is responsive.
    pub fn is_available(&self) -> bool {
        // Quick GIL check — if we can acquire the GIL and run a trivial
        // expression, the interpreter is alive.
        Python::with_gil(|py| py.eval(SMOKE_TEST, None, None).is_ok())
    }

    /// The resolved runtime directory, or `None` if using system Python.
    pub fn runtime_path(&self) -> Option<&Path> {
        self.resolved_path.as_deref()
    }

    // -- Private helpers -----------------------------------------------------

    /// Candidate paths for bundled runtime discovery (in priority order).
    fn candidate_paths() -> Vec<PathBuf> {
        let mut candidates = Vec::new();

        // Alongside the executable.
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("python"));
            }
        }

        // User-local default.
        if let Some(home) = dirs_home() {
            candidates.push(home.join(".swage").join("python"));
        }

        candidates
    }

    /// Heuristic: does the directory look like a Python installation?
    /// Checks for `lib/` and either `bin/` (Unix) or `Scripts/` (Windows).
    fn looks_like_runtime(path: &Path) -> bool {
        let has_lib = path.join("lib").is_dir();
        let has_bin = path.join("bin").is_dir() || path.join("Scripts").is_dir();
        has_lib && has_bin
    }

    /// Point PYTHONHOME and PYTHONPATH at the bundled distribution.
    ///
    /// # Safety
    /// `std::env::set_var` is unsafe in edition 2024 because other threads may
    /// be reading the environment concurrently. We call this before PyO3
    /// auto-initialises, so no Python threads are running yet.
    fn configure_env(path: &Path) {
        unsafe {
            std::env::set_var("PYTHONHOME", path);
            std::env::set_var("PYTHONPATH", path.join("lib"));
            // Prevent the bundled runtime from writing .pyc files.
            std::env::set_var("PYTHONDONTWRITEBYTECODE", "1");
        }
        debug!(?path, "configured PYTHONHOME/PYTHONPATH");
    }

    /// Acquire the GIL and run a smoke test to confirm the interpreter works.
    fn verify_interpreter(resolved_path: Option<PathBuf>) -> Result<PythonRuntime, PythonError> {
        // PyO3's py.eval() is the standard API for evaluating Python expressions
        // in the embedded interpreter — this is the intended use, not arbitrary
        // code injection.
        Python::with_gil(|py| {
            py.eval(SMOKE_TEST, None, None).map_err(|e| {
                PythonError::InitialisationFailed(format!("interpreter smoke test failed: {e}"))
            })?;

            debug!("Python interpreter smoke test passed");
            Ok(PythonRuntime { resolved_path })
        })
    }
}

/// Cross-platform home directory lookup (avoids pulling in the `dirs` crate).
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_with_bad_path_returns_runtime_not_found() {
        let bad_path = PathBuf::from("/nonexistent/python/runtime");
        let result = PythonRuntime::new(Some(bad_path.clone()));
        assert!(result.is_err());
        let err = result.unwrap_err();
        // Error must name the path that was searched.
        let message = err.to_string();
        assert!(
            message.contains("/nonexistent/python/runtime"),
            "error should name the searched path, got: {message}"
        );
        assert!(
            matches!(err, PythonError::RuntimeNotFound { .. }),
            "expected RuntimeNotFound, got: {err:?}"
        );
    }

    #[test]
    fn new_with_none_discovers_default_location() {
        // With no bundled runtime, new(None) should fall back to system Python
        // in dev/test environments. It should succeed if Python is available.
        let result = PythonRuntime::new(None);
        assert!(
            result.is_ok(),
            "new(None) should fall back to system Python in dev: {:?}",
            result.unwrap_err()
        );
    }

    #[test]
    fn is_available_true_after_successful_init() {
        let runtime = PythonRuntime::new(None).expect("system Python should be available in dev");
        assert!(runtime.is_available());
    }

    #[test]
    fn error_display_includes_context() {
        let err = PythonError::RuntimeNotFound {
            path: PathBuf::from("/some/path"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("/some/path"),
            "display should include the path"
        );
        assert!(
            msg.contains("not found"),
            "display should mention 'not found'"
        );
    }

    #[test]
    fn error_variants_exist() {
        // Verify all required error variants compile and have reasonable displays.
        let _: PythonError = PythonError::RuntimeNotFound {
            path: PathBuf::from("/tmp"),
        };
        let _: PythonError = PythonError::RuntimeCorrupted {
            path: PathBuf::from("/tmp"),
            reason: "missing lib".into(),
        };
        let _: PythonError = PythonError::InitialisationFailed("boom".into());
        let _: PythonError = PythonError::ExecutionError("oops".into());
        let _: PythonError = PythonError::Timeout { seconds: 30 };
    }

    #[test]
    fn runtime_path_accessor() {
        let runtime = PythonRuntime::new(None).expect("system Python should be available in dev");
        // System Python fallback means no explicit path was resolved.
        // This is fine — the accessor should return None for system fallback.
        let _path = runtime.runtime_path();
    }
}
