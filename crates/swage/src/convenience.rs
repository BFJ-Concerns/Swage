//! Convenience constructors for common component configurations.
//!
//! These functions cover the typical setup path — using them is optional;
//! you can construct each component directly if you need finer control.

use std::path::Path;

use swage_core::StorageError;
use swage_provider::builder::ProviderChainBuilder;
use swage_provider::circuit_breaker::CircuitBreakerConfig;
use swage_provider::provider::LlmProvider;
use swage_provider::retry::RetryConfig;
use swage_safety::LeakDetector;
use swage_storage::SqliteStorage;

// ---------------------------------------------------------------------------
// provider_with_reliability
// ---------------------------------------------------------------------------

/// Wrap a provider in a retry + circuit-breaker reliability stack with
/// sensible defaults.
///
/// The stack is composed as:
///
/// ```text
/// RetryProvider (3 retries, exponential backoff)
///   └─ CircuitBreakerProvider (opens after 5 failures, 30 s recovery)
///       └─ your provider
/// ```
///
/// Use [`swage_provider::builder::ProviderChainBuilder`] directly when you
/// need custom thresholds or a failover across multiple backends.
pub fn provider_with_reliability(
    provider: impl LlmProvider + 'static,
) -> Box<dyn LlmProvider> {
    ProviderChainBuilder::new()
        .provider(provider)
        .with_circuit_breaker(CircuitBreakerConfig::default())
        .with_retry(RetryConfig::default())
        .build()
}

// ---------------------------------------------------------------------------
// sqlite_storage
// ---------------------------------------------------------------------------

/// Open a SQLite storage backend at the given path.
///
/// Pass `":memory:"` for a transient in-memory database (useful for tests
/// and one-shot scripts).
///
/// # Errors
///
/// Returns [`StorageError`] if the database file cannot be opened or the
/// schema migration fails.
pub fn sqlite_storage(path: impl AsRef<Path>) -> Result<SqliteStorage, StorageError> {
    let path = path.as_ref();
    // The special SQLite in-memory DSN — delegate to the dedicated constructor
    // which skips file-system operations entirely.
    if path == Path::new(":memory:") {
        SqliteStorage::in_memory()
    } else {
        SqliteStorage::open(path)
    }
}

// ---------------------------------------------------------------------------
// standard_leak_detector
// ---------------------------------------------------------------------------

/// Create a [`LeakDetector`] with the default credential pattern library.
///
/// The default library detects common API key formats, tokens, and
/// high-entropy secrets. Use [`LeakDetector::new`] to add custom patterns.
pub fn standard_leak_detector() -> LeakDetector {
    LeakDetector::default()
}
