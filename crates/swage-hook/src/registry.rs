//! Hook registration and dispatch engine.
//!
//! Manages a collection of registered hooks, dispatching lifecycle events
//! to matching hooks in priority order with timeout enforcement and
//! configurable failure handling.

use std::sync::Arc;
use std::time::Duration;

use regex::Regex;
use tracing::{debug, warn};

use crate::types::{FailureMode, Hook, HookEvent, HookOutcome};

/// Default timeout for hook execution (30 seconds).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Associates a hook implementation with dispatch configuration.
///
/// Controls when and how a hook participates in event dispatch: which events
/// it responds to, its execution priority, timeout budget, and what happens
/// if it fails.
pub struct HookRegistration {
    /// The hook implementation.
    pub hook: Arc<dyn Hook>,

    /// Predicate filtering which event variants this hook responds to.
    /// When `None`, the hook responds to all events.
    pub event_filter: Option<fn(&HookEvent) -> bool>,

    /// Optional regex matched against the event name (`event_name()`).
    /// When present, the hook only fires if the pattern matches.
    pub matcher: Option<Regex>,

    /// Execution priority (lower = higher priority, default 0).
    pub priority: i32,

    /// Behaviour on timeout or panic (default FailOpen).
    pub failure_mode: FailureMode,

    /// Maximum execution time (default 30s).
    pub timeout: Duration,
}

