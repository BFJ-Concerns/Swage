//! Rate limiter — per-tool, per-agent sliding window enforcement.
//!
//! Tracks invocation timestamps per `(agent_id, tool_name)` pair and enforces
//! configurable per-minute and per-hour sliding windows. Denied calls never
//! consume capacity — only `Allowed` calls are recorded.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Per-agent, per-tool rate limit configuration.
///
/// Either or both windows can be configured. When both are set, both must pass
/// for the call to proceed. `None` on a field means that window is unlimited.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// Maximum calls allowed within any rolling 60-second window.
    pub requests_per_minute: Option<u32>,
    /// Maximum calls allowed within any rolling 3600-second window.
    pub requests_per_hour: Option<u32>,
}

// ---------------------------------------------------------------------------
// Decision
// ---------------------------------------------------------------------------

/// Outcome of a rate-limit check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitDecision {
    /// The call is within limits and has been recorded.
    Allowed,
    /// The call is blocked. The caller should wait at least `retry_after`
    /// before trying again.
    Limited { retry_after: Duration },
}

// ---------------------------------------------------------------------------
// Limiter
// ---------------------------------------------------------------------------

/// Sliding-window rate limiter, keyed by `(agent_id, tool_name)`.
///
/// Uses a `Mutex<HashMap>` for interior mutability so the same instance can
/// be shared across async tasks via `Arc<RateLimiter>` without `&mut` access.
pub struct RateLimiter {
    /// Maps `(agent_id, tool_name)` to the timestamps of recorded invocations,
    /// kept in insertion order (oldest first after pruning).
    state: Mutex<HashMap<(String, String), Vec<Instant>>>,
}

