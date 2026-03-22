//! Runtime utilities for bridging async and sync code.
//!
//! Provides `run_sync()` to execute async code from synchronous contexts by
//! creating an internal Tokio runtime. Detects nested runtime attempts and
//! returns an error instead of panicking.

use crate::error::RuntimeError;
use std::future::Future;

/// Executes an async future synchronously by creating an internal Tokio runtime.
///
/// This function is intended for use in synchronous contexts that need to call
/// async code. It creates a new Tokio runtime, runs the future to completion,
/// and returns the result.
///
/// # Errors
///
/// Returns `RuntimeError::NestedRuntime` if called from within an existing
/// Tokio runtime context. This prevents the panic that would occur from
/// attempting to create a nested runtime.
///
/// # Examples
///
/// ```
/// use swage_core::runtime::run_sync;
///
/// async fn async_computation() -> i32 {
///     42
/// }
///
/// fn sync_caller() -> Result<i32, swage_core::error::RuntimeError> {
///     run_sync(async_computation())
/// }
/// ```
pub fn run_sync<F>(future: F) -> Result<F::Output, RuntimeError>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    // Check if we're already inside a Tokio runtime context.
    // If Handle::try_current() succeeds, we're in a runtime and must not create another.
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(RuntimeError::NestedRuntime);
    }

    // Safe to create a new runtime since we're not in an existing one.
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| RuntimeError::CreationFailed(e.to_string()))?;

    Ok(runtime.block_on(future))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_sync_executes_future() {
        async fn test_future() -> i32 {
            42
        }

        let result = run_sync(test_future());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn run_sync_detects_nested_runtime() {
        // Create a runtime and test nested call detection.
        let runtime = tokio::runtime::Runtime::new().unwrap();

        runtime.block_on(async {
            async fn inner_future() -> i32 {
                1
            }

            let result = run_sync(inner_future());
            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), RuntimeError::NestedRuntime));
        });
    }
}