impl HookRegistration {
    /// Creates a registration with sensible defaults.
    ///
    /// Defaults: priority 0, FailOpen, 30s timeout, no filters.
    pub fn new(hook: Arc<dyn Hook>) -> Self {
        Self {
            hook,
            event_filter: None,
            matcher: None,
            priority: 0,
            failure_mode: FailureMode::FailOpen,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Sets the execution priority (builder pattern).
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Sets an event filter predicate (builder pattern).
    pub fn with_event_filter(mut self, filter: fn(&HookEvent) -> bool) -> Self {
        self.event_filter = Some(filter);
        self
    }

    /// Sets a regex matcher against event names (builder pattern).
    pub fn with_matcher(mut self, matcher: Regex) -> Self {
        self.matcher = Some(matcher);
        self
    }

    /// Sets the failure mode (builder pattern).
    pub fn with_failure_mode(mut self, mode: FailureMode) -> Self {
        self.failure_mode = mode;
        self
    }

    /// Sets the timeout duration (builder pattern).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Returns true if this registration should handle the given event.
    fn matches(&self, event: &HookEvent) -> bool {
        // Check the discriminant-level event filter
        if let Some(filter) = &self.event_filter
            && !filter(event)
        {
            return false;
        }

        // Check the regex matcher against the event name
        if let Some(matcher) = &self.matcher
            && !matcher.is_match(event.event_name())
        {
            return false;
        }

        true
    }
}

/// Registry managing hook lifecycle dispatch.
///
/// Each instance is independent, supporting per-agent scoping. Hooks are
/// dispatched sequentially in priority order with timeout enforcement.
pub struct HookRegistry {
    hooks: Vec<HookRegistration>,
}

/// Result of traced hook dispatch.
pub struct TracedDispatch {
    pub outcome: HookOutcome,
    pub modified_by_hooks: Vec<String>,
}

impl HookRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Registers a hook for lifecycle dispatch.
    pub fn register(&mut self, registration: HookRegistration) {
        self.hooks.push(registration);
    }

    /// Dispatches an event to all matching hooks in priority order.
    ///
    /// Hooks are sorted by priority (lower = higher priority) with stable
    /// ordering for ties (registration order preserved). Execution is
    /// sequential — each hook sees the (potentially modified) event from
    /// the previous hook.
    ///
    /// - `Block` stops the chain immediately and returns the block reason.
    /// - `ContinueModified` replaces the event for subsequent hooks.
    /// - Timeouts are handled per the hook's `failure_mode`.
    pub async fn dispatch(&self, event: HookEvent) -> HookOutcome {
        self.dispatch_traced(event).await.outcome
    }

    /// Dispatches an event while recording which hooks modified it.
    ///
    /// The returned hook-name list preserves execution order and includes only
    /// hooks that returned `ContinueModified`.
    pub async fn dispatch_traced(&self, event: HookEvent) -> TracedDispatch {
        if self.hooks.is_empty() {
            return TracedDispatch {
                outcome: HookOutcome::Continue,
                modified_by_hooks: Vec::new(),
            };
        }

        // Build a priority-sorted index. Stable sort preserves registration
        // order for equal priorities.
        let mut indices: Vec<usize> = (0..self.hooks.len()).collect();
        indices.sort_by_key(|&i| self.hooks[i].priority);

        let mut current_event = event;
        let mut was_modified = false;
        let mut modified_by_hooks = Vec::new();

        // Tracks the first RequestApproval seen. Block takes precedence over this
        // and exits the loop immediately. Among multiple approvals, the
        // highest-priority hook (which runs first) wins.
        let mut pending_approval: Option<HookOutcome> = None;

        for &idx in &indices {
            let reg = &self.hooks[idx];

            // Skip hooks that don't match this event
            if !reg.matches(&current_event) {
                continue;
            }

            let hook_name = reg.hook.name().to_owned();
            debug!(hook = %hook_name, event = %current_event.event_name(), "dispatching hook");

            let outcome = self.execute_with_timeout(reg, &current_event).await;

            match outcome {
                HookOutcome::Continue => {
                    // Proceed to next hook with the same event
                }
                HookOutcome::ContinueModified(modified) => {
                    debug!(hook = %hook_name, "hook modified event");
                    current_event = *modified;
                    was_modified = true;
                    modified_by_hooks.push(hook_name);
                }
                HookOutcome::Block(reason) => {
                    // Block is the highest-priority outcome — exit immediately,
                    // discarding any pending approval request.
                    debug!(hook = %hook_name, reason = %reason, "hook blocked execution");
                    return TracedDispatch {
                        outcome: HookOutcome::Block(reason),
                        modified_by_hooks,
                    };
                }
                approval @ HookOutcome::RequestApproval { .. } => {
                    // Only record the first approval: hooks run in priority order
                    // so the first one seen is the highest-priority requester.
                    if pending_approval.is_none() {
                        debug!(hook = %hook_name, "hook requested approval");
                        pending_approval = Some(approval);
                    } else {
                        debug!(hook = %hook_name, "hook requested approval (ignored — higher-priority approval already pending)");
                    }
                }
            }
        }

        // Approval outranks Continue/ContinueModified — surface it if present.
        if let Some(approval) = pending_approval {
            return TracedDispatch {
                outcome: approval,
                modified_by_hooks,
            };
        }

        // Propagate the final event state to the caller so they can
        // use the modified content (e.g. BeforeInbound content rewriting).
        let outcome = if was_modified {
            HookOutcome::ContinueModified(Box::new(current_event))
        } else {
            HookOutcome::Continue
        };

        TracedDispatch {
            outcome,
            modified_by_hooks,
        }
    }

    /// Executes a single hook with timeout enforcement.
    ///
    /// If the hook exceeds its timeout, the outcome is determined by the
    /// registration's failure mode. Panicking hooks are caught and treated
    /// identically to timeouts.
    // The timeout wraps a JoinHandle, so timeout expiry detaches the hook task
    // rather than aborting it mid-await. The failure mode below decides what the
    // dispatcher reports while the detached hook finishes independently.
    #[allow(clippy::disallowed_methods)]
    async fn execute_with_timeout(&self, reg: &HookRegistration, event: &HookEvent) -> HookOutcome {
        let hook = reg.hook.clone();
        let event_clone = event.clone();
        let timeout_duration = reg.timeout;
        let hook_name = reg.hook.name().to_owned();

        // Spawn the hook in a task so panics are caught by the JoinError
        let handle = tokio::spawn(async move { hook.execute(&event_clone).await });

        match tokio::time::timeout(timeout_duration, handle).await {
            // Hook completed within timeout
            Ok(Ok(outcome)) => outcome,

            // Hook panicked — treat as failure
            Ok(Err(join_error)) => {
                warn!(
                    hook = %hook_name,
                    error = %join_error,
                    "hook panicked during execution"
                );
                self.failure_outcome(&hook_name, reg.failure_mode, "panic")
            }

            // Hook timed out
            Err(_elapsed) => {
                warn!(
                    hook = %hook_name,
                    timeout_ms = timeout_duration.as_millis(),
                    "hook exceeded timeout"
                );
                self.failure_outcome(&hook_name, reg.failure_mode, "timeout")
            }
        }
    }

    /// Maps a failure (timeout or panic) to the appropriate outcome
    /// based on the hook's configured failure mode.
    fn failure_outcome(&self, hook_name: &str, mode: FailureMode, cause: &str) -> HookOutcome {
        match mode {
            FailureMode::FailOpen => {
                debug!(hook = %hook_name, cause = %cause, "FailOpen: continuing after failure");
                HookOutcome::Continue
            }
            FailureMode::FailClosed => {
                let reason = format!("hook '{hook_name}' {cause}");
                debug!(hook = %hook_name, cause = %cause, "FailClosed: blocking after failure");
                HookOutcome::Block(reason)
            }
        }
    }
}

impl Default for HookRegistry {
    fn default() -> Self {
        Self::new()
    }
}