impl RateLimiter {
    /// Create a new, empty rate limiter.
    pub fn new() -> Self {
        RateLimiter {
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Check whether the call is within limits and, if so, record it.
    ///
    /// Returns `Allowed` and records the timestamp when the call is within all
    /// configured windows. Returns `Limited { retry_after }` without recording
    /// when any limit is exceeded — denied calls never consume capacity.
    ///
    /// When both windows are hit simultaneously, `retry_after` reflects the
    /// more restrictive (larger) duration of the two.
    pub fn check(
        &self,
        agent_id: &str,
        tool_name: &str,
        config: &RateLimitConfig,
    ) -> RateLimitDecision {
        // No limits configured — fast path, no lock needed.
        if config.requests_per_minute.is_none() && config.requests_per_hour.is_none() {
            return RateLimitDecision::Allowed;
        }

        let now = Instant::now();
        let minute_window = Duration::from_secs(60);
        let hour_window = Duration::from_secs(3600);

        // The largest window we need to retain entries for.
        let max_window = if config.requests_per_hour.is_some() {
            hour_window
        } else {
            minute_window
        };

        let mut state = self
            .state
            .lock()
            .expect("rate-limit state lock should not be poisoned");
        let timestamps = state
            .entry((agent_id.to_owned(), tool_name.to_owned()))
            .or_default();

        // Prune entries that have aged out of the largest relevant window.
        timestamps.retain(|&t| now.duration_since(t) < max_window);

        // Check each configured window. Track the most restrictive retry_after
        // across all windows that are currently over limit.
        let mut limiting_retry: Option<Duration> = None;

        if let Some(limit) = config.requests_per_minute {
            let count = timestamps
                .iter()
                .filter(|&&t| now.duration_since(t) < minute_window)
                .count();

            if count >= limit as usize {
                // The oldest entry in this window will age out first.
                let oldest = timestamps
                    .iter()
                    .filter(|&&t| now.duration_since(t) < minute_window)
                    .min()
                    .copied()
                    .expect("count > 0 so at least one entry exists");
                let retry = minute_window.saturating_sub(now.duration_since(oldest));
                limiting_retry = Some(limiting_retry.map_or(retry, |e: Duration| e.max(retry)));
            }
        }

        if let Some(limit) = config.requests_per_hour {
            let count = timestamps.len(); // already pruned to max_window (hour)
            if count >= limit as usize {
                let oldest = timestamps.iter().min().copied().expect("count > 0");
                let retry = hour_window.saturating_sub(now.duration_since(oldest));
                limiting_retry = Some(limiting_retry.map_or(retry, |e: Duration| e.max(retry)));
            }
        }

        if let Some(retry_after) = limiting_retry {
            return RateLimitDecision::Limited { retry_after };
        }

        // Within all limits — record this invocation and allow.
        timestamps.push(now);
        RateLimitDecision::Allowed
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    // -- RateLimitConfig defaults --------------------------------------------

    #[test]
    fn rate_limit_config_default_is_unlimited() {
        let config = RateLimitConfig::default();
        assert!(config.requests_per_minute.is_none());
        assert!(config.requests_per_hour.is_none());
    }

    // -- Allowed when no limits configured -----------------------------------

    #[test]
    fn unlimited_config_always_allows() {
        let limiter = RateLimiter::new();
        let config = RateLimitConfig::default();

        for _ in 0..100 {
            assert_eq!(
                limiter.check("agent-1", "tool-a", &config),
                RateLimitDecision::Allowed,
            );
        }
    }

    // -- Per-minute limit ----------------------------------------------------

    #[test]
    fn per_minute_allows_within_limit() {
        let limiter = RateLimiter::new();
        let config = RateLimitConfig {
            requests_per_minute: Some(3),
            requests_per_hour: None,
        };

        for i in 0..3 {
            assert_eq!(
                limiter.check("agent-1", "tool-a", &config),
                RateLimitDecision::Allowed,
                "call {i} should be allowed"
            );
        }
    }

    #[test]
    fn per_minute_limits_when_exceeded() {
        let limiter = RateLimiter::new();
        let config = RateLimitConfig {
            requests_per_minute: Some(2),
            requests_per_hour: None,
        };

        // Exhaust the budget.
        assert_eq!(
            limiter.check("agent-1", "tool-a", &config),
            RateLimitDecision::Allowed
        );
        assert_eq!(
            limiter.check("agent-1", "tool-a", &config),
            RateLimitDecision::Allowed
        );

        // Third call should be limited.
        match limiter.check("agent-1", "tool-a", &config) {
            RateLimitDecision::Limited { retry_after } => {
                assert!(
                    retry_after > Duration::ZERO,
                    "retry_after should be positive"
                );
                assert!(
                    retry_after <= Duration::from_secs(60),
                    "retry_after should not exceed window: {retry_after:?}"
                );
            }
            RateLimitDecision::Allowed => panic!("expected Limited, got Allowed"),
        }
    }

    // -- Denied calls do not consume capacity --------------------------------

    #[test]
    fn limited_calls_do_not_consume_capacity() {
        let limiter = RateLimiter::new();
        let config = RateLimitConfig {
            requests_per_minute: Some(1),
            requests_per_hour: None,
        };

        // Exhaust the single-call budget.
        assert_eq!(
            limiter.check("agent-1", "tool-a", &config),
            RateLimitDecision::Allowed
        );

        // Two more limited calls — neither should increment the counter.
        assert!(matches!(
            limiter.check("agent-1", "tool-a", &config),
            RateLimitDecision::Limited { .. }
        ));
        assert!(matches!(
            limiter.check("agent-1", "tool-a", &config),
            RateLimitDecision::Limited { .. }
        ));

        // The counter should still be at 1 (only the Allowed call recorded).
        // Verify by relaxing the limit — with limit=2 and 1 recorded, next call is Allowed.
        let relaxed = RateLimitConfig {
            requests_per_minute: Some(2),
            requests_per_hour: None,
        };
        assert_eq!(
            limiter.check("agent-1", "tool-a", &relaxed),
            RateLimitDecision::Allowed,
            "only 1 entry recorded despite multiple Limited calls"
        );
    }

    // -- Per-agent isolation -------------------------------------------------

    #[test]
    fn different_agents_have_independent_counters() {
        let limiter = RateLimiter::new();
        let config = RateLimitConfig {
            requests_per_minute: Some(1),
            requests_per_hour: None,
        };

        // agent-1 exhausts its budget.
        assert_eq!(
            limiter.check("agent-1", "tool-a", &config),
            RateLimitDecision::Allowed
        );
        assert!(matches!(
            limiter.check("agent-1", "tool-a", &config),
            RateLimitDecision::Limited { .. }
        ));

        // agent-2 is unaffected.
        assert_eq!(
            limiter.check("agent-2", "tool-a", &config),
            RateLimitDecision::Allowed,
            "agent-2 should have its own independent counter"
        );
    }

    // -- Per-tool isolation --------------------------------------------------

    #[test]
    fn different_tools_have_independent_counters() {
        let limiter = RateLimiter::new();
        let config = RateLimitConfig {
            requests_per_minute: Some(1),
            requests_per_hour: None,
        };

        // tool-a exhausts its budget.
        assert_eq!(
            limiter.check("agent-1", "tool-a", &config),
            RateLimitDecision::Allowed
        );
        assert!(matches!(
            limiter.check("agent-1", "tool-a", &config),
            RateLimitDecision::Limited { .. }
        ));

        // tool-b is unaffected.
        assert_eq!(
            limiter.check("agent-1", "tool-b", &config),
            RateLimitDecision::Allowed,
            "tool-b should have its own independent counter"
        );
    }

    // -- Per-hour limit ------------------------------------------------------

    #[test]
    fn per_hour_limits_when_exceeded() {
        let limiter = RateLimiter::new();
        let config = RateLimitConfig {
            requests_per_minute: None,
            requests_per_hour: Some(2),
        };

        assert_eq!(
            limiter.check("agent-1", "tool-a", &config),
            RateLimitDecision::Allowed
        );
        assert_eq!(
            limiter.check("agent-1", "tool-a", &config),
            RateLimitDecision::Allowed
        );

        match limiter.check("agent-1", "tool-a", &config) {
            RateLimitDecision::Limited { retry_after } => {
                assert!(retry_after > Duration::ZERO);
                assert!(
                    retry_after <= Duration::from_secs(3600),
                    "retry_after should not exceed hour window: {retry_after:?}"
                );
            }
            RateLimitDecision::Allowed => panic!("expected Limited, got Allowed"),
        }
    }

    // -- Both windows: more restrictive retry_after wins ---------------------

    #[test]
    fn more_restrictive_retry_after_wins_when_both_limits_hit() {
        let limiter = RateLimiter::new();
        // Both limits set to 1 — both will be hit on the second call.
        // The hour window produces a larger retry_after than the minute window.
        let config = RateLimitConfig {
            requests_per_minute: Some(1),
            requests_per_hour: Some(1),
        };

        assert_eq!(
            limiter.check("agent-1", "tool-a", &config),
            RateLimitDecision::Allowed
        );

        match limiter.check("agent-1", "tool-a", &config) {
            RateLimitDecision::Limited { retry_after } => {
                // Both windows are hit; retry_after should reflect the hour window.
                assert!(
                    retry_after <= Duration::from_secs(3600),
                    "retry_after should be within the hour window"
                );
                assert!(retry_after > Duration::ZERO);
                // The hour retry should be >= minute retry (more restrictive wins).
                assert!(
                    retry_after > Duration::from_secs(59),
                    "should reflect the larger hour window, got {retry_after:?}"
                );
            }
            RateLimitDecision::Allowed => panic!("expected Limited, got Allowed"),
        }
    }
}
